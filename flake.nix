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
        let rustEnv = mkRustEnv { inherit pkgs; };
        in {
          inherit rustEnv;
          fixtures = import ./tests/fixtures.nix { inherit rustEnv pkgs; };
          # What a plain `cargo build` of the buildscript fixture needs; the
          # integration tests make their reference build in it. Python
          # serves the registry of the registry fixture.
          fixtureShell = pkgs.mkShell {
            packages = [ pkgs.cargo pkgs.rustc pkgs.pkg-config pkgs.python3 ];
            buildInputs = [ pkgs.zlib ];
          };
        });

      devShells = eachSystem (system: pkgs: {
        default = pkgs.mkShell {
          hardeningDisable = [ "all" ];

          packages = with pkgs; [ cargo rustc rustfmt clippy rust-analyzer ];
        };
      });
    };
}
