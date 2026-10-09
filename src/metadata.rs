//! What `cargo metadata` says about each package.

use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Metadata {
    pub packages: Vec<Package>,
    pub workspace_root: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Package {
    /// Opaque, and equal to the `pkg_id` of the package's units.
    pub id: String,
    pub name: String,
    pub version: String,
    /// `None` for a package found by path.
    pub source: Option<String>,
    pub manifest_path: String,
    pub links: Option<String>,
    /// Declared features, implicit ones of optional dependencies included.
    pub features: BTreeMap<String, Vec<String>>,
    pub targets: Vec<MetaTarget>,
    #[serde(default)]
    pub authors: Vec<String>,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub license: Option<String>,
    pub license_file: Option<String>,
    pub rust_version: Option<String>,
    pub readme: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MetaTarget {
    pub kind: Vec<String>,
    pub name: String,
    pub src_path: String,
}

impl Package {
    /// The directory of the package's `Cargo.toml`.
    pub fn manifest_dir(&self) -> &str {
        self.manifest_path
            .rsplit_once('/')
            .map_or("", |(dir, _)| dir)
    }

    /// Whether the package has a library or proc-macro target.
    pub fn has_lib(&self) -> bool {
        self.targets.iter().any(MetaTarget::is_lib)
    }

    /// The `CARGO_PKG_*` variables cargo sets for rustc and build scripts,
    /// except those that name paths.
    pub fn env(&self) -> BTreeMap<String, String> {
        let opt = |value: &Option<String>| value.clone().unwrap_or_default();
        // MAJOR.MINOR.PATCH[-PRE][+BUILD]: the build metadata may itself
        // contain hyphens, so it comes off first.
        let without_build = self.version.split('+').next().unwrap_or("");
        let (core, pre) = without_build.split_once('-').unwrap_or((without_build, ""));
        let mut numbers = core.split('.');
        let mut number = || numbers.next().unwrap_or("").to_string();
        BTreeMap::from([
            ("CARGO_PKG_NAME".to_string(), self.name.clone()),
            ("CARGO_PKG_VERSION".to_string(), self.version.clone()),
            ("CARGO_PKG_VERSION_MAJOR".to_string(), number()),
            ("CARGO_PKG_VERSION_MINOR".to_string(), number()),
            ("CARGO_PKG_VERSION_PATCH".to_string(), number()),
            ("CARGO_PKG_VERSION_PRE".to_string(), pre.to_string()),
            ("CARGO_PKG_AUTHORS".to_string(), self.authors.join(":")),
            ("CARGO_PKG_DESCRIPTION".to_string(), opt(&self.description)),
            ("CARGO_PKG_HOMEPAGE".to_string(), opt(&self.homepage)),
            ("CARGO_PKG_REPOSITORY".to_string(), opt(&self.repository)),
            ("CARGO_PKG_LICENSE".to_string(), opt(&self.license)),
            (
                "CARGO_PKG_LICENSE_FILE".to_string(),
                opt(&self.license_file),
            ),
            (
                "CARGO_PKG_RUST_VERSION".to_string(),
                opt(&self.rust_version),
            ),
            ("CARGO_PKG_README".to_string(), opt(&self.readme)),
        ])
    }
}

impl MetaTarget {
    pub fn is_lib(&self) -> bool {
        const LIB_KINDS: [&str; 6] = ["lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"];
        self.kind.iter().any(|k| LIB_KINDS.contains(&k.as_str()))
    }

    /// A binary, example, test or bench: a target with a root file of its own
    /// that no other target of the package reads.
    pub fn is_executable(&self) -> bool {
        const EXE_KINDS: [&str; 4] = ["bin", "example", "test", "bench"];
        self.kind.iter().any(|k| EXE_KINDS.contains(&k.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core_rs() -> Metadata {
        serde_json::from_str(include_str!("../testdata/core-rs/metadata.json")).unwrap()
    }

    fn package<'a>(meta: &'a Metadata, name: &str) -> &'a Package {
        meta.packages.iter().find(|p| p.name == name).unwrap()
    }

    #[test]
    fn decodes_core_rs() {
        let meta = core_rs();
        assert_eq!(meta.workspace_root, "/src");
        let root = package(&meta, "amber-store-core");
        assert_eq!(root.source, None);
        assert_eq!(root.manifest_dir(), "/src");
        assert!(root.has_lib());
        assert_eq!(
            root.targets
                .iter()
                .filter(|t| t.kind == ["example"])
                .count(),
            3
        );
        assert_eq!(package(&meta, "zstd-sys").links.as_deref(), Some("zstd"));
    }

    #[test]
    fn implicit_features_are_declared() {
        let meta = core_rs();
        let serde = package(&meta, "serde");
        assert!(serde.features.contains_key("derive"));
        assert!(serde.features.contains_key("serde_derive"));
    }

    #[test]
    fn version_parts() {
        let meta = core_rs();
        let env = package(&meta, "zstd-sys").env();
        assert_eq!(env["CARGO_PKG_VERSION"], "2.0.16+zstd.1.5.7");
        assert_eq!(env["CARGO_PKG_VERSION_MAJOR"], "2");
        assert_eq!(env["CARGO_PKG_VERSION_MINOR"], "0");
        assert_eq!(env["CARGO_PKG_VERSION_PATCH"], "16");
        assert_eq!(env["CARGO_PKG_VERSION_PRE"], "");

        let mut pre = package(&meta, "zstd-sys").clone();
        pre.version = "1.2.3-beta.1+build5".to_string();
        let env = pre.env();
        assert_eq!(env["CARGO_PKG_VERSION_PATCH"], "3");
        assert_eq!(env["CARGO_PKG_VERSION_PRE"], "beta.1");

        // A hyphen in the build metadata starts no pre-release.
        let env = package(&meta, "lz4-sys").env();
        assert_eq!(env["CARGO_PKG_VERSION"], "1.11.1+lz4-1.10.0");
        assert_eq!(env["CARGO_PKG_VERSION_PATCH"], "1");
        assert_eq!(env["CARGO_PKG_VERSION_PRE"], "");
    }
}
