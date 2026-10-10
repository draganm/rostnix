# mkRustEnv ties rostnix to one nixpkgs: that nixpkgs builds the tool and
# performs the Rust build. A cross package set builds for another platform:
# what runs while building is built for the machine that builds, and the
# rest for the platform the set is for.
{ pkgs
  # The package set of the machine that evaluates, when that is not the
  # machine that builds: the tool, cargo and rustc that plan come from it.
, evalPkgs ? null
  # The compiler inside derivations. Cargo also queries it while planning.
, rustc ? pkgs.buildPackages.rustc
  # The cargo that plans during evaluation. At build time it is only what
  # a test finds as CARGO.
, cargo ? pkgs.buildPackages.cargo
  # The same two for the machine that evaluates, when evalPkgs is given.
  # With a rustc or a cargo that is not nixpkgs' own, these are the same
  # versions built for that machine.
, evalRustc ? if evalPkgs == null then rustc else evalPkgs.rustc
, evalCargo ? if evalPkgs == null then cargo else evalPkgs.cargo
  # The linker of what is built for another platform than the machine
  # that builds. null means the one that fits the platform.
, linker ? null
}:
let
  inherit (pkgs) lib;
  inherit (pkgs.stdenv) buildPlatform hostPlatform;
  buildPkgs = pkgs.buildPackages;

  # Derivations run on the build platform.
  inherit (buildPlatform) system;

  isCross = !(lib.systems.equals buildPlatform hostPlatform);

  tool = buildPkgs.callPackage ./tool.nix { };

  # rustc links by calling `cc`, which in a derivation for another platform
  # would be the build machine's. So it is told the C compiler of that
  # platform, as nixpkgs' own Rust support tells it. WebAssembly is the
  # exception: rustc drives its linker as an lld, which a C compiler does
  # not understand and nixpkgs' wrapper of ld does not pass on.
  platformLinker =
    if hostPlatform.isWasm
    then "${pkgs.stdenv.cc.bintools.bintools}/bin/${pkgs.stdenv.cc.targetPrefix}wasm-ld"
    else "${pkgs.stdenv.cc}/bin/${pkgs.stdenv.cc.targetPrefix}cc";
  targetLinker = if linker != null then linker else platformLinker;

  builders = import ./builders.nix {
    inherit lib tool rustc cargo system isCross;
    inherit (buildPkgs) fetchurl runCommand;
    # Units that link, and build scripts, use the C compiler of the
    # platform they are for: the one the program runs on, or the machine
    # that builds.
    inherit (pkgs) stdenv;
    buildStdenv = buildPkgs.stdenv;
    linker = targetLinker;
  };

  fromNixpkgsCrateOverrides = import ./nixpkgs-overrides.nix { inherit lib; };

  buildRustApplication = import ./build-rust-application.nix {
    inherit lib rustc cargo isCross;
    inherit (buildPkgs) runCommand;
    # Installing an executable for macOS needs the tools that handle its
    # debug information.
    inherit (pkgs) runCommandCC;
    inherit (hostPlatform) isDarwin isElf;
    # What rewrites where an ELF file looks for its libraries.
    inherit (buildPkgs) patchelf;
    # nixpkgs names the tools for another platform with it in front.
    targetPrefix = pkgs.stdenv.cc.targetPrefix;
    mkBuilders = builders;
    # What runs during evaluation runs on the machine that evaluates.
    evalTool = if evalPkgs == null then tool else evalPkgs.callPackage ./tool.nix { };
    inherit evalRustc evalCargo;
    # The platform to build for and the machine that builds, as rustc
    # names them.
    target = hostPlatform.rust.rustcTarget;
    host = buildPlatform.rust.rustcTarget;
    # Tests are run where the machine that builds can run them.
    canExecute = buildPlatform.canExecute hostPlatform;
    # The ending rustc gives an executable of the platform.
    executableExtension =
      if hostPlatform.isWasm then ".wasm" else hostPlatform.extensions.executable;
  };
in
{
  inherit buildRustApplication tool rustc cargo builders fromNixpkgsCrateOverrides;
  # The linker given to rustc for the platform, when it is another than the
  # build machine's.
  linker = if isCross then targetLinker else null;
  # nixpkgs' own overrides for crates that need a library or a tool.
  nixpkgsCrateOverrides = fromNixpkgsCrateOverrides pkgs.defaultCrateOverrides;
}
