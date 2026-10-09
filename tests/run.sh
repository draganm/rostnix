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
# The registry one check serves, stopped whatever happens.
registry_pid=
trap '[ -z "$registry_pid" ] || kill "$registry_pid" 2>/dev/null; rm -rf "$work"' EXIT

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

# compare_with_cargo <fixture> <shell> <records attribute> <conformance flag or ""> <cargo arguments...>
# Runs cargo on a copy of the fixture's source, in the named flake shell and
# with a cargo home that shares only the download caches, as resolve's does.
# What cargo ran must be what the fixture's records say rostnix ran, apart
# from what the spec lists. The flags are split into words. CARGO_ROOT names
# the workspace's directory in the source, when it is not the root. Cargo
# fetches git repositories with the git command, as rostnix has it do.
compare_with_cargo() {
  local fixture="$1" shell="$2" attr="$3" flag="$4" src dir records
  shift 4
  src="$(nix eval --raw "${exec_opt[@]}" "$flake#fixtures.$fixture.src")"
  dir="$work/conformance-$fixture-$attr"
  mkdir -p "$dir/home" "$dir/src"
  ln -s "${CARGO_HOME:-$HOME/.cargo}/registry" "$dir/home/registry"
  ln -s "${CARGO_HOME:-$HOME/.cargo}/git" "$dir/home/git"
  cp -R "$src/." "$dir/src/"
  chmod -R u+w "$dir/src"
  (cd "$dir/src/${CARGO_ROOT:-.}" &&
    env -u RUSTFLAGS -u CARGO_BUILD_TARGET -u RUSTC_WRAPPER \
      CARGO_HOME="$dir/home" CARGO_TARGET_DIR="$dir/target" CARGO_NET_GIT_FETCH_WITH_CLI=true \
      nix develop "$flake#$shell" --command cargo "$@" >"$dir/cargo.out" 2>"$dir/cargo.log") ||
    fail "$fixture: the reference cargo $1 failed; see $(tail -n 5 "$dir/cargo.log")"
  records="$(build "fixtures.$fixture.$attr")"
  CARGO_TARGET_DIR="$work/target" nix develop "$flake" --command \
    cargo run --quiet --manifest-path "$root/Cargo.toml" --example conformance -- \
    $flag --root "$(cd "$dir/src" && pwd -P)" "$dir/cargo.log" "$records" ||
    fail "$fixture: rostnix does not run what cargo $1 runs (differences above)"
  rm -rf "$dir"
}

# check_conformance <fixture> <shell> <cargo build arguments...>
# Every rustc invocation and build-script run must be what `cargo build -vv`
# runs for the same source. The arguments name the fixture's profile and
# selection, release included: cargo's own default is dev.
check_conformance() {
  local fixture="$1" shell="$2"
  shift 2
  compare_with_cargo "$fixture" "$shell" unitRecords "" build -vv --locked "$@"
  echo "ok: $fixture: every invocation is cargo's"
}

# check_test_conformance <fixture> <shell> <cargo test arguments...>
# The same for the tests: what is compiled for them, and how each test is
# run, must be what `cargo test -vv` does. That the reference run succeeds
# also shows that the fixture's tests pass under cargo itself.
check_test_conformance() {
  local fixture="$1" shell="$2"
  shift 2
  compare_with_cargo "$fixture" "$shell" testUnitRecords "" test -vv --locked "$@"
  echo "ok: $fixture: every test is compiled and run as cargo does it"
}

# check_test_compile_conformance <fixture> <shell> <skipped test> <cargo test arguments...>
# For a project with a test that cannot run here: the reference is
# `cargo test --no-run`, and the runs are left out of the comparison. So is
# the test the fixture skips, which rostnix does not compile either.
check_test_compile_conformance() {
  local fixture="$1" shell="$2" skipped="$3"
  shift 3
  compare_with_cargo "$fixture" "$shell" testUnitRecords "--compile-only --without $skipped" \
    test --no-run -vv --locked "$@"
  echo "ok: $fixture: every test is compiled as cargo does it"
}

# check_incremental <fixture directory or store path> <arguments> <file to append to> <what must change> [label]
# Evaluates the project from two copies that differ in one file and lists
# the derivation names of the units and the test runs that differ, sorted.
check_incremental() {
  local src="$1" args="$2" file="$3" want="$4" a b got
  local label="${5:-$(basename "$1")}"
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
      changed = set: builtins.filter
        (n: a.\${set}.\${n}.drvPath != b.\${set}.\${n}.drvPath)
        (builtins.attrNames a.\${set});
      names = set: map (n: a.\${set}.\${n}.name) (changed set);
    in builtins.concatStringsSep \" \" (pkgs.lib.sort builtins.lessThan (pkgs.lib.unique (names \"units\" ++ names \"tests\")))
  ")"
  rm -rf "$a" "$b"
  [ "$got" = "$want" ] || fail "$label: editing $file changed [$got], want [$want]"
  echo "ok: $label: editing $file changes [$want]"
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
  # The message itself, not the source line a trace would quote.
  case "$msg" in
    *'error: rostnix: the selection builds no binary and no example'*)
      echo "ok: a selection with nothing to install is explained" ;;
    *) fail "unhelpful error for a selection with nothing to install: $msg" ;;
  esac
}

# extraSrc is about the source tree; on a crate from the registry it would
# silently do nothing.
check_override_foreign_extra_src() {
  local msg
  if msg="$(fixture_with buildscript '{
      crateOverrides = { libz-sys.extraSrc = [ "shared" ]; consumer.extraSrc = [ "shared" ]; };
    }' 'app.drvPath' 2>&1)"; then
    fail "crateOverrides: extraSrc on a registry package was accepted"
  fi
  case "$msg" in
    *'error: rostnix: crateOverrides.libz-sys.extraSrc is set for a package that is not part of the source tree'*)
      echo "ok: crateOverrides: extraSrc on a registry package is rejected" ;;
    *) fail "crateOverrides: unhelpful error for extraSrc on a registry package: $msg" ;;
  esac
}

# Paths and values as people write them: a leading ./, a trailing /, and a
# number or a boolean in env.
check_override_spellings() {
  local got
  got="$(fixture_with buildscript '{
      crateOverrides = {
        consumer.extraSrc = [ "./shared/" ];
        bs-native.env = { ROSTNIX_FIXTURE_NOTE = 7; FLAG = true; };
      };
    }' '
      let
        units = builtins.attrValues app.units;
        named = name: builtins.head (builtins.filter (u: u.name == name) units);
        plain = app.override or null;
      in "${builtins.toJSON (named "rustbsrun-bs-native-0.1.0").node.overrideEnv} ${toString (builtins.pathExists "${(named "rustbin-consumer").node.src}/shared/message.txt")}"
    ')" || fail "crateOverrides: spellings do not evaluate"
  [ "$got" = '{"FLAG":"1","ROSTNIX_FIXTURE_NOTE":"7"} 1' ] ||
    fail "crateOverrides: env and extraSrc came out as [$got]"
  echo "ok: crateOverrides: env values become strings and extraSrc paths are cleaned"
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
# the build scripts that depend on its build script through `links`, as
# consumer's does on bs-native's. Nothing else gets them.
check_override_inputs() {
  local got want
  got="$(fixture_with buildscript '{
      crateOverrides = {
        libz-sys.buildInputs = [ pkgs.zlib ];
        bs-native.buildInputs = [ pkgs.lz4 ];
        consumer.extraSrc = [ "shared" ];
      };
    }' '
      let
        units = builtins.attrValues app.units;
        named = name: builtins.filter (u: u.name == name) units;
        inputs = name: toString (map (i: i.pname) (builtins.head (named name)).buildInputs);
      in "bin: ${inputs "rustbin-consumer"}; native run: ${inputs "rustbsrun-bs-native-0.1.0"}; consumer run: ${inputs "rustbsrun-consumer-0.1.0"}; zlib run: ${inputs "rustbsrun-libz-sys-1.1.29"}; script: ${inputs "rustbs-bs-native-0.1.0"}"
    ')" || fail "crateOverrides: buildInputs do not evaluate"
  # consumer's script depends on both bs-native's and libz-sys's.
  want="bin: lz4 zlib; native run: lz4; consumer run: lz4 zlib; zlib run: zlib; script: "
  [ "$got" = "$want" ] || fail "crateOverrides: units got the inputs [$got], want [$want]"
  echo "ok: crateOverrides: buildInputs reach the units that link the package and the build scripts that depend on its"
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
  if grep -E -- '-(rustsrc|rustlib|rustmacro|rustbs|rustbsrun|rustbin|rusttest)-' <<<"$refs"; then
    fail "$1: the result refers to the build inputs listed above"
  fi
  echo "ok: $1: the result refers to no source tree, unit or test"
}

# check_stdenv <fixture>
# Only units that link, tests among them, and build-script runs build with
# stdenv and its C compiler; a library keeps the tool as its builder.
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
  [ "$got" = "stdenv: rustbin rustbs rustbsrun rustmacro rusttest; bare: rustlib" ] ||
    fail "$1: builders are [$got], want [stdenv: rustbin rustbs rustbsrun rustmacro rusttest; bare: rustlib]"
  echo "ok: $1: only libraries build without stdenv"
}

# test_log <fixture> <test target> <target kind>: prints the path of what
# that test printed when it ran. The kind tells a library's unit tests from
# those of a binary or an integration test of the same name.
test_log() {
  local out
  out="$(nix build --impure "${exec_opt[@]}" --no-link --print-out-paths --expr "
    let fixture = (builtins.getFlake \"$flake\").legacyPackages.\${builtins.currentSystem}.fixtures.$1;
    in builtins.head (builtins.filter (test: test.targetName == \"$2\" && test.targetKind == \"$3\") (builtins.attrValues fixture.tests))
  ")" || fail "$1: the tests of $2 ($3) do not build and pass"
  echo "$out/log"
}

# check_tests_ran <fixture> <test target> <target kind> <pattern its log must match>
# The pattern is an extended regular expression. One that asks for a count
# of passed tests cannot be met by a test executable with no test in it.
check_tests_ran() {
  local log
  log="$(test_log "$1" "$2" "$3")"
  grep -qE -- "$4" "$log" || fail "$1: the log of $2 ($3) does not match '$4': $(cat "$log")"
  echo "ok: $1: the tests of $2 ($3) ran: $(grep -E -- "$4" "$log" | head -n 1)"
}

# A failing test fails the build, and the log names the test. The fixture
# has an ignored test that panics; checkFlags makes the harness run it.
check_failing_test() {
  local msg
  if msg="$(nix build --impure "${exec_opt[@]}" --no-link -L --expr "
      (builtins.getFlake \"$flake\").legacyPackages.\${builtins.currentSystem}.rustEnv.buildRustApplication {
        pname = \"failing\";
        src = $root/tests/fixtures/hello;
        checkFlags = [ \"--include-ignored\" ];
      }" 2>&1)"; then
    fail "a build with a failing test succeeded"
  fi
  case "$msg" in
    *'test tests::fails_on_purpose ... FAILED'*) ;;
    *) fail "the log of a failing build does not name the failed test: $msg" ;;
  esac
  case "$msg" in
    *'rostnix: the test hello of hello 0.1.0 failed'*) echo "ok: a failing test fails the build and is named" ;;
    *) fail "a failing test is not reported by name: $msg" ;;
  esac
}

# skipTests leaves a test out, so that the application does not wait for
# it. An entry that names no test target skips nothing and is warned about.
check_skip_tests() {
  local got msg
  got="$(fixture_with hello '{ skipTests = [ "smoke" ]; }' '
    "${toString (map (test: test.targetName) (builtins.attrValues app.tests))}; ${toString (builtins.length app.testRuns)}"')" ||
    fail "skipTests does not evaluate"
  [ "$got" = "cli hello hello; 3" ] || fail "skipTests = [ smoke ] leaves the tests [$got], want [cli hello hello; 3]"
  msg="$(fixture_with hello '{ skipTests = [ "smok" ]; }' 'app.drvPath' 2>&1)" ||
    fail "a skipTests entry that names no test stops the evaluation: $msg"
  case "$msg" in
    *'skipTests names smok, which is no test target of this build; the test targets are cli, hello, smoke'*)
      echo "ok: skipTests leaves a test out and warns about an entry that names no test" ;;
    *) fail "no warning for a skipTests entry that names no test: $msg" ;;
  esac
}

# Without doCheck no test is planned: the graph is cargo build's alone.
check_no_check() {
  local got
  got="$(fixture_with hello '{ doCheck = false; }' '
    "${toString (builtins.length (builtins.attrNames app.tests))} ${toString (builtins.length app.graph.testUnits)} ${toString (app.graph.buildUnits == builtins.attrNames app.units)} ${toString (builtins.length app.testRuns)}"')" ||
    fail "doCheck = false does not evaluate"
  [ "$got" = "0 0 1 0" ] || fail "doCheck = false still plans tests: [$got]"
  echo "ok: doCheck = false plans no test"
}

# Tests share with the application every unit that both plans describe
# alike. In hello that is all of the application. In the workspace a
# dev-dependency turns a feature of ws-core on, so the ws-app the tests run
# is another unit than the one that is installed.
check_shared_units() {
  local got
  got="$(nix eval "${exec_opt[@]}" --raw "$flake#fixtures.hello.graph" --apply '
    g: toString (builtins.length (builtins.filter (key: !(builtins.elem key g.testUnits)) g.buildUnits))')" ||
    fail "hello: the graph does not evaluate"
  [ "$got" = 0 ] || fail "hello: $got units of the application are planned again for the tests"
  got="$(nix eval "${exec_opt[@]}" --raw "$flake#fixtures.workspace.graph" --apply '
    g:
    let
      apps = builtins.filter (key: builtins.match "ws-app-0.3.0-bin-ws-app-.*" key != null);
      installed = apps g.buildUnits;
      tested = apps g.testUnits;
    in toString [ (builtins.length installed) (builtins.length tested) (installed != tested) ]')" ||
    fail "workspace: the graph does not evaluate"
  [ "$got" = "1 1 1" ] || fail "workspace: installed and tested ws-app are [$got], want [1 1 1]"
  echo "ok: tests share the application's units where cargo plans them alike, and only there"
}

# What only the tests need may be something rostnix refuses. The error then
# says that it concerns the tests, and how to build without them. Here it
# is a test rooted outside its package, by a path with `..` in it.
check_tests_only_refusal() {
  local dir="$work/outside" msg
  mkdir -p "$dir/pkg"
  cp -R "$root/tests/fixtures/hello/." "$dir/pkg/"
  printf '\n[[test]]\nname = "outside"\npath = "../outside.rs"\n' >>"$dir/pkg/Cargo.toml"
  printf '#[test]\nfn passes() {}\n' >"$dir/outside.rs"
  outside_with() {
    nix eval --impure --raw "${exec_opt[@]}" --expr "
      ((builtins.getFlake \"$flake\").legacyPackages.\${builtins.currentSystem}.rustEnv.buildRustApplication {
        pname = \"outside\";
        src = $dir;
        cargoRoot = \"pkg\";
        doCheck = $1;
      }).drvPath" 2>&1
  }
  if msg="$(outside_with true)"; then
    fail "a test rooted outside its package was accepted"
  fi
  case "$msg" in
    *'the target outside of hello 0.1.0 has its root at'*'outside the package directory'*'this concerns the tests only'*'doCheck = false'*) ;;
    *) fail "unhelpful error for a test that cannot be planned: $msg" ;;
  esac
  outside_with false >/dev/null ||
    fail "doCheck = false does not build without the test that cannot be planned"
  rm -rf "$dir"
  echo "ok: what only the tests need and cannot be planned is explained; doCheck = false builds without it"
}

# A crate from a registry other than crates.io. The registry is served
# from this machine, and named in the cargo home of whoever builds: a home
# made up for this check, which also says how to build, to show that this
# part of it is not listened to.
check_registry() {
  local served="$work/registry" home="$work/registry-home" port=18473
  local python out got crate="graph.sources.\"rostnix-fixture-dep-0.1.0\".crate"
  mkdir -p "$served" "$home"
  python="$(nix develop "$flake#fixtureShell" --command sh -c 'command -v python3')"
  "$python" "$root/tests/registry.py" make "$served" "$port" >/dev/null
  "$python" "$root/tests/registry.py" serve "$served" "$port" &
  registry_pid=$!
  for _ in {1..50}; do
    curl -sf "http://127.0.0.1:$port/index/config.json" >/dev/null && break
    sleep 0.2
  done
  # What answers must be the server just started, not one left over from
  # an earlier run, which would serve a directory that is gone.
  kill -0 "$registry_pid" 2>/dev/null && curl -sf "http://127.0.0.1:$port/index/config.json" >/dev/null ||
    fail "registry: the registry does not run on port $port; is another program using it?"
  printf '[registries.fixture]\nindex = "sparse+http://127.0.0.1:%s/index/"\n\n[build]\nrustflags = ["--cfg", "from_the_callers_home"]\n\n[env]\nFROM_THE_CALLERS_HOME = "1"\n' \
    "$port" >"$home/config.toml"

  out="$(CARGO_HOME="$home" build fixtures.registry)" || fail "registry: the fixture does not build"
  got="$("$out/bin/from-registry")"
  [ "$got" = "registry ok: 42" ] || fail "registry: from-registry printed '$got', want 'registry ok: 42'"
  echo "ok: registry: a crate from another registry is built, and its test passes"

  got="$(CARGO_HOME="$home" nix eval "${exec_opt[@]}" --json "$flake#fixtures.registry.graph" \
    --apply 'graph: { inherit (graph) rustflags configEnv; }')"
  [ "$got" = '{"configEnv":[],"rustflags":[]}' ] ||
    fail "registry: the caller's cargo home says how to build, and was listened to: $got"
  echo "ok: registry: the caller's cargo home says where crates come from and not how to build"

  # The registry's own config.json says where its crates are downloaded.
  got="$(CARGO_HOME="$home" nix eval "${exec_opt[@]}" --raw "$flake#fixtures.registry.$crate.url")"
  [ "$got" = "http://127.0.0.1:$port/crates/rostnix-fixture-dep/rostnix-fixture-dep-0.1.0.crate" ] ||
    fail "registry: the crate would be downloaded from '$got'"
  CARGO_HOME="$home" nix build "${exec_opt[@]}" --rebuild --no-link "$flake#fixtures.registry.$crate" ||
    fail "registry: downloading the crate does not reproduce the pre-seeded file"
  echo "ok: registry: downloading the crate from the registry reproduces the pre-seeded file"

  kill "$registry_pid" 2>/dev/null || true
  wait "$registry_pid" 2>/dev/null || true
  registry_pid=
}

# A registry that names no address to download from without a token: the
# derivation that stands for the crate file cannot fetch it, and says what
# to do instead.
check_registry_without_address() {
  local msg
  if msg="$(nix build --impure "${exec_opt[@]}" --no-link -L --expr "
      let rustEnv = (builtins.getFlake \"$flake\").legacyPackages.\${builtins.currentSystem}.rustEnv;
      in ((rustEnv.builders { srcStr = \"/nowhere\"; }).fetchCrate {
        pname = \"private-dep\";
        version = \"1.0.0\";
        sha256 = \"7b1f0c1b0f9d5c7a3d1e6a4c8e2b9f0d6a5c4b3e2f1a0d9c8b7a6f5e4d3c2b1a\";
        url = null;
        registry = \"sparse+https://crates.example.com/index/\";
      }).crate" 2>&1)"; then
    fail "a crate with no address to download from was built"
  fi
  case "$msg" in
    *'private-dep 1.0.0 comes from the registry sparse+https://crates.example.com/index/'*'substituter'*)
      echo "ok: a crate of a registry that needs a token says how to get it" ;;
    *) fail "unhelpful error for a crate with no address to download from: $msg" ;;
  esac
}

# The flags of the cargo configuration can be replaced: a project that
# links with mold on its developers' machines must still build here.
check_rustflags_argument() {
  local got
  got="$(fixture_with config '{ cargoRoot = "ws"; rustflags = [ "--cfg" "from_argument" ]; }' \
    'builtins.toJSON app.graph.rustflags')" || fail "the rustflags argument does not evaluate"
  [ "$got" = '["--cfg","from_argument"]' ] ||
    fail "rustflags = [ --cfg from_argument ] gives the flags $got"
  echo "ok: config: the rustflags argument takes the place of the configuration's flags"
}

# A linker the cargo configuration names is not used: rostnix links with
# the C compiler of its nixpkgs. Evaluation says so, by the key that sets
# it.
check_unapplied_config() {
  local dir="$work/linker" msg
  mkdir -p "$dir/.cargo"
  cp -R "$root/tests/fixtures/hello/." "$dir/"
  printf '[target."cfg(all())"]\nlinker = "clang"\n' >"$dir/.cargo/config.toml"
  msg="$(nix eval --impure --raw "${exec_opt[@]}" --expr "
      ((builtins.getFlake \"$flake\").legacyPackages.\${builtins.currentSystem}.rustEnv.buildRustApplication {
        pname = \"linker\";
        src = $dir;
      }).drvPath" 2>&1)" || fail "a cargo configuration that names a linker stops the evaluation: $msg"
  case "$msg" in
    *"the cargo configuration sets target.'cfg(all())'.linker, which is not applied"*)
      echo "ok: a setting of the cargo configuration that is not applied is named" ;;
    *) fail "no warning for a linker in the cargo configuration: $msg" ;;
  esac
  rm -rf "$dir"
}

check_run hello hello '{"greeting":"hello","n":42}'
check_conformance hello default --profile release
check_test_conformance hello default --profile release
check_tests_ran hello cli test "test result: ok\. 6 passed"
check_tests_ran hello smoke test "test result: ok\. 1 passed"
check_tests_ran hello hello lib "test result: ok\. 1 passed; 0 failed; 1 ignored"
check_main_program hello hello
check_stdenv hello
check_no_intermediate_refs hello
check_fetch_fallback hello anyhow
check_failing_test
check_skip_tests
check_no_check
check_shared_units
check_tests_only_refusal

# A workspace: the default selection is every member, and a narrower one
# with a feature builds one binary. Its tests cover a proc macro's unit
# tests, a test without the harness, and a binary that the tests get with
# another feature set than the one installed.
check_run workspace ws-app "hello from ws-app: 3"
check_run workspace ws-tool "ws-tool ok 7"
check_bins workspace "ws-app ws-tool"
check_conformance workspace default --profile release
check_test_conformance workspace default --profile release
check_tests_ran workspace plain test "^plain test ran$"
check_tests_ran workspace ws_macros proc-macro "test result: ok\. 1 passed"
check_tests_ran workspace cli test "test result: ok\. 2 passed"
check_run workspace-shout ws-app "HELLO FROM WS-APP: 3"
check_bins workspace-shout "ws-app"
check_conformance workspace-shout default --profile release --package ws-app --bin ws-app --features shout
# Tests are those of the selected package; naming a binary does not narrow
# them.
check_test_conformance workspace-shout default --profile release --package ws-app --features shout

# Dependencies from git repositories: serde, a workspace with a proc macro
# and build scripts, by tag, and itoa by revision. Each is the tree of the
# revision Cargo.lock names.
check_run gitdeps gitdeps "gitdeps ok: sum 7"
check_conformance gitdeps default --profile release
check_test_conformance gitdeps default --profile release
check_tests_ran gitdeps gitdeps bin "test result: ok\. 1 passed"
check_no_intermediate_refs gitdeps
git_revs="$(nix eval "${exec_opt[@]}" --raw "$flake#fixtures.gitdeps.graph.sources" --apply '
  sources: toString (map (key: "${key}=${sources.${key}.rev}")
    (builtins.filter (key: sources.${key} ? rev) (builtins.attrNames sources)))')"
[ "$git_revs" = "git-itoa-af77385d0daf=af77385d0daf4d0e949e81f2588be2e44f69f086 git-serde-a866b336f14a=a866b336f14aa57a07f0d0be9f8762746e64ecb4" ] ||
  fail "gitdeps: the git sources are [$git_revs]"
echo "ok: gitdeps: each repository is fetched at the revision Cargo.lock names"

# The cargo configuration: flags from target tables, which take the place
# of [build]'s, and variables that are plain, forced, and relative to the
# directory of the file that sets them. The workspace lies below the source
# root, with a configuration file at each level.
check_run config configured \
  "config ok: plain=plain message=hello-from-data unix=true expression=true triple=true flag=true build=false script=flags n=7"
CARGO_ROOT=ws check_conformance config default --profile release
CARGO_ROOT=ws check_test_conformance config default --profile release
check_tests_ran config configured bin "test result: ok\. 2 passed"
check_no_intermediate_refs config
check_rustflags_argument
check_unapplied_config

# A crate from a registry other than crates.io.
check_registry
check_registry_without_address

# Build scripts. The note comes from an override's env, the message from a
# file an override's extraSrc adds, and pc from the pkg-config and zlib that
# overrides supply.
zlib_version="$(nix eval --raw "$flake#fixtureShell.buildInputs" --apply 'inputs: (builtins.head inputs).version')"
check_run buildscript consumer \
  "add=5 answer=42 note=from-override generated=from-build-script cfg=yes old=yes msg=shared-message zlib=ok pc=$zlib_version"
ROSTNIX_FIXTURE_NOTE=from-override check_conformance buildscript fixtureShell --profile release
ROSTNIX_FIXTURE_NOTE=from-override check_test_conformance buildscript fixtureShell --profile release
check_tests_ran buildscript bs_native lib "test result: ok\. 2 passed"
check_no_intermediate_refs buildscript
check_override_typo
check_override_unmatched
check_override_inputs
check_override_foreign_extra_src
check_override_spellings

# One project under four profiles. The release profile aborts on panic;
# its tests, one of which expects a panic, unwind as cargo has them do.
for profile in release thin nolto dev; do
  check_run "profiles-$profile" profiles "profiles ok 12345"
  check_conformance "profiles-$profile" default --profile "$profile"
  check_test_conformance "profiles-$profile" default --profile "$profile"
  check_tests_ran "profiles-$profile" profiles bin "test result: ok\. 2 passed"
done
# dev keeps debug information. On macOS that lives in the units' object
# files, which the result must not keep alive: it gets a .dSYM instead.
check_no_intermediate_refs profiles-dev
if [ "$(uname)" = Darwin ]; then
  [ -d "$(build fixtures.profiles-dev)/bin/profiles.dSYM" ] ||
    fail "profiles-dev: no .dSYM bundle beside the executable"
  [ ! -e "$(build fixtures.profiles-release)/bin/profiles.dSYM" ] ||
    fail "profiles-release: a .dSYM bundle for an executable without debug information"
  echo "ok: profiles-dev: debug information is in a .dSYM bundle"
fi

# Patient zero. Building it runs its test suite, apart from the one test
# that runs cargo.
core_rs="$(build fixtures.core-rs)"
"$core_rs/bin/amber-store" --help >/dev/null || fail "core-rs: amber-store --help fails"
mkdir -p "$work/tree/sub" && echo one >"$work/tree/a.txt" && echo two >"$work/tree/sub/b.txt"
key="$("$core_rs/bin/amber-store" --store "$work/store" ingest "$work/tree")"
"$core_rs/bin/amber-store" --store "$work/store" restore "$key" "$work/restored"
diff -r "$work/tree" "$work/restored" || fail "core-rs: amber-store does not restore what it ingested"
echo "ok: core-rs: amber-store ingests and restores a tree"
check_conformance core-rs default --profile release --example amber-store
check_no_intermediate_refs core-rs
core_rs_tests="$(nix eval "${exec_opt[@]}" --raw "$flake#fixtures.core-rs" --apply '
  app: "${toString (builtins.length app.testRuns)} of ${toString (builtins.length (builtins.attrNames app.graph.tests))}"')"
[ "$core_rs_tests" = "21 of 22" ] ||
  fail "core-rs: the application waits for $core_rs_tests tests, want 21 of 22"
echo "ok: core-rs: 21 of its 22 test executables ran"
# The CLI test finds the example beside its own executable. The pack store
# test copies golden files out of the source and writes to the copies.
check_tests_ran core-rs cli_e2e test "test result: ok\. [1-9][0-9]* passed"
check_tests_ran core-rs golden_packstore test "test result: ok\. 4 passed"
check_tests_ran core-rs amber_store_core lib "test result: ok\. [1-9][0-9]* passed"
# The one test Nix itself rules out: it creates a setuid file.
check_tests_ran core-rs tar_extract test "test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out"
check_test_compile_conformance core-rs default amber_bench_smoke --profile release

# rostnix builds itself, and its own unit tests pass in a derivation.
self="$(build fixtures.self)"
# Without a command it prints its usage and exits 2.
usage="$("$self/bin/rostnix" 2>&1 || true)"
case "$usage" in
  'usage: rostnix'*) ;;
  *) fail "self: the self-built rostnix printed '$usage', not its usage" ;;
esac
echo "ok: self: rostnix builds itself"
check_conformance self default --profile release
check_test_conformance self default --profile release
check_tests_ran self rostnix lib "test result: ok\. [1-9][0-9][0-9] passed"

# An edit rebuilds the units that see the file and what depends on them.
# A test is such a unit: it is built and run again when it could tell the
# difference.
hello="$root/tests/fixtures/hello"
check_incremental "$hello" '{ }' src/lib.rs \
  "rustbin-extra rustbin-hello rustlib-hello-0.1.0 rusttest-cli rusttest-hello rusttest-smoke"
# The binary, its own unit tests, and the integration tests, which run it.
check_incremental "$hello" '{ }' src/main.rs "rustbin-hello rusttest-cli rusttest-hello rusttest-smoke"
# A test's own file is seen by that test alone.
check_incremental "$hello" '{ }' tests/smoke.rs "rusttest-smoke"
# Data under tests/ is seen by everything built as a test, and by nothing
# that is installed.
check_incremental "$hello" '{ }' tests/data/expected.json "rusttest-cli rusttest-hello rusttest-smoke"
# An example is rebuilt, and the integration tests find it beside
# themselves. Unit tests are promised no example.
check_incremental "$hello" '{ }' examples/extra.rs "rustbin-extra rusttest-cli rusttest-smoke"
# Without tests, tests and examples are not seen at all.
check_incremental "$hello" '{ doCheck = false; }' tests/smoke.rs ""
check_incremental "$hello" '{ doCheck = false; }' examples/extra.rs ""
check_incremental "$hello" '{ doCheck = false; }' src/lib.rs "rustbin-hello rustlib-hello-0.1.0"

workspace="$root/tests/fixtures/workspace"
check_incremental "$workspace" '{ doCheck = false; }' crates/core/src/lib.rs "rustbin-ws-app rustbin-ws-tool rustlib-ws-core-0.3.0"
# A package inside another's directory is not part of the outer one.
check_incremental "$workspace" '{ doCheck = false; }' crates/core/nested/src/lib.rs "rustbin-ws-app rustbin-ws-tool rustlib-ws-nested-0.3.0"
# Binaries of one package do not see each other.
check_incremental "$workspace" '{ doCheck = false; }' app/src/bin/ws-tool.rs "rustbin-ws-tool"
check_incremental "$workspace" '{ doCheck = false; }' crates/macros/src/lib.rs "rustbin-ws-app rustbin-ws-tool rustmacro-ws-macros-0.3.0"
# With tests: the tests of the package and of what depends on it run again,
# and no other package's.
check_incremental "$workspace" '{ }' crates/core/src/lib.rs \
  "rustbin-ws-app rustbin-ws-tool rustlib-ws-core-0.3.0 rusttest-cli rusttest-common rusttest-plain rusttest-ws-app rusttest-ws-tool rusttest-ws_core"
check_incremental "$workspace" '{ }' crates/core/tests/plain.rs "rusttest-plain"
# tests/common.rs is a test of its own to cargo and a module of plain.rs,
# which says `mod common;`: both see it, and nothing else does.
check_incremental "$workspace" '{ }' crates/core/tests/common.rs "rusttest-common rusttest-plain"
check_incremental "$workspace" '{ }' app/src/bin/ws-tool.rs "rustbin-ws-tool rusttest-cli rusttest-ws-tool"

# A file an override's extraSrc adds is seen by that package's units.
buildscript="$root/tests/fixtures/buildscript"
check_incremental "$buildscript" '{ crateOverrides.consumer.extraSrc = [ "shared" ]; }' shared/message.txt \
  "rustbin-consumer rustbs-consumer-0.1.0 rustbsrun-consumer-0.1.0 rusttest-consumer"

# A file that a relative variable of the cargo configuration names is seen
# by every unit of a local package, although it lies outside the package.
# The crate from the registry is given no path into this source, and is
# rebuilt neither when that file changes nor when the source does.
config="$root/tests/fixtures/config"
check_incremental "$config" '{ cargoRoot = "ws"; }' data/message.txt \
  "rustbin-configured rustbs-configured-0.1.0 rustbsrun-configured-0.1.0 rusttest-configured"
check_incremental "$config" '{ cargoRoot = "ws"; }' ws/app/src/main.rs \
  "rustbin-configured rustbsrun-configured-0.1.0 rusttest-configured"

core_rs_src="$(nix eval --raw "${exec_opt[@]}" "$flake#fixtures.core-rs.src")"
check_incremental "$core_rs_src" '{ examples = [ "amber-store" ]; doCheck = false; }' src/lib.rs \
  "rustbin-amber-store rustlib-amber-store-core-0.10.0" core-rs
check_incremental "$core_rs_src" '{ examples = [ "amber-store" ]; doCheck = false; }' tests/cbor.rs "" core-rs
check_incremental "$core_rs_src" '{ examples = [ "amber-store" ]; doCheck = false; }' examples/amber-bench.rs "" core-rs
# With tests, editing one of the 21 integration tests builds and runs that
# one again.
check_incremental "$core_rs_src" '{ examples = [ "amber-store" ]; }' tests/cbor.rs "rusttest-cbor" core-rs

check_exec_error
check_no_executable_error

echo "all integration checks passed"
