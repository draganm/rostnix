# buildRustApplication asks cargo for its plan during evaluation, through
# builtins.exec, and turns it into derivations.
{ lib, runCommand, rustc, cargo, tool, mkBuilders }:

{ pname
, version ? null
, src
, cargoRoot ? "."
, packages ? [ ]
, bins ? [ ]
, examples ? [ ]
, features ? [ ]
, allFeatures ? false
, noDefaultFeatures ? false
, profile ? "release"
, crateOverrides ? { }
  # Tests are not built yet; these three are accepted and ignored.
, doCheck ? true
, checkFlags ? [ ]
, skipTests ? [ ]
, meta ? { }
}:
let
  exec = builtins.exec or (throw ''
    rostnix runs `rostnix resolve` during evaluation and needs builtins.exec.
    Enable it in one of these ways:
      nix build --option allow-unsafe-native-code-during-evaluation true ...
      NIX_CONFIG="allow-unsafe-native-code-during-evaluation = true" nix build ...
      allow-unsafe-native-code-during-evaluation = true    (in nix.conf)
    A flake's nixConfig cannot enable it.
  '');

  srcStr = "${src}";

  # Nix builds the tool, cargo and rustc before running this, if they are
  # missing.
  graphFn = exec [
    "${tool}/bin/rostnix"
    "resolve"
    (builtins.toJSON {
      cargo = "${cargo}/bin/cargo";
      rustc = "${rustc}/bin/rustc";
      src = srcStr;
      inherit (builtins) storeDir;
      inherit cargoRoot packages bins examples features allFeatures noDefaultFeatures profile;
      overrideKeys = lib.attrNames crateOverrides;
    })
  ];

  graph = graphFn (mkBuilders { inherit srcStr crateOverrides; });

  # A misspelt attribute would otherwise be ignored, and the build would
  # fail later for want of what it was meant to supply.
  overrideAttrs = [ "buildInputs" "nativeBuildInputs" "env" "extraSrc" ];
  unknownOverrides = lib.concatLists (lib.mapAttrsToList
    (key: entry: map (attr: "crateOverrides.${key}.${attr}")
      (lib.attrNames (removeAttrs entry overrideAttrs)))
    crateOverrides);

  # An entry that no package takes changes nothing, so it is most likely a
  # mistake. It is a warning and not an error because the same key may
  # match on another platform or with other features.
  takenKeys = lib.filter (key: key != null)
    (map (package: package.override) (lib.attrValues graph.packages));
  unmatchedOverrides = map (key: "crateOverrides.${key}")
    (lib.filter (key: !lib.elem key takenKeys) (lib.attrNames crateOverrides));

  checked =
    assert lib.assertMsg (unknownOverrides == [ ])
      "rostnix: unknown ${lib.concatStringsSep ", " unknownOverrides}; a crateOverrides entry takes ${lib.concatStringsSep ", " overrideAttrs}";
    assert lib.assertMsg (graph.bins != { })
      "rostnix: the selection builds no binary and no example, so there is nothing to install; name what to build with `bins` or `examples`";
    lib.warnIf (unmatchedOverrides != [ ])
      "rostnix: ${lib.concatStringsSep ", " unmatchedOverrides} ${if lib.length unmatchedOverrides == 1 then "names" else "name"} no package of this build and ${if lib.length unmatchedOverrides == 1 then "has" else "have"} no effect; a key is a package name"
      graph;

  binNames = lib.attrNames checked.bins;
in
runCommand (if version == null then pname else "${pname}-${version}")
{
  # With one executable, `nix run` needs no flags.
  meta = lib.optionalAttrs (lib.length binNames == 1) { mainProgram = lib.head binNames; } // meta;
  passthru = {
    inherit rustc cargo;
    graph = checked;
    inherit (checked) units bins;
    # What each unit ran, one file per unit, for comparing with cargo.
    unitRecords = runCommand "${pname}-unit-records" { } ''
      mkdir $out
      ${lib.concatStrings (lib.mapAttrsToList
        (key: unit: "ln -s ${unit}/unit.json $out/${lib.escapeShellArg key}.json\n")
        checked.units)}
    '';
  };
}
  ''
    mkdir -p $out/bin
    ${lib.concatMapStrings
      (name: "cp ${checked.bins.${name}}/bin/${lib.escapeShellArg name} $out/bin/\n")
      binNames}
  ''
