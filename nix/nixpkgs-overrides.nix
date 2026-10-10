# Turns crate overrides written for nixpkgs' buildRustCrate, such as
# pkgs.defaultCrateOverrides, into crateOverrides entries.
#
# An entry there is a function from the crate's attributes to attributes of
# its build. Three things of what it returns mean the same here: the
# libraries, the tools, and the environment variables, which buildRustCrate
# takes as attributes in capitals or in an `env` set. The rest, patches and
# hooks of a build this is not, is left out.
{ lib }:

overrides:
let
  isVariable = name: value:
    builtins.match "[A-Z][A-Z0-9_]*" name != null
    && (builtins.isString value || builtins.isBool value || builtins.isInt value
      || builtins.isPath value || lib.isDerivation value);
in
lib.mapAttrs
  (name: override:
    let
      # What little of a crate is known before the build is planned. An
      # entry that asks for more fails when its crate is built, and only
      # then: each entry is read when a package of the build takes it.
      crate = { crateName = name; pname = name; version = "0.0.0"; features = [ ]; };
      attrs = if builtins.isFunction override then override crate else override;
    in
    {
      buildInputs = attrs.buildInputs or [ ];
      nativeBuildInputs = attrs.nativeBuildInputs or [ ];
      env = lib.filterAttrs isVariable attrs // (attrs.env or { });
      # nixpkgs knows hundreds of crates and a build has few of them.
      optional = true;
    })
  overrides
