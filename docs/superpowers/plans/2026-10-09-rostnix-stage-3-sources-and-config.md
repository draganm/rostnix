# rostnix Stage 3 (Other Sources and Build Configuration) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A project builds when it depends on crates from git repositories and from registries other than crates.io, and when its `.cargo/config.toml` sets `rustflags` or `[env]`. Each of those is refused or ignored today.

**Architecture:** Three additions to what stages 1 and 2 built. A git dependency becomes a source fetched by `builtins.fetchGit` at the revision in `Cargo.lock`. The private cargo home that `resolve` plans with is given the caller's registry configuration and credentials, so cargo can reach other registries, and their crates are added to the store as crates.io's are. `resolve` asks cargo for its merged configuration and puts `rustflags` and `[env]` into the graph, from where every unit gets them.

**Tech Stack:** As before, plus the `cargo-platform` crate, cargo's own parser for `cfg(…)` keys.

**Spec:** `docs/superpowers/specs/2026-10-09-rostnix-design.md`, section "Later stages". Task 7 moves what this stage builds into the body of the spec.

This plan fixes files, interfaces and acceptance checks. It does not repeat the code: the session that wrote it executes it.

## Global Constraints

- Everything in the earlier plans still holds: the flake's toolchain, the `exec` option, `path:` references, no built binaries in the tree, every module `pub`.
- Flag and environment rules follow cargo 1.95. Where this plan and `cargo -vv` disagree, cargo is right and the conformance test decides.
- Only the project's own `.cargo/config.toml` files say how things are built. From the caller's cargo home come the tables that say where crates come from, and nothing else.
- Work happens on branch `stage-3`. Commit messages end with the two attribution lines of stage 1's plan.

## Facts verified (cargo 1.95, Nix 2.26.1, aarch64-darwin)

| Fact | Evidence |
|---|---|
| A git package's `source` in `Cargo.lock` and in `cargo metadata` is `git+<url>?<rev\|tag\|branch>=<x>#<full revision>`. Its manifest lies under `<cargo home>/git/checkouts/<repo>-<hash>/<short revision>/`, in a subdirectory when the repository is a workspace. Every package of one repository has the same `source`. | A project depending on `serde` (a workspace, by tag) and `itoa` (by revision) from GitHub. |
| Cargo accepts a cargo home whose `git` is a link into another one. | Same probe. |
| Cargo's built-in git cannot authenticate where the git command can. | On a machine that rewrites `https://github.com/` to SSH, the fetch failed with "no authentication methods succeeded" and worked with `CARGO_NET_GIT_FETCH_WITH_CLI=true`. |
| `builtins.fetchGit { url; rev; submodules = true; shallow = true; }` needs no `ref`, is allowed in pure evaluation, and gives the tree cargo checked out. | `diff -r` of the two for `serde`, the `.git` directory aside. |
| A sparse registry's cache holds its `config.json`, with the download template, at `<cargo home>/registry/index/<directory>/config.json`; a crate's file is at `registry/cache/<directory>/`, beside where it is unpacked. | Looked at for crates.io. |
| `cargo -Z unstable-options config get --format json` prints the merged configuration on stable cargo when `RUSTC_BOOTSTRAP=1`; `--show-origin` names the file each value comes from. | Run on a project with `[build]`, `[target.*]` and `[env]`. |
| When any `[target.<triple>]` or matching `[target.'cfg(…)']` table has `rustflags`, `build.rustflags` is not used. The triple's flags come first, then those of the matching `cfg` tables in the order of their keys. A string is split at whitespace. | `cargo test -vv`. |
| The flags are the last arguments cargo itself gives rustc, after `--cap-lints`, and they go to every unit: registry crates, build scripts and proc macros included. A build script is told them in `CARGO_ENCODED_RUSTFLAGS`, separated by `0x1f`, and its `CARGO_CFG_*` come from `rustc --print=cfg` run with them. | Same log. |
| Every rustc invocation, build-script run and test run is given the `[env]` values, registry crates included. A value with `relative = true` is the path from the directory that holds the `.cargo` directory. A value is not set when the variable is already in cargo's environment, unless it says `force = true`. | Same log: `TERM` forced, `HOME` left alone. |

## Review Focus

1. **A package in a subdirectory of a git repository** (`serde_core` in `serde`): its unit runs in that subdirectory of the fetched tree, and a path dependency inside the repository is the same source. Pinned by a `graph` test and the `gitdeps` fixture.
2. **`[target.'cfg(…)']` beside `[build]`**: the precedence above. Pinned by a unit test and by conformance on the `config` fixture.
3. **`CARGO_WORKSPACE_DIR = { value = "", relative = true }`**: a relative value that names a directory holding local packages must not make every registry crate depend on the whole source. Pinned by a `graph` test and by `check_incremental` on the `config` fixture.
4. **A registry that needs a token**: the fallback download cannot authenticate, so the derivation must say what to do instead of failing with a bare 403. Pinned by a unit test and by the `registry` check in `tests/run.sh`.
5. **A caller's `config.toml` that sets `[build]` or `[env]`**: it must not reach the build. Pinned by a `cargohome` test.

## Interfaces

### The private cargo home

Links to the caller's `registry`, `git`, `.package-cache`, `.package-cache-mutate`, `credentials.toml` and `credentials`. A `config.toml` with the tables `registries`, `registry`, `source`, `net`, `http` and `credential-alias` of the caller's `config.toml`, where it has them.

Carried over from the caller's environment, beside what stage 1 lists: `CARGO_REGISTRIES_*`, `CARGO_REGISTRY_*`, `SSH_AUTH_SOCK`, `GIT_SSH`, `GIT_SSH_COMMAND`, `GIT_ASKPASS` and `SSH_ASKPASS`. `CARGO_NET_GIT_FETCH_WITH_CLI=true` is set unless the caller's environment or `[net]` table says otherwise: Nix fetches the same repositories with the git command, and both should succeed or fail together.

### The generated graph

```nix
  rustflags = [ "--cfg" "from_config" ];
  configEnv = [
    { name = "PLAIN"; value = "plain"; force = false; relative = null; holdsSource = false; }
    { name = "DATA"; value = ""; force = false; relative = "data/message.txt"; holdsSource = false; }
  ];
  sources."git-serde-a866b336f14a" = b.fetchGit {
    name = "rustsrc-serde-a866b33";
    url = "https://github.com/serde-rs/serde";
    rev = "a866b336f14aa57a07f0d0be9f8762746e64ecb4";
  };
  sources."dep-0.1.0" = b.fetchCrate { pname = "dep"; version = "0.1.0"; sha256 = "…"; url = null; registry = "sparse+https://…"; };
  packages."serde_core-1.0.228" = { /* … */ manifestDir = "serde_core"; local = false; };
```

`relative` is a path from the source root, `""` for the root itself. `holdsSource` says that a local package lies at or under it. `url = null` stands for a crate whose registry gives no download address that works without credentials.

### What each unit gets

- **`rustflags`**: every compile, after its own arguments and before what its build script printed. Every build-script run: `CARGO_ENCODED_RUSTFLAGS`, and `rustc --print=cfg` is run with them.
- **`configEnv`**: every compile, build-script run and test, laid under what cargo sets and over nothing that is already in the builder's environment unless `force`.
  - A plain value goes to every unit as it is.
  - A relative value, for a unit of a local package: the path in the unit's own view, which for a test is its writable copy. The path is added to the view unless it holds local packages.
  - A relative value, for any other unit: a store copy that holds that path alone. Withheld when the path holds local packages.
- A unit's record names the relative values it was given, by path from the source root, and the names withheld from it.

### Conformance

`conformance --root <source root>`: the value of a relative variable is compared as a path from the source root, and a withheld variable is taken out of cargo's invocations for packages outside the root.

## Tasks

### Task 1: Git dependencies

**Files:** `src/cargohome.rs`, `src/graph.rs`, `src/emit.rs`, `src/lints.rs`, `nix/builders.nix`, `tests/fixtures/gitdeps/`, `testdata/gitdeps/`

- [ ] The cargo home links `git`, carries the SSH and git variables and defaults to the git command.
- [ ] `graph`: a git source per repository and revision; a package's directory inside it; lints inherited from the repository's own workspace root.
- [ ] `b.fetchGit`.
- [ ] Tests against recorded cargo output for the fixture; the fixture builds and runs, and conforms.

### Task 2: `rustflags`

**Files:** `src/config.rs`, `src/resolve.rs`, `src/graph.rs`, `src/emit.rs`, `src/compile.rs`, `src/buildscript.rs`, `src/node.rs`, `nix/builders.nix`

- [ ] `config::rustflags(config, host, cfgs)` with cargo's precedence; `resolve` reads the configuration and `rustc --print=cfg`.
- [ ] Compiles append them; build-script runs encode them and print cfgs with them.

### Task 3: `[env]`

**Files:** `src/config.rs`, `src/resolve.rs`, `src/graph.rs`, `src/emit.rs`, `src/compile.rs`, `src/buildscript.rs`, `src/testrun.rs`, `src/node.rs`, `nix/builders.nix`

- [ ] `config::env(config, origins, src, local package directories)`: plain and relative entries, `force`, the base of a relative value from the file that sets it, `holdsSource`.
- [ ] The three builders apply them as described above.

### Task 4: Other registries

**Files:** `src/cargohome.rs`, `src/graph.rs`, `src/emit.rs`, `nix/builders.nix`

- [ ] The cargo home's `config.toml` and credential links, and the carried registry variables.
- [ ] `graph`: any `registry+` or `sparse+` source; the download address from the registry's cached `config.json`; none when it requires authentication or cannot be read.
- [ ] `fetchCrate` with `url = null` fails with what to do.

### Task 5: Fixtures and conformance

**Files:** `tests/fixtures/{gitdeps,config,registry}/`, `tests/registry.py`, `tests/fixtures.nix`, `examples/conformance.rs`, `flake.nix`

- [ ] `gitdeps`: `serde` with `derive` by tag (a workspace with a proc macro, build scripts and symlinks) and `itoa` by revision.
- [ ] `config`: a workspace below the source root, configuration files at both levels, `rustflags` from `cfg` tables over `[build]`, and `[env]` plain, forced, not forced, relative to a data file and relative to the workspace root.
- [ ] `registry`: a crate from a sparse registry served on localhost, configured in the caller's cargo home.
- [ ] `conformance --root`.

### Task 6: The driver

**Files:** `tests/run.sh`

- [ ] Each new fixture builds, runs its tests and conforms. Editing the data file a relative variable names rebuilds what was given it. A source edit does not rebuild a registry crate although a variable names the workspace root.
- [ ] The registry fixture builds with the registry reachable, its download reproduces the pre-seeded file, and the derivation for a registry without a usable address explains itself.
- [ ] Verify: `tests/run.sh` ends with `all integration checks passed`.

### Task 7: Documents

**Files:** the spec, `README.md`

- [ ] The spec describes git sources, registries and configuration where it describes the rest, and "Later stages" keeps stage 4 only. README: the three additions, what evaluation needs for them, what is still not supported.
