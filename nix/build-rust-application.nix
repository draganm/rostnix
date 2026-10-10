# buildRustApplication asks cargo for its plan during evaluation, through
# builtins.exec, and turns it into derivations.
{ lib, runCommand, runCommandCC, isDarwin, rustc, cargo, tool, mkBuilders }:

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
  # Whether to build the tests of the selected packages and run them.
, doCheck ? true
  # Arguments for every test executable.
, checkFlags ? [ ]
  # Names of test targets that are not run.
, skipTests ? [ ]
  # The flags every rustc gets. null means those of the project's cargo
  # configuration; a list takes their place.
, rustflags ? null
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
      inherit cargoRoot packages bins examples features allFeatures noDefaultFeatures profile doCheck rustflags;
      overrideKeys = lib.attrNames crateOverrides;
    })
  ];

  # The builders are told what the graph says of the cargo configuration.
  # That part of the graph does not depend on them, so this is no circle.
  graph = graphFn (mkBuilders {
    inherit srcStr crateOverrides checkFlags;
    inherit (graph) rustflags configEnv;
  });

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

  # extraSrc widens what a local package sees of the source tree. A crate
  # from a registry has its own source, so the entry would do nothing.
  foreignExtraSrc = map (package: "crateOverrides.${package.override}.extraSrc")
    (lib.filter
      (package: package.override != null && !package.local
        && (crateOverrides.${package.override}.extraSrc or [ ]) != [ ])
      (lib.attrValues graph.packages));

  # A skipTests entry that names no test target skips nothing, and the test
  # it was meant for runs.
  testTargets = map (test: test.targetName) (lib.attrValues graph.tests);
  unmatchedSkips = lib.optionals doCheck
    (lib.filter (name: !lib.elem name testTargets) skipTests);

  checked =
    assert lib.assertMsg (unknownOverrides == [ ])
      "rostnix: unknown ${lib.concatStringsSep ", " unknownOverrides}; a crateOverrides entry takes ${lib.concatStringsSep ", " overrideAttrs}";
    assert lib.assertMsg (foreignExtraSrc == [ ])
      "rostnix: ${lib.concatStringsSep ", " foreignExtraSrc} is set for a package that is not part of the source tree; extraSrc adds files of the source tree to a local package";
    assert lib.assertMsg (graph.bins != { })
      "rostnix: the selection builds no binary and no example, so there is nothing to install; name what to build with `bins` or `examples`";
    lib.warnIf (unmatchedOverrides != [ ])
      "rostnix: ${lib.concatStringsSep ", " unmatchedOverrides} ${if lib.length unmatchedOverrides == 1 then "names" else "name"} no package of this build and ${if lib.length unmatchedOverrides == 1 then "has" else "have"} no effect; a key is a package name"
      (lib.warnIf (unmatchedSkips != [ ])
        "rostnix: skipTests names ${lib.concatStringsSep ", " unmatchedSkips}, which ${if lib.length unmatchedSkips == 1 then "is no test target" else "are no test targets"} of this build; the test targets are ${lib.concatStringsSep ", " (lib.unique testTargets)}"
        graph);

  binNames = lib.attrNames checked.bins;

  # The tests the application waits for: every one that is not skipped.
  testRuns = lib.filterAttrs (_: test: !lib.elem test.targetName skipTests) checked.tests;
  skipped = lib.attrNames (removeAttrs checked.tests (lib.attrNames testRuns));

  # What was run, one file per record, for comparing with cargo.
  records = name: files: runCommand "${pname}-${name}" { } ''
    mkdir $out
    ${lib.concatStrings (lib.mapAttrsToList
      (key: file: "ln -s ${file} $out/${lib.escapeShellArg key}.json\n")
      files)}
  '';
  recordsOf = keys: lib.genAttrs keys (key: "${checked.units.${key}}/unit.json");

  # Copies one executable out of its unit. macOS leaves debug information
  # in the object files and has the executable point at them, which would
  # keep every unit, and through the units the sources and the compiler,
  # alive for as long as the result is. Such an executable gets its debug
  # information collected in a .dSYM bundle beside it, where debuggers and
  # backtraces look for it, and loses the pointers.
  install = name:
    let file = "$out/bin/${lib.escapeShellArg name}";
    in ''
      cp ${checked.bins.${name}}/bin/${lib.escapeShellArg name} $out/bin/
    '' + lib.optionalString isDarwin ''
      # A count, not `grep -q`: stdenv sets pipefail, and nm cut short by
      # grep leaving early would read as "no such entries".
      if [ "$(nm -a ${file} 2>/dev/null | grep -c ' OSO ' || true)" != 0 ]; then
        chmod u+w ${file}
        dsymutil ${file} -o ${file}.dSYM
        $STRIP -S ${file}
      fi
    '';
in
(if isDarwin then runCommandCC else runCommand) (if version == null then pname else "${pname}-${version}")
{
  # With one executable, `nix run` needs no flags.
  meta = lib.optionalAttrs (lib.length binNames == 1) { mainProgram = lib.head binNames; } // meta;
  # The application is built only when its tests pass and what `cargo test`
  # builds beside them, the examples, compiles. Both are inputs and leave
  # nothing in the result, so the result does not refer to them.
  testRuns = lib.attrValues testRuns;
  inherit (checked) testBuilds;
  passthru = {
    inherit rustc cargo;
    # The source tree cargo planned from.
    src = srcStr;
    graph = checked;
    inherit (checked) units bins;
    # The tests that are run, each under its unit's key.
    tests = testRuns;
    # What `cargo build` would run, and what `cargo test` would: the units
    # of each plan, and with the second how each test was run. A skipped
    # test is neither compiled nor run, so it leaves no record.
    unitRecords = records "unit-records" (recordsOf checked.buildUnits);
    testUnitRecords = records "test-unit-records"
      (recordsOf (lib.subtractLists skipped checked.testUnits)
        // lib.mapAttrs' (key: test: lib.nameValuePair "${key}-run" "${test}/run.json") testRuns);
  };
}
  ''
    mkdir -p $out/bin
    ${lib.concatMapStrings install binNames}
  ''
