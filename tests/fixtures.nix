# The integration fixtures, built with the rustEnv under test.
{ rustEnv, pkgs }:
let
  inherit (pkgs) lib;
  profiles = profile: rustEnv.buildRustApplication {
    pname = "profiles-${profile}";
    src = ./fixtures/profiles;
    inherit profile;
  };
in
{
  hello = rustEnv.buildRustApplication {
    pname = "hello";
    src = ./fixtures/hello;
  };

  # Four members, one of them a proc macro and one inside another's
  # directory. The default selection is every member.
  workspace = rustEnv.buildRustApplication {
    pname = "workspace";
    version = "0.3.0";
    src = ./fixtures/workspace;
  };
  workspace-shout = rustEnv.buildRustApplication {
    pname = "workspace-shout";
    src = ./fixtures/workspace;
    packages = [ "ws-app" ];
    bins = [ "ws-app" ];
    features = [ "shout" ];
  };

  # Build scripts that compile C, generate code and pass metadata on, with
  # everything crateOverrides can supply.
  buildscript = rustEnv.buildRustApplication {
    pname = "buildscript";
    src = ./fixtures/buildscript;
    crateOverrides = {
      # The build script puts this in the program's output.
      bs-native.env.ROSTNIX_FIXTURE_NOTE = "from-override";
      consumer = {
        # Its build script asks pkg-config for zlib's version.
        nativeBuildInputs = [ pkgs.pkg-config ];
        buildInputs = [ pkgs.zlib ];
        # It includes a file from outside its package directory.
        extraSrc = [ "shared" ];
      };
      libz-sys = {
        nativeBuildInputs = [ pkgs.pkg-config ];
        buildInputs = [ pkgs.zlib ];
      };
    };
  };

  # Dependencies from git repositories: a workspace with a proc macro and
  # build scripts, by tag, and a single package by revision.
  gitdeps = rustEnv.buildRustApplication {
    pname = "gitdeps";
    src = ./fixtures/gitdeps;
  };

  # A workspace below the source root, with cargo configuration at both
  # levels: rustflags from target tables, and [env] variables that are
  # plain, forced, and relative to a data file and to the workspace.
  config = rustEnv.buildRustApplication {
    pname = "config";
    src = ./fixtures/config;
    cargoRoot = "ws";
  };

  # One project under four profiles: fat LTO with panic=abort, thin LTO,
  # no LTO, and dev with debug information.
  profiles-release = profiles "release";
  profiles-thin = profiles "thin";
  profiles-nolto = profiles "nolto";
  profiles-dev = profiles "dev";

  # Patient zero: a library whose CLI is an example, with C-compiling build
  # scripts, thin LTO and a dependency that is also a cdylib.
  core-rs = rustEnv.buildRustApplication {
    pname = "amber-store";
    version = "0.10.0";
    src = builtins.fetchTree {
      type = "github";
      owner = "amber-store";
      repo = "core-rs";
      rev = "e6e900b7a0f41b3540a319c1367fd167921d5d8c";
    };
    examples = [ "amber-store" ];
    # It runs `cargo build`, which needs the network and a target directory.
    skipTests = [ "amber_bench_smoke" ];
    # It expects an extracted file to keep its setuid bit, and Nix lets no
    # build create a setuid file.
    checkFlags = [ "--skip" "golden_tar_extracts" ];
  };

  # rostnix builds itself and runs its own unit tests. The source is
  # narrowed to what cargo and the tests read, so that editing the docs or
  # the Nix library rebuilds nothing.
  self = rustEnv.buildRustApplication {
    pname = "rostnix";
    src = lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions [ ../Cargo.toml ../Cargo.lock ../src ../examples ../testdata ];
    };
  };
}
