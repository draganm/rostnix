# rostnix

[![tests](https://github.com/draganm/rostnix/actions/workflows/tests.yml/badge.svg)](https://github.com/draganm/rostnix/actions/workflows/tests.yml)

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
- **Tests are steps too.** Each test executable `cargo test` would build is
  built and run in its own derivation, and the application is built only
  when they pass. An edit runs again the tests that could tell the
  difference.
- **No lockfile for Nix.** Crate hashes are the checksums already in
  `Cargo.lock`, and git dependencies are fetched at the revisions it names.
  A Rust project commits no Nix code or hash that depends on `Cargo.toml`
  or `Cargo.lock`.

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
| `pkgs` | required | The nixpkgs that builds the tool and performs the build. A cross package set builds for its platform; see [Building for another platform](#building-for-another-platform). |
| `rustc` | `pkgs.buildPackages.rustc` | The compiler inside derivations. Cargo also queries it while planning. |
| `cargo` | `pkgs.buildPackages.cargo` | The cargo that plans during evaluation. |
| `linker` | `null` | What rustc links with for a platform other than the build machine's. `null` means the one that fits the platform. |
| `evalPkgs` | `null` | The package set of the machine that evaluates, when it is another kind of machine than the one that builds. |
| `evalRustc`, `evalCargo` | those of `evalPkgs` | The rustc and cargo that plan when `evalPkgs` is given. |

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
| `doCheck` | `true` where the build machine runs the platform's programs | Build and run the tests of the selected packages; see [Tests](#tests). |
| `checkFlags` | `[ ]` | Arguments for every test executable, such as `[ "--skip" "needs_network" ]`. |
| `skipTests` | `[ ]` | Names of test targets that are neither built nor run. |
| `rustflags` | `null` | The flags every rustc gets. `null` means those of the project's [cargo configuration](#cargo-configuration); a list takes their place. |

The selection means what it means to `cargo build`. With neither `bins` nor
`examples`, cargo builds the library and every binary of the selected
packages. With either, it builds only what they name.

Every binary and example the selection builds lands in `$out/bin` under its
target name. A library the selection builds for use from outside Rust, with
the crate type `cdylib` or `staticlib`, lands in `$out/lib` under the name
a linker looks for, such as `libfoo.so` and `libfoo.a`. On macOS, an
executable or dynamic library built with debug information has a `.dSYM`
bundle beside it. A selection that builds none of these, which is what a
project with only a Rust library gives, is an error that says so. A
project whose executable is an example names it:

```nix
rustEnv.buildRustApplication {
  pname = "amber-store";
  src = ./.;
  examples = [ "amber-store" ];
}
```

The result's `passthru` has `units`, `bins`, `libs` and `tests`, each a set
of derivations, so one step can be built alone:

```bash
opt=(--option allow-unsafe-native-code-during-evaluation true)
nix eval "${opt[@]}" .#default.units --apply builtins.attrNames
nix build "${opt[@]}" '.#default.units."serde-1.0.229-lib-3fa94c1e"'
```

A unit's output holds `unit.json`, which records the rustc command line and
environment it ran with.

### Tests

With `doCheck`, which is on unless you turn it off, rostnix also asks cargo
what `cargo test` would build for the selected packages, with the same
features and profile: the unit tests of each library and binary, the
integration tests, and the examples. `bins` and `examples` choose what is
installed and do not narrow what is tested. Doc tests are not run.

Each test executable is one derivation, which compiles it and runs it. The
application depends on all of them, so a failing test fails the build and
its output is in the build log. Everything a test shares with the
application is built once: a dependency is built a second time only where
cargo would build it differently for tests, for instance when a
dev-dependency turns one of its features on.

A test is compiled and run as under cargo:

- in a writable copy of its package, so it can create files beside itself,
  and a fixture it copies out of the source can be changed;
- from `target/<profile>/deps/`, with the package's binaries and examples
  in the directories beside for an integration test, which also gets
  `CARGO_BIN_EXE_<name>` and `CARGO_TARGET_TMPDIR`;
- with the arguments in `checkFlags`, and the tools and environment of its
  package's `crateOverrides` entry.

```nix
rustEnv.buildRustApplication {
  pname = "app";
  src = ./.;
  # tests/e2e.rs needs a server that is not there.
  skipTests = [ "e2e" ];
  # One test function in any test executable, by the harness's own flag.
  checkFlags = [ "--skip" "resolves_the_public_name" ];
}
```

`skipTests` takes target names: the file name of an integration test
without `.rs`, or the name of a library or binary for its unit tests. An
entry that names no test target gets a warning. One test can be built and
run alone, and leaves what it printed in its output:

```bash
nix eval "${opt[@]}" .#default.tests --apply builtins.attrNames
nix build "${opt[@]}" '.#default.tests."app-0.1.0-test-e2e-91d0c3aa"'
cat result/log
```

The application also depends on the examples `cargo test` builds, so one
that does not compile fails the build as it fails `cargo test`.

Limits worth knowing:

- What a Nix build forbids, a test cannot do: reach the network (in a
  sandboxed build), create a setuid file, or on Linux set an extended
  attribute.
- Tests are built with `profile`, which is `release` unless you say
  otherwise, so `debug_assert!` and overflow checks are off as under
  `cargo test --release`.
- Only the test itself is compiled in the writable copy. A library that
  returns `env!("CARGO_MANIFEST_DIR")` from its own, non-test code hands
  its tests a read-only store path without `tests/`.
- The copy holds the test's package, not the workspace around it. Name
  files outside the package in `extraSrc`.
- A name in `skipTests` skips every test target of that name, in every
  selected package. Every test executable gets the same `checkFlags`.
- A test is an input of the application: editing one gives the application
  a new store path although its binaries are unchanged.
- If something only the tests need cannot be planned, the error says so
  and `doCheck = false` builds without tests.

### Git dependencies and other registries

Neither needs anything in Nix. A dependency from a git repository is
fetched during evaluation, by Nix's `builtins.fetchGit`, at the revision
`Cargo.lock` names and with your own git credentials; private repositories
work if `git` can reach them. Cargo fetches the repository as well, to
plan, and is told to use the `git` command for it unless a cargo
configuration or `CARGO_NET_GIT_FETCH_WITH_CLI` says otherwise, so that
both read the same git and ssh settings.

A crate from another registry is found the way cargo finds it: through the
registry settings and tokens of your cargo home, or the project's own
`.cargo/config.toml`. Its file is added to the Nix store during
evaluation, like a crates.io crate's. A machine that did not evaluate the
project downloads it from the address the registry publishes. A registry
that wants a token for downloads has no such address, so its crates build
on the machine that evaluated, or come from a substituter.

From your cargo home rostnix takes only what says where crates come from
and how to reach them: the tables `[registries]`, `[registry]`, `[source]`,
`[net]`, `[http]` and `[credential-alias]` of its `config.toml`, and your
credentials. Anything there about how to build is ignored.

### Cargo configuration

The project's own `.cargo/config.toml` files, those of the workspace and
of the directories above it inside `src`, say how to build, and two of
their settings are applied:

- **`rustflags`**, from `[build]` and from `[target.<triple>]` and
  `[target.'cfg(…)']` tables, by cargo's rule: when a target table that
  matches has flags, `[build]`'s are not used. Every rustc invocation gets
  them, and build scripts are told them.
- **`[env]`**, for every rustc invocation, build script and test, with
  `force` meaning what it means to cargo.

An `[env]` value with `relative = true` names a path of the source, as in
the usual

```toml
[env]
CARGO_WORKSPACE_DIR = { value = "", relative = true }
```

Your own packages find the path in their own source, and their tests in
their writable copy. Crates from a registry or from git are not given such
a variable, where cargo gives it to everything: a path of your source
would become an input of every crate, and all of them would be rebuilt
whenever something under it changes. A crate that does need one, a `-sys`
crate whose build script is pointed at a configuration file of yours, can
be given it:

```nix
crateOverrides.some-sys.env.SOME_CONFIG = "${./config/some.toml}";
```

Other settings that act at build time are not applied, and evaluation
warns about them: `linker` and `runner` of a `[target]` table and
`build.rustc-wrapper`. rostnix links with the C compiler of the nixpkgs you
give it. If your configuration's flags only make sense with such a linker,
`-C link-arg=-fuse-ld=mold` say, name the flags you do want:

```nix
rustEnv.buildRustApplication {
  pname = "app";
  src = ./.;
  rustflags = [ "--cfg" "tokio_unstable" ];
}
```

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
| `nativeBuildInputs` | `[ ]` | Tools the package's build script, rustc invocations and tests run, such as `pkg-config`. |
| `env` | `{ }` | Environment of the package's build script, of its rustc invocations and of its tests. |
| `extraSrc` | `[ ]` | Local packages only: files and directories, relative to `src`, that the package reads from outside what its steps see. |

Any other attribute in an entry is an error, as is `extraSrc` for a crate
that is not part of the source tree. A key that names no package of the
build gets a warning, because such an entry changes nothing; `optional =
true` in an entry turns that warning off.

nixpkgs already knows what many crates need, in the `defaultCrateOverrides`
of its own `buildRustCrate`. `rustEnv.nixpkgsCrateOverrides` is that
knowledge as entries, to which you add your own:

```nix
crateOverrides = rustEnv.nixpkgsCrateOverrides // {
  my-crate.extraSrc = [ "proto" ];
};
```

Of each nixpkgs entry it takes `buildInputs`, `nativeBuildInputs` and the
environment variables, and leaves out the rest: patches, hooks and flags.
`rustEnv.fromNixpkgsCrateOverrides` does the same for a set of your own
that is written for `buildRustCrate`.

### Building for another platform

Pass a cross package set, and the build is for its platform:

```nix
rustEnv = rostnix.lib.mkRustEnv { pkgs = pkgs.pkgsCross.wasi32; };
```

Cargo plans with `--target`. Build scripts, proc macros and what they
depend on are built for the machine that builds, and everything else for
the platform, with its linker. A build script that compiles C compiles it
for the platform: it runs with that platform's C compiler as `CC` and the
build machine's as `HOST_CC`. In `crateOverrides`, write libraries and
tools as you would in a nixpkgs package, from the cross package set; a
proc macro or a build-machine dependency that takes the entry gets the
same packages for the build machine. An entry's `env` is given as written
to both, so a path in it names the platform's library for both.

Executables are installed under the names rustc gives them, `app.wasm` for
WebAssembly. Tests are built and run by default only where the build
machine can run them; elsewhere `doCheck` is off.

rustc links with the platform's C compiler, and for WebAssembly with
`wasm-ld`, which unlike a C compiler from nixpkgs is not told where the
libraries of `buildInputs` are: there a build script must name them, as
one that asks `pkg-config` does. `linker` of `mkRustEnv` names another. nixpkgs must have a rustc
that can build for the platform, which for most cross package sets means
building rustc first.

When the machine that evaluates is another kind than the one that builds,
a Mac that hands the build to a Linux builder, say, tell `mkRustEnv` both:

```nix
rustEnv = rostnix.lib.mkRustEnv {
  pkgs = nixpkgs.legacyPackages.x86_64-linux;        # builds
  evalPkgs = nixpkgs.legacyPackages.aarch64-darwin;  # evaluates
};
```

### What a step sees of a local package

Rust has no list of the files a target reads, so a step gets its package
directory, narrowed by three rules:

- Directories of other packages inside it are left out.
- `examples/`, `tests/` and `benches/` are left out, unless the step builds
  an example, test or bench. Everything built as a test, unit tests
  included, keeps all three: a test may read whatever lies in its package.
- The root files of the package's other binaries, examples, tests and
  benches are left out. A build script's run still sees them all, since
  build scripts read source files on their own.

A file that the step's own root file names stays in any case: by
`mod common;`, by `#[path = "…"]`, or by `include_str!("…")` and its like
with a literal path. So `tests/common.rs` stays for the tests that use it
as a module, and an example stays for a library that includes it in its
documentation.

Editing `src/main.rs` therefore rebuilds the binary and not the library,
and editing `tests/e2e.rs` builds and runs that test again and nothing
else. A step that reads a file outside this
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
  cache lacks. It uses your cargo home's download caches, registry settings
  and credentials, and nothing of what your configuration says about
  building: `[build]` and `[env]` in `~/.cargo/config.toml`, `RUSTFLAGS`
  and the like do not change the build.
- For git dependencies: `git` on your `PATH`, and access to the
  repositories. Each is fetched twice the first time, once by cargo and
  once by Nix.
- Import-from-derivation (on by default): the tool, cargo and rustc are
  built or fetched during evaluation the first time.
- A recent Nix: rostnix is tested with Nix 2.26 and 2.31. Pre-seeding uses
  `nix store add --mode flat`; with an older `nix` that add fails and crates
  are downloaded at build time instead.

Crate files are added to the Nix store during evaluation, from cargo's
cache, so nothing is downloaded twice. A crate is downloaded by a
derivation only when the build happens on a machine that did not evaluate
it.

## Not yet supported

Path dependencies outside `src`, dependencies built as Rust `dylib`s, doc
tests and benches, vendored sources, Git LFS, a package that names its own
target, and running tests through a `runner`. Path dependencies outside
`src` are rejected during evaluation with a message naming them.

Build scripts run with their package directory read-only: one that writes
outside `OUT_DIR` fails.

With `evalPkgs`, cargo decides the dependencies of build scripts and proc
macros for the machine that evaluates. A build dependency that only one of
the two kinds of machine has is planned wrongly.

The integration tests pass on aarch64-darwin and on x86_64-linux (NixOS).
Building for another platform has been tested for WebAssembly
(`pkgsCross.wasi32`) on both, and on x86_64-linux for Linux with musl
(`pkgsCross.musl64`), which links with a C compiler and whose tests run on
the build machine. `evalPkgs` has been tested by building for x86_64-darwin
from aarch64-darwin and for i686-linux from x86_64-linux. A build for
another processor was made once by hand, for aarch64 Linux from x86_64
Linux, and its results were not run. A build for macOS from another
machine has not been made.

## Development

```bash
nix develop --command cargo test   # unit tests
tests/run.sh                       # integration tests: real nix builds
```

The integration tests build small fixtures, rostnix itself, and
[amber-store/core-rs](https://github.com/amber-store/core-rs) at a pinned
commit, and run the test suite of each. For each they compare every rustc
invocation, build-script run and test run with what `cargo build -vv` and
`cargo test -vv` do for the same source, and check that an edit rebuilds
only the steps it should. Two fixtures are also built for WebAssembly and
run under wasmtime, and on x86_64 Linux for musl, with their tests. On an
Apple Silicon Mac with Rosetta one fixture is planned there and built as
x86_64, and on x86_64 Linux as i686. They fetch two repositories from
GitHub and serve a small registry on port 18473 of this machine.

Both run for every pull request and every push to `main`, the integration
tests on x86_64 Linux, on arm64 Linux and on an Apple Silicon Mac; see
`.github/workflows/tests.yml`.

The design is in `docs/superpowers/specs/2026-10-09-rostnix-design.md`.

## License

MIT, see [LICENSE](LICENSE).
