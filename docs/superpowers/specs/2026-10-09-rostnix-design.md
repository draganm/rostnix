# rostnix design

Date: 2026-10-09

## Goal

Build Rust programs with Nix one compile step per derivation, with nothing to
check in when `Cargo.toml` or `Cargo.lock` changes. rostnix does for Rust what
[gonixgo](https://github.com/draganm/gonixgo) does for Go, at the granularity
of [crate2nix](https://github.com/nix-community/crate2nix) or finer.

1. **Cargo plans during evaluation.** A Nix-built binary runs through
   `builtins.exec`, asks cargo for its build graph, and prints Nix code
   describing the build of the application and all of its dependencies.
2. **rustc builds in derivations.** Each step of cargo's plan is one
   derivation. Cargo calls such a step a *unit*: one rustc invocation, or one
   run of a build script. Cargo itself does not run at build time.
3. **Nothing generated is checked in.** A Rust project commits `Cargo.toml`
   and `Cargo.lock`, and no Nix code or hash derived from them. Crate hashes
   are the checksums already in `Cargo.lock`.

One nixpkgs instance is passed in. It builds the tool and performs the build.

### How it differs from crate2nix

crate2nix's `tools.nix` can also generate its `Cargo.nix` during evaluation,
through import-from-derivation. rostnix differs in four ways:

- Cargo resolves the graph. Features, profiles and target selection are
  cargo's own, not a reimplementation in Nix.
- Cargo runs as the user who evaluates, with that user's download cache. Each
  crate is downloaded once, and from stage 3 private registries and git
  repositories work with the user's own credentials.
- There is one derivation per unit rather than per crate: a build script's
  compile, its run, and the library it serves are separate.
- Each unit gets the flags of cargo's profile for it: LTO, `panic`,
  per-package overrides and the build-script profile.

The price is `allow-unsafe-native-code-during-evaluation`, as for gonixgo.

### Success criteria

- A flake with `src = ./.` builds a Rust project that has crates.io
  dependencies, build scripts and proc macros.
- Patient zero builds: the `amber-store` example of
  [amber-store/core-rs](https://github.com/amber-store/core-rs), and the
  binary runs. From stage 2 its test suite passes, apart from one test that
  runs cargo itself.
- For every unit, rostnix passes rustc the flags `cargo build -v` passes,
  apart from the differences listed under
  [Conformance with cargo](#conformance-with-cargo).
- Editing a file of one local crate rebuilds the units that see the file and
  the units that depend on them. Nothing else is rebuilt.
- Changing `Cargo.toml` or `Cargo.lock` requires no regeneration step.
- rostnix builds itself.

## Patient zero

core-rs is the first real project rostnix must build, pinned at commit
`e6e900b7a0f41b3540a319c1367fd167921d5d8c` (v0.10.0). With cargo 1.95,
`cargo build --release --example amber-store` plans 103 units for it.

| Property of core-rs | What it demands |
|---|---|
| One package: a library, three examples and 21 integration tests, with no `[[bin]]`. The CLI is the example `amber-store`. | Examples are selectable and installable in stage 1. |
| 72 crates in `Cargo.lock`, all from crates.io. 65 are needed on aarch64-darwin. | Stage 1's sources are enough. |
| 17 packages have build scripts. Several, among them `zstd-sys`, `lz4-sys` and `libsqlite3-sys`, compile C with the `cc` crate. | Build-script runs have a C compiler, and their static libraries reach the link. |
| `zstd-sys`, `lz4-sys` and `libsqlite3-sys` set `links`. `zstd-safe`'s build script depends on the run of `zstd-sys`'s. | `DEP_*` variables. |
| Proc macros: `thiserror-impl`, `clap_derive`, `serde_derive`. | Dynamic libraries loaded by rustc. |
| `[profile.release] lto = "thin"`. | Cargo's per-unit LTO rules. |
| `redb`'s library is both `cdylib` and `rlib`. | A library unit can need the linker. |
| `libc` is built twice, with different features. | Units, not packages, are the nodes of the graph. |
| The examples use dev-dependencies (`clap`, `serde`). | Dev-dependencies follow from the selection. |
| Tests read `tests/golden` through `env!("CARGO_MANIFEST_DIR")`. `cli_e2e` looks for `examples/amber-store` beside the directory of its own executable. `amber_bench_smoke` runs `cargo build`. | Stage 2: test runs are laid out like cargo's target directory, and a test target can be skipped. |

## Facts verified

Probed on aarch64-darwin with Nix 2.26.1, cargo 1.86.0, and the cargo and
rustc 1.95.0 of nixpkgs 26.05. The design relies on them. gonixgo's spec
verified the facts about `builtins.exec` on the same Nix; they are not
repeated here.

| Fact | Evidence |
|---|---|
| Stable cargo prints its unit graph when `RUSTC_BOOTSTRAP=1` is set. | `cargo build --unit-graph -Z unstable-options` printed a `"version":1` graph on 1.86 and 1.95. |
| The unit graph separates a build script's compile, its run, the library, proc macros and examples, each with features, profile and dependencies under their extern names. | The core-rs graph. |
| Cargo no longer prints its rustc command lines. | On 1.95, `--build-plan` is `unexpected argument`. It still worked on 1.86. |
| Planning writes nothing into the source or a target directory, and works on a read-only source with `--locked`. | Run against a store path; no `target` directory appeared. |
| Planning downloads only the crates the selection needs, and `cargo metadata --filter-platform` needs no others. | A fresh cargo home held 65 of core-rs's 72 crates after either; unfiltered `cargo metadata --offline` then failed for want of a Windows crate. |
| A `Cargo.lock` checksum is the SHA-256 of the `.crate` file in cargo's cache. | Compared for `anyhow` 1.0.104. |
| `nix store add --mode flat --hash-algo sha256 --name N F` yields the path of `pkgs.fetchurl { name = N; sha256 = <checksum>; }`. | Identical paths from the add, from `nix-store --print-fixed-path` and from `fetchurl`. |
| Cargo accepts a cargo home whose `registry`, `.package-cache` and `.package-cache-mutate` are symlinks into another one, and `CARGO_GC_AUTO_FREQUENCY=never`. | Cargo 1.95 planned core-rs offline from such a home. |
| A crate that uses a proc macro compiles without the proc macro's own dependencies present. An ordinary dependency's dependencies must be present. | rustc compiled a user of `serde_derive` without `syn`, `quote` and `proc-macro2`, and failed without `serde_core`. |
| Package ids differ between cargo versions. | 1.86 prints `name version (source)`, 1.95 `source#name@version`. |
| nixpkgs' rustc runs in a bare derivation with no `PATH` and writes an rlib. | A `derivation` whose builder is `${rustc}/bin/rustc` built. |
| static.crates.io serves `<name>/<name>-<version>.crate`, also for a version containing `+`. | HTTP 200 for `lz4-sys-1.11.1+lz4-1.10.0.crate`. |

## User-facing API

```nix
{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
  inputs.rostnix.url = "github:draganm/rostnix";

  outputs = { nixpkgs, rostnix, ... }:
    let
      system = "aarch64-darwin";
      pkgs = nixpkgs.legacyPackages.${system};
      rustEnv = rostnix.lib.mkRustEnv { inherit pkgs; };
    in {
      packages.${system}.default = rustEnv.buildRustApplication {
        pname = "amber-store";
        src = ./.;
        examples = [ "amber-store" ];
      };
    };
}
```

Build with:

```bash
nix build --option allow-unsafe-native-code-during-evaluation true
```

Non-flake use: `import rostnix { inherit pkgs; }` returns the same set as
`mkRustEnv`.

### `mkRustEnv`

| Argument | Default | Meaning |
|---|---|---|
| `pkgs` | required | The nixpkgs that builds the tool and performs the build. |
| `rustc` | `pkgs.buildPackages.rustc` | The compiler inside derivations. Cargo also queries it while planning. |
| `cargo` | `pkgs.buildPackages.cargo` | The cargo that plans during evaluation. |

Returns `{ buildRustApplication, tool, rustc, cargo, builders }`.

rostnix's flag rules follow cargo 1.95, the cargo of nixpkgs 26.05. Other
toolchains can be passed but are not tested.

### `buildRustApplication`

| Argument | Default | Meaning |
|---|---|---|
| `pname` | required | Derivation name. |
| `version` | `null` | Appended to the derivation name when set. |
| `src` | required | The source tree. |
| `cargoRoot` | `"."` | Directory inside `src` that holds the workspace's `Cargo.toml` and `Cargo.lock`. |
| `packages` | `[ ]` | Workspace members to build, as `cargo build -p`. Empty means cargo's default members. |
| `bins` | `[ ]` | Binaries to build, as `--bin`. |
| `examples` | `[ ]` | Examples to build, as `--example`. |
| `features` | `[ ]` | As `--features`. |
| `allFeatures` | `false` | As `--all-features`. |
| `noDefaultFeatures` | `false` | As `--no-default-features`. |
| `profile` | `"release"` | As `--profile`. |
| `crateOverrides` | `{ }` | See [crateOverrides](#crateoverrides). |
| `doCheck` | `true` | Build and run tests; see [Tests](#tests-stage-2). Accepted and ignored before stage 2. |
| `checkFlags` | `[ ]` | Arguments for every test executable. |
| `skipTests` | `[ ]` | Test targets that are not run. |
| `meta` | `{ }` | Passed through. |

The selection means what it means to `cargo build`. With neither `bins` nor
`examples`, cargo builds the library and every binary of the selected
packages. With either, it builds only what they name.

Every binary and example the selection builds lands in `$out/bin` under its
target name. A selection that builds neither is an error that names `bins`
and `examples`. `passthru` exposes `graph`, `units`
and `bins`, each unit a derivation, so one unit can be built alone, and
`unitRecords`, a directory of every unit's `unit.json`.

## Components

### 1. The `rostnix` binary

Written in Rust. It is built with `rustPlatform.buildRustPackage` and
`cargoLock.lockFile`, which reads the checksums in rostnix's own `Cargo.lock`,
so rostnix's flake carries no dependency hash. Its dependencies are `serde`,
`serde_json`, `toml` and `sha2`, kept few because the tool is built during the
first evaluation.

| Subcommand | Runs | Purpose |
|---|---|---|
| `resolve <json>` | at evaluation, via `builtins.exec` | Runs cargo, pre-seeds crates, prints the graph as Nix. |
| `compile` | in a derivation | Runs rustc for one unit. |
| `run-build-script` | in a derivation | Runs one build script and records what it printed. |

Build-time subcommands read their node from the derivation's attributes.
Their derivations use `__structuredAttrs`, so the node arrives as JSON in the
attributes file and its size is not bounded by the environment.

### 2. The static Nix library

`nix/` holds `mkRustEnv` and the builder functions the generated code calls:
`fetchCrate`, `localSource`, `compile` and `runBuildScript`. Each defines how
one kind of node is built. All graph wiring is in the generated code.

### 3. The generated graph

`resolve` prints one Nix function. It is produced on every evaluation and
never written to disk.

```nix
b: rec {
  cargoVersion = "1.95.0";
  host = "aarch64-apple-darwin";

  sources."zstd-sys-2.0.16+zstd.1.5.7" = b.fetchCrate {
    pname = "zstd-sys";
    version = "2.0.16+zstd.1.5.7";
    sha256 = "…";
    url = "https://static.crates.io/crates/zstd-sys/zstd-sys-2.0.16+zstd.1.5.7.crate";
  };

  packages."zstd-sys-2.0.16+zstd.1.5.7" = {
    name = "zstd-sys";
    version = "2.0.16+zstd.1.5.7";
    local = false;
    manifestDir = "";
    links = "zstd";
    env = { CARGO_PKG_LICENSE = "MIT/Apache-2.0"; /* the other CARGO_PKG_* */ };
  };

  units."zstd-sys-2.0.16+zstd.1.5.7-build-script-5c0e8a1f" = b.compile {
    name = "rustbs-zstd-sys-2.0.16+zstd.1.5.7";
    package = packages."zstd-sys-2.0.16+zstd.1.5.7";
    src = sources."zstd-sys-2.0.16+zstd.1.5.7";
    kind = "build-script";
    crateName = "build_script_build";
    srcPath = "build.rs";
    metadata = "5c0e8a1f93b2d7c4";
    linked = true;
    rustcArgs = [ "--edition=2018" "--crate-type" "bin" "-C" "embed-bitcode=no" /* … */ ];
    deps = [ { name = "cc"; unit = units."cc-…-lib-…"; } /* … */ ];
    overrides = [ ];
  };

  units."zstd-sys-2.0.16+zstd.1.5.7-run-build-script-91d0c3aa" = b.runBuildScript {
    name = "rustbsrun-zstd-sys-2.0.16+zstd.1.5.7";
    package = packages."zstd-sys-2.0.16+zstd.1.5.7";
    src = sources."zstd-sys-2.0.16+zstd.1.5.7";
    script = units."zstd-sys-2.0.16+zstd.1.5.7-build-script-5c0e8a1f";
    features = [ "default" "legacy" "zdict_builder" ];
    env = { OPT_LEVEL = "3"; DEBUG = "false"; PROFILE = "release"; };
    linksDeps = [ ];
  };

  units."zstd-sys-2.0.16+zstd.1.5.7-lib-0b7e99f1" = b.compile {
    name = "rustlib-zstd-sys-2.0.16+zstd.1.5.7";
    package = packages."zstd-sys-2.0.16+zstd.1.5.7";
    src = sources."zstd-sys-2.0.16+zstd.1.5.7";
    kind = "lib";
    crateName = "zstd_sys";
    srcPath = "src/lib.rs";
    metadata = "0b7e99f1a4c2d680";
    linked = false;
    rustcArgs = [ "--edition=2018" "--crate-type" "lib" "-C" "opt-level=3" "-C" "linker-plugin-lto" /* … */ ];
    buildScript = units."zstd-sys-2.0.16+zstd.1.5.7-run-build-script-91d0c3aa";
    deps = [ ];
  };

  sources."amber-store-core-0.10.0-example-amber-store" = b.localSource {
    name = "rustsrc-amber-store-core-0.10.0";
    dir = ".";
    exclude = [ "tests" "benches" "examples/amber-bench.rs" "examples/repair-interop.rs" ];
  };

  bins."amber-store" = units."amber-store-core-0.10.0-example-amber-store-7d21e0b3";
}
```

`deps` lists direct dependencies only, each under the name the unit's source
uses for it, renames included. A unit key is the package with its version,
the kind of target, the target's name where a package can have several of
that kind, and the first eight digits of the unit's `metadata`. Derivation
names are computed by the tool, so name sanitising lives in one place.

The tool and the Nix library ship from one source tree and the tool is built
from the same revision as the library, so the contract between generated code
and builders cannot drift and carries no version number.

## Evaluation flow

`buildRustApplication` does the following when Nix evaluates it.

1. It checks `builtins ? exec` and throws an error naming the option if it is
   missing.
2. It calls

   ```nix
   builtins.exec [
     "${tool}/bin/rostnix" "resolve"
     (builtins.toJSON {
       cargo = "${cargo}/bin/cargo";
       rustc = "${rustc}/bin/rustc";
       src = "${src}";
       storeDir = builtins.storeDir;
       overrideKeys = builtins.attrNames crateOverrides;
       inherit cargoRoot packages bins examples features allFeatures
         noDefaultFeatures profile;
     })
   ]
   ```

   Nix builds the tool, cargo and rustc first if they are not in the store.
3. The tool reads the host triple from `rustc -vV` and runs, in
   `src/cargoRoot`:
   - `cargo build --unit-graph -Z unstable-options --locked` with the
     selection, for the units;
   - `cargo metadata --format-version 1 --locked --filter-platform <host>`,
     for what the unit graph leaves out of each package: its manifest fields,
     declared features, `links` and targets.

   The two are joined on the package id, which the tool treats as opaque.
4. It classifies each package as a registry crate or local (inside `src`).
5. It pre-seeds the `.crate` file of every registry package that owns a unit.
6. It prints the graph. `buildRustApplication` applies it to the builders and
   returns the application derivation.

### Environment for cargo

Cargo runs with a clean environment and a private cargo home, so the same
source plans to the same graph in every shell.

- **Carried over** from the caller: `HOME`, `PATH`, `TMPDIR`, the proxy
  variables, the certificate variables (`SSL_CERT_FILE`, `NIX_SSL_CERT_FILE`,
  `CURL_CA_BUNDLE`), and `CARGO_HTTP_*` and `CARGO_NET_*`.
- **Set by the tool:** `CARGO_HOME` (the private home), `RUSTC`,
  `RUSTC_BOOTSTRAP=1`, `CARGO_TARGET_DIR` (a temporary directory that stays
  empty) and `CARGO_GC_AUTO_FREQUENCY=never`.
- **Dropped:** everything else, among it `RUSTFLAGS`, `CARGO_BUILD_*`,
  `CARGO_PROFILE_*` and `RUSTC_WRAPPER`.

`RUSTC_BOOTSTRAP` is set for these two cargo runs only. It never reaches a
derivation, where it would change what build scripts detect.

The private cargo home is a temporary directory, removed afterwards, that
holds three symlinks into the caller's cargo home (`$CARGO_HOME`, else
`~/.cargo`): `registry`, `.package-cache` and `.package-cache-mutate`. Cargo
therefore downloads into the caller's cache and takes the caller's locks, but
does not read the caller's `config.toml`. What the links point to is
created first if the caller's cargo home lacks it. Automatic cache cleaning
is off because the private home has no record of what was used when.

The project's own `.cargo/config.toml`, inside `src`, is read by cargo as
usual. What it says about planning, such as profiles, applies. What acts only
at build time, `rustflags` and `[env]`, is not applied before stage 3.

### What evaluation requires

- `allow-unsafe-native-code-during-evaluation = true`, via `--option`,
  `nix.conf` or `NIX_CONFIG`. A flake's `nixConfig` does not work.
- A committed `Cargo.lock` that matches `Cargo.toml`.
- Network access, or a cargo cache that already holds the project's crates.
- Import-from-derivation allowed (the default), since the tool, cargo and
  rustc are built or fetched during evaluation.
- A recent Nix: rostnix is developed against Nix 2.26. Pre-seeding uses
  `nix store add --mode flat`; where that fails, crates are downloaded at
  build time instead.

## Sources

### Registry crates

For each registry package that owns a unit:

1. **Checksum.** Taken from the package's entry in `Cargo.lock`.
2. **Store path.** Fixed-output, flat SHA-256, name `<name>-<version>.crate`,
   under `storeDir`. The tool computes it from the checksum; nothing is
   hashed.
3. **Pre-seed.** If the path does not exist, the tool runs
   `nix store add --mode flat --hash-algo sha256 --name <name> <file>` on the
   file in cargo's cache and checks the printed path equals the computed one.
   A mismatch is an error: the cached file is not what `Cargo.lock` names.
4. **Emit.** `b.fetchCrate { pname; version; sha256; url; }`.

Pre-seeding runs concurrently across crates. If `nix` is not on `PATH` or
the add fails, the tool warns on stderr and continues.

`fetchCrate` is two derivations: `pkgs.fetchurl` for the `.crate` file, and
one that unpacks it into a source tree, named `rustsrc-<name>-<version>`.
Because of pre-seeding the download normally never runs. It is the fallback
for a derivation built on a machine that did not evaluate it.

In stage 1 a package from a git repository or another registry is an error
that names it.

### Local crates

`b.localSource { name; dir; exclude; }` is a `builtins.path` copy of `src`
that holds the package directory `dir`, the directories leading to it, and
nothing under an excluded path. Paths are relative to `src`, and the copy
keeps `src`'s layout.

Rust has no list of the files a target reads, so a unit gets a view of its
package directory, narrowed by three rules:

1. Directories of other packages inside it are left out.
2. `examples/`, `tests/` and `benches/` are left out, unless the unit is
   itself an example, a test or a bench; then its own directory stays.
3. The root file of every other binary, example, test and bench is left out,
   and its directory when that root is a `main.rs` in a directory of its own.

For core-rs the library sees the package without `examples`, `tests` and
`benches`, and the example `amber-store` sees it without `tests`, `benches`
and the other two examples. Editing a test rebuilds nothing in stage 1, and
editing one example rebuilds only that example.

A unit that reads a file outside its view fails with rustc's or the build
script's own "file not found". `crateOverrides.<name>.extraSrc` adds paths
back. Files that no target reads, such as documentation, still rebuild the
package when they change; passing a filtered `src` avoids that.

A local unit runs with the workspace root as its working directory and a
relative source path, as under cargo. A path dependency outside `src` is an
error that names it.

## Derivations

| Kind | Name | One per |
|---|---|---|
| crate file | `<name>-<version>.crate` | registry crate version |
| crate source | `rustsrc-<name>-<version>` | registry crate version |
| build-script compile | `rustbs-<name>-<version>` | unit |
| build-script run | `rustbsrun-<name>-<version>` | unit |
| library | `rustlib-<name>-<version>` | unit |
| proc macro | `rustmacro-<name>-<version>` | unit |
| binary or example | `rustbin-<target>` | unit |
| application | `<pname>[-<version>]` | `buildRustApplication` call |

One package yields several units of a kind when cargo plans it more than
once, as it does for `libc` in core-rs.

### `compile`

Runs rustc once. Outputs:

- `$out/lib/`: the rlib of a library, and the dynamic library of a proc
  macro or `cdylib`;
- `$out/bin/<name>` for a binary, an example or a build script;
- `$out/unit.json`: the artifact, what dependents need (below), and the
  arguments and environment rustc ran with.

**Dependencies.** rustc needs the rlib of every transitive dependency, not
only the direct ones. The generated Nix names direct dependencies; each
library's `unit.json` lists its own transitive ones, and `compile` takes the
union over its direct dependencies. Because `unit.json` names those store
paths, they are in the output's closure and so in the sandbox of whatever
depends on it. The list stops at a proc macro, whose users do not need its
dependencies, and never includes a build script's.

The same file carries the library search paths (`-L`) printed by every build
script in the unit's closure, which cargo passes to every dependent rustc.

**Linking.** A unit links when it has a crate type of `bin`, `proc-macro`,
`cdylib` or `dylib`. Units that do not link are a bare `derivation` whose
builder is the tool; they do not use stdenv. Units that link are built with
`stdenv.mkDerivation`, which supplies the C compiler, the linker and the
platform's libraries, and with the `buildInputs` of every overridden package
in their closure. `resolve` lists those packages in the node's `overrides`.

### `runBuildScript`

Runs the executable a `build-script` unit produced, in the package directory,
with `stdenv.mkDerivation` and the package's override entry. Outputs:

- `$out/out/`, which is `OUT_DIR` itself, so paths the script prints or
  embeds stay valid;
- `$out/output`, the script's stdout;
- `$out/unit.json`, the directives parsed from it.

See [Build scripts](#build-scripts).

### Application

Copies each executable out of its unit into `$out/bin`. A unit's output
refers to its dependencies through `unit.json`; the copy does not, so the
application's closure holds only what the executables themselves refer to.

macOS needs one more step when the profile keeps debug information. There
it stays in the object files, and the executable points at them, which
would keep every unit alive, and through `unit.json` the sources and the
toolchain. Such an executable gets a `.dSYM` bundle beside it, made with
`dsymutil`, and has the pointers stripped. ripgrep, whose release profile
sets `debug = 1`, showed the need.

## rustc flags

`resolve` computes every flag that does not depend on a store path and puts
it in the node's `rustcArgs`, by cargo 1.95's rules:

- `--crate-name`, `--edition`, one `--crate-type` per crate type, and for a
  proc macro `-C prefer-dynamic` and `--extern proc_macro`;
- the unit's profile: `-C opt-level`, `-C panic`, `-C codegen-units`,
  `-C debuginfo`, `-C debug-assertions`, `-C overflow-checks`, `-C strip`,
  `-C split-debuginfo` and `-C rpath`, each only where cargo passes it;
- LTO, by the rules of cargo's `lto.rs`, which choose per unit among
  `-C lto`, `-C linker-plugin-lto`, `-C embed-bitcode=no` and no flag, from
  the profile, the unit's crate types and what its dependents need;
- `--cfg 'feature="…"'` for each enabled feature, and the `--check-cfg`
  values cargo derives from the package's declared features;
- lints: `--cap-lints allow` for a package that is not local, and for a local
  package the flags of its `[lints]` table, including one inherited from
  `[workspace.lints]`;
- `-C metadata` and `-C extra-filename`.

`compile` adds what depends on paths: the source file, `--out-dir`,
`--emit=link`, `-L dependency=`, one `--extern` per direct dependency, the
flags its build script printed, and `--remap-path-prefix`.

**Metadata.** `metadata` is a hash of the unit's identity: package name,
version and source, target, mode, features, profile, and the `metadata` of
its dependencies. It keeps two versions of a crate, or two builds of one
version, apart in one binary. It contains no store path, so it is stable
across machines. It is not cargo's value.

**Path remapping.** Each source store path is remapped to
`<name>-<version>`, so panic messages and debug information do not mention
the store and an executable does not keep its sources alive.

**Environment.** rustc gets the variables cargo sets: `CARGO_PKG_*`,
`CARGO_MANIFEST_DIR`, `CARGO_MANIFEST_PATH`, `CARGO_CRATE_NAME`,
`CARGO_BIN_NAME`, `CARGO_PRIMARY_PACKAGE`, `CARGO`, `OUT_DIR` when the
package has a build script, and what that script set with `rustc-env`.

## Build scripts

`run-build-script` sets the environment cargo documents for build scripts:
`OUT_DIR`, `TARGET`, `HOST`, `NUM_JOBS`, `OPT_LEVEL`, `DEBUG`, `PROFILE`,
`RUSTC`, `RUSTDOC`, `CARGO`, `CARGO_MANIFEST_DIR`, `CARGO_MANIFEST_LINKS`,
`CARGO_PKG_*`, `CARGO_ENCODED_RUSTFLAGS`, one `CARGO_FEATURE_<NAME>` per
enabled feature, and `CARGO_CFG_*` from `rustc --print=cfg`, queried in the
derivation as cargo queries it.

It reads `cargo::` and `cargo:` lines from the script's stdout:

| Directive | Effect |
|---|---|
| `rustc-link-lib`, `rustc-link-search`, `rustc-flags` | `-l` for the package's own library, or for its executables when it has none; `-L` for the package's units and for every unit that depends on them. |
| `rustc-cfg`, `rustc-check-cfg`, `rustc-env` | Flags and environment of the package's own units. |
| `rustc-link-arg` and its `-bin`, `-bins`, `-cdylib`, `-examples`, `-tests`, `-benches` forms | `-C link-arg` on the kinds of unit each form names. |
| `metadata`, and any other `KEY=VALUE` in the one-colon form | `DEP_<LINKS>_<KEY>` for the build scripts of packages that depend on this one, when this package sets `links`. |
| `warning` | Printed to the build log for a local package. A foreign package's warnings stay in `$out/output`, as cargo shows them only with `-vv`. |
| `error` | The derivation fails. |
| `rerun-if-changed`, `rerun-if-env-changed` | Ignored: Nix decides when to rerun. |

A build-script run depends on the runs its unit lists in the graph, which
are those of the `links` packages it depends on. It reads their `unit.json`
for the `DEP_*` values.

The package directory is a store path and read-only. A build script that
writes outside `OUT_DIR` fails.

## crateOverrides

```nix
crateOverrides = {
  libz-sys = {
    buildInputs = [ pkgs.zlib ];
    nativeBuildInputs = [ pkgs.pkg-config ];
  };
  my-crate.extraSrc = [ "proto" "README.md" ];
};
```

A key is a package name.

| Attribute | Default | Meaning |
|---|---|---|
| `buildInputs` | `[ ]` | Libraries. The package's build-script run gets them, and so does every unit that links and has the package in its closure. |
| `nativeBuildInputs` | `[ ]` | Tools the package's build script and rustc invocations run, such as `pkg-config`. |
| `env` | `{ }` | Environment of the package's build script and of its rustc invocations. |
| `extraSrc` | `[ ]` | Local packages only: files and directories, relative to `src`, added to the view of every unit of the package. |

A package with an entry builds all its units with stdenv. Any other
attribute is an error. A key that names no package of the build gets a
warning, because such an entry changes nothing. It is not an error because
the same key may match on another platform or with other features.

core-rs needs no entry: its native libraries are compiled from the sources
their crates bundle.

## Conformance with cargo

rostnix reconstructs what cargo would run, so the reconstruction is tested
against cargo. For a fixture, the conformance test runs `cargo build -vv`
with the same toolchain and selection, reads the rustc command lines and
environments cargo prints, and compares them with the `argv` and `env`
recorded in each unit's `unit.json`.

Each side is reduced to a set of normalised invocations: the flags, and the
values of the variables cargo sets. The two sets must be equal, which also
means both sides have the same units. Normalising removes these differences:

- paths, by replacing each side's directories with symbolic names;
- `-C metadata` and `-C extra-filename`, which rostnix computes its own way;
- `--emit`, `--error-format`, `--json` and `-C incremental`, which serve
  cargo's own bookkeeping;
- `--extern` naming an `.rmeta` under cargo, which pipelines, and the rlib
  under rostnix;
- `--remap-path-prefix`, which rostnix adds;
- `--cap-lints warn`, which cargo passes for foreign packages in place of
  `allow` because `-vv` asks to see their warnings;
- what a `crateOverrides` entry adds to the environment, which `unit.json`
  records apart from what cargo would set;
- `NUM_JOBS`, `CARGO_MAKEFLAGS` and the library path variables, which
  describe the machine.

## Tests (stage 2)

With `doCheck`, `resolve` makes a second pass with
`cargo test --no-run --unit-graph`, the same selection and the same profile.
A unit that both passes plan alike has the same `metadata` and is the same
derivation, so tests share every dependency they can with the application.
A dependency that gains features from dev-dependencies is a separate unit,
as under cargo.

- **Test executables.** Each unit of mode `test`, whether a library's unit
  tests or an integration test, is a `compile` with `--test`. Integration
  tests get `CARGO_BIN_EXE_<name>` for their package's binaries.
- **Test runs.** One derivation per test executable. It copies the
  executable to `target/<profile>/deps/` and the examples cargo planned to
  `target/<profile>/examples/` inside its build directory, which is the
  layout of cargo's target directory. It runs the executable from the package
  directory with the environment cargo sets for tests and the arguments in
  `checkFlags`. Its output is the log.
- **Sources.** A test unit sees its package directory with `tests/`, less
  the other tests' root files. The run sees the same tree.
- **Skipping.** `skipTests` names test targets that are not run. core-rs
  needs `skipTests = [ "amber_bench_smoke" ]`: that test runs `cargo build`,
  which cannot work in a derivation.

The application depends on every test run, so a failing test fails the
build. Doc tests are not run.

`doCheck` defaults to `true` from stage 2 on. In stage 1 it is accepted and
ignored.

## Later stages

**Stage 3, other sources and build configuration.**

- Git dependencies are fetched with `builtins.fetchGit` at the revision in
  `Cargo.lock`, with the evaluating user's git credentials.
- Other registries: the private cargo home gets the `[registries]`,
  `[registry]`, `[source]`, `[net]`, `[http]` and `[credential-alias]` tables
  of the caller's `config.toml` and a link to the caller's credentials.
  Crates are pre-seeded as for crates.io. The fallback download cannot
  authenticate, so a private crate builds only where it was evaluated or
  substituted.
- `rustflags` and `[env]` from the project's `.cargo/config.toml` are
  applied to the units cargo applies them to.

**Stage 4, cross-compilation and the rest.**

- Cargo plans with `--target`; units for the target get `--target` and the
  cross linker of `pkgs.stdenv.cc`, units for the build machine do not.
  `evalPkgs` names the package set of the evaluating machine when it differs
  from the build platform.
- `cdylib` and `staticlib` targets can be installed.
- An adapter reads `buildInputs` and `nativeBuildInputs` from nixpkgs'
  `defaultCrateOverrides`.

## Error handling

The tool writes diagnostics to stderr, which Nix passes through, and exits
non-zero. Nix then reports that the program failed.

| Situation | Behaviour |
|---|---|
| `builtins.exec` unavailable | `buildRustApplication` throws, naming `allow-unsafe-native-code-during-evaluation` and the three ways to set it. `mkRustEnv` itself does not need `exec`. |
| `Cargo.lock` missing or out of date | Reports cargo's `--locked` message and says to commit an up-to-date `Cargo.lock`. |
| A crate cannot be downloaded | Reports cargo's message. |
| A package from git or another registry, before stage 3 | Names the package and its source. |
| A path dependency outside `src` | Names the package and the resolved path. |
| A package without a checksum in `Cargo.lock` | Names the package. |
| Pre-seeded path differs from the computed one | Names the crate and its cache file, and says the file does not match `Cargo.lock`. |
| `nix` missing or `nix store add` fails | Warning only; the download derivation covers it. |
| A unit of a kind or mode this version does not build | Names the unit and its mode. |
| A unit planned for another target, before stage 4 | Names the target and says cross-compilation is not supported yet. |
| The selection builds no binary or example | `buildRustApplication` throws, naming `bins` and `examples`. |
| Unknown `crateOverrides` attribute | `buildRustApplication` throws, naming it and the attributes an entry takes. |
| A build script fails or prints `cargo::error` | The run derivation fails with the script's output in its log. |

## Not in this version

Doc tests, benches, `cargo doc`, workspaces whose root is outside `src`,
`vendor` directories and source replacement, artifact dependencies,
`build-std`, `-Z` features other than the unit graph, content-addressed
derivations, sccache or any compiler wrapper, and Windows.

## Testing rostnix

### Unit tests (Rust)

- Decoding the unit graph and `cargo metadata`, from recorded output of
  cargo 1.95.
- `rustcArgs` for recorded units against recorded `cargo build -vv` lines,
  covering profiles, LTO modes, lints and proc macros.
- The LTO rules, on small graphs whose expected flags were recorded from
  cargo 1.95.
- Store-path computation against `nix-store --print-fixed-path`.
- The Nix emitter, with golden files.
- Parsing of build-script output, every directive in both forms.
- Source views: the exclusion rules on sample package layouts.

### Integration fixtures

Small Rust projects under `tests/fixtures/`, and patient zero, built by a
script with real `nix build` and the `exec` option. They run outside the Nix
sandbox because they need `exec` and the network.

| Fixture | Covers |
|---|---|
| `hello` | One package with a library and a binary, crates.io dependencies with build scripts and a proc macro, the default selection. |
| `workspace` | Four members, one of them a proc macro and one nested in another's directory; `packages`, `bins` and `features`; a renamed dependency; lints inherited from the workspace. |
| `buildscript` | A local build script that generates code into `OUT_DIR`, compiles C, sets `links` and metadata read by a dependent's build script; `rustc-cfg` and `rustc-env`; a `crateOverrides` entry that supplies zlib through `pkg-config`; `extraSrc`. |
| `profiles` | One project built under four profiles: fat LTO with `panic = "abort"`, `opt-level = "s"`, `codegen-units` and a per-package override; thin LTO; no LTO; and `dev`. |
| core-rs | Patient zero at its pinned commit, fetched with `builtins.fetchTree`: the `amber-store` example. |
| rostnix | rostnix builds itself with `buildRustApplication`. |

Each fixture asserts:

- The executables run and print the expected output.
- [Conformance with cargo](#conformance-with-cargo) holds.
- Editing one file changes only the expected derivation paths. For core-rs:
  editing `src/lib.rs` changes the library and the example and no
  dependency; editing a file under `tests/` changes nothing.

Anything that needs `builtins.exec` lives under the flake's
`legacyPackages`, which `nix flake check` and `nix flake show` do not
evaluate. `packages.rostnix` is the tool built with `buildRustPackage` and
needs no option.

## Repository layout

```
flake.nix            lib.mkRustEnv, packages.rostnix, legacyPackages, dev shell
default.nix          { pkgs }: non-flake entry point
Cargo.toml           one package: library and binary
Cargo.lock
src/main.rs          subcommand dispatch
src/lib.rs           the modules below
src/unitgraph.rs     running cargo, decoding the unit graph
src/metadata.rs      decoding cargo metadata
src/lockfile.rs      checksums from Cargo.lock
src/cargohome.rs     the private cargo home and cargo's environment
src/graph.rs         the graph model: units, packages, keys, closures
src/lto.rs           cargo's per-unit LTO rules
src/lints.rs         [lints] tables to flags
src/flags.rs         the rustc flags that do not depend on paths
src/localsrc.rs      source views of local packages
src/storepath.rs     fixed-output store paths, name sanitising
src/seed.rs          pre-seeding .crate files
src/emit.rs          graph to Nix
src/resolve.rs       the evaluation-time pipeline
src/node.rs          reading a derivation's node from its attributes
src/compile.rs       the compile subcommand
src/buildscript.rs   the run-build-script subcommand, directive parsing
nix/                 mk-rust-env.nix, tool.nix, builders.nix, build-rust-application.nix
examples/conformance.rs   comparing units with a cargo build -vv log
testdata/            recorded cargo output for the unit tests
tests/fixtures/      integration fixtures
tests/fixtures.nix   the fixtures, core-rs and rostnix itself as builds
tests/run.sh         integration driver
```

## Build order

1. **crates.io end to end.** Resolve, the private cargo home, pre-seeding,
   the emitter, `fetchCrate`, `localSource`, `compile` for libraries, proc
   macros, binaries, examples and build scripts, `runBuildScript`,
   `crateOverrides`, the application derivation, and the conformance test.
   Done when every fixture above passes: core-rs's `amber-store` builds and
   runs, and rostnix builds itself.
2. **Tests.** The `cargo test` pass, test executables, test runs in cargo's
   layout, `doCheck`, `checkFlags`, `skipTests`. Done when core-rs's test
   suite passes apart from `amber_bench_smoke`.
3. **Other sources and build configuration.** Git dependencies, other
   registries, `rustflags` and `[env]`.
4. **Cross-compilation and the rest.** `--target`, `evalPkgs`, `cdylib` and
   `staticlib` outputs, the nixpkgs overrides adapter.

Each stage ends with its fixtures passing, and each gets its own
implementation plan, written when the previous stage is done. The first plan
covers stage 1 only.
