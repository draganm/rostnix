//! The evaluation-time pipeline: ask cargo for its plan, put the crates in
//! the store, print the graph.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::cargohome::Cargo;
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
    fn test_graph_args(&self) -> Vec<String> {
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
        args
    }

    fn unit_graph_args(&self) -> Vec<String> {
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
        args
    }

    fn metadata_args(&self, host: &str) -> Vec<String> {
        let mut args: Vec<String> = [
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--filter-platform",
            host,
        ]
        .map(String::from)
        .to_vec();
        args.extend(self.feature_args());
        args
    }
}

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
    let host = cargo.host()?;
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
    let units = plan(&request.unit_graph_args())?;
    let test_units = request
        .do_check
        .then(|| plan(&request.test_graph_args()))
        .transpose()?;
    let metadata: Metadata =
        serde_json::from_slice(&cargo.output(&workspace, &request.metadata_args(&host))?).map_err(
            |err| format!("cargo {cargo_version} printed metadata this version cannot read: {err}"),
        )?;

    let graph = graph::build(&Inputs {
        units: &units,
        test_units: test_units.as_ref(),
        metadata: &metadata,
        checksums: &checksums,
        src,
        cargo_root: &cargo_root,
        host: &host,
        cargo_version: &cargo_version,
        override_keys: &request.override_keys,
        read_manifest: &|path| {
            let text = fs::read_to_string(path).map_err(|err| format!("reading {path}: {err}"))?;
            toml::from_str(&text).map_err(|err| format!("parsing {path}: {err}").into())
        },
    })?;

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
            request("").unit_graph_args().join(" "),
            "build --unit-graph -Z unstable-options --locked --profile release"
        );
        assert_eq!(
            request("").metadata_args("aarch64-apple-darwin").join(" "),
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
            req.unit_graph_args().join(" "),
            "build --unit-graph -Z unstable-options --locked --profile thin --package a --package b \
             --bin tool --example demo --features x,dep/y --no-default-features"
        );
        req.all_features = true;
        assert!(req
            .metadata_args("h")
            .join(" ")
            .ends_with("--features x,dep/y --all-features --no-default-features"));
    }

    // Tests are those of the selected packages, with the features of the
    // build. Naming a binary to install does not narrow them.
    #[test]
    fn tests_are_planned_for_the_selected_packages() {
        let mut req = request("");
        assert_eq!(
            req.test_graph_args().join(" "),
            "test --no-run --unit-graph -Z unstable-options --locked --profile release"
        );
        req.packages = vec!["a".into()];
        req.bins = vec!["tool".into()];
        req.examples = vec!["demo".into()];
        req.features = vec!["x".into()];
        req.profile = "thin".into();
        assert_eq!(
            req.test_graph_args().join(" "),
            "test --no-run --unit-graph -Z unstable-options --locked --profile thin --package a --features x"
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
