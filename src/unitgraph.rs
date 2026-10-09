//! Cargo's unit graph, as `cargo build --unit-graph` prints it.

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct UnitGraph {
    pub version: u32,
    pub units: Vec<Unit>,
    pub roots: Vec<usize>,
}

/// One step of cargo's plan: a rustc invocation or a build-script run.
#[derive(Debug, Clone, Deserialize)]
pub struct Unit {
    /// Opaque: its form differs between cargo versions.
    pub pkg_id: String,
    pub target: Target,
    pub profile: Profile,
    /// The target triple, or `None` for the machine that builds.
    pub platform: Option<String>,
    pub mode: String,
    pub features: Vec<String>,
    pub dependencies: Vec<UnitDep>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Target {
    pub kind: Vec<String>,
    pub crate_types: Vec<String>,
    pub name: String,
    pub src_path: String,
    pub edition: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UnitDep {
    pub index: usize,
    pub extern_crate_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    pub name: String,
    pub opt_level: String,
    /// `false`, `true`, `off`, or a named mode such as `thin`.
    pub lto: String,
    pub codegen_units: Option<u32>,
    debuginfo: Option<Value>,
    pub split_debuginfo: Option<String>,
    pub debug_assertions: bool,
    pub overflow_checks: bool,
    pub rpath: bool,
    pub panic: String,
    strip: Value,
}

impl Profile {
    /// The value for `-C debuginfo=`, or `None` when debug information is off.
    pub fn debuginfo(&self) -> Option<String> {
        match self.debuginfo.as_ref()? {
            Value::Number(n) if n.as_u64() == Some(0) => None,
            Value::Number(n) => Some(n.to_string()),
            Value::String(s) => Some(s.clone()),
            _ => None,
        }
    }

    /// The value for `-C strip=`, or `None` when nothing is stripped.
    ///
    /// Cargo prints `{"resolved":{"Named":"debuginfo"}}`, `{"resolved":"None"}`
    /// or the same under `deferred`.
    pub fn strip(&self) -> Option<String> {
        let inner = self.strip.get("resolved").or_else(|| self.strip.get("deferred"))?;
        inner.get("Named")?.as_str().map(str::to_string)
    }
}

impl Target {
    pub fn is_custom_build(&self) -> bool {
        self.kind.iter().any(|k| k == "custom-build")
    }

    pub fn is_proc_macro(&self) -> bool {
        self.kind.iter().any(|k| k == "proc-macro")
    }

    /// Built for the machine that builds, whatever the target: build scripts
    /// and proc macros.
    pub fn for_host(&self) -> bool {
        self.is_custom_build() || self.is_proc_macro()
    }

    /// The crate name rustc is given: the target name with `-` as `_`.
    pub fn crate_name(&self) -> String {
        self.name.replace('-', "_")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core_rs() -> UnitGraph {
        serde_json::from_str(include_str!("../testdata/core-rs/unit-graph.json")).unwrap()
    }

    #[test]
    fn decodes_the_core_rs_graph() {
        let graph = core_rs();
        assert_eq!(graph.version, 1);
        assert_eq!(graph.units.len(), 103);
        assert_eq!(graph.roots.len(), 1);
        let root = &graph.units[graph.roots[0]];
        assert_eq!(root.target.kind, ["example"]);
        assert_eq!(root.target.name, "amber-store");
        assert_eq!(root.target.crate_name(), "amber_store");

        let mut modes: Vec<&str> = graph.units.iter().map(|u| u.mode.as_str()).collect();
        modes.sort();
        modes.dedup();
        assert_eq!(modes, ["build", "run-custom-build"]);
    }

    #[test]
    fn libc_is_planned_twice() {
        let graph = core_rs();
        let libc = graph
            .units
            .iter()
            .filter(|u| u.pkg_id.contains("libc@") && u.target.kind == ["lib"])
            .count();
        assert_eq!(libc, 2);
    }

    #[test]
    fn profile_fields_cargo_encodes_as_values() {
        let graph = core_rs();
        let root = &graph.units[graph.roots[0]];
        assert_eq!(root.profile.lto, "thin");
        assert_eq!(root.profile.debuginfo(), None);
        assert_eq!(root.profile.strip().as_deref(), Some("debuginfo"));

        let dev: Profile = serde_json::from_str(
            r#"{"name":"dev","opt_level":"0","lto":"false","codegen_backend":null,
                "codegen_units":null,"debuginfo":2,"split_debuginfo":"unpacked",
                "debug_assertions":true,"overflow_checks":true,"rpath":false,
                "incremental":true,"panic":"unwind","strip":{"deferred":"None"}}"#,
        )
        .unwrap();
        assert_eq!(dev.debuginfo().as_deref(), Some("2"));
        assert_eq!(dev.strip(), None);
    }
}
