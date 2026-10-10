# buildRustApplication asks cargo for its plan during evaluation, through
# builtins.exec, and turns it into derivations.
#
# evalTool, evalCargo and evalRustc run on the machine that evaluates.
# `target` and `host` are the platform to build for and the machine that
# builds, as rustc names them, and isCross says whether they are two.
{ lib, runCommand, runCommandCC, isDarwin, isElf, patchelf, targetPrefix, rustc, cargo, mkBuilders
, evalTool, evalCargo, evalRustc, target, host, isCross, canExecute, executableExtension }:

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
  # Whether to build the tests of the selected packages and run them. What
  # is built for a platform the build machine cannot run is not tested.
, doCheck ? canExecute
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
    "${evalTool}/bin/rostnix"
    "resolve"
    (builtins.toJSON {
      cargo = "${evalCargo}/bin/cargo";
      rustc = "${evalRustc}/bin/rustc";
      src = srcStr;
      inherit (builtins) storeDir;
      inherit target host;
      cross = isCross;
      inherit cargoRoot packages bins examples features allFeatures noDefaultFeatures profile doCheck rustflags;
      overrideKeys = lib.attrNames crateOverrides;
    })
  ];

  # The builders are told what the graph says of the whole build: the
  # target it was planned for and what the cargo configuration sets. That
  # part of the graph does not depend on them, so this is no circle.
  graph = graphFn (mkBuilders {
    inherit srcStr crateOverrides checkFlags;
    inherit (graph) rustflags configEnv target;
  });

  # A misspelt attribute would otherwise be ignored, and the build would
  # fail later for want of what it was meant to supply.
  overrideAttrs = [ "buildInputs" "nativeBuildInputs" "env" "extraSrc" "optional" ];
  unknownOverrides = lib.concatLists (lib.mapAttrsToList
    (key: entry: map (attr: "crateOverrides.${key}.${attr}")
      (lib.attrNames (removeAttrs entry overrideAttrs)))
    crateOverrides);

  # An entry that no package takes changes nothing, so it is most likely a
  # mistake. It is a warning and not an error because the same key may
  # match on another platform or with other features. An entry marked
  # `optional` is one of a collection, of which a build takes what it has.
  takenKeys = lib.filter (key: key != null)
    (map (package: package.override) (lib.attrValues graph.packages));
  unmatchedOverrides = map (key: "crateOverrides.${key}")
    (lib.filter (key: !lib.elem key takenKeys && !(crateOverrides.${key}.optional or false))
      (lib.attrNames crateOverrides));

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
    assert lib.assertMsg (graph.bins != { } || graph.libs != { })
      "rostnix: the selection builds no binary, no example and no library for use from outside Rust (a cdylib or a staticlib), so there is nothing to install; name what to build with `bins` or `examples`";
    lib.warnIf (unmatchedOverrides != [ ])
      "rostnix: ${lib.concatStringsSep ", " unmatchedOverrides} ${if lib.length unmatchedOverrides == 1 then "names" else "name"} no package of this build and ${if lib.length unmatchedOverrides == 1 then "has" else "have"} no effect; a key is a package name"
      (lib.warnIf (unmatchedSkips != [ ])
        "rostnix: skipTests names ${lib.concatStringsSep ", " unmatchedSkips}, which ${if lib.length unmatchedSkips == 1 then "is no test target" else "are no test targets"} of this build; the test targets are ${lib.concatStringsSep ", " (lib.unique testTargets)}"
        graph);

  # An executable is called after its target, with the ending its platform
  # gives executables: none on Unix, .wasm for WebAssembly.
  binNames = map (name: name + executableExtension) (lib.attrNames checked.bins);

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

  # macOS leaves debug information in the object files and has what was
  # linked from them point at them, which would keep every unit, and
  # through the units the sources and the compiler, alive for as long as
  # the result is. Such a file gets its debug information collected in a
  # .dSYM bundle beside it, where debuggers and backtraces look for it, and
  # loses the pointers.
  collectDebugInfo = lib.optionalString isDarwin ''
    # A tool for the platform's files: under the platform's name where
    # nixpkgs has it so, which is when the build machine is another.
    platformTool() {
      if command -v "${targetPrefix}$1" >/dev/null; then
        echo "${targetPrefix}$1"
      else
        echo "$1"
      fi
    }
    # Without nm nothing would seem to point anywhere.
    command -v "$NM" >/dev/null || { echo "rostnix: no nm to look for debug information with" >&2; exit 1; }
    collectDebugInfo() {
      # A count, not `grep -q`: stdenv sets pipefail, and nm cut short by
      # grep leaving early would read as "no such entries".
      if [ "$("$NM" -a "$1" 2>/dev/null | grep -c ' OSO ' || true)" != 0 ]; then
        chmod u+w "$1"
        "$(platformTool dsymutil)" "$1" -o "$1.dSYM"
        $STRIP -S "$1"
      fi
    }
  '';

  # On Linux and its like, nixpkgs' linker has what it links look for
  # libraries in the lib directory of the derivation that links it. That is
  # the unit's, where nothing is, and naming it would keep the unit alive
  # and through it every other. nixpkgs' own builds end by dropping the
  # directories that hold nothing the file needs, and so does this. A file
  # that is linked statically has no such list.
  dropUnusedRunPaths = lib.optionalString isElf ''
    dropUnusedRunPaths() {
      if patchelf --print-rpath "$1" >/dev/null 2>&1; then
        chmod u+w "$1"
        patchelf --shrink-rpath "$1"
      fi
    }
  '';

  # Copies one executable out of its unit.
  install = name:
    let file = lib.escapeShellArg (name + executableExtension);
    in ''
      mkdir -p $out/bin
      cp ${checked.bins.${name}}/bin/${file} $out/bin/
    '' + lib.optionalString isDarwin ''
      collectDebugInfo $out/bin/${file}
    '' + lib.optionalString isElf ''
      dropUnusedRunPaths $out/bin/${file}
    '';

  # Copies the libraries of one unit, under the names a linker looks for.
  # A dynamic library for macOS records where it is, and what is linked
  # against it looks for it there: that is where it was linked, in its
  # unit, until it is told its place in the result.
  installLibraries = name: ''
    mkdir -p $out/lib
    for file in ${checked.libs.${name}}/install/*; do
      # rustc leaves out a kind of library the platform does not have.
      [ -e "$file" ] || continue
      cp -L "$file" $out/lib/
  '' + lib.optionalString isDarwin ''
      case $file in *.dylib)
        library=$out/lib/''${file##*/}
        chmod u+w "$library"
        "$(platformTool install_name_tool)" -id "$library" "$library"
        collectDebugInfo "$library"
      esac
  '' + lib.optionalString isElf ''
      dropUnusedRunPaths "$out/lib/''${file##*/}"
  '' + ''
    done
  '';
in
(if isDarwin then runCommandCC else runCommand) (if version == null then pname else "${pname}-${version}")
{
  # With one executable, `nix run` needs no flags.
  meta = lib.optionalAttrs (lib.length binNames == 1) { mainProgram = lib.head binNames; } // meta;
  nativeBuildInputs = lib.optional isElf patchelf;
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
    inherit (checked) units bins libs;
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
    ${collectDebugInfo}
    ${dropUnusedRunPaths}
    ${lib.concatMapStrings install (lib.attrNames checked.bins)}
    ${lib.concatMapStrings installLibraries (lib.attrNames checked.libs)}
  ''
