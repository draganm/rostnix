# rostnix Stage 2 (Tests) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** With `doCheck`, which is on by default, `buildRustApplication` builds and runs every test executable `cargo test` would build, each in its own derivation. The application depends on every one, so a failing test fails the build. core-rs's test suite passes apart from `amber_bench_smoke` and the one test Nix rules out.

**Architecture:** `rostnix resolve` asks cargo for a second plan, `cargo test --no-run --unit-graph`, and merges it with the build plan: a unit both plans describe alike gets the same key and is one derivation. A unit of mode `test` is built by a new builder, `rostnix test`, which makes a writable copy of the unit's source with a cargo-style `target/` directory in it, compiles the test into that, and runs it there.

**Tech Stack:** As stage 1. `std::io::pipe` (Rust 1.87) for the test log.

**Spec:** `docs/superpowers/specs/2026-10-09-rostnix-design.md`, section "Tests". Task 8 writes back into it what this stage learned.

This plan fixes files, interfaces and acceptance checks. It does not repeat the code: the session that wrote it executes it.

## Revision during execution

The plan first had two derivations per test, one that compiles the executable and one that runs it, as the spec did. Built that way, 19 of core-rs's 21 test executables passed. `golden_packstore` did not: it copies golden files out of `env!("CARGO_MANIFEST_DIR")` and writes to the copies, and a test compiled in its own derivation has a store path there, whose files are read-only and whose copies are too. No path can be compiled into a test in one derivation and be writable in another on macOS, where a build directory's name is not known in advance. So a test is compiled where it runs. The cost: a test is compiled again whenever it is run again, which beyond a change to its own inputs happens when a binary or an example it finds beside itself changes, or `checkFlags` does.

The other failure, `golden_tar_extracts`, is Nix's own rule: no build may create a setuid file. The core-rs fixture skips it by name.

## Global Constraints

- Everything in stage 1's plan still holds: the flake's toolchain, the `exec` option, `path:` references, no built binaries in the tree, every module `pub`.
- Flag and environment rules follow cargo 1.95. Where this plan and `cargo test -vv` disagree, cargo is right and the conformance test decides.
- New derivation name prefix: `rusttest-` (a test, compiled and run).
- Doc tests are not run. Units of mode `doctest` are left out of the graph, not refused.
- Work happens on branch `stage-2`. Commit messages end with the two attribution lines of stage 1's plan.

## Facts verified (cargo 1.95, aarch64-darwin)

| Fact | Evidence |
|---|---|
| `cargo test --no-run --unit-graph -Z unstable-options` prints a version 1 graph. | Run on the `hello` fixture: modes `build`, `run-custom-build`, `test` and `doctest`. |
| Its roots are the library in mode `test`, each binary in mode `test`, each integration test, each example in mode `build`, and a `doctest` unit. An integration test depends on its package's binaries, built in mode `build`. | Same graph. |
| A test unit is compiled without `--crate-type` and with `--test` between the profile's flags and the features. With `harness = false` it gets `--cfg test` instead. A proc macro's unit tests keep `-C prefer-dynamic` and `--extern proc_macro`. | `cargo test -vv` on the fixtures. |
| Integration tests are compiled with `CARGO_BIN_EXE_<name>` for each binary of their package and with `CARGO_TARGET_TMPDIR`. Binaries get no `--extern`. | Same log. |
| A test runs in its package directory, from `target/<profile>/deps/<crate>-<hash>`, with `CARGO`, `CARGO_MANIFEST_DIR`, `CARGO_MANIFEST_PATH`, `CARGO_PKG_*`, the library path variable, and for an integration test `CARGO_BIN_EXE_<name>` again. A package with a build script also gets `OUT_DIR` and what the script set with `rustc-env`. It does not get `CARGO_CRATE_NAME`, `CARGO_PRIMARY_PACKAGE` or `CARGO_TARGET_TMPDIR`. | A test that prints its environment, and the `Running` lines of `cargo test -vv`, which include the environment. |
| The rustc information cargo caches in a target directory outlives `RUSTC_BOOTSTRAP`. A reference build must not share a target directory with a `--unit-graph` run. | Build scripts were given unstable `CARGO_CFG_*` values in a target directory a `--unit-graph` run had used. |
| core-rs's tests find golden files through `env!("CARGO_MANIFEST_DIR")`, `golden_packstore` writes to copies of them, and `cli_e2e` finds `examples/amber-store` beside the directory of its own executable. | Read in the pinned source, and the failure described above. |
| A Nix build cannot create a setuid file. | `chmod 4755` in a bare derivation: `Operation not permitted`. |

## Review Focus

1. **A unit both plans contain** (the library of `hello`): one key, one derivation, and everything a node holds must follow from what is hashed. `CARGO_PRIMARY_PACKAGE` depends on a plan's roots, so it joins the hash. Pinned by a `graph` test and by `tests/run.sh` comparing `buildUnits` with `testUnits`.
2. **A dependency that gains a feature from a dev-dependency** (`ws-core` in the workspace fixture): the test plan's unit is another derivation, and the installed binary is not the one the tests ran. Pinned in `tests/run.sh`: the installed `ws-app` prints lower case, the one its integration test runs prints upper case.
3. **A profile with `panic = "abort"`**: cargo builds tests and everything under them with unwinding. Pinned by a `#[should_panic]` test in the `profiles` fixture under the `release` profile.
4. **A test that writes**: into its working directory, into `CARGO_TARGET_TMPDIR`, and to a copy of a fixture it found through `env!("CARGO_MANIFEST_DIR")`. Pinned by tests in `hello`.
5. **A `skipTests` entry that matches nothing**: a warning that names it, as for an unmatched `crateOverrides` key. Pinned in `tests/run.sh`.

## Interfaces

### The resolve request

One more field: `doCheck` (boolean). When true, `resolve` also runs
`cargo test --no-run --unit-graph -Z unstable-options --locked --profile <profile>`
with the request's `packages` and features. `bins` and `examples` narrow what is installed, not what is tested, so they are not passed.

### The generated graph

Added to what stage 1 emits:

```nix
  units."hello-0.1.0-test-cli-1a2b3c4d" = b.test {
    name = "rusttest-cli";
    kind = "test";          # what is built
    targetKind = "test";    # lib, bin, example, test, bench, proc-macro or custom-build; on every unit
    # the rest as for b.compile, and:
    profileDir = "release";                     # debug for dev and test, else the profile's name
    executables = [ units."…-bin-hello-…" units."…-example-extra-…" ];
  };
  tests."hello-0.1.0-test-cli-1a2b3c4d" = units."hello-0.1.0-test-cli-1a2b3c4d";
  testBuilds = [ units."…-example-extra-…" ];   # what cargo test builds without running
  buildUnits = [ "…" ];   # keys of the units cargo build plans
  testUnits = [ "…" ];    # keys of the units cargo test plans
```

`tests = { };` when `doCheck` is off. A test's `deps` hold what it links; the binaries cargo makes it depend on are among its `executables`.

`executables` are the package's binaries and examples for a target of kind `test` or `bench`, and nothing for unit tests.

### Derivation attributes of a test

`rustc`, `cargo` and `node`: everything a `compile` node has, and `profileDir`, `executables` (store paths) and `args` (`checkFlags`).

Output: `$out/log`, what the test printed; `$out/unit.json`, the record of the compilation; `$out/run.json`, the record of the run, with `kind = "test-run"`, `pkg`, `crateName`, `argv`, `env`, `overrideEnv` and `cwd`.

### What a test derivation does

1. Copies `src` to `$NIX_BUILD_TOP/source` and makes the copy writable.
2. Creates `source/<workDir>/target/` with `tmp/` and `<profileDir>/deps/`. Copies each binary among `executables` to `<profileDir>/` and each example to `<profileDir>/examples/`.
3. Runs rustc in the copy, as `compile` would but into `<profileDir>/deps/` and without path remapping. A target of kind `test` or `bench` is given `CARGO_BIN_EXE_<name>` and `CARGO_TARGET_TMPDIR`.
4. Runs the executable in `source/<manifestDir>` with `args`, the environment the facts table lists, and `RUST_TEST_THREADS` from `NIX_BUILD_CORES` unless something set it.
5. Writes everything the test prints both to the build log and to `$out/log`.

### Compile

- `rustc-link-arg-tests`, `-benches`, `-bins` and `-examples` go by the target's kind, whatever the mode.
- A test unit's view of its package keeps `examples/`, `tests/` and `benches/`, less the other targets' roots.
- For every unit, a root of another target that the unit's own root names as a module (`mod common;`, `#[path = "…"]`) stays in its view.

### `buildRustApplication`

- `doCheck` (default `true`), `checkFlags`, `skipTests` take effect.
- The application lists every test that is not skipped among its inputs, and `testBuilds`.
- `passthru.tests`: the tests that are run, by key. `passthru.testUnitRecords`: the records of `testUnits` and of the runs, without the skipped tests. `passthru.unitRecords` holds `buildUnits` only.
- A `skipTests` entry that names no test target is warned about.

## Tasks

### Task 1: Planning tests

**Files:** `src/resolve.rs`, `src/graph.rs`, `src/flags.rs`, `src/localsrc.rs`, `src/lto.rs`, `src/emit.rs`, `testdata/hello/`

- [x] Record `cargo test --no-run --unit-graph`, `cargo build --unit-graph` and `cargo metadata` for the `hello` fixture into `testdata/hello/`.
- [x] `Request.do_check` and `test_graph_args()`.
- [x] `graph::build` takes the test plan beside the build plan: kinds `Test` and skipped (`doctest`, and a library example in the test plan); `primary` in the unit hash; `target_kind`; a test's `profile_dir` and `executables`; `Graph.tests`, `build_units`, `test_units`.
- [x] `flags::base_args`: `--test` or `--cfg test`, no `--crate-type`. `harness` read from the manifest.
- [x] `localsrc::exclusions`: a test unit keeps `tests/`.
- [x] Emit `targetKind`, `b.test`, `tests`, `buildUnits`, `testUnits`.
- [x] Tests: the shared library unit has one key in both plans; the test units, their views and what they find beside them; `--test` flags against the recorded `cargo test -vv` lines; a `doctest` root is left out; the emitter's output.

### Task 2: Compiling tests

**Files:** `src/node.rs`, `src/compile.rs`

- [x] `CompileNode.target_kind`; link arguments by target kind; `plan_at` for a source tree and an output directory of the caller's choosing, with or without remapping.
- [x] Tests for each.

### Task 3: The test builder

**Files:** `src/testrun.rs`, `src/node.rs`, `src/main.rs`, `src/lib.rs`

- [x] `rostnix test` as described above, with `layout()`, `compile_plan()` and `run_env()` separate from the side effects so that they are unit-tested.

### Task 4: The Nix library

**Files:** `nix/builders.nix`, `nix/build-rust-application.nix`

- [x] `test`; `doCheck`, `checkFlags`, `skipTests`; the application's dependency on the tests; `passthru.tests`, `testUnitRecords`; `unitRecords` narrowed to `buildUnits`.

### Task 5: Fixtures

**Files:** `tests/fixtures/`, `tests/fixtures.nix`

- [x] `hello`: an integration test that runs the binary through `CARGO_BIN_EXE_hello`, finds the example beside its own executable, reads a data file through the working directory and through `env!("CARGO_MANIFEST_DIR")`, writes to the working directory, to `CARGO_TARGET_TMPDIR` and to a copy of a fixture, and checks the environment the facts table lists; a unit test in the library; an ignored test that fails.
- [x] `workspace`: unit tests in a library and in the proc macro, a `harness = false` test, a dev-dependency that turns a feature on, an integration test that runs both binaries.
- [x] `buildscript`: a unit test that calls the C function and reads `OUT_DIR` and the script's `rustc-env` value at run time.
- [x] `profiles`: a `#[should_panic]` test.
- [x] `core-rs`: `skipTests = [ "amber_bench_smoke" ]` and `checkFlags = [ "--skip" "golden_tar_extracts" ]`. `self`: `testdata` joins the source.

### Task 6: Conformance for tests

**Files:** `examples/conformance.rs`

- [x] Test runs are a third kind of invocation, named by the executable without its hash. `rustdoc` lines of the log are left out. `CARGO_BIN_EXE_*` is a variable although its name may hold a hyphen, and its values compare by file name. `--compile-only` leaves runs out on both sides; `--without <crate>` leaves a skipped test out.

### Task 7: The driver

**Files:** `tests/run.sh`

- [x] `check_test_conformance` against `cargo test -vv` (core-rs: `--no-run`, `--compile-only`, `--without amber_bench_smoke`).
- [x] A failing test fails the build (`checkFlags = [ "--include-ignored" ]`); `skipTests` leaves a test out and warns about an entry that matches nothing; `doCheck = false` plans no tests.
- [x] The library of `hello` is one derivation in both plans; the workspace's two `ws-app` binaries differ.
- [x] `check_incremental` covers tests; expectations updated: editing a test builds and runs that test again and nothing else.
- [x] The result refers to no test.
- [x] Verify: `tests/run.sh` ends with `all integration checks passed`, with core-rs's tests among what it ran.

### Task 8: What the fixtures did not meet

**Files:** `src/localsrc.rs`, `src/graph.rs`, `src/testrun.rs`, `src/resolve.rs`, `nix/build-rust-application.nix`, `examples/conformance.rs`, `tests/run.sh`, the `workspace` fixture

Found by thinking through real projects and by an independent review of the branch.

- [x] `tests/common.rs` beside tests that say `mod common;`: `localsrc::declared_modules` searches a unit's root for `mod name;` and `path = "…"`, and such a file stays in the view. Pinned by the `workspace` fixture and by `check_incremental`.
- [x] A test that leaves a process running, which holds the test's output open: `testrun::tee` stops when the test exits. Pinned by a unit test.
- [x] A test that looks through `examples/` or `benches/`: everything built as a test keeps all three directories.
- [x] An example that does not compile: the application depends on `testBuilds`.
- [x] A target rooted outside its package through `..`: the root is normalised before it is checked. Something only the tests need that is refused: the error says so and names `doCheck = false`.
- [x] Cargo's order of the library search path for a test: what build scripts built comes first.
- [x] The conformance example compares multisets and reads `CARGO_BIN_EXE_<name>` with a hyphen in the name; `check_tests_ran` names the kind of test and asks for a count that an empty test executable cannot meet.

### Task 9: Documents

**Files:** the spec, `README.md`

- [x] The spec's "Tests" section, API table, facts, fixture table and layout match what was built, with what follows from the design: what a test's dependencies have compiled in, what the copy holds, how coarse `skipTests` and `checkFlags` are, and that a test is an input of the application. README: tests, `checkFlags`, `skipTests`, what is not run, the limits.
