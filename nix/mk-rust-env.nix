# mkRustEnv ties rostnix to one nixpkgs: that nixpkgs builds the tool and
# performs the Rust build.
{ pkgs
  # The compiler inside derivations. Cargo also queries it while planning.
, rustc ? pkgs.buildPackages.rustc
  # The cargo that plans during evaluation. It does not run at build time.
, cargo ? pkgs.buildPackages.cargo
}:
let
  inherit (pkgs) lib;
  buildPkgs = pkgs.buildPackages;

  # Derivations run on the build platform.
  inherit (pkgs.stdenv.buildPlatform) system;

  tool = buildPkgs.callPackage ./tool.nix { };

  builders = import ./builders.nix {
    inherit lib tool rustc cargo system;
    inherit (buildPkgs) fetchurl runCommand;
    # Units that link, and build scripts, use the C compiler for the
    # platform the program runs on.
    inherit (pkgs) stdenv;
  };

  buildRustApplication = import ./build-rust-application.nix {
    inherit lib tool rustc cargo;
    inherit (buildPkgs) runCommand;
    mkBuilders = builders;
  };
in
{
  inherit buildRustApplication tool rustc cargo builders;
}
