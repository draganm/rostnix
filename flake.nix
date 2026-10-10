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
          # one: an Apple Silicon Mac runs Intel programs through Rosetta.
          elsewhere = { aarch64-darwin = "x86_64-darwin"; }.${system} or null;
          wasi = pkgs.pkgsCross.wasi32;
        in {
          inherit rustEnv;
          fixtures = import ./tests/fixtures.nix {
            inherit rustEnv pkgs mkRustEnv;
            elsewherePkgs =
              if elsewhere == null then null else nixpkgs.legacyPackages.${elsewhere};
          };
          # What a plain `cargo build` of the buildscript fixture needs; the
          # integration tests make their reference build in it. Python
          # serves the registry of the registry fixture.
          fixtureShell = pkgs.mkShell {
            packages = [ pkgs.cargo pkgs.rustc pkgs.pkg-config pkgs.python3 ];
            buildInputs = [ pkgs.zlib ];
          };
          # The same for a build for WebAssembly: a compiler that has that
          # platform's standard library, the linker rostnix gives rustc,
          # and a C compiler for each of the two platforms.
          fixtureShellWasi = wasi.mkShell {
            nativeBuildInputs = [ wasi.buildPackages.cargo wasi.buildPackages.rustc ];
            depsBuildBuild = [ wasi.buildPackages.stdenv.cc ];
            CARGO_TARGET_WASM32_WASIP1_LINKER = (mkRustEnv { pkgs = wasi; }).linker;
          };
          # What runs a program built for WebAssembly.
          inherit (pkgs) wasmtime;
          # The other kind of machine, for the tests to ask whether this
          # one is set up to build as it.
          elsewhere = if elsewhere == null then "" else elsewhere;
        });

      devShells = eachSystem (system: pkgs: {
        default = pkgs.mkShell {
          hardeningDisable = [ "all" ];

          packages = with pkgs; [ cargo rustc rustfmt clippy rust-analyzer ];
        };
      });
    };
}
