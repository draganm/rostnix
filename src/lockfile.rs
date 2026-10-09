//! The checksums in `Cargo.lock`.

use std::collections::HashMap;

use serde::Deserialize;

use crate::Result;

#[derive(Deserialize)]
struct Lock {
    #[serde(default)]
    package: Vec<LockPackage>,
    /// Where the first lockfile format keeps its checksums, under keys of
    /// the form `checksum <name> <version> (<source>)`.
    #[serde(default)]
    metadata: HashMap<String, toml::Value>,
}

#[derive(Deserialize)]
struct LockPackage {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
}

/// The SHA-256 of each registry package's `.crate` file.
pub struct Checksums(HashMap<(String, String, String), String>);

impl Checksums {
    pub fn parse(text: &str) -> Result<Checksums> {
        let lock: Lock =
            toml::from_str(text).map_err(|err| format!("parsing Cargo.lock: {err}"))?;
        let entries = lock
            .package
            .into_iter()
            .filter_map(|pkg| Some(((pkg.name, pkg.version, pkg.source?), pkg.checksum?)));
        let mut sums: HashMap<_, _> = entries.collect();
        for (key, value) in &lock.metadata {
            let Some(rest) = key.strip_prefix("checksum ") else {
                continue;
            };
            let mut parts = rest.splitn(3, ' ');
            let (Some(name), Some(version), Some(source)) =
                (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let source = source.trim_start_matches('(').trim_end_matches(')');
            if let Some(sum) = value.as_str().filter(|sum| *sum != "<none>") {
                sums.insert(
                    (name.to_string(), version.to_string(), source.to_string()),
                    sum.to_string(),
                );
            }
        }
        Ok(Checksums(sums))
    }

    pub fn get(&self, name: &str, version: &str, source: &str) -> Option<&str> {
        let key = (name.to_string(), version.to_string(), source.to_string());
        self.0.get(&key).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";

    const LOCK: &str = r#"
version = 4

[[package]]
name = "app"
version = "0.1.0"
dependencies = ["itoa 1.0.15", "itoa 0.4.8"]

[[package]]
name = "itoa"
version = "0.4.8"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "b71991ff56294aa922b450139ee08b3bfc70982c6b2c7562771375cf73542dd4"

[[package]]
name = "itoa"
version = "1.0.15"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "4a5f13b858c8d314ee3e8f639011f7ccefe71f97f96e50151fb991f267928e2c"
"#;

    #[test]
    fn finds_each_version_and_skips_path_packages() {
        let sums = Checksums::parse(LOCK).unwrap();
        assert_eq!(
            sums.get("itoa", "0.4.8", CRATES_IO),
            Some("b71991ff56294aa922b450139ee08b3bfc70982c6b2c7562771375cf73542dd4")
        );
        assert_eq!(
            sums.get("itoa", "1.0.15", CRATES_IO),
            Some("4a5f13b858c8d314ee3e8f639011f7ccefe71f97f96e50151fb991f267928e2c")
        );
        assert_eq!(sums.get("app", "0.1.0", CRATES_IO), None);
        assert_eq!(
            sums.get("itoa", "1.0.15", "registry+https://example.com/index"),
            None
        );
    }

    // Cargo still accepts a lockfile in its first format under --locked.
    #[test]
    fn reads_checksums_from_the_metadata_table() {
        let sums = Checksums::parse(
            r#"
[[package]]
name = "app"
version = "0.1.0"

[[package]]
name = "itoa"
version = "0.4.8"
source = "registry+https://github.com/rust-lang/crates.io-index"

[metadata]
"checksum itoa 0.4.8 (registry+https://github.com/rust-lang/crates.io-index)" = "b71991ff56294aa922b450139ee08b3bfc70982c6b2c7562771375cf73542dd4"
"checksum gitdep 1.0.0 (git+https://example.com/x#abc)" = "<none>"
"#,
        )
        .unwrap();
        assert_eq!(
            sums.get("itoa", "0.4.8", CRATES_IO),
            Some("b71991ff56294aa922b450139ee08b3bfc70982c6b2c7562771375cf73542dd4")
        );
        assert_eq!(
            sums.get("gitdep", "1.0.0", "git+https://example.com/x#abc"),
            None
        );
    }

    #[test]
    fn rejects_what_is_not_toml() {
        assert!(Checksums::parse("[[package").is_err());
    }
}
