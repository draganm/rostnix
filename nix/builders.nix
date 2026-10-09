# The builder functions the generated graph calls. Each defines how one
# kind of node becomes a derivation. The wiring between nodes is in the
# graph `rostnix resolve` prints.
{ lib, fetchurl, runCommand, stdenv, tool, rustc, cargo, system }:

# Per-application settings.
{ srcStr, crateOverrides ? { } }:
let
  builder = "${tool}/bin/rostnix";
  rustcBin = "${rustc}/bin/rustc";
  cargoBin = "${cargo}/bin/cargo";

  # The crateOverrides entry of a package, if it has one.
  overrideOf = package:
    if package.override == null then { } else crateOverrides.${package.override};

  # "a/b/c" gives [ "a" "a/b" ]: every directory on the way to a path.
  parents = path:
    let parts = lib.splitString "/" path;
    in lib.genList (i: lib.concatStringsSep "/" (lib.take (i + 1) parts)) (lib.length parts - 1);

  # Whether rel is base or lies under it; "." is the source root.
  isUnder = base: rel: base == "." || rel == base || lib.hasPrefix "${base}/" rel;

  # A store copy of the source holding the directory `dir` without the
  # paths in `exclude`, plus the paths in `extra`, all relative to the
  # source root. A change to any other file leaves it untouched.
  mkLocalSource = { name, dir, exclude, extra ? [ ] }:
    let
      keepDir = lib.genAttrs (lib.concatMap parents ([ dir ] ++ extra)) (_: true);
      excluded = rel: lib.any (e: isUnder e rel) exclude;
      added = rel: lib.any (e: isUnder e rel) extra;
    in
    builtins.path {
      inherit name;
      path = srcStr;
      filter = path: type:
        let rel = lib.removePrefix "${srcStr}/" path;
        in added rel
          || (isUnder dir rel && !excluded rel)
          || (type == "directory" && keepDir ? ${rel});
    };

  # The source tree of a unit: a fetched crate, or a view of the local
  # source that the package's override may widen.
  sourceOf = src: override:
    if src ? localSource
    then mkLocalSource (src.localSource // { extra = override.extraSrc or [ ]; })
    else src;
in
{
  # A registry crate: its .crate file, then the tree unpacked from it.
  # resolve adds the file to the store under the path its Cargo.lock
  # checksum dictates, so the download runs only where that did not happen.
  fetchCrate = { pname, version, sha256, url }:
    let crate = fetchurl { name = "${pname}-${version}.crate"; inherit url sha256; };
    in runCommand "rustsrc-${pname}-${version}" { passthru = { inherit crate; }; } ''
      mkdir $out
      tar -xzf ${crate} -C $out --strip-components=1
    '';

  # A view of the local source. It becomes a store path once the unit that
  # uses it knows what its package's override adds.
  localSource = args: { localSource = args; };

  # One rustc invocation. A unit that does not link needs no stdenv: the
  # tool is the builder. One that links needs the C compiler and linker,
  # and the libraries of every overridden package it links.
  compile = node:
    let
      inherit (node) package;
      override = overrideOf package;
      attrs = {
        rustc = rustcBin;
        cargo = cargoBin;
        node = {
          inherit (node) kind crateName targetName edition srcPath metadata rustcArgs tailArgs passL;
          inherit (package) manifestDir workDir local;
          pkg = { inherit (package) name version; };
          src = "${sourceOf node.src override}";
          remapTo = "${package.name}-${package.version}";
          env = package.env // node.env;
          deps = map (dep: { inherit (dep) name; path = "${dep.unit}"; }) node.deps;
          buildScript = if node.buildScript == null then null else "${node.buildScript}";
          overrideEnv = override.env or { };
        };
      };
    in
    if !node.linked && package.override == null then
      derivation
        ({
          inherit (node) name;
          inherit system builder;
          args = [ "compile" ];
          __structuredAttrs = true;
        } // attrs)
    else
      stdenv.mkDerivation ({
        inherit (node) name;
        __structuredAttrs = true;
        strictDeps = true;
        nativeBuildInputs = override.nativeBuildInputs or [ ];
        buildInputs = lib.unique
          (lib.concatMap (key: crateOverrides.${key}.buildInputs or [ ]) node.overrides);
        buildCommand = "${builder} compile";
      } // attrs);

  # One run of a build script. Build scripts compile C and look for
  # libraries, so they always get stdenv and their package's override.
  #
  # A script that depends on another through `links` builds against that
  # package's native library, so it gets the libraries of that package's
  # override too, and of whatever that one depends on in turn. Outside Nix
  # those are simply installed where every compiler finds them.
  runBuildScript = node:
    let
      inherit (node) package;
      override = overrideOf package;
      libraries = lib.unique ((override.buildInputs or [ ])
        ++ lib.concatMap (dep: dep.unit.libraries) node.linksDeps);
    in
    stdenv.mkDerivation {
      inherit (node) name;
      __structuredAttrs = true;
      strictDeps = true;
      rustc = rustcBin;
      cargo = cargoBin;
      node = {
        inherit (node) features debugAssertions;
        inherit (package) manifestDir local;
        pkg = { inherit (package) name version; };
        src = "${sourceOf node.src override}";
        script = "${node.script}";
        env = package.env // node.env;
        linksDeps = map (dep: { inherit (dep) links; path = "${dep.unit}"; }) node.linksDeps;
        overrideEnv = override.env or { };
      };
      nativeBuildInputs = override.nativeBuildInputs or [ ];
      buildInputs = libraries;
      buildCommand = "${builder} run-build-script";
      passthru = { inherit libraries; };
    };
}
