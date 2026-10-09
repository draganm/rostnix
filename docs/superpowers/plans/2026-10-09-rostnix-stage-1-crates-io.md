# rostnix Stage 1 (crates.io, End to End) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A flake with `src = ./.` and no generated files builds a Rust project with crates.io dependencies, one derivation per cargo unit, planned at evaluation time through `builtins.exec`. Patient zero, the `amber-store` example of amber-store/core-rs, builds and runs.

**Architecture:** One Rust binary, `rostnix`, has two roles. At evaluation time `rostnix resolve` runs `cargo build --unit-graph` and `cargo metadata`, pre-seeds each `.crate` file into the Nix store under the path its `Cargo.lock` checksum dictates, and prints a Nix function describing every source, unit and binary. At build time `rostnix compile` and `rostnix run-build-script` are the builders of the derivations that function creates. A small Nix library (`nix/`) defines how each node kind is built and exposes `mkRustEnv { pkgs }` and `buildRustApplication`.

**Tech Stack:** Rust 2021 (`serde`, `serde_json`, `toml`, `sha2`), Nix 2.26 with flakes, nixpkgs `nixos-26.05` (cargo and rustc 1.95.0).

**Spec:** `docs/superpowers/specs/2026-10-09-rostnix-design.md`

This plan fixes files, interfaces and acceptance checks. It does not repeat the code: the session that wrote it executes it.

## Global Constraints

- Run cargo through the flake's toolchain: `nix develop --command cargo …`. The host's own cargo is older.
- Any Nix command that evaluates a fixture needs `--option allow-unsafe-native-code-during-evaluation true`. A flake's `nixConfig` cannot supply it.
- `.#` flake references see only git-tracked files. The integration driver uses `path:` and sees untracked files too.
- Never leave built binaries in the tree: no `target/` directory survives a task, always pass `--no-link` to `nix build`, delete any `result` symlink.
- Derivation name prefixes: `rustsrc-` (source tree), `rustbs-` (build-script compile), `rustbsrun-` (build-script run), `rustlib-`, `rustmacro-`, `rustbin-`.
- Flag rules follow cargo 1.95. Where this plan and `cargo build -vv` disagree, cargo is right and the conformance test decides.
- No `internal`-style hiding: every module is `pub` in `src/lib.rs`.
- Stage 1 rejects, with an explicit error from `resolve`: packages from git or another registry, path dependencies outside `src`, units planned for another target, and unit modes other than `build` and `run-custom-build`. `doCheck`, `checkFlags` and `skipTests` are accepted and ignored.
- Work happens on branch `stage-1`. Every commit message ends with:

  ```
  Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01V4TYW756A4F2xMvi8CQtiJ
  ```

## Review Focus

1. **A crate version containing `+`** (`lz4-sys 1.11.1+lz4-1.10.0`): store names, attribute keys and the download URL must all survive it. Pinned by `storepath` tests and the core-rs build.
2. **One package planned twice** (`libc` in core-rs, as a build dependency and as a normal one): the two units need distinct keys, `metadata` and derivations. Pinned by a `graph` test on the recorded core-rs graph.
3. **Strings that are Nix syntax** (a description containing `${`, a quote or a backslash in `CARGO_PKG_DESCRIPTION`): the generated Nix must evaluate back to the same string. Pinned by an `emit` round-trip test through `nix eval`.
4. **A build script that prints the old one-colon form, the new two-colon form and unrelated lines**: every directive is recognised, unknown one-colon keys become metadata, noise is ignored. Pinned by `buildscript` parser tests.
5. **A selection with no binary or example** (core-rs with defaults): a clear evaluation error naming `bins` and `examples`, not an empty `$out/bin`. Pinned in `tests/run.sh`.

## File Structure

```
flake.nix                      lib.mkRustEnv, packages.rostnix, legacyPackages.{rustEnv,fixtures}, dev shell
default.nix                    args: import ./nix/mk-rust-env.nix args
Cargo.toml  Cargo.lock         package rostnix: library and binary
src/main.rs                    subcommand dispatch: resolve, compile, run-build-script
src/lib.rs                     pub mod list
src/unitgraph.rs               serde types for cargo's unit graph
src/metadata.rs                serde types for cargo metadata
src/lockfile.rs                checksums from Cargo.lock
src/cargohome.rs               private cargo home, cargo's environment, running cargo
src/lto.rs                     cargo's per-unit LTO rules
src/lints.rs                   [lints] tables to flags
src/flags.rs                   rustc flags that do not depend on paths
src/localsrc.rs                source views of local packages
src/storepath.rs               fixed-output store paths, nix base32
src/seed.rs                    pre-seeding .crate files
src/graph.rs                   units + packages to the graph model
src/emit.rs                    graph to Nix
src/resolve.rs                 the evaluation-time pipeline
src/node.rs                    reading a derivation's node; unit.json types
src/compile.rs                 the compile subcommand
src/buildscript.rs             the run-build-script subcommand, directive parsing
examples/conformance.rs        compares unit.json records with a cargo build -vv log
nix/mk-rust-env.nix  nix/tool.nix  nix/builders.nix  nix/build-rust-application.nix
tests/fixtures/{hello,workspace,buildscript,profiles}/
tests/fixtures.nix             the fixtures, core-rs and rostnix itself as buildRustApplication calls
tests/run.sh                   integration driver
testdata/                      recorded cargo output for unit tests
README.md  LICENSE  .gitignore  .envrc
```

## Interfaces

### Derivation attributes (Nix to tool)

Every `compile` and `runBuildScript` derivation has `__structuredAttrs = true` and the attributes `rustc` (path to the binary), `cargo` (path to the binary) and `node`. The tool reads them and `outputs.out` from `$NIX_ATTRS_JSON_FILE`.

`node` for `compile`:

| Field | Meaning |
|---|---|
| `kind` | `lib`, `proc-macro`, `bin`, `example` or `build-script` |
| `pkg` | `{ name, version }` |
| `crateName`, `targetName`, `edition` | as cargo names them |
| `src`, `manifestDir`, `workDir`, `srcPath` | source store path; package directory and working directory relative to it; source file relative to the package directory |
| `local` | whether the package is local (relative source path, workspace root as cwd) |
| `remapTo` | what the source path is remapped to: `<name>-<version>` |
| `rustcArgs` | static flags from `--crate-type` through `-C strip`, in cargo's order |
| `tailArgs` | static flags cargo puts after the dependencies: `--extern proc_macro`, `--cap-lints allow` |
| `env` | static environment: `CARGO_PKG_*`, `CARGO_CRATE_NAME`, `CARGO_BIN_NAME`, `CARGO_PRIMARY_PACKAGE` |
| `deps` | `[ { name, path } ]`: extern name and the dependency unit's output |
| `buildScript` | output of the package's build-script run, or null |
| `passL` | whether this unit takes the script's `-l` flags |
| `overrideEnv` | `env` of the package's `crateOverrides` entry |

`node` for `runBuildScript`: `pkg`, `src`, `manifestDir`, `script` (output of the build-script compile), `features`, `debugAssertions`, `env` (static: `CARGO_PKG_*`, `OPT_LEVEL`, `DEBUG`, `PROFILE`, `TARGET`, `HOST`, `CARGO_MANIFEST_LINKS`), `linksDeps` (`[ { links, path } ]`), `overrideEnv`.

### `unit.json` (tool to tool)

Written by `compile`:

```json
{ "kind": "lib", "pkg": { "name": "…", "version": "…" }, "crateName": "…",
  "artifact": "<path for --extern, or the executable>",
  "transitive": ["<directories dependents pass as -L dependency=>"],
  "native": ["<-L values from build scripts in the closure>"],
  "argv": ["rustc", "…"], "env": { "…": "…" }, "cwd": "…" }
```

A library's `transitive` is its own `lib` directory plus every dependency's `transitive`. A proc macro's is its own directory only. Executables have none.

Written by `run-build-script`:

```json
{ "kind": "run-build-script", "pkg": { "name": "…", "version": "…" },
  "outDir": "<$out/out>",
  "libraryPaths": [], "libraryLinks": [], "linkArgs": [{ "target": "all", "arg": "…" }],
  "cfgs": [], "checkCfgs": [], "env": [["K", "V"]], "metadata": [["key", "value"]],
  "argv": ["<script>"], "envRecorded": { "…": "…" }, "cwd": "…" }
```

`linkArgs[].target` is `all`, `cdylib`, `bins`, `bin:<name>`, `tests`, `benches` or `examples`.

### Generated graph (tool to Nix)

As in the spec: `b: rec { cargoVersion; host; sources.<key>; packages.<key>; units.<key>; bins.<name>; roots; }`. `packages.<key>` is `{ name; version; local; manifestDir; workDir; links; env; override; }` where `override` is the matching `crateOverrides` key or null. A local unit's `src` is an inline `b.localSource { name; dir; exclude; }`. Linking units carry `overrides`, the keys of every overridden package in their closure.

## Tasks

### Task 1: Scaffold

**Files:** `flake.nix`, `default.nix`, `Cargo.toml`, `Cargo.lock`, `src/main.rs`, `src/lib.rs`, `nix/tool.nix`, `nix/mk-rust-env.nix`, `.gitignore`, `.envrc`, `LICENSE`

- [ ] Flake on `nixos-26.05` with a dev shell holding `cargo rustc rustfmt clippy`; `lib.mkRustEnv`; `packages.rostnix`; `legacyPackages.<system>.{rustEnv,fixtures}`.
- [ ] `nix/tool.nix`: `rustPlatform.buildRustPackage` with `cargoLock.lockFile` and a `lib.fileset` source of `Cargo.toml`, `Cargo.lock` and `src/` only, `doCheck = false`.
- [ ] `rostnix` with no arguments prints usage and exits 2.
- [ ] Verify: `nix build --no-link .#rostnix` succeeds; `nix develop --command cargo test` passes.

### Task 2: Store paths and the lockfile

**Files:** `src/storepath.rs`, `src/lockfile.rs`

**Produces:** `storepath::fixed_flat_sha256(store_dir: &str, name: &str, sha256_hex: &str) -> String`; `storepath::sanitize_name(&str) -> String`; `lockfile::Checksums::parse(&str) -> Result<Checksums>`; `Checksums::get(name, version, source) -> Option<&str>`.

- [ ] Test `fixed_flat_sha256("/nix/store", "anyhow-1.0.104.crate", "330a5ed07fa54e4702c9d6c4174f74427fc0ef6e214bbd677ae50a5099946470")` equals `/nix/store/7xybb3ddg063d2g44a5sc5rf6rdz77dc-anyhow-1.0.104.crate`.
- [ ] Test a name with `+` is kept and a name with `@` or a space is sanitised to `-`.
- [ ] Test `Checksums` on a lockfile with a registry package, a path package without checksum, and two versions of one crate.

### Task 3: Decoding cargo's output

**Files:** `src/unitgraph.rs`, `src/metadata.rs`, `testdata/core-rs/{unit-graph.json,metadata.json}`

**Produces:** `unitgraph::{UnitGraph, Unit, Target, Profile, UnitDep}`, `metadata::{Metadata, Package, MetaTarget}`, all `Deserialize`. `Profile::strip() -> Option<String>`, `Profile::debuginfo() -> Option<String>` render the two fields cargo encodes as JSON values.

- [ ] Record both files from core-rs at its pinned commit with cargo 1.95 and paths rewritten to `/src` and `/cargo-home`.
- [ ] Test: 103 units, one root, modes are only `build` and `run-custom-build`, `libc` has two `lib` units.

### Task 4: LTO, lints and flags

**Files:** `src/lto.rs`, `src/lints.rs`, `src/flags.rs`

**Produces:** `lto::generate(&UnitGraph) -> Vec<Lto>` indexed by unit; `lints::rustflags(manifest: &toml::Table, workspace: Option<&toml::Table>) -> Result<Vec<String>>`; `flags::base_args(unit, lto, declared_features, lint_flags, metadata, primary) -> Vec<String>`; `flags::tail_args(unit, local) -> Vec<String>`.

- [ ] `lto.rs` is a port of cargo's `lto.rs`: `generate` and `calculate` with the same merge table.
- [ ] Tests on hand-built graphs: thin LTO bin with an rlib dependency gives `Run(thin)` and `OnlyBitcode`; a `cdylib`+`rlib` dependency gives `ObjectAndBitcode` and passes it down; `lto = "off"` gives `Off`; `false` gives `OnlyObject`; proc macros and build scripts and their dependencies give `OnlyObject`.
- [ ] Lints: levels as strings and as tables, priority ordering then reverse name, tools other than `rust` prefixed, `cargo` lints dropped, `unexpected_cfgs.check-cfg` appended, `workspace = true` inherits.
- [ ] Flags test against the recorded cargo 1.95 lines for `probe` in the spec's probes: a build script, a proc macro, a library under thin LTO and the final binary.

### Task 5: Source views

**Files:** `src/localsrc.rs`

**Produces:** `localsrc::exclusions(pkg_dir: &str, other_pkg_dirs: &[String], targets: &[TargetInfo], unit_target: &TargetInfo) -> Vec<String>`, paths relative to `src`, sorted, with no entry under another entry.

- [ ] Tests: core-rs library (`examples`, `tests`, `benches`); core-rs example `amber-store` (`tests`, `benches`, the other two example files); a package with `src/main.rs` and `src/bin/tool/main.rs` seen from the library and from each binary; a nested package.

### Task 6: The graph and the emitter

**Files:** `src/graph.rs`, `src/emit.rs`

**Produces:** `graph::build(inputs: GraphInputs) -> Result<Graph>` where `Graph { cargo_version, host, sources, packages, units, bins, roots }`; `emit::to_nix(&Graph) -> String`; `emit::quote(&str) -> String`.

- [ ] `graph::build` joins units with packages on the opaque package id, classifies packages, rejects what stage 1 rejects, computes `metadata` bottom-up, unit keys, `overrides` closures, `passL`, static env and args.
- [ ] Test on the recorded core-rs graph: 103 units, distinct keys, `libc` twice, one `bins` entry `amber-store`, `zstd-safe`'s run lists `zstd-sys`'s run in `linksDeps`.
- [ ] Test `emit::quote` round-trips `${`, `"`, `\`, newline and `''` through `nix eval`.
- [ ] Golden test of `to_nix` for a three-unit graph.

### Task 7: Resolve

**Files:** `src/cargohome.rs`, `src/seed.rs`, `src/resolve.rs`, `src/main.rs`

**Produces:** `rostnix resolve '<json>'` printing the graph. Request fields: `cargo`, `rustc`, `src`, `storeDir`, `cargoRoot`, `packages`, `bins`, `examples`, `features`, `allFeatures`, `noDefaultFeatures`, `profile`, `overrideKeys`.

- [ ] Private cargo home with the three symlinks, clean environment as in the spec, temporary target directory, both removed on exit.
- [ ] Pre-seed concurrently; a path mismatch is an error naming the crate and the cache file; a failing `nix` is a warning.
- [ ] Verify by hand: `rostnix resolve` on a store copy of core-rs prints Nix that `nix-instantiate --parse` accepts.

### Task 8: Build-time subcommands

**Files:** `src/node.rs`, `src/compile.rs`, `src/buildscript.rs`

- [ ] `buildscript::parse_output(stdout, pkg_name) -> Result<ScriptOutput>` with tests for every directive in both forms, `cargo::error`, unknown one-colon keys as metadata, and noise.
- [ ] `run-build-script`: `CARGO_CFG_*` from `rustc --print=cfg` plus `feature` and the profile's `debug_assertions`, `DEP_*` from `linksDeps`, `OUT_DIR=$out/out`, cwd the package directory.
- [ ] `compile`: argv and environment as in the spec; renames the executable to the target name; writes `unit.json`.

### Task 9: The Nix library

**Files:** `nix/builders.nix`, `nix/build-rust-application.nix`, `nix/mk-rust-env.nix`, `tests/fixtures.nix`, `tests/fixtures/hello/`

- [ ] `fetchCrate`, `localSource`, `compile` (bare derivation unless it links or its package has an override), `runBuildScript`, the application.
- [ ] Checks in `build-rust-application.nix`: `builtins.exec` present; unknown override attributes; unmatched override keys warned; no binary or example.
- [ ] Verify: `fixtures.hello` builds and prints `{"greeting":"hello","n":42}`.

### Task 10: Patient zero

**Files:** `tests/fixtures.nix`

- [ ] `fixtures.core-rs` from `builtins.fetchTree` at `e6e900b7a0f41b3540a319c1367fd167921d5d8c` with `examples = [ "amber-store" ]`.
- [ ] Verify: it builds; `amber-store --help` exits 0; a store round trip with the binary works.

### Task 11: Conformance

**Files:** `examples/conformance.rs`, `nix/build-rust-application.nix` (`passthru.unitRecords`)

- [ ] The example reads a `cargo build -vv` log and a directory of `unit.json` files, normalises both as the spec lists (and maps cargo's `--cap-lints warn`, which `-vv` causes, to `allow`), and prints every invocation present on one side only. Exit 1 on any difference.
- [ ] Verify: no difference for `hello` and for core-rs. Fix `flags.rs`, `compile.rs` and `buildscript.rs` until that holds.

### Task 12: The remaining fixtures and the driver

**Files:** `tests/fixtures/{workspace,buildscript,profiles}/`, `tests/fixtures.nix`, `tests/run.sh`

- [ ] Fixtures as in the spec's table; `fixtures.self` builds rostnix with itself.
- [ ] `tests/run.sh`: run checks, conformance per fixture, rebuild granularity (`check_incremental` over `units`), the missing-`exec` error, the no-executable error, override typo and unmatched-key checks, the fetch fallback with `--rebuild`, and that the application refers to no `rust*-` intermediate.
- [ ] Verify: `tests/run.sh` ends with `all integration checks passed`.

### Task 13: README

**Files:** `README.md`

- [ ] Use, `mkRustEnv`, `buildRustApplication`, `crateOverrides`, what evaluation needs, what is not supported yet, development.
