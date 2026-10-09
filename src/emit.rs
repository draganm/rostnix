//! The graph as Nix: one function from the builders to the set of
//! everything to build.

use std::collections::BTreeMap;
use std::fmt::Write;

use crate::graph::{CompileUnit, Graph, RunUnit, SrcRef, UnitNode};

/// A Nix string literal that evaluates to `text`.
pub fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            // Every dollar is escaped, so none can start an interpolation.
            '$' => out.push_str("\\$"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn list(items: &[String]) -> String {
    if items.is_empty() {
        return "[ ]".to_string();
    }
    let quoted: Vec<String> = items.iter().map(|item| quote(item)).collect();
    format!("[ {} ]", quoted.join(" "))
}

fn attrs(map: &BTreeMap<String, String>) -> String {
    if map.is_empty() {
        return "{ }".to_string();
    }
    let pairs: Vec<String> = map
        .iter()
        .map(|(key, value)| format!("{} = {};", quote(key), quote(value)))
        .collect();
    format!("{{ {} }}", pairs.join(" "))
}

fn optional(value: &Option<String>) -> String {
    value.as_deref().map_or("null".to_string(), quote)
}

fn unit_ref(key: &str) -> String {
    format!("units.{}", quote(key))
}

fn src(src: &SrcRef) -> String {
    match src {
        SrcRef::Registry(key) | SrcRef::Git(key) => format!("sources.{}", quote(key)),
        SrcRef::Local { name, dir, exclude } => format!(
            "b.localSource {{ name = {}; dir = {}; exclude = {}; }}",
            quote(name),
            quote(if dir.is_empty() { "." } else { dir }),
            list(exclude)
        ),
    }
}

pub fn to_nix(graph: &Graph) -> String {
    let mut out = String::new();
    // Writing to a String cannot fail.
    let mut line = |text: String| writeln!(out, "{text}").unwrap();

    line("b: rec {".to_string());
    line(format!("  cargoVersion = {};", quote(&graph.cargo_version)));
    line(format!("  host = {};", quote(&graph.host)));

    for (key, source) in &graph.sources {
        // A registry other than crates.io is named, for the message of a
        // download that cannot be made.
        let registry = source
            .registry
            .as_ref()
            .map(|registry| format!(" registry = {};", quote(registry)))
            .unwrap_or_default();
        line(format!(
            "  sources.{} = b.fetchCrate {{ pname = {}; version = {}; sha256 = {}; url = {};{registry} }};",
            quote(key),
            quote(&source.pname),
            quote(&source.version),
            quote(&source.sha256),
            optional(&source.url),
        ));
    }
    for (key, source) in &graph.git_sources {
        line(format!(
            "  sources.{} = b.fetchGit {{ name = {}; url = {}; rev = {}; ref = {}; }};",
            quote(key),
            quote(&source.name),
            quote(&source.url),
            quote(&source.rev),
            optional(&source.git_ref)
        ));
    }
    // A project with no dependency to fetch still has the set.
    if graph.sources.is_empty() && graph.git_sources.is_empty() {
        line("  sources = { };".to_string());
    }

    for (key, pkg) in &graph.packages {
        line(format!("  packages.{} = {{", quote(key)));
        line(format!("    name = {};", quote(&pkg.name)));
        line(format!("    version = {};", quote(&pkg.version)));
        line(format!("    local = {};", pkg.local));
        line(format!("    manifestDir = {};", quote(&pkg.manifest_dir)));
        line(format!("    workDir = {};", quote(&pkg.work_dir)));
        line(format!("    links = {};", optional(&pkg.links)));
        line(format!("    override = {};", optional(&pkg.override_key)));
        line(format!("    env = {};", attrs(&pkg.env)));
        line("  };".to_string());
    }

    for (key, unit) in &graph.units {
        match unit {
            UnitNode::Compile(unit) => compile(&mut line, key, unit),
            UnitNode::Run(unit) => run(&mut line, key, unit),
        }
    }

    for (name, key) in &graph.bins {
        line(format!("  bins.{} = {};", quote(name), unit_ref(key)));
    }
    // A selection with no executable has an empty set, which is what the
    // Nix side reports on.
    if graph.bins.is_empty() {
        line("  bins = { };".to_string());
    }
    let roots: Vec<String> = graph.roots.iter().map(|key| unit_ref(key)).collect();
    line(format!("  roots = [ {} ];", roots.join(" ")));

    for key in &graph.tests {
        line(format!("  tests.{} = {};", quote(key), unit_ref(key)));
    }
    // No tests were asked for, or the selection has none.
    if graph.tests.is_empty() {
        line("  tests = { };".to_string());
    }
    let builds: Vec<String> = graph.test_builds.iter().map(|key| unit_ref(key)).collect();
    line(format!("  testBuilds = [ {} ];", builds.join(" ")));
    line(format!("  buildUnits = {};", list(&graph.build_units)));
    line(format!("  rustflags = {};", list(&graph.rustflags)));
    let variables: Vec<String> = graph
        .config_env
        .iter()
        .map(|env| {
            format!(
                "{{ name = {}; value = {}; force = {}; relative = {}; slash = {}; holdsSource = {}; }}",
                quote(&env.entry.name),
                quote(&env.entry.value),
                env.entry.force,
                optional(&env.entry.relative),
                env.entry.slash,
                env.holds_source
            )
        })
        .collect();
    line(format!("  configEnv = [ {} ];", variables.join(" ")));
    line(format!("  testUnits = {};", list(&graph.test_units)));
    line("}".to_string());
    out
}

fn compile(line: &mut impl FnMut(String), key: &str, unit: &CompileUnit) {
    // A test is compiled like anything else, and then run.
    let builder = if unit.test.is_some() {
        "test"
    } else {
        "compile"
    };
    line(format!("  units.{} = b.{builder} {{", quote(key)));
    line(format!("    name = {};", quote(&unit.name)));
    line(format!("    package = packages.{};", quote(&unit.package)));
    line(format!("    src = {};", src(&unit.src)));
    line(format!("    kind = {};", quote(unit.kind)));
    line(format!("    targetKind = {};", quote(unit.target_kind)));
    line(format!("    crateName = {};", quote(&unit.crate_name)));
    line(format!("    targetName = {};", quote(&unit.target_name)));
    line(format!("    edition = {};", quote(&unit.edition)));
    line(format!("    srcPath = {};", quote(&unit.src_path)));
    line(format!("    metadata = {};", quote(&unit.metadata)));
    line(format!("    linked = {};", unit.linked));
    line(format!("    passL = {};", unit.pass_l));
    line(format!("    rustcArgs = {};", list(&unit.rustc_args)));
    line(format!("    tailArgs = {};", list(&unit.tail_args)));
    line(format!("    env = {};", attrs(&unit.env)));
    let deps: Vec<String> = unit
        .deps
        .iter()
        .map(|(name, key)| format!("{{ name = {}; unit = {}; }}", quote(name), unit_ref(key)))
        .collect();
    line(format!("    deps = [ {} ];", deps.join(" ")));
    line(format!(
        "    buildScript = {};",
        unit.build_script
            .as_deref()
            .map_or("null".to_string(), unit_ref)
    ));
    line(format!("    overrides = {};", list(&unit.overrides)));
    if let Some(test) = &unit.test {
        line(format!("    profileDir = {};", quote(&test.profile_dir)));
        let executables: Vec<String> = test.executables.iter().map(|key| unit_ref(key)).collect();
        line(format!("    executables = [ {} ];", executables.join(" ")));
    }
    line("  };".to_string());
}

fn run(line: &mut impl FnMut(String), key: &str, unit: &RunUnit) {
    line(format!("  units.{} = b.runBuildScript {{", quote(key)));
    line(format!("    name = {};", quote(&unit.name)));
    line(format!("    package = packages.{};", quote(&unit.package)));
    line(format!("    src = {};", src(&unit.src)));
    line(format!("    script = {};", unit_ref(&unit.script)));
    line(format!("    features = {};", list(&unit.features)));
    line(format!("    debugAssertions = {};", unit.debug_assertions));
    line(format!("    env = {};", attrs(&unit.env)));
    let deps: Vec<String> = unit
        .links_deps
        .iter()
        .map(|(links, key)| format!("{{ links = {}; unit = {}; }}", quote(links), unit_ref(key)))
        .collect();
    line(format!("    linksDeps = [ {} ];", deps.join(" ")));
    line("  };".to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{PackageNode, Source, TestInfo};

    #[test]
    fn quote_escapes_what_nix_would_read() {
        assert_eq!(quote("plain"), "\"plain\"");
        assert_eq!(quote("a \"b\" \\ c"), "\"a \\\"b\\\" \\\\ c\"");
        assert_eq!(quote("${x} $y $${z}"), "\"\\${x} \\$y \\$\\${z}\"");
        assert_eq!(quote("line\nnext\ttab"), "\"line\\nnext\\ttab\"");
    }

    // A description is free text: whatever a crate author wrote must come
    // back out of Nix unchanged.
    #[test]
    fn quote_round_trips_through_nix() {
        if std::process::Command::new("nix-instantiate")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("nix-instantiate is not available; skipping");
            return;
        }
        for text in [
            "plain",
            "with \"quotes\" and \\ backslash",
            "${interpolation} and $${double} and $dollar and ''two quotes''",
            "feature=\"std\"",
            "line\nbreak\tand tab",
            "cfg(feature, values(\"a\", \"b\"))",
            "unicode — ü",
        ] {
            let output = std::process::Command::new("nix-instantiate")
                .args(["--eval", "--json", "--expr", &quote(text)])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{text:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let back: String = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(back, text);
        }
    }

    fn sample() -> Graph {
        let mut graph = Graph {
            cargo_version: "1.95.0".into(),
            host: "aarch64-apple-darwin".into(),
            ..Graph::default()
        };
        graph.sources.insert(
            "dep-1.0.0+x".into(),
            Source {
                pname: "dep".into(),
                version: "1.0.0+x".into(),
                sha256: "abc".into(),
                url: Some("https://static.crates.io/crates/dep/dep-1.0.0+x.crate".into()),
                registry: None,
                cargo_src_dir: "/cargo-home/registry/src/index/dep-1.0.0+x".into(),
            },
        );
        graph.packages.insert(
            "dep-1.0.0+x".into(),
            PackageNode {
                name: "dep".into(),
                version: "1.0.0+x".into(),
                local: false,
                manifest_dir: "".into(),
                work_dir: "".into(),
                links: Some("dep".into()),
                override_key: None,
                env: BTreeMap::from([(
                    "CARGO_PKG_DESCRIPTION".to_string(),
                    "costs $5 \"or\" ${less}".to_string(),
                )]),
            },
        );
        graph.units.insert(
            "dep-1.0.0+x-build-script-aaaaaaaa".into(),
            UnitNode::Compile(CompileUnit {
                name: "rustbs-dep-1.0.0+x".into(),
                package: "dep-1.0.0+x".into(),
                src: SrcRef::Registry("dep-1.0.0+x".into()),
                kind: "build-script",
                target_kind: "custom-build",
                crate_name: "build_script_build".into(),
                target_name: "build-script-build".into(),
                edition: "2021".into(),
                src_path: "build.rs".into(),
                metadata: "aaaaaaaaaaaaaaaa".into(),
                linked: true,
                pass_l: false,
                rustc_args: vec![
                    "--crate-type".into(),
                    "bin".into(),
                    "--cfg".into(),
                    "feature=\"std\"".into(),
                ],
                tail_args: vec!["--cap-lints".into(), "allow".into()],
                env: BTreeMap::from([(
                    "CARGO_CRATE_NAME".to_string(),
                    "build_script_build".to_string(),
                )]),
                deps: vec![],
                build_script: None,
                overrides: vec![],
                test: None,
            }),
        );
        graph.units.insert(
            "dep-1.0.0+x-run-build-script-bbbbbbbb".into(),
            UnitNode::Run(RunUnit {
                name: "rustbsrun-dep-1.0.0+x".into(),
                package: "dep-1.0.0+x".into(),
                src: SrcRef::Registry("dep-1.0.0+x".into()),
                script: "dep-1.0.0+x-build-script-aaaaaaaa".into(),
                features: vec!["std".into()],
                debug_assertions: false,
                env: BTreeMap::from([("OPT_LEVEL".to_string(), "3".to_string())]),
                links_deps: vec![],
            }),
        );
        graph.units.insert(
            "app-0.1.0-bin-app-cccccccc".into(),
            UnitNode::Compile(CompileUnit {
                name: "rustbin-app".into(),
                package: "dep-1.0.0+x".into(),
                src: SrcRef::Local {
                    name: "rustsrc-app-0.1.0".into(),
                    dir: "".into(),
                    exclude: vec!["tests".into()],
                },
                kind: "bin",
                target_kind: "bin",
                crate_name: "app".into(),
                target_name: "app".into(),
                edition: "2021".into(),
                src_path: "src/main.rs".into(),
                metadata: "cccccccccccccccc".into(),
                linked: true,
                pass_l: true,
                rustc_args: vec![],
                tail_args: vec![],
                env: BTreeMap::new(),
                deps: vec![("dep".into(), "dep-1.0.0+x-build-script-aaaaaaaa".into())],
                build_script: Some("dep-1.0.0+x-run-build-script-bbbbbbbb".into()),
                overrides: vec!["dep".into()],
                test: None,
            }),
        );
        graph
            .bins
            .insert("app".into(), "app-0.1.0-bin-app-cccccccc".into());
        graph.roots.push("app-0.1.0-bin-app-cccccccc".into());
        graph.build_units = graph.units.keys().cloned().collect();
        graph
    }

    #[test]
    fn golden() {
        let expected = r#"b: rec {
  cargoVersion = "1.95.0";
  host = "aarch64-apple-darwin";
  sources."dep-1.0.0+x" = b.fetchCrate { pname = "dep"; version = "1.0.0+x"; sha256 = "abc"; url = "https://static.crates.io/crates/dep/dep-1.0.0+x.crate"; };
  packages."dep-1.0.0+x" = {
    name = "dep";
    version = "1.0.0+x";
    local = false;
    manifestDir = "";
    workDir = "";
    links = "dep";
    override = null;
    env = { "CARGO_PKG_DESCRIPTION" = "costs \$5 \"or\" \${less}"; };
  };
  units."app-0.1.0-bin-app-cccccccc" = b.compile {
    name = "rustbin-app";
    package = packages."dep-1.0.0+x";
    src = b.localSource { name = "rustsrc-app-0.1.0"; dir = "."; exclude = [ "tests" ]; };
    kind = "bin";
    targetKind = "bin";
    crateName = "app";
    targetName = "app";
    edition = "2021";
    srcPath = "src/main.rs";
    metadata = "cccccccccccccccc";
    linked = true;
    passL = true;
    rustcArgs = [ ];
    tailArgs = [ ];
    env = { };
    deps = [ { name = "dep"; unit = units."dep-1.0.0+x-build-script-aaaaaaaa"; } ];
    buildScript = units."dep-1.0.0+x-run-build-script-bbbbbbbb";
    overrides = [ "dep" ];
  };
  units."dep-1.0.0+x-build-script-aaaaaaaa" = b.compile {
    name = "rustbs-dep-1.0.0+x";
    package = packages."dep-1.0.0+x";
    src = sources."dep-1.0.0+x";
    kind = "build-script";
    targetKind = "custom-build";
    crateName = "build_script_build";
    targetName = "build-script-build";
    edition = "2021";
    srcPath = "build.rs";
    metadata = "aaaaaaaaaaaaaaaa";
    linked = true;
    passL = false;
    rustcArgs = [ "--crate-type" "bin" "--cfg" "feature=\"std\"" ];
    tailArgs = [ "--cap-lints" "allow" ];
    env = { "CARGO_CRATE_NAME" = "build_script_build"; };
    deps = [  ];
    buildScript = null;
    overrides = [ ];
  };
  units."dep-1.0.0+x-run-build-script-bbbbbbbb" = b.runBuildScript {
    name = "rustbsrun-dep-1.0.0+x";
    package = packages."dep-1.0.0+x";
    src = sources."dep-1.0.0+x";
    script = units."dep-1.0.0+x-build-script-aaaaaaaa";
    features = [ "std" ];
    debugAssertions = false;
    env = { "OPT_LEVEL" = "3"; };
    linksDeps = [  ];
  };
  bins."app" = units."app-0.1.0-bin-app-cccccccc";
  roots = [ units."app-0.1.0-bin-app-cccccccc" ];
  tests = { };
  testBuilds = [  ];
  buildUnits = [ "app-0.1.0-bin-app-cccccccc" "dep-1.0.0+x-build-script-aaaaaaaa" "dep-1.0.0+x-run-build-script-bbbbbbbb" ];
  rustflags = [ ];
  configEnv = [  ];
  testUnits = [ ];
}
"#;
        assert_eq!(to_nix(&sample()), expected);
    }

    #[test]
    fn the_cargo_configuration_is_part_of_the_graph() {
        use crate::config::EnvEntry;
        use crate::graph::ConfigEnv;
        let mut graph = sample();
        graph.rustflags = vec!["--cfg".into(), "feature=\"x\"".into()];
        graph.config_env = vec![
            ConfigEnv {
                entry: EnvEntry {
                    name: "PLAIN".into(),
                    value: "costs $5".into(),
                    force: true,
                    relative: None,
                    slash: false,
                },
                holds_source: false,
            },
            ConfigEnv {
                entry: EnvEntry {
                    name: "ROOT".into(),
                    value: String::new(),
                    force: false,
                    relative: Some(String::new()),
                    slash: true,
                },
                holds_source: true,
            },
        ];
        let nix = to_nix(&graph);
        assert!(
            nix.contains("\n  rustflags = [ \"--cfg\" \"feature=\\\"x\\\"\" ];\n"),
            "{nix}"
        );
        assert!(
            nix.contains(
                "\n  configEnv = [ { name = \"PLAIN\"; value = \"costs \\$5\"; force = true; relative = null; slash = false; holdsSource = false; } \
                 { name = \"ROOT\"; value = \"\"; force = false; relative = \"\"; slash = true; holdsSource = true; } ];\n"
            ),
            "{nix}"
        );
    }

    // A test is a unit like any other, built by another builder, which is
    // told what to put beside the test before running it.
    #[test]
    fn a_test_is_a_unit_with_what_it_runs_beside() {
        let mut graph = sample();
        let UnitNode::Compile(app) = &graph.units["app-0.1.0-bin-app-cccccccc"] else {
            panic!()
        };
        let mut test = app.clone();
        test.name = "rusttest-cli".into();
        test.kind = "test";
        test.target_kind = "test";
        test.test = Some(TestInfo {
            profile_dir: "release".into(),
            executables: vec!["app-0.1.0-bin-app-cccccccc".into()],
        });
        graph.units.insert(
            "app-0.1.0-test-cli-dddddddd".into(),
            UnitNode::Compile(test),
        );
        graph.tests = vec!["app-0.1.0-test-cli-dddddddd".into()];
        graph.test_units = graph.tests.clone();
        let nix = to_nix(&graph);
        for expected in [
            "  units.\"app-0.1.0-test-cli-dddddddd\" = b.test {\n    name = \"rusttest-cli\";\n",
            "    kind = \"test\";\n    targetKind = \"test\";\n",
            "    overrides = [ \"dep\" ];\n    profileDir = \"release\";\n    \
             executables = [ units.\"app-0.1.0-bin-app-cccccccc\" ];\n  };\n",
            "  tests.\"app-0.1.0-test-cli-dddddddd\" = units.\"app-0.1.0-test-cli-dddddddd\";\n",
            "\n  testUnits = [ \"app-0.1.0-test-cli-dddddddd\" ];\n",
        ] {
            assert!(nix.contains(expected), "no {expected:?} in:\n{nix}");
        }
        assert!(!nix.contains("tests = { };"), "{nix}");
        // What is not a test is told nothing of the kind.
        assert_eq!(nix.matches("profileDir").count(), 1);
    }

    #[test]
    fn git_repositories_and_other_registries_are_sources_too() {
        use crate::graph::GitSource;
        let mut graph = sample();
        graph.git_sources.insert(
            "git-serde-a866b336f14a".into(),
            GitSource {
                name: "rustsrc-serde-a866b33".into(),
                url: "https://github.com/serde-rs/serde".into(),
                rev: "a866b336f14aa57a07f0d0be9f8762746e64ecb4".into(),
                git_ref: Some("refs/tags/v1.0.228".into()),
            },
        );
        let private = graph.sources.get_mut("dep-1.0.0+x").unwrap();
        private.url = None;
        private.registry = Some("sparse+https://crates.example.com/index/".into());
        let nix = to_nix(&graph);
        assert!(
            nix.contains(
                "  sources.\"git-serde-a866b336f14a\" = b.fetchGit { name = \"rustsrc-serde-a866b33\"; \
                 url = \"https://github.com/serde-rs/serde\"; \
                 rev = \"a866b336f14aa57a07f0d0be9f8762746e64ecb4\"; ref = \"refs/tags/v1.0.228\"; };\n"
            ),
            "{nix}"
        );
        assert!(
            nix.contains(
                "sha256 = \"abc\"; url = null; registry = \"sparse+https://crates.example.com/index/\"; };\n"
            ),
            "{nix}"
        );
        // A project with git dependencies only has no empty set beside them.
        graph.sources.clear();
        assert!(!to_nix(&graph).contains("sources = { };"));
    }

    // A library-only selection has no executables, and a project without
    // registry dependencies no sources. Both sets must still be there to be
    // asked about.
    #[test]
    fn empty_sets_are_emitted() {
        let mut graph = sample();
        graph.bins.clear();
        graph.sources.clear();
        let nix = to_nix(&graph);
        assert!(nix.contains("\n  bins = { };\n"), "{nix}");
        assert!(nix.contains("\n  sources = { };\n"), "{nix}");
        assert!(!to_nix(&sample()).contains("= { };\n  roots"));

        if std::process::Command::new("nix-instantiate")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let expr = format!(
            "let g = ({nix}) {{ fetchCrate = a: a; localSource = a: a; compile = a: a; runBuildScript = a: a; test = a: a; }}; \
             in [ (g.bins == {{ }}) (g.sources == {{ }}) (g.tests == {{ }}) ]"
        );
        let output = std::process::Command::new("nix-instantiate")
            .args(["--eval", "--strict", "--json", "--expr", &expr])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "[true,true,true]"
        );
    }

    // The emitted text must be Nix that evaluates, with every reference
    // between nodes resolving.
    #[test]
    fn emitted_graph_evaluates() {
        if std::process::Command::new("nix-instantiate")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("nix-instantiate is not available; skipping");
            return;
        }
        let expr = format!(
            "let g = ({}) {{ fetchCrate = a: a; localSource = a: a; compile = a: a; runBuildScript = a: a; }}; \
             in [ g.bins.app.buildScript.script.package.env.CARGO_PKG_DESCRIPTION (builtins.length g.roots) g.bins.app.src.dir ]",
            to_nix(&sample())
        );
        let output = std::process::Command::new("nix-instantiate")
            .args(["--eval", "--strict", "--json", "--expr", &expr])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            value,
            serde_json::json!(["costs $5 \"or\" ${less}", 1, "."])
        );
    }
}
