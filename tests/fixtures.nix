# The integration fixtures, built with the rustEnv under test. elsewherePkgs
# is the package set of another kind of machine that this one can build
# as, and crossPkgs one for another platform that a C compiler links for,
# where there is one.
{ rustEnv, pkgs, mkRustEnv, elsewherePkgs ? null, crossPkgs ? null }:
let
  inherit (pkgs) lib;
  cross =
    if crossPkgs == null
    then throw "rostnix: the tests know no platform that ${pkgs.stdenv.buildPlatform.system} builds for with a C compiler as the linker"
    else mkRustEnv { pkgs = crossPkgs; };
  # For WebAssembly, which the build machine cannot run: nixpkgs has its
  # compiler ready, and a build needs nothing of the platform but a linker.
  wasi = mkRustEnv { pkgs = pkgs.pkgsCross.wasi32; };
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

  # The same with nixpkgs' own overrides, which know what libz-sys needs.
  buildscript-nixpkgs = rustEnv.buildRustApplication {
    pname = "buildscript";
    src = ./fixtures/buildscript;
    crateOverrides = rustEnv.nixpkgsCrateOverrides // {
      bs-native.env.ROSTNIX_FIXTURE_NOTE = "from-override";
      consumer = {
        nativeBuildInputs = [ pkgs.pkg-config ];
        buildInputs = [ pkgs.zlib ];
        extraSrc = [ "shared" ];
      };
    };
  };

  # A library for use from C, dynamic and static, whose build script
  # compiles C, and a program in Rust that uses it.
  libs = rustEnv.buildRustApplication {
    pname = "libs";
    src = ./fixtures/libs;
  };

  # With debug information, which on macOS a dynamic library points at as
  # an executable does.
  libs-dev = rustEnv.buildRustApplication {
    pname = "libs-dev";
    src = ./fixtures/libs;
    profile = "dev";
  };

  # For another platform: proc macros and build scripts are built for the
  # machine that builds and the rest for WebAssembly, C included.
  hello-wasi = wasi.buildRustApplication {
    pname = "hello";
    src = ./fixtures/hello;
  };
  libs-wasi = wasi.buildRustApplication {
    pname = "libs";
    src = ./fixtures/libs;
  };

  # For another platform that its C compiler links, and whose programs the
  # build machine runs: the tests are built for it and run.
  hello-cross = cross.buildRustApplication {
    pname = "hello";
    src = ./fixtures/hello;
  };
  libs-cross = cross.buildRustApplication {
    pname = "libs";
    src = ./fixtures/libs;
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

  # A dependency from a registry other than crates.io: the one that
  # tests/registry.py serves. This evaluates only while that runs and the
  # cargo home of whoever evaluates names it; tests/run.sh sees to both.
  registry = rustEnv.buildRustApplication {
    pname = "registry";
    src = ./fixtures/registry;
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
    # One test expects an extracted file to keep its setuid bit, and Nix
    # lets no build create a setuid file. Another sets an extended
    # attribute, which on Linux Nix lets no build do either.
    checkFlags = [ "--skip" "golden_tar_extracts" ]
      ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux [ "--skip" "export_extract_roundtrip" ];
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

  # Planned on this machine and built on another kind: the plan must be
  # one for that machine, down to the tests, which run there.
  hello-elsewhere =
    if elsewherePkgs == null
    then throw "rostnix: the tests know no other kind of machine that ${pkgs.stdenv.buildPlatform.system} builds as"
    else
      (mkRustEnv { pkgs = elsewherePkgs; evalPkgs = pkgs; }).buildRustApplication {
        pname = "hello";
        src = ./fixtures/hello;
      };
}
