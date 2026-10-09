#!/usr/bin/env bash
# Integration tests: build the fixtures with real nix and check the results.
# They need builtins.exec and, on the first run, the network, so they run
# outside the Nix sandbox.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
flake="path:$root"
exec_opt=(--option allow-unsafe-native-code-during-evaluation true)

# Everything the tests write: reference builds, copies of fixtures, and the
# build of the conformance example. Nothing is built inside the repository.
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# build <attribute under legacyPackages.<system>>: prints the output path.
build() {
  nix build "${exec_opt[@]}" --no-link --print-out-paths "$flake#$1"
}

# check_run <fixture> <binary> <expected stdout>
check_run() {
  local out got
  out="$(build "fixtures.$1")"
  got="$("$out/bin/$2")"
  [ "$got" = "$3" ] || fail "$1: $2 printed '$got', want '$3'"
  echo "ok: $1: $2 runs"
}

# check_bins <fixture> <binaries, space separated>
# The application installs exactly the binaries and examples selected.
check_bins() {
  local out got
  out="$(build "fixtures.$1")"
  got="$(ls "$out/bin" | tr '\n' ' ' | sed 's/ $//')"
  [ "$got" = "$2" ] || fail "$1: installs [$got], want [$2]"
  echo "ok: $1: installs [$2]"
}

# check_conformance <fixture> <shell> <cargo build arguments...>
# Every rustc invocation and build-script run must be what `cargo build -vv`
# runs for the same source, apart from what the spec lists. The reference
# build runs in the named flake shell, with a cargo home that shares only
# the download cache, as resolve's does.
check_conformance() {
  local fixture="$1" shell="$2" src dir records
  shift 2
  src="$(nix eval --raw "${exec_opt[@]}" "$flake#fixtures.$fixture.src")"
  dir="$work/conformance-$fixture"
  mkdir -p "$dir/home" "$dir/src"
  ln -s "${CARGO_HOME:-$HOME/.cargo}/registry" "$dir/home/registry"
  cp -R "$src/." "$dir/src/"
  chmod -R u+w "$dir/src"
  (cd "$dir/src" &&
    env -u RUSTFLAGS -u CARGO_BUILD_TARGET -u RUSTC_WRAPPER \
      CARGO_HOME="$dir/home" CARGO_TARGET_DIR="$dir/target" \
      nix develop "$flake#$shell" --command cargo build -vv --locked "$@" >/dev/null 2>"$dir/cargo.log") ||
    fail "$fixture: the reference cargo build failed; see $(tail -n 5 "$dir/cargo.log")"
  records="$(build "fixtures.$fixture.unitRecords")"
  CARGO_TARGET_DIR="$work/target" nix develop "$flake" --command \
    cargo run --quiet --manifest-path "$root/Cargo.toml" --example conformance -- "$dir/cargo.log" "$records" ||
    fail "$fixture: rostnix does not run what cargo runs (differences above)"
  rm -rf "$dir"
  echo "ok: $fixture: every invocation is cargo's"
}

# check_incremental <fixture directory or store path> <arguments> <file to append to> <what must change>
# Evaluates the project from two copies that differ in one file and lists
# the derivation names of the units that differ.
check_incremental() {
  local src="$1" args="$2" file="$3" want="$4" a b got
  a="$(mktemp -d "$work/a.XXXXXX")"
  b="$(mktemp -d "$work/b.XXXXXX")"
  cp -R "$src/." "$a/"
  cp -R "$src/." "$b/"
  chmod -R u+w "$a" "$b"
  printf '\n// edited\n' >>"$b/$file"
  got="$(nix eval --impure --raw "${exec_opt[@]}" --expr "
    let
      flake = builtins.getFlake \"$flake\";
      pkgs = flake.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem};
      inherit (flake.legacyPackages.\${builtins.currentSystem}) rustEnv;
      build = src: rustEnv.buildRustApplication ({ pname = \"incremental\"; inherit src; } // $args);
      a = build $a;
      b = build $b;
      changed = builtins.filter
        (n: a.units.\${n}.drvPath != b.units.\${n}.drvPath)
        (builtins.attrNames a.units);
    in builtins.concatStringsSep \" \" (pkgs.lib.unique (map (n: a.units.\${n}.name) changed))
  ")"
  rm -rf "$a" "$b"
  [ "$got" = "$want" ] || fail "$(basename "$src"): editing $file changed [$got], want [$want]"
  echo "ok: $(basename "$src"): editing $file changes [$want]"
}

# Without the option, evaluation must say what to set. The option is
# turned off explicitly, in case nix.conf turns it on.
check_exec_error() {
  local msg
  if msg="$(nix build --option allow-unsafe-native-code-during-evaluation false \
    --no-link "$flake#fixtures.hello" 2>&1)"; then
    fail "building without builtins.exec succeeded"
  fi
  case "$msg" in
    *allow-unsafe-native-code-during-evaluation*) echo "ok: missing builtins.exec is explained" ;;
    *) fail "unhelpful error without builtins.exec: $msg" ;;
  esac
}

# fixture_with <fixture directory> <arguments> <expression over app>
# Evaluates the expression with app bound to the fixture built with the
# given arguments; pkgs is the flake's nixpkgs.
fixture_with() {
  nix eval --impure --raw "${exec_opt[@]}" --expr "
    let
      flake = builtins.getFlake \"$flake\";
      pkgs = flake.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem};
      app = flake.legacyPackages.\${builtins.currentSystem}.rustEnv.buildRustApplication
        ({ pname = \"with\"; src = $root/tests/fixtures/$1; } // $2);
    in $3"
}

# A selection without a binary or an example has nothing to install, which
# is what a library-only project gives by default.
check_no_executable_error() {
  local msg
  if msg="$(fixture_with workspace '{ packages = [ "ws-core" ]; }' 'app.drvPath' 2>&1)"; then
    fail "a selection with no binary and no example was accepted"
  fi
  case "$msg" in
    *'`bins` or `examples`'*) echo "ok: a selection with nothing to install is explained" ;;
    *) fail "unhelpful error for a selection with nothing to install: $msg" ;;
  esac
}

# A misspelt attribute must not be ignored.
check_override_typo() {
  local msg
  if msg="$(fixture_with buildscript '{ crateOverrides.bs-native.buildInput = [ ]; }' 'app.drvPath' 2>&1)"; then
    fail "crateOverrides: an unknown attribute was accepted"
  fi
  case "$msg" in
    *'crateOverrides.bs-native.buildInput;'*) echo "ok: crateOverrides: an unknown attribute is rejected by name" ;;
    *) fail "crateOverrides: unhelpful error for an unknown attribute: $msg" ;;
  esac
}

# An entry that names no package changes nothing, so it is most likely a
# mistake. It is a warning, since the key may match on another platform.
check_override_unmatched() {
  local msg
  msg="$(fixture_with buildscript '{
      crateOverrides = {
        bs-nativ.env.X = "1";
        libz-sys.buildInputs = [ pkgs.zlib ];
        consumer.extraSrc = [ "shared" ];
      };
    }' 'app.drvPath' 2>&1)" || fail "crateOverrides: a key that names no package stops the evaluation: $msg"
  case "$msg" in
    *'crateOverrides.bs-nativ names no package'*) ;;
    *) fail "crateOverrides: no warning for the key that names no package: $msg" ;;
  esac
  case "$msg" in
    *'crateOverrides.libz-sys'* | *'crateOverrides.consumer'*)
      fail "crateOverrides: the warning names a key that a package takes: $msg" ;;
  esac
  echo "ok: crateOverrides: a key that names no package is warned about"
}

# Libraries of an overridden package reach every unit that links it, and
# only those.
check_override_inputs() {
  local got
  got="$(fixture_with buildscript '{
      crateOverrides = {
        libz-sys.buildInputs = [ pkgs.zlib ];
        consumer.extraSrc = [ "shared" ];
      };
    }' '
      let
        units = builtins.attrValues app.units;
        named = name: builtins.filter (u: u.name == name) units;
        inputs = name: toString (map (i: i.pname) (builtins.head (named name)).buildInputs);
      in "bin: ${inputs "rustbin-consumer"}; script: ${inputs "rustbs-bs-native-0.1.0"}"
    ')" || fail "crateOverrides: buildInputs do not evaluate"
  [ "$got" = "bin: zlib; script: " ] || fail "crateOverrides: units got the inputs [$got], want [bin: zlib; script: ]"
  echo "ok: crateOverrides: buildInputs reach the units that link the package"
}

# With one executable, nix run finds it through meta.mainProgram.
check_main_program() {
  local got
  got="$(nix eval "${exec_opt[@]}" --raw "$flake#fixtures.$1.meta.mainProgram")" ||
    fail "$1: meta.mainProgram does not evaluate"
  [ "$got" = "$2" ] || fail "$1: meta.mainProgram is '$got', want '$2'"
  echo "ok: $1: meta.mainProgram is $2"
}

# The download derivation normally never runs, because resolve pre-seeds its
# output. Force it to run and let nix compare the result with the store.
check_fetch_fallback() {
  local version
  version="$(awk -v name="$2" '$0 == "name = \"" name "\"" { getline; gsub(/version = |"/, ""); print; exit }' \
    "$root/tests/fixtures/$1/Cargo.lock")"
  [ -n "$version" ] || fail "$1: $2 is not in Cargo.lock"
  nix build "${exec_opt[@]}" --rebuild --no-link "$flake#fixtures.$1.graph.sources.\"$2-$version\".crate" ||
    fail "$1: downloading $2 $version does not reproduce the pre-seeded file"
  echo "ok: $1: downloading $2 $version reproduces the pre-seeded file"
}

# check_no_intermediate_refs <fixture>
# The result must not keep a source tree or a unit alive.
check_no_intermediate_refs() {
  local out refs
  out="$(build "fixtures.$1")"
  refs="$(nix-store --query --references "$out")"
  if grep -E -- '-(rustsrc|rustlib|rustmacro|rustbs|rustbsrun|rustbin)-' <<<"$refs"; then
    fail "$1: the result refers to the build inputs listed above"
  fi
  echo "ok: $1: the result refers to no source tree or unit"
}

# check_stdenv <fixture>
# Only units that link, and build-script runs, build with stdenv and its C
# compiler; a library keeps the tool as its builder.
check_stdenv() {
  local got
  got="$(nix eval "${exec_opt[@]}" --raw "$flake#fixtures.$1.units" --apply '
    units:
    let
      prefixes = set: builtins.concatStringsSep " " (builtins.attrNames (builtins.listToAttrs
        (map (u: { name = builtins.head (builtins.split "-" u.name); value = true; }) set)));
      all = builtins.attrValues units;
    in "stdenv: ${prefixes (builtins.filter (u: u ? stdenv) all)}; bare: ${prefixes (builtins.filter (u: !(u ? stdenv)) all)}"
  ')" || fail "$1: units do not evaluate"
  [ "$got" = "stdenv: rustbin rustbs rustbsrun rustmacro; bare: rustlib" ] ||
    fail "$1: builders are [$got], want [stdenv: rustbin rustbs rustbsrun rustmacro; bare: rustlib]"
  echo "ok: $1: only libraries build without stdenv"
}

check_run hello hello '{"greeting":"hello","n":42}'
check_conformance hello default
check_main_program hello hello
check_stdenv hello
check_no_intermediate_refs hello
check_fetch_fallback hello anyhow

# A workspace: the default selection is every member, and a narrower one
# with a feature builds one binary.
check_run workspace ws-app "hello from ws-app: 3"
check_run workspace ws-tool "ws-tool ok 7"
check_bins workspace "ws-app ws-tool"
check_conformance workspace default
check_run workspace-shout ws-app "HELLO FROM WS-APP: 3"
check_bins workspace-shout "ws-app"
check_conformance workspace-shout default --package ws-app --bin ws-app --features shout

# Build scripts. The note comes from an override's env, the message from a
# file an override's extraSrc adds, and pc from the pkg-config and zlib that
# overrides supply.
zlib_version="$(nix eval --raw "$flake#fixtureShell.buildInputs" --apply 'inputs: (builtins.head inputs).version')"
check_run buildscript consumer \
  "add=5 answer=42 note=from-override generated=from-build-script cfg=yes old=yes msg=shared-message zlib=ok pc=$zlib_version"
ROSTNIX_FIXTURE_NOTE=from-override check_conformance buildscript fixtureShell
check_no_intermediate_refs buildscript
check_override_typo
check_override_unmatched
check_override_inputs

# One project under four profiles.
for profile in release thin nolto dev; do
  check_run "profiles-$profile" profiles "profiles ok 12345"
  check_conformance "profiles-$profile" default --profile "$profile"
done

# Patient zero.
core_rs="$(build fixtures.core-rs)"
"$core_rs/bin/amber-store" --help >/dev/null || fail "core-rs: amber-store --help fails"
mkdir -p "$work/tree/sub" && echo one >"$work/tree/a.txt" && echo two >"$work/tree/sub/b.txt"
key="$("$core_rs/bin/amber-store" --store "$work/store" ingest "$work/tree")"
"$core_rs/bin/amber-store" --store "$work/store" restore "$key" "$work/restored"
diff -r "$work/tree" "$work/restored" || fail "core-rs: amber-store does not restore what it ingested"
echo "ok: core-rs: amber-store ingests and restores a tree"
check_conformance core-rs default --profile release --example amber-store
check_no_intermediate_refs core-rs

# rostnix builds itself.
self="$(build fixtures.self)"
"$self/bin/rostnix" 2>&1 | grep -q 'usage: rostnix' || fail "self: the self-built rostnix does not print its usage"
echo "ok: self: rostnix builds itself"
check_conformance self default

# An edit rebuilds the units that see the file and what depends on them.
hello="$root/tests/fixtures/hello"
check_incremental "$hello" '{ }' src/lib.rs "rustbin-hello rustlib-hello-0.1.0"
check_incremental "$hello" '{ }' src/main.rs "rustbin-hello"
# Tests and examples that are not built are not seen.
check_incremental "$hello" '{ }' tests/smoke.rs ""
check_incremental "$hello" '{ }' examples/extra.rs ""

workspace="$root/tests/fixtures/workspace"
check_incremental "$workspace" '{ }' crates/core/src/lib.rs "rustbin-ws-app rustbin-ws-tool rustlib-ws-core-0.3.0"
# A package inside another's directory is not part of the outer one.
check_incremental "$workspace" '{ }' crates/core/nested/src/lib.rs "rustbin-ws-app rustbin-ws-tool rustlib-ws-nested-0.3.0"
# Binaries of one package do not see each other.
check_incremental "$workspace" '{ }' app/src/bin/ws-tool.rs "rustbin-ws-tool"
check_incremental "$workspace" '{ }' crates/macros/src/lib.rs "rustbin-ws-app rustbin-ws-tool rustmacro-ws-macros-0.3.0"

# A file an override's extraSrc adds is seen by that package's units.
buildscript="$root/tests/fixtures/buildscript"
check_incremental "$buildscript" '{ crateOverrides.consumer.extraSrc = [ "shared" ]; }' shared/message.txt \
  "rustbin-consumer rustbs-consumer-0.1.0 rustbsrun-consumer-0.1.0"

core_rs_src="$(nix eval --raw "${exec_opt[@]}" "$flake#fixtures.core-rs.src")"
check_incremental "$core_rs_src" '{ examples = [ "amber-store" ]; }' src/lib.rs \
  "rustbin-amber-store rustlib-amber-store-core-0.10.0"
check_incremental "$core_rs_src" '{ examples = [ "amber-store" ]; }' tests/cbor.rs ""
check_incremental "$core_rs_src" '{ examples = [ "amber-store" ]; }' examples/amber-bench.rs ""

check_exec_error
check_no_executable_error

echo "all integration checks passed"
