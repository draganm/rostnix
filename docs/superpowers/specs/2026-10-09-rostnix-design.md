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
   run of a build script. A test is a unit too: its derivation compiles the
   test executable and runs it. Cargo itself does not run at build time.
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
  runs cargo itself and one that creates a setuid file, which Nix lets no
  build do.
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
| Tests read `tests/golden` through `env!("CARGO_MANIFEST_DIR")`, and `golden_packstore` copies files from there and writes to the copies. `cli_e2e` looks for `examples/amber-store` beside the directory of its own executable. `amber_bench_smoke` runs `cargo build`. `golden_tar_extracts` expects an extracted file to keep its setuid bit. | Stage 2: a test is compiled and run in a writable copy of its package, laid out like cargo's target directory. A test target can be skipped, and so can one test by the harness's own flag. |

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
| `cargo test --no-run --unit-graph -Z unstable-options` prints a version 1 graph. Its roots are each library and binary in mode `test`, each integration test, each example in mode `build`, and a `doctest` unit per library. An integration test depends on its package's binaries, built in mode `build`. | The graph of the `hello` fixture, recorded in `testdata/hello/`. |
| A test is compiled without `--crate-type` and with `--test` between the profile's flags and the features; with `harness = false`, `--cfg test` instead. A proc macro's unit tests keep `-C prefer-dynamic` and `--extern proc_macro`. An integration test is compiled with `CARGO_BIN_EXE_<name>` for each binary of its package and with `CARGO_TARGET_TMPDIR`, and gets no `--extern` for a binary. | `cargo test -vv` on the fixtures. |
| A test runs in its package directory, from `target/<profile>/deps/<crate>-<hash>`, with `CARGO`, `CARGO_MANIFEST_DIR`, `CARGO_MANIFEST_PATH`, `CARGO_PKG_*` and the library path variable; an integration test with `CARGO_BIN_EXE_<name>` again; a package with a build script with `OUT_DIR` and what the script set with `rustc-env`. It is not given `CARGO_CRATE_NAME`, `CARGO_PRIMARY_PACKAGE` or `CARGO_TARGET_TMPDIR`. | A test that prints its environment, and the `Running` lines of `cargo test -vv`, which include it. |
| A file copied out of the store with `fs::copy` cannot be written to: the copy keeps the mode of the original. | core-rs's `golden_packstore` failed with `PermissionDenied` when it was compiled against a store path. |
| A Nix build cannot create a setuid file. | `chmod 4755` in a bare derivation: `Operation not permitted`, with `sandbox = relaxed` on macOS. |

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
| `doCheck` | `true` | Build and run the tests of the selected packages; see [Tests](#tests). |
| `checkFlags` | `[ ]` | Arguments for every test executable. |
| `skipTests` | `[ ]` | Names of test targets that are neither built nor run. |
| `meta` | `{ }` | Passed through. |

The selection means what it means to `cargo build`. With neither `bins` nor
`examples`, cargo builds the library and every binary of the selected
packages. With either, it builds only what they name.

Every binary and example the selection builds lands in `$out/bin` under its
target name. A selection that builds neither is an error that names `bins`
and `examples`. `passthru` exposes `graph`, `units`, `bins` and `tests`,
each unit a derivation, so one unit can be built alone. `tests` holds the
tests that are run. `unitRecords` is a directory of the `unit.json` of
every unit `cargo build` plans, and `testUnitRecords` the same for
`cargo test`, with the record of each test's run.

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
| `test` | in a derivation | Compiles one test executable and runs it. |

Build-time subcommands read their node from the derivation's attributes.
Their derivations use `__structuredAttrs`, so the node arrives as JSON in the
attributes file and its size is not bounded by the environment.

### 2. The static Nix library

`nix/` holds `mkRustEnv` and the builder functions the generated code calls:
`fetchCrate`, `localSource`, `compile`, `runBuildScript` and `test`. Each
defines how one kind of node is built. All graph wiring is in the generated
code.

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
   - with `doCheck`, `cargo test --no-run --unit-graph -Z unstable-options
     --locked` with the selected packages, for the units of the tests;
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

- **Carried over** from the caller: `HOME`, `USER`, `LOGNAME`, `PATH`,
  `TMPDIR`, the proxy variables, the certificate variables (`SSL_CERT_FILE`,
  `NIX_SSL_CERT_FILE`, `CURL_CA_BUNDLE`), and `CARGO_HTTP_*` and
  `CARGO_NET_*`.
- **Set by the tool:** `CARGO_HOME` (the private home), `RUSTC`,
  `RUSTC_BOOTSTRAP=1`, `CARGO_TARGET_DIR` (a temporary directory that stays
  empty), `CARGO_GC_AUTO_FREQUENCY=never` and
  `CARGO_TERM_PROGRESS_WHEN=never`.
- **Dropped:** everything else, among it `RUSTFLAGS`, `CARGO_BUILD_*`,
  `CARGO_PROFILE_*` and `RUSTC_WRAPPER`.

`RUSTC_BOOTSTRAP` is set for these cargo runs only. It never reaches a
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

1. **Checksum.** Taken from the package's entry in `Cargo.lock`, or from
   the `[metadata]` table of a lockfile in the first format.
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
   itself an example, a test or a bench, or has its root in one of them;
   then that directory stays, as it does for a unit whose root names a
   file in it as a module. Every unit built as a test keeps all three, the
   unit tests of a library or a binary included: a test runs in its package
   directory and may read whatever lies there, and one that looks through a
   directory that is missing finds nothing wrong. It loses only the root
   files of other targets, by the next rule.
3. The root file of every other binary, example, test and bench is left out.
   Where cargo found such a target as a directory, `src/bin/<name>/main.rs`
   and its like under `examples/`, `tests/` and `benches/`, the directory is
   left out. A `main.rs` anywhere else hides only itself, since its
   directory may hold other targets' modules. A root that the unit's own
   root names as a module stays: `tests/common.rs` is a test of its own to
   cargo, and a module to the tests beside it that say `mod common;`.
   `resolve` finds such declarations, and `#[path = "…"]` attributes, by
   searching the text of the root file. One written by a macro, or
   declared in a module file rather than in the root, is not found.

A build-script run is exempt from the third rule. Build scripts read source
files that rustc is never told about, the roots of the package's
executables among them, as `cxx_build::bridge("src/main.rs")` does.

For core-rs the library sees the package without `examples`, `tests` and
`benches`, and the example `amber-store` sees it without `tests`, `benches`
and the other two examples. The test `cbor` sees it without the root files
of the three examples and of the other 20 tests. Editing `tests/cbor.rs`
builds and runs that test again and nothing else; without `doCheck` it
rebuilds nothing.

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
| test, compiled and run | `rusttest-<target>` | unit |
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
They keep cargo's order: the unit's own script, then its dependencies' by
package, and across all of them the paths inside the `OUT_DIR` of the script
that printed them before the others, so that a library a script built wins
over one of the same name found elsewhere. The file also carries the linker
arguments scripts ask of cdylibs, which cargo passes to every cdylib that
has the script's package in its closure.

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
the store and an executable does not keep its sources alive. The `OUT_DIR`
of the package's build script is remapped to `<name>-<version>/out` for the
same reason: code generated there is compiled from there, and a panic in it
would otherwise name the script's run, whose `unit.json` refers to the
script, the compiler and everything the script was built from.

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

It reads `cargo::` and `cargo:` lines from the script's stdout, ignoring
whitespace around a line as cargo does:

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
| `buildInputs` | `[ ]` | Libraries. The package's build-script run gets them; so does every build-script run that depends on it through `links`, directly or not; and so does every unit that links and has the package in its closure. |
| `nativeBuildInputs` | `[ ]` | Tools the package's build script and rustc invocations run, such as `pkg-config`. |
| `env` | `{ }` | Environment of the package's build script and of its rustc invocations. |
| `extraSrc` | `[ ]` | Local packages only: files and directories, relative to `src`, added to the view of every unit of the package. |

A package with an entry builds all its units with stdenv. `env` values
become strings as `toString` makes them, and an `extraSrc` path may be
written with a leading `./` or a trailing `/`. `extraSrc` on a package that
is not local is an error, because it would do nothing. Any other attribute
is an error. A key that names no package of the build gets a
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
- the order of flags, except among lints and among the `-L` and `-l` flags
  from build scripts, where order decides which lint level or which library
  wins;
- `--remap-path-prefix`, which rostnix adds;
- `--cap-lints warn`, which cargo passes for foreign packages in place of
  `allow` because `-vv` asks to see their warnings;
- what a `crateOverrides` entry adds to the environment, which `unit.json`
  records apart from what cargo would set;
- `NUM_JOBS`, `CARGO_MAKEFLAGS` and the library path variables, which
  describe the machine.

Tests are compared the same way, against `cargo test -vv`: what is compiled
for them, and each run of a test executable with its arguments and the
environment cargo prints for it. A test executable is named by its crate,
without the hash in its file name, and the value of `CARGO_BIN_EXE_<name>`
by the file it names. Doc tests, which cargo runs through rustdoc, are left
out of cargo's side. For a project with a test that cannot run where the
reference is made, the reference is `cargo test --no-run` and only what is
compiled is compared.

## Tests

With `doCheck`, `resolve` asks cargo for a second plan,
`cargo test --no-run --unit-graph`, with the selected packages, the same
features and the same profile. `bins` and `examples` choose what is
installed; they do not narrow what is tested. A unit that both plans
describe alike has the same `metadata` and is the same derivation, so tests
share every dependency they can with the application. A dependency that
gains features from dev-dependencies is a separate unit, as under cargo.
Because `CARGO_PRIMARY_PACKAGE` follows from a plan's roots, whether a
unit's package is among them is part of its `metadata`.

A unit of mode `test` is a library's or a binary's unit tests, or an
integration test. Its derivation, built by `rostnix test`, compiles the test
executable and runs it. The two are one derivation because cargo gives a
test a promise that two could not keep: what the test is told when it is
compiled, `CARGO_MANIFEST_DIR` above all, names a directory that is still
there when it runs, and that it may write to. A test compiled in one
derivation would have a store path compiled into it. Its fixtures would be
read-only, and so would the copies it makes of them, since a copy keeps the
mode of the original. core-rs's `golden_packstore` does exactly that.

The derivation:

1. copies the unit's view of the source into its build directory and makes
   the copy writable;
2. creates cargo's target directory at the workspace root of the copy:
   `target/tmp`, `target/<profile>/deps` and `target/<profile>/examples`,
   where `<profile>` is `debug` for the profiles `dev` and `test`, and the
   profile's name otherwise;
3. for an integration test or a bench, copies the package's binaries into
   `target/<profile>/` and its examples into `target/<profile>/examples/`.
   Unit tests get none: cargo promises them none, as `cargo test --lib`
   builds no binary;
4. runs rustc as `compile` would, in the copy and into
   `target/<profile>/deps`, with `--test` in place of the crate types, or
   `--cfg test` for a target that says `harness = false`. Paths are not
   remapped, since nothing of a test is installed. An integration test or a
   bench is given `CARGO_BIN_EXE_<name>` for each of the binaries and
   `CARGO_TARGET_TMPDIR`;
5. runs the executable in the package directory of the copy, with the
   arguments in `checkFlags` and the environment cargo sets for a test:
   `CARGO`, `CARGO_MANIFEST_DIR`, `CARGO_MANIFEST_PATH`, `CARGO_PKG_*`, the
   library path variable, `CARGO_BIN_EXE_<name>` again for an integration
   test, and `OUT_DIR` with the `rustc-env` values of the package's build
   script. `RUST_TEST_THREADS` is set to the cores the build was given,
   unless something set it already. What the test prints goes to the build
   log and to the output. The derivation ends when the test does, as
   `cargo test` returns then: it does not wait for a process the test left
   behind, a server it started and did not stop, although that process
   still holds the test's output open.

Its output holds `log`, what the test printed, and the records of the
compilation and of the run, `unit.json` and `run.json`. The executable is
not kept: the paths compiled into it are gone with the build directory.

In the generated graph a test is a unit built by `b.test`, which takes what
`b.compile` takes and two attributes more:

```nix
  units."hello-0.1.0-test-cli-1a2b3c4d" = b.test {
    name = "rusttest-cli";
    kind = "test";          # what is built
    targetKind = "test";    # what the target is: lib, bin, example, test, bench, …
    # … as for b.compile …
    profileDir = "release";
    executables = [ units."hello-0.1.0-bin-hello-…" units."hello-0.1.0-example-extra-…" ];
  };
  tests."hello-0.1.0-test-cli-1a2b3c4d" = units."hello-0.1.0-test-cli-1a2b3c4d";
  testBuilds = [ units."hello-0.1.0-example-extra-…" ];   # built by cargo test, not run
  buildUnits = [ /* keys of the units cargo build plans */ ];
  testUnits = [ /* keys of the units cargo test plans */ ];
```

A test derivation is built with stdenv and gets the tools and the
environment of its package's `crateOverrides` entry, and the libraries of
every overridden package it links.

- **Sources.** A test unit sees its package directory with `examples/`,
  `tests/` and `benches/`, less the root files of the other targets. It is
  compiled from that tree and runs in it.
- **Skipping.** `skipTests` names test targets that are neither built nor
  run: the file name of an integration test without `.rs`, or the name of a
  library or a binary for its unit tests. An entry that names no test target
  gets a warning that lists the targets. One test inside an executable is
  skipped with the harness's own flag, `checkFlags = [ "--skip" "name" ]`.
- **What cargo plans and rostnix leaves out.** Doc tests, which rustdoc
  compiles and runs. An example that is a library, which `cargo test`
  builds only to see that it compiles.

The application lists every test that is not skipped among its inputs, so a
failing test fails the build. It also lists what `cargo test` builds
without running, the examples, so that one that does not compile fails the
build as it fails `cargo test`. Neither leaves anything in the result, so
the result does not refer to them.

What follows from this design:

- **The promise holds for the test crate, not for what it links.** The
  library under test and any helper crate are compiled in their own
  derivations, from the store. `env!("CARGO_MANIFEST_DIR")` in their
  non-test code names a read-only view without `tests/`. A test that gets
  its fixture directory from such a function does not find it.
- **The copy holds the unit's view, not the workspace.** The workspace's
  `Cargo.toml` and `Cargo.lock` and the other members are not in it unless
  `extraSrc` names them.
- **`skipTests` and `checkFlags` are coarse.** A name in `skipTests` skips
  every test target of that name: in every selected package, and a
  library's unit tests together with an integration test named like the
  library. Every test executable gets the same `checkFlags`.
- **A test is an input of the application.** Editing a test, `checkFlags` or
  `skipTests` gives the application another store path although its
  executables are the same, and what depends on it is built again.
- **Tests are built with the application's profile.** With `release`, the
  default, `debug_assert!` and overflow checks are off, as under
  `cargo test --release`.
- **What only the tests need can stop the evaluation.** A dev-dependency
  from a git repository is refused like any other. The message then says
  that it concerns the tests only and that `doCheck = false` builds without
  them.

What Nix forbids a build, a test cannot do. In a sandboxed build there is
no network. No build can create a setuid file, sandboxed or not.

core-rs needs two settings. `skipTests = [ "amber_bench_smoke" ]`: that
test runs `cargo build`. `checkFlags = [ "--skip" "golden_tar_extracts" ]`:
that one extracts a setuid file from an archive and checks its mode.

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
| An example that is not an executable | Names the example and its crate type. |
| A target whose root file is outside its package directory, a `path` with `..` in it included | Names the target and the file. |
| Two selected executables with one name | Names both units and says to select one. |
| A unit planned for another target, before stage 4 | Names the target and says cross-compilation is not supported yet. |
| The selection builds no binary or example | `buildRustApplication` throws, naming `bins` and `examples`. |
| Unknown `crateOverrides` attribute | `buildRustApplication` throws, naming it and the attributes an entry takes. |
| `extraSrc` on a package that is not local | `buildRustApplication` throws, naming the entry. |
| A build script fails or prints `cargo::error` | The run derivation fails with the script's output in its log. |
| A test fails | Its derivation fails with what the test printed in its log, and the application is not built. |
| A `skipTests` entry names no test target | Warning that names the entry and lists the test targets. |
| Something only the tests need is refused, or cargo cannot plan them | The error, followed by a line that says it concerns the tests only and names `doCheck = false`. |

## Not in this version

Doc tests, benches, `cargo doc`, workspaces whose root is outside `src`,
`vendor` directories and source replacement, artifact dependencies,
`build-std`, `-Z` features other than the unit graph, content-addressed
derivations, sccache or any compiler wrapper, dependencies built as Rust
`dylib`s, and Windows.

Known gaps, none of which the fixtures meet:

- An integration test is given `CARGO_BIN_EXE_<name>` for the binaries
  cargo plans to build for it. Cargo also sets the variable for a binary
  it does not build because its `required-features` are off.
- A test that looks in the target directory for a binary or an example of
  another workspace member does not find it. Cargo makes no promise that
  it is there: `cargo test -p <package>` does not build it.
- A custom profile defined in cargo configuration rather than in
  `Cargo.toml` is taken to descend from `release` when a build script is
  told `PROFILE`.
- A local path dependency that belongs to another workspace gets this
  workspace's `[workspace.lints]` when it says `lints.workspace = true`.
- On Linux, C objects a build script compiles with debug information may
  name their sources in the store, which the executable then refers to.
  Linux is untested as a whole.

## Testing rostnix

### Unit tests (Rust)

- Decoding the unit graph and `cargo metadata`, from recorded output of
  cargo 1.95.
- `rustcArgs` for recorded units against recorded `cargo build -vv` lines,
  covering profiles, LTO modes, lints and proc macros, and against
  `cargo test -vv` lines for tests.
- The two plans of the `hello` fixture, recorded from cargo 1.95: what they
  share, which units are tests, what each test sees and finds beside it.
- The layout and the environment of a test.
- The LTO rules, on small graphs whose expected flags were recorded from
  cargo 1.95.
- Store-path computation against `nix-store --print-fixed-path`.
- The Nix emitter, with golden files.
- Parsing of build-script output, every directive in both forms.
- Source views: the exclusion rules on sample package layouts, and the
  search for module declarations.

### Integration fixtures

Small Rust projects under `tests/fixtures/`, and patient zero, built by a
script with real `nix build` and the `exec` option. They run outside the Nix
sandbox because they need `exec` and the network.

| Fixture | Covers |
|---|---|
| `hello` | One package with a library and a binary, crates.io dependencies with build scripts and a proc macro, the default selection. Tests: unit tests, an ignored test that fails, and an integration test of what a test may rely on: the binary through `CARGO_BIN_EXE_`, the example beside its own executable, data read through the working directory and `CARGO_MANIFEST_DIR`, a working directory and a `CARGO_TARGET_TMPDIR` it can write to, a copied fixture it can change, and the environment. |
| `workspace` | Four members, one of them a proc macro and one nested in another's directory; `packages`, `bins` and `features`; a renamed dependency; lints inherited from the workspace. Tests: unit tests of a library and of the proc macro, a test with `harness = false`, a helper file in `tests/` that a test names as a module and that cargo also builds as a test, and a dev-dependency that turns a feature on, so that the binary the tests run is not the one installed. |
| `buildscript` | A local build script that generates code into `OUT_DIR`, compiles C, sets `links` and metadata read by a dependent's build script; `rustc-cfg` and `rustc-env`; a `crateOverrides` entry that supplies zlib through `pkg-config`; `extraSrc`. Tests: unit tests that call the C function and read `OUT_DIR` and the script's `rustc-env` value at run time. |
| `profiles` | One project built under four profiles: fat LTO with `panic = "abort"`, `opt-level = "s"`, `codegen-units` and a per-package override; thin LTO; no LTO; and `dev`. Tests: one that expects a panic, which cargo has unwind under every profile. |
| core-rs | Patient zero at its pinned commit, fetched with `builtins.fetchTree`: the `amber-store` example, and its test suite: 21 of 22 test executables, with one test skipped by name. |
| rostnix | rostnix builds itself with `buildRustApplication` and runs its own unit tests. |

Each fixture asserts:

- The executables run and print the expected output.
- Its tests pass, under rostnix and under cargo itself.
- [Conformance with cargo](#conformance-with-cargo) holds, for the build and
  for the tests.
- Editing one file changes only the expected derivation paths. For core-rs:
  editing `src/lib.rs` changes the library and the example and no
  dependency; editing `tests/cbor.rs` changes that test alone, and nothing
  without `doCheck`.

The driver also checks that a failing test fails the build and is named,
that `skipTests` leaves a test out and warns about an entry that matches
nothing, that `doCheck = false` plans no test, and that the result refers
to no test.

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
src/localsrc.rs      source views of local packages, module declarations
src/storepath.rs     fixed-output store paths, name sanitising
src/seed.rs          pre-seeding .crate files
src/emit.rs          graph to Nix
src/resolve.rs       the evaluation-time pipeline
src/node.rs          reading a derivation's node from its attributes
src/compile.rs       the compile subcommand
src/buildscript.rs   the run-build-script subcommand, directive parsing
src/testrun.rs       the test subcommand: a test compiled and run in a copy of its source
nix/                 mk-rust-env.nix, tool.nix, builders.nix, build-rust-application.nix
examples/conformance.rs   comparing units and test runs with a cargo -vv log
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
2. **Tests.** The `cargo test` plan, tests compiled and run in cargo's
   layout, `doCheck`, `checkFlags`, `skipTests`. Done when core-rs's test
   suite passes apart from `amber_bench_smoke` and the one test Nix rules
   out.
3. **Other sources and build configuration.** Git dependencies, other
   registries, `rustflags` and `[env]`.
4. **Cross-compilation and the rest.** `--target`, `evalPkgs`, `cdylib` and
   `staticlib` outputs, the nixpkgs overrides adapter.

Each stage ends with its fixtures passing, and each gets its own
implementation plan, written when the previous stage is done. The first plan
covers stage 1 only.
