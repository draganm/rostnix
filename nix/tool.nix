# The rostnix binary. nixpkgs reads the checksums in Cargo.lock, so there is
# no dependency hash to maintain.
{ lib, rustPlatform }:
rustPlatform.buildRustPackage {
  pname = "rostnix";
  version = "0.1.0";
  # Only what the binary is built from. Editing docs, tests or the Nix
  # library must not rebuild the tool, and with it every unit.
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [ ../Cargo.toml ../Cargo.lock ../src ];
  };
  cargoLock.lockFile = ../Cargo.lock;
  # The unit tests read recorded cargo output that is not part of src.
  doCheck = false;
  meta = {
    description = "Builds Rust programs with one Nix derivation per cargo unit and nothing generated to check in";
    license = lib.licenses.mit;
    mainProgram = "rostnix";
  };
}
