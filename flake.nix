{
  description = "rostnix";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

    systems.url = "github:nix-systems/default";

  };

  outputs = { self, nixpkgs, systems, ... }@inputs:
    let
      eachSystem = f:
        nixpkgs.lib.genAttrs (import systems)
        (system: f system nixpkgs.legacyPackages.${system});
      mkRustEnv = import ./nix/mk-rust-env.nix;
    in {

      lib = { inherit mkRustEnv; };

      packages = eachSystem (system: pkgs: rec {
        rostnix = (mkRustEnv { inherit pkgs; }).tool;
        default = rostnix;
      });

      # Anything that needs builtins.exec lives under legacyPackages, which
      # `nix flake check` and `nix flake show` do not evaluate.
      legacyPackages = eachSystem (system: pkgs:
        let
          rustEnv = mkRustEnv { inherit pkgs; };
          # Another kind of machine that this one builds as, where there is
          # one: an Apple Silicon Mac runs Intel programs through Rosetta,
          # and a 64-bit Intel Linux runs 32-bit ones.
          elsewhere = {
            aarch64-darwin = "x86_64-darwin";
            x86_64-linux = "i686-linux";
          }.${system} or null;
          elsewherePkgs =
            if elsewhere == null then null else nixpkgs.legacyPackages.${elsewhere};
          wasi = pkgs.pkgsCross.wasi32;
          # Another platform whose programs its C compiler links and this
          # machine runs, where nixpkgs' cache has a rustc for it: Linux
          # with another C library.
          crossPkgs = { x86_64-linux = pkgs.pkgsCross.musl64; }.${system} or null;
          # What a plain `cargo build --target` needs: a compiler that has
          # the platform's standard library, the linker rostnix gives
          # rustc, and a C compiler for each of the two platforms.
          crossShell = cross: cross.mkShell {
            nativeBuildInputs = [ cross.buildPackages.cargo cross.buildPackages.rustc ];
            depsBuildBuild = [ cross.buildPackages.stdenv.cc ];
            "CARGO_TARGET_${cross.stdenv.hostPlatform.rust.cargoEnvVarTarget}_LINKER" =
              (mkRustEnv { pkgs = cross; }).linker;
          };
        in {
          inherit rustEnv;
          fixtures = import ./tests/fixtures.nix {
            inherit rustEnv pkgs mkRustEnv elsewherePkgs crossPkgs;
          };
          # What a plain `cargo build` of the buildscript fixture needs; the
          # integration tests make their reference build in it. Python
          # serves the registry of the registry fixture.
          fixtureShell = pkgs.mkShell {
            packages = [ pkgs.cargo pkgs.rustc pkgs.pkg-config pkgs.python3 ];
            buildInputs = [ pkgs.zlib ];
          };
          # The same for the builds for other platforms, and for the one on
          # another kind of machine.
          fixtureShellWasi = crossShell wasi;
          fixtureShellCross = crossShell crossPkgs;
          fixtureShellElsewhere = elsewherePkgs.mkShell {
            packages = [ elsewherePkgs.cargo elsewherePkgs.rustc ];
          };
          # What runs a program built for WebAssembly.
          inherit (pkgs) wasmtime;
          # The other kind of machine, for the tests to ask whether this
          # one is set up to build as it, and the platform a C compiler
          # links for. Empty where the tests know none.
          elsewhere = if elsewhere == null then "" else elsewhere;
          crossTarget =
            if crossPkgs == null then "" else crossPkgs.stdenv.hostPlatform.rust.rustcTarget;
        });

      devShells = eachSystem (system: pkgs: {
        default = pkgs.mkShell {
          hardeningDisable = [ "all" ];

          packages = with pkgs; [ cargo rustc rustfmt clippy rust-analyzer ];
        };
      });
    };
}
