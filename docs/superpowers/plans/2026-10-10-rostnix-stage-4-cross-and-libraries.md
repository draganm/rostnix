# rostnix Stage 4 (Cross-Compilation, Library Outputs, nixpkgs Overrides) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** A cross package set builds a Rust project for another platform. A project can be evaluated on a machine that is not the one that builds it. `cdylib` and `staticlib` targets are installed. The crate overrides that nixpkgs keeps for `buildRustCrate` can be used as they are.

**Architecture:** `resolve` plans with `--target` when the platform to build for is not the one cargo runs on, and the graph says of every unit whether it is for the target or for the machine that builds. The Nix side gives each kind its own stdenv and gives target units the cross linker. A library unit with a `cdylib` or `staticlib` crate type leaves its files under the names cargo would give them, and the application installs those of the selection's roots. An adapter turns nixpkgs' `defaultCrateOverrides` into `crateOverrides` entries.

**Tech Stack:** As before.

**Spec:** `docs/superpowers/specs/2026-10-09-rostnix-design.md`, section "Later stages". Task 7 moves what this stage builds into the body of the spec, which then has no later stage.

This plan fixes files, interfaces and acceptance checks. It does not repeat the code: the session that wrote it executes it.

## Global Constraints

- Everything in the earlier plans still holds.
- A native build is planned and built exactly as before: no `--target`, no linker flag, one stdenv.
- Flag and environment rules follow cargo 1.95 with `--target`. Where this plan and `cargo -vv` disagree, cargo is right and the conformance test decides.
- Work happens on branch `stage-4`. Commit messages end with the two attribution lines of stage 1's plan.

## Facts verified (cargo 1.95, Nix 2.26.1, nixpkgs 26.05, aarch64-darwin)

| Fact | Evidence |
|---|---|
| With `--target`, the unit graph gives every unit a `platform`: the triple for what is built for the target, `null` for build scripts, proc macros and what they depend on. A build script is compiled for the host and its run is for the target. | `cargo build --unit-graph --target wasm32-wasip1` on the `hello` fixture: 14 units with the triple, 14 with `null`. |
| A target unit gets `--target <triple>` and, when cargo is told a linker for the target, `-C linker=<path>`, libraries included. A host unit gets neither. Target artifacts go to `target/<triple>/<profile>/`. | `cargo build -vv --target wasm32-wasip1`. |
| A build-script run for the target has `TARGET=<triple>`, `HOST=<the machine's>`, `RUSTC_LINKER=<the linker>` and the target's `CARGO_CFG_*`. | Same log. |
| With `--target`, the configuration's `rustflags` go to target units only. | Cargo's `env_args`, read; the conformance test checks it. |
| An executable for `wasm32-wasip1` is `<name>.wasm`, a `cdylib` `<name>.wasm` without `lib`, a `staticlib` `lib<name>.a`. | `rustc --print=file-names`. |
| nixpkgs' cross rustc and C toolchain for `pkgsCross.wasi32` are in the binary cache for aarch64-darwin. Those for `pkgsCross.x86_64-darwin` and `pkgsCross.aarch64-multiplatform` are not: rustc would be built from source. | `nix build --dry-run`. |
| rustc links WebAssembly with `wasm-ld` and calls it as an lld. Given the C compiler driver that nixpkgs' own Rust support names as the linker, the link fails (`clang: error: unknown argument: '-flavor'`), and so it does with the wrapped `ld`. It works with the unwrapped `wasm-ld` of the toolchain's binutils, and the result runs under wasmtime. | Tried each. |
| This machine also builds for x86_64-darwin (`extra-platforms`, Rosetta), and that platform's native rustc, cargo and C compiler are in the binary cache. | `nix config show`, `nix build --dry-run`. |
| `stdenv.buildPlatform.canExecute stdenv.hostPlatform` is false for wasm and for aarch64-darwin to x86_64-darwin. | `nix eval`. |

## Review Focus

1. **A native build must not change.** No `--target`, no `-C linker`, the same stdenv for every unit. Pinned by every earlier fixture's conformance check.
2. **A package built for both sides** (a dependency of the program and of a build script): two units, one with `--target` and one without, each with its own stdenv. Pinned by a `graph` test that puts a second unit beside a recorded one, and end to end by the `libs` fixture built for wasm: its `rostnix-tables` is used by a build script and by the library, says which machine each build was for, and passes its `links` metadata on.
3. **A build script that compiles C for the target**: it runs on the build machine with the cross C compiler as `CC` and the build machine's as `HOST_CC`. Pinned by the `libs` fixture built for wasm.
4. **Executable names that carry an extension** (`hello.wasm`): the unit's artifact, the installed file and `meta.mainProgram`. Pinned by the wasm fixture.
5. **An installed dynamic library must be usable**: its file name is cargo's, without the hash of the unit, and on macOS its install name is where it is installed. Pinned by a C program that links against it.

## Interfaces

### `mkRustEnv`

| Argument | Default | Meaning |
|---|---|---|
| `evalPkgs` | `null` | The package set of the machine that evaluates, when it is not the one that builds: the tool, cargo and rustc that plan are taken from it. |
| `linker` | by platform | The linker of units built for another platform than the build machine's: the C compiler of `pkgs.stdenv`, or its `wasm-ld` when the platform is WebAssembly. |

Returns in addition `nixpkgsCrateOverrides`, which is `pkgs.defaultCrateOverrides` as `crateOverrides` entries, and `fromNixpkgsCrateOverrides`, the function that makes it.

### The resolve request

Two more fields: `target`, the rustc triple of `pkgs.stdenv.hostPlatform`, and `host`, that of `pkgs.stdenv.buildPlatform`. `resolve` plans with `--target <target>` unless `target`, `host` and the triple of the rustc it runs are all the same. `cargo metadata` is filtered for the target and for the machine cargo runs on.

### The generated graph

```nix
  host = "aarch64-apple-darwin";   # the machine that builds
  target = "wasm32-wasip1";        # null when cargo was not given --target
  units."…" = b.compile { /* … */ target = "wasm32-wasip1"; };   # null for a unit of the build machine
  libs."rostnix_fixture_lib" = units."…-lib-…";                  # roots with a cdylib or staticlib crate type
```

### What each unit gets

- **A unit with a `target`**: `--target`, `pkgs.stdenv`, and `-C linker=<linker>` when `pkgs` is a cross package set. Its build-script run also gets `RUSTC_LINKER`, the build machine's C compiler as `HOST_CC` and `HOST_CXX`, and `PKG_CONFIG_ALLOW_CROSS=1`.
- **A unit without**, in a graph that has a target: `pkgs.buildPackages.stdenv`, none of the configuration's `rustflags`, and the libraries of its overrides for the build machine.
- A test's directory in cargo's layout is `target/<triple>/<profile>/`.
- A library unit with a `cdylib` or `staticlib` crate type has `$out/install/`, which names those files as cargo's target directory does: without the unit's hash.

### `buildRustApplication`

- `doCheck` defaults to whether the build machine can run the host platform's programs.
- Installs each unit of `libs` into `$out/lib`. On macOS a dynamic library's install name becomes its installed path. A selection is refused only when it builds neither an executable nor such a library.
- Executables keep the extension their platform gives them.
- A `crateOverrides` entry may say `optional = true`: no warning when it names no package.

## Tasks

### Task 1: Planning for a target

**Files:** `src/resolve.rs`, `src/cargohome.rs`, `src/graph.rs`, `src/emit.rs`, `src/config.rs`, `testdata/hello-wasi/`

- [x] `Request.target` and `host`; `--target` in both plans; two `--filter-platform`s; the configuration's flags settled for the target.
- [x] `graph`: a unit's platform, in the hash and in the node; one target per graph; `HOST` and `TARGET` of build-script runs; a test's profile directory; `libs`.
- [x] Tests against recorded cargo output for `hello` planned for wasm.

### Task 2: Building for a target

**Files:** `src/node.rs`, `src/compile.rs`, `src/buildscript.rs`, `src/testrun.rs`, `nix/builders.nix`, `nix/mk-rust-env.nix`, `nix/build-rust-application.nix`

- [x] `--target` and `-C linker`; artifacts with an extension; `rustc --print=cfg --target`; `RUSTC_LINKER`.
- [x] Two stdenvs, the linker by platform, `evalPkgs`, `doCheck`'s default, executables installed whatever their name ends in.

### Task 3: Library outputs

**Files:** `src/compile.rs`, `src/graph.rs`, `src/emit.rs`, `nix/build-rust-application.nix`, `tests/fixtures/libs/`

- [x] `$out/install/` for `cdylib` and `staticlib`; `libs`; installation, with the install name on macOS.

### Task 4: nixpkgs overrides

**Files:** `nix/nixpkgs-overrides.nix`, `nix/mk-rust-env.nix`, `nix/build-rust-application.nix`

- [x] `fromNixpkgsCrateOverrides`: `buildInputs`, `nativeBuildInputs` and the variables an entry sets, each entry read only when a package of the build takes it; `optional`.

### Task 5: Fixtures and the driver

**Files:** `flake.nix`, `tests/fixtures.nix`, `tests/fixtures/`, `tests/run.sh`, `examples/conformance.rs`

- [x] `hello` and `libs`, whose build script compiles C, built for wasm, run under wasmtime, and compared with `cargo --target`.
- [x] `hello` built for the other platform this machine builds for, planned here: its tests run, and both plans conform.
- [x] `libs`: a `cdylib` and `staticlib` that a C program links against, natively; the same crate for wasm.
- [x] `buildscript` with nixpkgs' own override for `libz-sys`.
- [x] Verify: `tests/run.sh` ends with `all integration checks passed`.

### Task 6: Review

- [x] An independent review of the branch; its findings fixed, each with a test.

What the review found, and what became of it:

- The nixpkgs adapter dropped variables an entry writes in an `env` set. Fixed; the driver's adapter check has such an entry.
- A package set for another platform of the build machine's own triple (`pkgsStatic` on macOS) was planned without a target, so nothing told its units apart and none got a linker. `resolve` is now told whether the platforms differ. Pinned by a unit test and by `check_cross_same_triple`, which evaluates such a build.
- A test of the target was told its package's binaries under `CARGO_BIN_EXE_<file name>`, ending included. It is the target's name now, which the compile record carries. Pinned by a unit test.
- A test of the target was given the build machine's standard library directory. Fixed; the tests of `hello-elsewhere` run with the target named.
- The install step called `nm`, `dsymutil` and `install_name_tool` by their plain names, which a build for macOS from another machine does not have. It takes the platform's now. No such build can be made here, so only the native path is tested.
- Documented rather than changed: an override's `env` goes to the units of both machines as written, and for WebAssembly the libraries of `buildInputs` are not on the linker's path.
- Tests made to say what they claim: a `dev` build's installed libraries are listed, the build script of `libz-sys` is checked to run with what nixpkgs names, and the `libs` fixture has a package that is built for both machines.

### Task 7: Documents

**Files:** the spec, `README.md`

- [x] The spec describes cross-compilation, library outputs and the adapter where it describes the rest, and has no later stage. README: the same, and what is still not supported.
