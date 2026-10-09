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
        SrcRef::Registry(key) => format!("sources.{}", quote(key)),
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
        line(format!(
            "  sources.{} = b.fetchCrate {{ pname = {}; version = {}; sha256 = {}; url = {}; }};",
            quote(key),
            quote(&source.pname),
            quote(&source.version),
            quote(&source.sha256),
            quote(&source.url)
        ));
    }
    // A project with no registry dependency still has the set.
    if graph.sources.is_empty() {
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
    line("}".to_string());
    out
}

fn compile(line: &mut impl FnMut(String), key: &str, unit: &CompileUnit) {
    line(format!("  units.{} = b.compile {{", quote(key)));
    line(format!("    name = {};", quote(&unit.name)));
    line(format!("    package = packages.{};", quote(&unit.package)));
    line(format!("    src = {};", src(&unit.src)));
    line(format!("    kind = {};", quote(unit.kind)));
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
    use crate::graph::{PackageNode, Source};

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
                url: "https://static.crates.io/crates/dep/dep-1.0.0+x.crate".into(),
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
            }),
        );
        graph
            .bins
            .insert("app".into(), "app-0.1.0-bin-app-cccccccc".into());
        graph.roots.push("app-0.1.0-bin-app-cccccccc".into());
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
}
"#;
        assert_eq!(to_nix(&sample()), expected);
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
            "let g = ({nix}) {{ fetchCrate = a: a; localSource = a: a; compile = a: a; runBuildScript = a: a; }}; \
             in [ (g.bins == {{ }}) (g.sources == {{ }}) ]"
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
            "[true,true]"
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
