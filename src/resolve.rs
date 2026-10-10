//! The evaluation-time pipeline: ask cargo for its plan, put the crates in
//! the store, print the graph.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::cargohome::Cargo;
use crate::config;
use crate::graph::{self, Graph, Inputs};
use crate::lockfile::Checksums;
use crate::metadata::Metadata;
use crate::seed::{self, Crate};
use crate::storepath::sanitize_name;
use crate::unitgraph::UnitGraph;
use crate::{emit, Result};

/// What `buildRustApplication` asks for.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    pub cargo: String,
    pub rustc: String,
    pub src: String,
    pub store_dir: String,
    pub cargo_root: String,
    pub packages: Vec<String>,
    pub bins: Vec<String>,
    pub examples: Vec<String>,
    pub features: Vec<String>,
    pub all_features: bool,
    pub no_default_features: bool,
    pub profile: String,
    pub override_keys: Vec<String>,
    /// Whether to plan the tests too.
    pub do_check: bool,
    /// The flags every rustc gets, in place of those of the cargo
    /// configuration, when the caller names them.
    #[serde(default)]
    pub rustflags: Option<Vec<String>>,
    /// The triple of the platform to build for, and that of the machine
    /// that builds. Both are the triple of the rustc that plans when the
    /// caller names neither.
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    /// Whether the two are different platforms to the caller. They can be
    /// and have one triple: nixpkgs' static and LLVM package sets are built
    /// with other C compilers than the machine's own.
    #[serde(default)]
    pub cross: bool,
}

impl Request {
    /// The feature flags, which `cargo build` and `cargo metadata` share.
    fn feature_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if !self.features.is_empty() {
            args.extend(["--features".to_string(), self.features.join(",")]);
        }
        if self.all_features {
            args.push("--all-features".to_string());
        }
        if self.no_default_features {
            args.push("--no-default-features".to_string());
        }
        args
    }

    /// What `cargo test` is asked to plan: the tests of the selected
    /// packages. `bins` and `examples` narrow what is installed, not what
    /// is tested.
    fn test_graph_args(&self, target: Option<&str>) -> Vec<String> {
        let mut args: Vec<String> = [
            "test",
            "--no-run",
            "--unit-graph",
            "-Z",
            "unstable-options",
            "--locked",
            "--profile",
            &self.profile,
        ]
        .map(String::from)
        .to_vec();
        for package in &self.packages {
            args.extend(["--package".to_string(), package.clone()]);
        }
        args.extend(self.feature_args());
        args.extend(target_args(target));
        args
    }

    fn unit_graph_args(&self, target: Option<&str>) -> Vec<String> {
        let mut args: Vec<String> = [
            "build",
            "--unit-graph",
            "-Z",
            "unstable-options",
            "--locked",
            "--profile",
            &self.profile,
        ]
        .map(String::from)
        .to_vec();
        for (flag, values) in [
            ("--package", &self.packages),
            ("--bin", &self.bins),
            ("--example", &self.examples),
        ] {
            for value in values {
                args.extend([flag.to_string(), value.clone()]);
            }
        }
        args.extend(self.feature_args());
        args.extend(target_args(target));
        args
    }

    /// `platforms` are the triples something is built for: the machine
    /// cargo runs on, for build scripts and proc macros, and the target
    /// when that is another.
    fn metadata_args(&self, platforms: &[&str]) -> Vec<String> {
        let mut args: Vec<String> = ["metadata", "--format-version", "1", "--locked"]
            .map(String::from)
            .to_vec();
        for platform in platforms {
            args.extend(["--filter-platform".to_string(), platform.to_string()]);
        }
        args.extend(self.feature_args());
        args
    }
}

fn target_args(target: Option<&str>) -> Vec<String> {
    target
        .map(|triple| vec!["--target".to_string(), triple.to_string()])
        .unwrap_or_default()
}

/// The triple to give cargo as `--target`, if any. Cargo plans for the
/// machine it runs on unless told otherwise, so it is told whenever the
/// platform to build for is another: the caller cross-compiles, or
/// evaluates on a machine of another kind than the one that builds. Told a
/// target, cargo also says of each unit which side it is for, which the
/// caller needs whenever its two platforms differ, in triple or not.
///
/// The triples are compared as text: the caller's are nixpkgs' names and
/// the planning machine's is rustc's own. Should the two ever name one
/// machine differently, cargo is told a target it would not have needed,
/// and builds the same.
fn planned_target<'a>(
    target: &'a str,
    build_host: &str,
    planning_host: &str,
    cross: bool,
) -> Option<&'a str> {
    (cross || target != build_host || target != planning_host).then_some(target)
}

/// What follows an error that the tests alone cause.
const TESTS_ONLY: &str = "this concerns the tests only: what is installed can be planned without them. Set doCheck = false to build without tests";

/// `"."`, `"./a/"` and the like as a path relative to the source root, with
/// `""` for the root.
fn normalize_dir(dir: &str) -> String {
    dir.split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/")
}

/// Resolves the request given as JSON and returns the graph as Nix.
pub fn run(request: &str) -> Result<String> {
    let request: Request = serde_json::from_str(request)
        .map_err(|err| format!("the resolve request is not what this version expects: {err}"))?;
    let src = request.src.trim_end_matches('/');
    let cargo_root = normalize_dir(&request.cargo_root);
    let workspace: PathBuf = if cargo_root.is_empty() {
        src.into()
    } else {
        Path::new(src).join(&cargo_root)
    };
    if !workspace.join("Cargo.toml").exists() {
        return Err(format!(
            "there is no Cargo.toml in {}; set cargoRoot to the directory that holds it",
            workspace.display()
        )
        .into());
    }
    let lock_file = workspace.join("Cargo.lock");
    let lock = fs::read_to_string(&lock_file).map_err(|err| {
        format!(
            "reading {}: {err}; commit a Cargo.lock, it is where the crate hashes come from",
            lock_file.display()
        )
    })?;
    let checksums = Checksums::parse(&lock)?;

    let cargo = Cargo::new(&request.cargo, &request.rustc)?;
    // The machine cargo runs on now, the machine that will build, and the
    // platform the result is for.
    let planning_host = cargo.host()?;
    let host = request
        .host
        .clone()
        .unwrap_or_else(|| planning_host.clone());
    let target_triple = request.target.clone().unwrap_or_else(|| host.clone());
    let target = planned_target(&target_triple, &host, &planning_host, request.cross);
    let cargo_version = cargo.version(&workspace)?;

    let plan = |args: &[String]| -> Result<UnitGraph> {
        let units: UnitGraph =
            serde_json::from_slice(&cargo.output(&workspace, args)?).map_err(|err| {
                format!(
                    "cargo {cargo_version} printed a unit graph this version cannot read: {err}"
                )
            })?;
        if units.version != 1 {
            return Err(format!("cargo {cargo_version} prints version {} of the unit graph; this version reads version 1", units.version).into());
        }
        Ok(units)
    };
    let units = plan(&request.unit_graph_args(target))?;
    let test_units = request
        .do_check
        .then(|| plan(&request.test_graph_args(target)))
        .transpose()
        .map_err(|err| format!("{err}\n{TESTS_ONLY}"))?;
    let mut platforms = vec![planning_host.as_str()];
    if target_triple != planning_host {
        platforms.push(&target_triple);
    }
    let metadata: Metadata =
        serde_json::from_slice(&cargo.output(&workspace, &request.metadata_args(&platforms))?)
            .map_err(|err| {
                format!("cargo {cargo_version} printed metadata this version cannot read: {err}")
            })?;

    // What the project's cargo configuration says about how to build.
    // Cargo merges its configuration files; only the project's own can say
    // anything of the kind, since the private cargo home says none of it.
    let configured = |what: &[&str]| -> Result<Vec<u8>> {
        let mut args: Vec<String> = ["-Z", "unstable-options", "config", "get"]
            .map(String::from)
            .to_vec();
        args.extend(what.iter().map(|arg| arg.to_string()));
        cargo.output_quietly(&workspace, &args)
    };
    let cargo_config: serde_json::Value =
        serde_json::from_slice(&configured(&["--format", "json"])?).map_err(|err| {
            format!("cargo {cargo_version} printed a configuration this version cannot read: {err}")
        })?;
    // The flags are those of the platform to build for. With `--target`
    // cargo gives them to what is built for it and to nothing else.
    let (configured_flags, cfgs) = if config::has_cfg_tables(&cargo_config) {
        config::settled_rustflags(&cargo_config, &target_triple, &|flags| {
            cargo.print_cfg(flags, target)
        })?
    } else {
        (
            config::rustflags(&cargo_config, &target_triple, None),
            Vec::new(),
        )
    };
    let rustflags = request.rustflags.clone().unwrap_or(configured_flags);
    let unapplied = config::unapplied(&cargo_config, &target_triple, &cfgs);
    if !unapplied.is_empty() {
        eprintln!(
            "rostnix: warning: the cargo configuration sets {}, which is not applied: rostnix links with the C compiler of its nixpkgs and runs rustc and tests itself",
            unapplied.join(", ")
        );
    }
    // Which file sets a relative variable decides what it is relative to.
    let has_relative = cargo_config
        .get("env")
        .and_then(|env| env.as_object())
        .is_some_and(|env| env.values().any(|value| value.get("relative").is_some()));
    let origins = if has_relative {
        config::origins(&String::from_utf8_lossy(&configured(&[
            "--show-origin",
            "env",
        ])?))
    } else {
        Default::default()
    };
    let config_env = config::env(&cargo_config, &origins, src, &workspace.to_string_lossy());

    let build = |test_units: Option<&UnitGraph>| {
        graph::build(&Inputs {
            units: &units,
            test_units,
            metadata: &metadata,
            checksums: &checksums,
            src,
            cargo_root: &cargo_root,
            host: &host,
            target,
            cargo_version: &cargo_version,
            override_keys: &request.override_keys,
            read_manifest: &|path| {
                let text =
                    fs::read_to_string(path).map_err(|err| format!("reading {path}: {err}"))?;
                toml::from_str(&text).map_err(|err| format!("parsing {path}: {err}").into())
            },
            read_source: &|path| fs::read_to_string(path).ok(),
            rustflags: &rustflags,
            config_env: &config_env,
        })
    };
    let graph = match build(test_units.as_ref()) {
        Ok(graph) => graph,
        // What is refused may be something only the tests need: a
        // dev-dependency from git, say. Then the way out is to do without
        // the tests, and the message says so.
        Err(err) if test_units.is_some() && build(None).is_ok() => {
            return Err(format!("{err}\n{TESTS_ONLY}").into());
        }
        Err(err) => return Err(err),
    };

    // The crates must be added while the private cargo home still exists:
    // their paths go through it.
    seed::seed(&request.store_dir, &crates(&graph))?;
    drop(cargo);
    Ok(emit::to_nix(&graph))
}

/// The `.crate` file of each registry package, found beside the directory
/// cargo unpacked it into: `registry/src/<index>/<name>-<version>` has its
/// archive at `registry/cache/<index>/<name>-<version>.crate`.
fn crates(graph: &Graph) -> Vec<Crate> {
    graph
        .sources
        .values()
        .map(|source| {
            let cache_file = match source.cargo_src_dir.rsplit_once("/registry/src/") {
                Some((home, rest)) => format!("{home}/registry/cache/{rest}.crate"),
                None => format!("{}.crate", source.cargo_src_dir),
            };
            Crate {
                name: sanitize_name(&format!("{}-{}.crate", source.pname, source.version)),
                sha256: source.sha256.clone(),
                cache_file: cache_file.into(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(extra: &str) -> Request {
        serde_json::from_str(&format!(
            r#"{{"cargo":"/c","rustc":"/r","src":"/s","storeDir":"/nix/store","cargoRoot":".",
                "packages":[],"bins":[],"examples":[],"features":[],"allFeatures":false,
                "noDefaultFeatures":false,"profile":"release","overrideKeys":[],"doCheck":true{extra}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn default_selection_is_plain_cargo_build() {
        assert_eq!(
            request("").unit_graph_args(None).join(" "),
            "build --unit-graph -Z unstable-options --locked --profile release"
        );
        assert_eq!(
            request("")
                .metadata_args(&["aarch64-apple-darwin"])
                .join(" "),
            "metadata --format-version 1 --locked --filter-platform aarch64-apple-darwin"
        );
    }

    #[test]
    fn selection_and_features_become_cargo_flags() {
        let mut req = request("");
        req.packages = vec!["a".into(), "b".into()];
        req.bins = vec!["tool".into()];
        req.examples = vec!["demo".into()];
        req.features = vec!["x".into(), "dep/y".into()];
        req.no_default_features = true;
        req.profile = "thin".into();
        assert_eq!(
            req.unit_graph_args(None).join(" "),
            "build --unit-graph -Z unstable-options --locked --profile thin --package a --package b \
             --bin tool --example demo --features x,dep/y --no-default-features"
        );
        req.all_features = true;
        assert!(req
            .metadata_args(&["h"])
            .join(" ")
            .ends_with("--features x,dep/y --all-features --no-default-features"));
    }

    // Tests are those of the selected packages, with the features of the
    // build. Naming a binary to install does not narrow them.
    #[test]
    fn tests_are_planned_for_the_selected_packages() {
        let mut req = request("");
        assert_eq!(
            req.test_graph_args(None).join(" "),
            "test --no-run --unit-graph -Z unstable-options --locked --profile release"
        );
        req.packages = vec!["a".into()];
        req.bins = vec!["tool".into()];
        req.examples = vec!["demo".into()];
        req.features = vec!["x".into()];
        req.profile = "thin".into();
        assert_eq!(
            req.test_graph_args(None).join(" "),
            "test --no-run --unit-graph -Z unstable-options --locked --profile thin --package a --features x"
        );
    }

    // Cargo plans for the machine it runs on unless it is told a target.
    #[test]
    fn a_target_is_named_when_it_is_not_where_cargo_runs() {
        let (mac, wasm, intel) = (
            "aarch64-apple-darwin",
            "wasm32-wasip1",
            "x86_64-apple-darwin",
        );
        // Building here for here.
        assert_eq!(planned_target(mac, mac, mac, false), None);
        // Cross-compiling.
        assert_eq!(planned_target(wasm, mac, mac, true), Some(wasm));
        // Evaluating here what another kind of machine builds for itself.
        assert_eq!(planned_target(intel, intel, mac, false), Some(intel));
        // Evaluating on the platform that another machine builds for.
        assert_eq!(planned_target(mac, intel, mac, true), Some(mac));
        // Two platforms of one triple, as with a static package set: the
        // units of each side must still be told apart.
        assert_eq!(planned_target(mac, mac, mac, true), Some(mac));

        let req = request("");
        assert_eq!(
            req.unit_graph_args(Some(wasm)).join(" "),
            "build --unit-graph -Z unstable-options --locked --profile release --target wasm32-wasip1"
        );
        assert!(req
            .test_graph_args(Some(wasm))
            .join(" ")
            .ends_with("--profile release --target wasm32-wasip1"));
        // Packages are needed for both: what runs while building, and
        // what is built.
        assert_eq!(
            req.metadata_args(&[mac, wasm]).join(" "),
            "metadata --format-version 1 --locked --filter-platform aarch64-apple-darwin \
             --filter-platform wasm32-wasip1"
        );
    }

    #[test]
    fn an_unknown_request_field_is_refused() {
        let err = run(r#"{"cargo":"/c","surprise":1}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("resolve request"), "{err}");
    }

    #[test]
    fn directories() {
        assert_eq!(normalize_dir("."), "");
        assert_eq!(normalize_dir(""), "");
        assert_eq!(normalize_dir("./a/b/"), "a/b");
    }
}
