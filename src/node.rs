//! What passes between derivations: the node a derivation is given in its
//! attributes, and the `unit.json` it leaves for the units that depend on
//! it.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::Result;

/// The attributes of a derivation built by this tool.
#[derive(Debug, Deserialize)]
pub struct Attrs<N> {
    pub rustc: String,
    pub cargo: String,
    pub node: N,
    pub outputs: Outputs,
}

#[derive(Debug, Deserialize)]
pub struct Outputs {
    pub out: String,
}

/// Reads the attributes of the derivation being built. Its derivations use
/// `__structuredAttrs`, so they are in the file Nix names.
pub fn load_attrs<N: DeserializeOwned>() -> Result<Attrs<N>> {
    let file = std::env::var("NIX_ATTRS_JSON_FILE")
        .map_err(|_| "NIX_ATTRS_JSON_FILE is not set: this command runs inside a derivation with __structuredAttrs")?;
    let text = fs::read_to_string(&file).map_err(|err| format!("reading {file}: {err}"))?;
    serde_json::from_str(&text).map_err(|err| format!("parsing {file}: {err}").into())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PkgRef {
    pub name: String,
    pub version: String,
}

/// The node of a `compile` derivation.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompileNode {
    /// What is built; `test` for any target built as a test.
    pub kind: String,
    /// What the target is, whatever it is built as.
    pub target_kind: String,
    pub pkg: PkgRef,
    pub crate_name: String,
    pub target_name: String,
    pub edition: String,
    /// The source store path, and the package directory, the working
    /// directory and the root source file below it.
    pub src: String,
    pub manifest_dir: String,
    pub work_dir: String,
    pub src_path: String,
    pub local: bool,
    pub remap_to: String,
    pub metadata: String,
    pub rustc_args: Vec<String>,
    pub tail_args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub deps: Vec<Dep>,
    pub build_script: Option<String>,
    pub pass_l: bool,
    #[serde(default)]
    pub override_env: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct Dep {
    /// The name the unit's source uses for the dependency.
    pub name: String,
    /// The dependency's output.
    pub path: String,
}

/// The node of a `runBuildScript` derivation.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunNode {
    pub pkg: PkgRef,
    pub local: bool,
    pub src: String,
    pub manifest_dir: String,
    /// The output of the unit that compiled the script.
    pub script: String,
    pub features: Vec<String>,
    pub debug_assertions: bool,
    pub env: BTreeMap<String, String>,
    pub links_deps: Vec<LinksDep>,
    #[serde(default)]
    pub override_env: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct LinksDep {
    /// The `links` name of the dependency.
    pub links: String,
    /// The output of its build-script run.
    pub path: String,
}

/// The node of a `test` derivation: what the test is compiled from, as for
/// a `compile`, and what it is run with.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestNode {
    #[serde(flatten)]
    pub compile: CompileNode,
    /// The profile's directory in cargo's target directory.
    pub profile_dir: String,
    /// The outputs of the units that built the binaries and examples the
    /// test finds beside itself.
    pub executables: Vec<String>,
    /// Arguments for the test executable.
    pub args: Vec<String>,
}

/// What the run of a test leaves behind, beside its log and the record of
/// its compilation.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestRecord {
    pub kind: String,
    pub pkg: PkgRef,
    pub crate_name: String,
    /// What the test ran with: the environment cargo would set, and what
    /// the package's override added to it.
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub override_env: BTreeMap<String, String>,
    pub cwd: String,
}

/// What a `compile` leaves behind.
#[derive(Debug, Serialize, Deserialize)]
pub struct CompileRecord {
    pub kind: String,
    pub pkg: PkgRef,
    #[serde(rename = "crateName")]
    pub crate_name: String,
    /// What dependents name with `--extern`, or the executable.
    pub artifact: String,
    /// The directories dependents pass as `-L dependency=`.
    pub transitive: Vec<String>,
    /// The `-L` values of every build script in the unit's closure: those
    /// inside the `OUT_DIR` of the script that printed them, and the others.
    pub native: Vec<String>,
    #[serde(rename = "nativeExternal", default)]
    pub native_external: Vec<String>,
    /// Linker arguments the build scripts in the closure ask of any cdylib.
    #[serde(rename = "cdylibLinkArgs", default)]
    pub cdylib_link_args: Vec<String>,
    /// What rustc ran with: the environment cargo would set, and what the
    /// package's override added to it.
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    #[serde(rename = "overrideEnv", default)]
    pub override_env: BTreeMap<String, String>,
    pub cwd: String,
}

/// What a `run-build-script` leaves behind: the directives its script
/// printed.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    pub kind: String,
    pub pkg: Option<PkgRef>,
    pub out_dir: String,
    pub library_paths: Vec<String>,
    pub library_links: Vec<String>,
    pub link_args: Vec<LinkArg>,
    pub cfgs: Vec<String>,
    pub check_cfgs: Vec<String>,
    pub env: Vec<(String, String)>,
    pub metadata: Vec<(String, String)>,
    /// What the script ran with: the environment cargo would set, and what
    /// the package's override added to it.
    pub argv: Vec<String>,
    pub env_recorded: BTreeMap<String, String>,
    pub override_env: BTreeMap<String, String>,
    pub cwd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkArg {
    /// `all`, `cdylib`, `bins`, `bin:<name>`, `tests`, `benches` or `examples`.
    pub target: String,
    pub arg: String,
}

pub const RECORD_FILE: &str = "unit.json";
/// Where a test keeps the record of its run, beside that of its
/// compilation.
pub const RUN_RECORD_FILE: &str = "run.json";

pub fn read_record<R: DeserializeOwned>(unit_out: &str) -> Result<R> {
    let file = Path::new(unit_out).join(RECORD_FILE);
    let text =
        fs::read_to_string(&file).map_err(|err| format!("reading {}: {err}", file.display()))?;
    serde_json::from_str(&text).map_err(|err| format!("parsing {}: {err}", file.display()).into())
}

pub fn write_record<R: Serialize>(unit_out: &str, record: &R) -> Result<()> {
    write_record_as(unit_out, RECORD_FILE, record)
}

pub fn write_record_as<R: Serialize>(unit_out: &str, name: &str, record: &R) -> Result<()> {
    let file = Path::new(unit_out).join(name);
    let text = serde_json::to_string_pretty(record)?;
    fs::write(&file, text + "\n").map_err(|err| format!("writing {}: {err}", file.display()).into())
}
