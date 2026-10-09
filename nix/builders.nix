# The builder functions the generated graph calls. Each defines how one
# kind of node becomes a derivation. The wiring between nodes is in the
# graph `rostnix resolve` prints.
{ lib, fetchurl, runCommand, stdenv, tool, rustc, cargo, system }:

# Per-application settings, and what the project's cargo configuration
# says: the flags every rustc gets and the variables of its [env] table.
{ srcStr, crateOverrides ? { }, checkFlags ? [ ], rustflags ? [ ], configEnv ? [ ] }:
let
  builder = "${tool}/bin/rostnix";
  rustcBin = "${rustc}/bin/rustc";
  cargoBin = "${cargo}/bin/cargo";

  # The crateOverrides entry of a package, if it has one.
  overrideOf = package:
    if package.override == null then { } else crateOverrides.${package.override};

  # An override's environment with every value a string, as nixpkgs takes
  # `env`: 1 and true are "1".
  envOf = override: lib.mapAttrs (_: toString) (override.env or { });

  # "./proto/" and "proto" name the same directory.
  cleanPath = path: lib.removeSuffix "/" (lib.removePrefix "./" path);

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

  # A relative variable of the cargo configuration names a path of the
  # source. One that holds a local package names the source itself; the
  # others name data: a configuration file, a directory of assets.
  relativeEnv = lib.filter (variable: variable.relative != null) configEnv;
  dataEnv = lib.filter (variable: !variable.holdsSource) relativeEnv;

  # A store copy of the source that holds one such path and nothing else,
  # for the units that have no view of the source to find it in.
  dataCopies = lib.listToAttrs (map
    (variable: lib.nameValuePair variable.name (mkLocalSource {
      name = lib.strings.sanitizeDerivationName "rustenv-${variable.name}";
      dir = variable.relative;
      exclude = [ ];
    }))
    dataEnv);

  # The source tree of a unit: a fetched crate, or a view of the local
  # source. The package's override may widen the view, and the data a
  # relative variable names is part of every view.
  sourceOf = src: override:
    if src ? localSource
    then
      mkLocalSource (src.localSource // {
        extra = map cleanPath (override.extraSrc or [ ]) ++ map (variable: variable.relative) dataEnv;
      })
    else src;

  # The [env] variables as a unit of a package gets them. A plain value
  # goes to every unit. A unit of a local package finds a relative path in
  # its own tree, so it is told the path and no value. Any other unit is
  # given the copy that holds the path, unless the path is the source
  # itself: a crate from elsewhere must not be rebuilt on every edit.
  configEnvOf = package: lib.concatMap
    (variable:
      if variable.relative == null || package.local
      then [{ inherit (variable) name value force relative slash; }]
      else if variable.holdsSource then [ ]
      else [{
        inherit (variable) name force relative slash;
        value = "${dataCopies.${variable.name}}/${variable.relative}${lib.optionalString variable.slash "/"}";
      }])
    configEnv;
  withheldEnvOf = package:
    lib.optionals (!package.local)
      (map (variable: variable.name) (lib.filter (variable: variable.holdsSource) relativeEnv));
  # What the tool is told about a unit that rustc compiles.
  compileNode = node: override:
    let inherit (node) package;
    in {
      inherit (node) kind targetKind crateName targetName edition srcPath metadata rustcArgs tailArgs passL;
      inherit (package) manifestDir workDir local;
      pkg = { inherit (package) name version; };
      src = "${sourceOf node.src override}";
      remapTo = "${package.name}-${package.version}";
      env = package.env // node.env;
      deps = map (dep: { inherit (dep) name; path = "${dep.unit}"; }) node.deps;
      buildScript = if node.buildScript == null then null else "${node.buildScript}";
      overrideEnv = envOf override;
      inherit rustflags;
      configEnv = configEnvOf package;
      withheldEnv = withheldEnvOf package;
    };

  # The libraries of every overridden package a unit links.
  linkedLibraries = node: lib.unique
    (lib.concatMap (key: crateOverrides.${key}.buildInputs or [ ]) node.overrides);
in
{
  # A registry crate: its .crate file, then the tree unpacked from it.
  # resolve adds the file to the store under the path its Cargo.lock
  # checksum dictates, so the download runs only where that did not happen.
  #
  # A registry other than crates.io may give no address that works without
  # the caller's token. Then there is nothing to download with, and the
  # derivation that stands for the file can only say so.
  fetchCrate = { pname, version, sha256, url, registry ? null }:
    let
      name = "${pname}-${version}.crate";
      crate =
        if url != null then fetchurl { inherit name url sha256; }
        else
          runCommand name
            {
              outputHashMode = "flat";
              outputHashAlgo = "sha256";
              outputHash = sha256;
              inherit registry;
            } ''
            echo "rostnix: ${pname} ${version} comes from the registry $registry, which names no address to download it from without credentials." >&2
            echo "The file is put into the Nix store where the project is evaluated. Build on that machine, or get this path from a substituter." >&2
            exit 1
          '';
    in runCommand "rustsrc-${pname}-${version}" { passthru = { inherit crate; }; } ''
      mkdir $out
      tar -xzf ${crate} -C $out --strip-components=1
    '';

  # A git repository at the revision Cargo.lock names, fetched during
  # evaluation by the git of whoever evaluates, with their credentials.
  # Cargo checks submodules out, so they are fetched too.
  fetchGit = { name, url, rev }:
    builtins.fetchGit { inherit name url rev; submodules = true; shallow = true; };

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
        node = compileNode node override;
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
        buildInputs = linkedLibraries node;
        buildCommand = "${builder} compile";
      } // attrs);

  # One test executable, compiled and run. Both happen in a writable copy
  # of the source, with the binaries and examples the test finds beside
  # itself where cargo would have put them. Tests start programs and look
  # for tools, so the derivation gets stdenv, and the tools and the
  # environment of its package's override.
  test = node:
    let
      inherit (node) package;
      override = overrideOf package;
    in
    stdenv.mkDerivation {
      inherit (node) name;
      __structuredAttrs = true;
      strictDeps = true;
      rustc = rustcBin;
      cargo = cargoBin;
      node = compileNode node override // {
        inherit (node) profileDir;
        executables = map (unit: "${unit}") node.executables;
        args = checkFlags;
      };
      nativeBuildInputs = override.nativeBuildInputs or [ ];
      buildInputs = linkedLibraries node;
      buildCommand = "${builder} test";
      passthru = {
        inherit (node) targetName targetKind;
        packageName = package.name;
      };
    };

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
        overrideEnv = envOf override;
        inherit rustflags;
        configEnv = configEnvOf package;
        withheldEnv = withheldEnvOf package;
      };
      nativeBuildInputs = override.nativeBuildInputs or [ ];
      buildInputs = libraries;
      buildCommand = "${builder} run-build-script";
      passthru = { inherit libraries; };
    };
}
