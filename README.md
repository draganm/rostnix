# rostnix

Build Rust programs with Nix one compile step per derivation, with nothing
to check in when `Cargo.toml` or `Cargo.lock` changes.

rostnix does for Rust what [gonixgo](https://github.com/draganm/gonixgo)
does for Go. Cargo plans and rustc builds:

- **Cargo plans during evaluation.** A Nix-built binary runs through
  `builtins.exec`, asks cargo for its build graph, and prints the Nix code
  for the build. Features, profiles and target selection are cargo's own.
- **rustc builds in derivations.** Each step of cargo's plan, one rustc
  invocation or one build-script run, is its own derivation. An edit
  rebuilds the steps that see the file and the steps that depend on them.
  Cargo does not run at build time.
- **No lockfile for Nix.** Crate hashes are the checksums already in
  `Cargo.lock`. A Rust project commits no Nix code or hash that depends on
  `Cargo.toml` or `Cargo.lock`.

[crate2nix](https://github.com/nix-community/crate2nix) can also generate
its build during evaluation. rostnix differs in that cargo resolves the
graph rather than a reimplementation in Nix, in building per compile step
rather than per crate, and in giving each step the flags of cargo's profile
for it: LTO, `panic`, per-package overrides.

## Use

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
        pname = "app";
        src = ./.;
      };
    };
}
```

```bash
nix build --option allow-unsafe-native-code-during-evaluation true
```

With `allow-unsafe-native-code-during-evaluation` on, any Nix expression
you evaluate can run programs as you. Prefer passing `--option` per command
for projects you trust over enabling it in `nix.conf`. `nix flake check` and
`nix flake show` on a flake that exposes a rostnix package under `packages`
need the option too.

The `pkgs` you pass builds the rostnix tool and performs the Rust build.
Without flakes, `import rostnix { inherit pkgs; }` returns the same set as
`mkRustEnv`.

### `mkRustEnv`

| Argument | Default | Meaning |
|---|---|---|
| `pkgs` | required | The nixpkgs that builds the tool and performs the build. |
| `rustc` | `pkgs.buildPackages.rustc` | The compiler inside derivations. Cargo also queries it while planning. |
| `cargo` | `pkgs.buildPackages.cargo` | The cargo that plans during evaluation. |

rostnix's flag rules follow cargo 1.95, the cargo of nixpkgs 26.05. Other
toolchains can be passed but are not tested.

### `buildRustApplication`

| Argument | Default | Meaning |
|---|---|---|
| `pname` | required | Derivation name. |
| `version` | `null` | Appended to the derivation name. |
| `src` | required | The source tree. |
| `cargoRoot` | `"."` | Directory inside `src` that holds the workspace's `Cargo.toml` and `Cargo.lock`. |
| `packages` | `[ ]` | Workspace members to build, as `cargo build -p`. Empty means cargo's default members. |
| `bins` | `[ ]` | Binaries to build, as `--bin`. |
| `examples` | `[ ]` | Examples to build, as `--example`. |
| `features` | `[ ]` | As `--features`. |
| `allFeatures` | `false` | As `--all-features`. |
| `noDefaultFeatures` | `false` | As `--no-default-features`. |
| `profile` | `"release"` | As `--profile`. |
| `crateOverrides` | `{ }` | Libraries and tools for crates that need them; see [crateOverrides](#crateoverrides). |

The selection means what it means to `cargo build`. With neither `bins` nor
`examples`, cargo builds the library and every binary of the selected
packages. With either, it builds only what they name.

Every binary and example the selection builds lands in `$out/bin` under its
target name. On macOS, one built with debug information has a `.dSYM`
bundle beside it. A selection that builds neither, which is what a library-only
project gives by default, is an error that says so. A project whose
executable is an example names it:

```nix
rustEnv.buildRustApplication {
  pname = "amber-store";
  src = ./.;
  examples = [ "amber-store" ];
}
```

The result's `passthru` has `units` and `bins`, each a set of derivations,
so one step can be built alone:

```bash
opt=(--option allow-unsafe-native-code-during-evaluation true)
nix eval "${opt[@]}" .#default.units --apply builtins.attrNames
nix build "${opt[@]}" '.#default.units."serde-1.0.229-lib-3fa94c1e"'
```

A unit's output holds `unit.json`, which records the rustc command line and
environment it ran with.

### crateOverrides

A crate whose build script compiles bundled C needs nothing from you: build
scripts run with the C compiler of the `pkgs` you pass. One that needs a
library or a tool from nixpkgs gets an entry, keyed by package name:

```nix
rustEnv.buildRustApplication {
  pname = "app";
  src = ./.;
  crateOverrides = {
    libz-sys = {
      buildInputs = [ pkgs.zlib ];
      nativeBuildInputs = [ pkgs.pkg-config ];
    };
    my-crate.extraSrc = [ "proto" ];
  };
}
```

| Attribute | Default | Meaning |
|---|---|---|
| `buildInputs` | `[ ]` | Libraries. The package's build-script run gets them, and so do the build scripts that build against its native library (those that depend on it through `links`) and every step that links the package. |
| `nativeBuildInputs` | `[ ]` | Tools the package's build script and rustc invocations run, such as `pkg-config`. |
| `env` | `{ }` | Environment of the package's build script and of its rustc invocations. |
| `extraSrc` | `[ ]` | Local packages only: files and directories, relative to `src`, that the package reads from outside what its steps see. |

Any other attribute in an entry is an error, as is `extraSrc` for a crate
that is not part of the source tree. A key that names no package of the
build gets a warning, because such an entry changes nothing.

### What a step sees of a local package

Rust has no list of the files a target reads, so a step gets its package
directory, narrowed by three rules:

- Directories of other packages inside it are left out.
- `examples/`, `tests/` and `benches/` are left out, unless the step builds
  an example, test or bench.
- The root files of the package's other binaries, examples, tests and
  benches are left out. A build script's run still sees them, since build
  scripts read source files on their own.

Editing `src/main.rs` therefore rebuilds the binary and not the library,
and editing a test rebuilds nothing. A step that reads a file outside this
view, such as `include_str!("../../README.md")` from a workspace member,
fails with "file not found" until `extraSrc` names the file.

Files no target reads, such as documentation, still rebuild the package
when they change. Passing a narrowed `src`, for example with
`lib.fileset`, avoids that.

## What evaluation needs

- `allow-unsafe-native-code-during-evaluation = true`, from `--option`,
  `NIX_CONFIG` or `nix.conf`. A flake's `nixConfig` cannot set it.
- A committed `Cargo.lock` that matches `Cargo.toml`.
- The project's crates: cargo runs during evaluation and downloads what its
  cache lacks. It uses your cargo home's download cache and nothing else of
  your configuration: `~/.cargo/config.toml`, `RUSTFLAGS` and the like do
  not change the build.
- Import-from-derivation (on by default): the tool, cargo and rustc are
  built or fetched during evaluation the first time.
- A recent Nix: rostnix is developed against Nix 2.26. Pre-seeding uses
  `nix store add --mode flat`; with an older `nix` that add fails and crates
  are downloaded at build time instead.

Crate files are added to the Nix store during evaluation, from cargo's
cache, so nothing is downloaded twice. A crate is downloaded by a
derivation only when the build happens on a machine that did not evaluate
it.

## Not yet supported

Tests (`doCheck`, `checkFlags` and `skipTests` are accepted and ignored),
dependencies from git repositories and from registries other than
crates.io, path dependencies outside `src`, `rustflags` and `[env]` from
`.cargo/config.toml`, cross-compilation, installing `cdylib` and `staticlib`
targets, dependencies built as Rust `dylib`s, doc tests and benches. Git and registry dependencies, path
dependencies outside `src` and builds for another target are rejected
during evaluation with a message naming them.

Build scripts run with their package directory read-only: one that writes
outside `OUT_DIR` fails.

The integration tests have been run on aarch64-darwin only; Linux is
untested.

## Development

```bash
nix develop --command cargo test   # unit tests
tests/run.sh                       # integration tests: real nix builds
```

The integration tests build small fixtures, rostnix itself, and
[amber-store/core-rs](https://github.com/amber-store/core-rs) at a pinned
commit. For each they compare every rustc invocation and build-script run
with what `cargo build -vv` runs for the same source, and check that an
edit rebuilds only the steps it should.

The design is in `docs/superpowers/specs/2026-10-09-rostnix-design.md`.

## License

MIT, see [LICENSE](LICENSE).
