//! The `compile` subcommand: one rustc invocation, as cargo would make it,
//! with every path naming the store.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use crate::localsrc::{is_under, join};
use crate::node::{self, Attrs, CompileNode, CompileRecord, RunRecord};
use crate::Result;

/// A rustc invocation and what the unit hands on to its dependents.
#[derive(Debug)]
pub struct Invocation {
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: String,
    /// Where rustc writes: `$out/lib` or `$out/bin`.
    pub out_dir: String,
    pub transitive: Vec<String>,
    pub native: Vec<String>,
}

pub fn run() -> Result<()> {
    let attrs: Attrs<CompileNode> = node::load_attrs()?;
    let node = &attrs.node;
    let out = &attrs.outputs.out;

    let mut deps = Vec::new();
    for dep in &node.deps {
        deps.push((dep.name.clone(), node::read_record::<CompileRecord>(&dep.path)?));
    }
    let script = node.build_script.as_deref().map(node::read_record::<RunRecord>).transpose()?;
    let inv = plan(&attrs.rustc, &attrs.cargo, node, out, &deps, script.as_ref());

    fs::create_dir_all(&inv.out_dir)?;
    let status = Command::new(&inv.argv[0])
        .args(&inv.argv[1..])
        .current_dir(&inv.cwd)
        .envs(&inv.env)
        .status()
        .map_err(|err| format!("running {}: {err}", inv.argv[0]))?;
    if !status.success() {
        return Err(format!(
            "rustc failed for {} of {} {}",
            node.target_name, node.pkg.name, node.pkg.version
        )
        .into());
    }

    let artifact = artifact(node, &inv.out_dir)?;
    node::write_record(
        out,
        &CompileRecord {
            kind: node.kind.clone(),
            pkg: node.pkg.clone(),
            crate_name: node.crate_name.clone(),
            artifact,
            transitive: inv.transitive,
            native: inv.native,
            argv: inv.argv,
            env: inv.env,
            cwd: inv.cwd,
        },
    )
}

fn is_executable(node: &CompileNode) -> bool {
    matches!(node.kind.as_str(), "bin" | "example" | "build-script")
}

fn has_crate_type(node: &CompileNode, crate_type: &str) -> bool {
    node.rustc_args.windows(2).any(|pair| pair[0] == "--crate-type" && pair[1] == crate_type)
}

/// Whether a linker argument a build script asked for applies to this unit.
fn link_arg_applies(target: &str, node: &CompileNode) -> bool {
    match target {
        "all" => true,
        "cdylib" => node.kind == "lib" && has_crate_type(node, "cdylib"),
        "bins" => node.kind == "bin",
        "examples" => node.kind == "example",
        // Tests and benches are not built yet.
        "tests" | "benches" => false,
        other => other.strip_prefix("bin:").is_some_and(|name| node.kind == "bin" && node.target_name == name),
    }
}

fn push_unique(list: &mut Vec<String>, items: impl IntoIterator<Item = String>) {
    for item in items {
        if !list.contains(&item) {
            list.push(item);
        }
    }
}

/// Works out the rustc invocation of a unit from its node, the records of
/// its direct dependencies and what its build script printed.
pub fn plan(
    rustc: &str,
    cargo: &str,
    node: &CompileNode,
    out: &str,
    deps: &[(String, CompileRecord)],
    script: Option<&RunRecord>,
) -> Invocation {
    let out_dir = format!("{out}/{}", if is_executable(node) { "bin" } else { "lib" });
    let pkg_root = join(&node.src, &node.manifest_dir);

    // Cargo names a local package's source relative to the workspace root
    // and runs rustc there; anything else gets an absolute path and runs in
    // its package directory.
    let (src_arg, cwd) = if node.local && is_under(&node.manifest_dir, &node.work_dir) {
        let from_src = join(&node.manifest_dir, &node.src_path);
        let rel = from_src.strip_prefix(&node.work_dir).unwrap_or(&from_src).trim_start_matches('/');
        (rel.to_string(), join(&node.src, &node.work_dir))
    } else {
        (join(&pkg_root, &node.src_path), pkg_root.clone())
    };

    // rustc must find every transitive dependency, not only the direct ones.
    let mut search: Vec<String> = Vec::new();
    let mut native: Vec<String> = script.map(|s| s.library_paths.clone()).unwrap_or_default();
    for (_, record) in deps {
        push_unique(&mut search, record.transitive.iter().cloned());
        push_unique(&mut native, record.native.iter().cloned());
    }

    let mut argv: Vec<String> = vec![
        rustc.to_string(),
        "--crate-name".to_string(),
        node.crate_name.clone(),
        format!("--edition={}", node.edition),
        src_arg,
    ];
    argv.extend(node.rustc_args.iter().cloned());
    argv.extend(["--emit=link".to_string(), "--out-dir".to_string(), out_dir.clone()]);
    for dir in &search {
        argv.extend(["-L".to_string(), format!("dependency={dir}")]);
    }
    for (name, record) in deps {
        argv.extend(["--extern".to_string(), format!("{name}={}", record.artifact)]);
    }
    argv.extend(node.tail_args.iter().cloned());

    for path in &native {
        argv.extend(["-L".to_string(), path.clone()]);
    }
    if let Some(script) = script {
        if node.pass_l {
            for lib in &script.library_links {
                argv.extend(["-l".to_string(), lib.clone()]);
            }
        }
        for link_arg in &script.link_args {
            if link_arg_applies(&link_arg.target, node) {
                argv.extend(["-C".to_string(), format!("link-arg={}", link_arg.arg)]);
            }
        }
        for cfg in &script.cfgs {
            argv.extend(["--cfg".to_string(), cfg.clone()]);
        }
        for check_cfg in &script.check_cfgs {
            argv.extend(["--check-cfg".to_string(), check_cfg.clone()]);
        }
    }
    // Keeps the store out of panic messages and debug information, and with
    // it the sources out of the closure of what is built.
    argv.extend(["--remap-path-prefix".to_string(), format!("{}={}", node.src, node.remap_to)]);

    let mut env = node.env.clone();
    env.insert("CARGO".to_string(), cargo.to_string());
    env.insert("CARGO_MANIFEST_DIR".to_string(), pkg_root.clone());
    env.insert("CARGO_MANIFEST_PATH".to_string(), join(&pkg_root, "Cargo.toml"));
    if let Some(script) = script {
        env.insert("OUT_DIR".to_string(), script.out_dir.clone());
        env.extend(script.env.iter().cloned());
    }
    env.extend(node.override_env.clone());

    // What dependents need. A proc macro is loaded by rustc on its own, and
    // nothing links an executable.
    let lib_dir = out_dir.clone();
    let (transitive, native) = match node.kind.as_str() {
        "lib" => {
            let mut all = vec![lib_dir];
            push_unique(&mut all, search);
            (all, native)
        }
        "proc-macro" => (vec![lib_dir], Vec::new()),
        _ => (Vec::new(), Vec::new()),
    };

    Invocation { argv, env, cwd, out_dir, transitive, native }
}

/// Finds what rustc wrote, and gives an executable its target name.
fn artifact(node: &CompileNode, out_dir: &str) -> Result<String> {
    let stem = format!("{}-{}", node.crate_name, node.metadata);
    if is_executable(node) {
        let built = Path::new(out_dir).join(&stem);
        let named = Path::new(out_dir).join(&node.target_name);
        fs::rename(&built, &named)
            .map_err(|err| format!("rustc did not write {}: {err}", built.display()))?;
        return Ok(named.to_string_lossy().into_owned());
    }

    let mut files: Vec<String> = fs::read_dir(out_dir)?
        .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned()))
        .filter(|name| name.starts_with(&format!("lib{stem}.")))
        .collect();
    files.sort();
    // A library is named by its rlib; a proc macro has only its dynamic
    // library.
    let chosen = files
        .iter()
        .find(|name| name.ends_with(".rlib"))
        .or_else(|| files.first())
        .ok_or_else(|| format!("rustc wrote no lib{stem}.* into {out_dir}"))?;
    Ok(format!("{out_dir}/{chosen}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{LinkArg, PkgRef};

    fn node(kind: &str, local: bool) -> CompileNode {
        serde_json::from_value(serde_json::json!({
            "kind": kind,
            "pkg": { "name": "pkg", "version": "1.0.0" },
            "crateName": "the_crate",
            "targetName": "the-crate",
            "edition": "2021",
            "src": "/nix/store/src",
            "manifestDir": if local { "crates/pkg" } else { "" },
            "workDir": "",
            "srcPath": "src/lib.rs",
            "local": local,
            "remapTo": "pkg-1.0.0",
            "metadata": "0123456789abcdef",
            "rustcArgs": ["--crate-type", if kind == "lib" { "lib" } else { "bin" }, "-C", "opt-level=3"],
            "tailArgs": if local { serde_json::json!([]) } else { serde_json::json!(["--cap-lints", "allow"]) },
            "env": { "CARGO_PKG_NAME": "pkg", "CARGO_CRATE_NAME": "the_crate" },
            "deps": [],
            "buildScript": null,
            "passL": kind == "lib",
        }))
        .unwrap()
    }

    fn record(kind: &str, name: &str, transitive: &[&str], native: &[&str]) -> CompileRecord {
        CompileRecord {
            kind: kind.to_string(),
            pkg: PkgRef { name: name.to_string(), version: "1.0.0".to_string() },
            crate_name: name.to_string(),
            artifact: format!("/nix/store/{name}/lib/lib{name}-x.rlib"),
            transitive: transitive.iter().map(|s| s.to_string()).collect(),
            native: native.iter().map(|s| s.to_string()).collect(),
            argv: vec![],
            env: BTreeMap::new(),
            cwd: String::new(),
        }
    }

    #[test]
    fn registry_library_with_dependencies() {
        let deps = vec![
            ("a".to_string(), record("lib", "a", &["/nix/store/a/lib", "/nix/store/shared/lib"], &["native=/n/a"])),
            ("renamed".to_string(), record("lib", "b", &["/nix/store/b/lib", "/nix/store/shared/lib"], &[])),
            ("mac".to_string(), record("proc-macro", "mac", &["/nix/store/mac/lib"], &[])),
        ];
        let inv = plan("/rustc", "/cargo", &node("lib", false), "/out", &deps, None);
        assert_eq!(
            inv.argv.join(" "),
            "/rustc --crate-name the_crate --edition=2021 /nix/store/src/src/lib.rs --crate-type lib -C opt-level=3 \
             --emit=link --out-dir /out/lib \
             -L dependency=/nix/store/a/lib -L dependency=/nix/store/shared/lib -L dependency=/nix/store/b/lib \
             -L dependency=/nix/store/mac/lib \
             --extern a=/nix/store/a/lib/liba-x.rlib --extern renamed=/nix/store/b/lib/libb-x.rlib \
             --extern mac=/nix/store/mac/lib/libmac-x.rlib --cap-lints allow -L native=/n/a \
             --remap-path-prefix /nix/store/src=pkg-1.0.0"
        );
        assert_eq!(inv.cwd, "/nix/store/src");
        assert_eq!(inv.env["CARGO_MANIFEST_DIR"], "/nix/store/src");
        assert_eq!(inv.env["CARGO_MANIFEST_PATH"], "/nix/store/src/Cargo.toml");
        assert_eq!(inv.env["CARGO"], "/cargo");
        assert!(!inv.env.contains_key("OUT_DIR"));
        // Dependents need this library, everything under it, and the proc
        // macro in case its macros are re-exported.
        assert_eq!(
            inv.transitive,
            ["/out/lib", "/nix/store/a/lib", "/nix/store/shared/lib", "/nix/store/b/lib", "/nix/store/mac/lib"]
        );
        assert_eq!(inv.native, ["native=/n/a"]);
    }

    #[test]
    fn local_unit_runs_in_the_workspace_root_with_a_relative_path() {
        let inv = plan("/rustc", "/cargo", &node("lib", true), "/out", &[], None);
        assert_eq!(inv.cwd, "/nix/store/src");
        assert_eq!(inv.argv[4], "crates/pkg/src/lib.rs");
        assert_eq!(inv.env["CARGO_MANIFEST_DIR"], "/nix/store/src/crates/pkg");
        assert!(!inv.argv.contains(&"--cap-lints".to_string()));
    }

    #[test]
    fn local_unit_in_a_workspace_below_the_source_root() {
        let mut n = node("lib", true);
        n.work_dir = "ws".to_string();
        n.manifest_dir = "ws/crates/pkg".to_string();
        let inv = plan("/rustc", "/cargo", &n, "/out", &[], None);
        assert_eq!(inv.cwd, "/nix/store/src/ws");
        assert_eq!(inv.argv[4], "crates/pkg/src/lib.rs");

        // A package outside the workspace directory is built like a foreign one.
        n.manifest_dir = "elsewhere".to_string();
        let inv = plan("/rustc", "/cargo", &n, "/out", &[], None);
        assert_eq!(inv.cwd, "/nix/store/src/elsewhere");
        assert_eq!(inv.argv[4], "/nix/store/src/elsewhere/src/lib.rs");
    }

    fn script() -> RunRecord {
        RunRecord {
            kind: "run-build-script".to_string(),
            out_dir: "/nix/store/run/out".to_string(),
            library_paths: vec!["native=/nix/store/run/out".to_string()],
            library_links: vec!["static=foo".to_string()],
            link_args: vec![
                LinkArg { target: "all".to_string(), arg: "-Wl,-all".to_string() },
                LinkArg { target: "bins".to_string(), arg: "-Wl,-bins".to_string() },
                LinkArg { target: "bin:the-crate".to_string(), arg: "-Wl,-mine".to_string() },
                LinkArg { target: "bin:other".to_string(), arg: "-Wl,-other".to_string() },
                LinkArg { target: "examples".to_string(), arg: "-Wl,-examples".to_string() },
                LinkArg { target: "cdylib".to_string(), arg: "-Wl,-cdylib".to_string() },
            ],
            cfgs: vec!["has_foo".to_string()],
            check_cfgs: vec!["cfg(has_foo)".to_string()],
            env: vec![("FROM_SCRIPT".to_string(), "1".to_string())],
            ..RunRecord::default()
        }
    }

    #[test]
    fn library_takes_what_its_build_script_printed() {
        let deps = vec![("a".to_string(), record("lib", "a", &["/nix/store/a/lib"], &["native=/n/a"]))];
        let inv = plan("/rustc", "/cargo", &node("lib", false), "/out", &deps, Some(&script()));
        let tail = inv.argv.join(" ");
        let tail = tail.split("--cap-lints allow ").nth(1).unwrap();
        assert_eq!(
            tail,
            "-L native=/nix/store/run/out -L native=/n/a -l static=foo -C link-arg=-Wl,-all \
             --cfg has_foo --check-cfg cfg(has_foo) --remap-path-prefix /nix/store/src=pkg-1.0.0"
        );
        assert_eq!(inv.env["OUT_DIR"], "/nix/store/run/out");
        assert_eq!(inv.env["FROM_SCRIPT"], "1");
        // Its own search paths come first and are handed on with the rest.
        assert_eq!(inv.native, ["native=/nix/store/run/out", "native=/n/a"]);
    }

    #[test]
    fn binary_of_a_package_with_a_library_takes_no_l_flags() {
        let mut n = node("bin", true);
        n.pass_l = false;
        let inv = plan("/rustc", "/cargo", &n, "/out", &[], Some(&script()));
        let args = inv.argv.join(" ");
        assert!(!args.contains("-l static=foo"), "{args}");
        assert!(args.contains("-L native=/nix/store/run/out"), "{args}");
        assert!(
            args.contains("-C link-arg=-Wl,-all -C link-arg=-Wl,-bins -C link-arg=-Wl,-mine --cfg has_foo"),
            "{args}"
        );
        assert_eq!(inv.out_dir, "/out/bin");
        // Nothing links an executable.
        assert!(inv.transitive.is_empty() && inv.native.is_empty());
    }

    #[test]
    fn override_env_wins() {
        let mut n = node("lib", false);
        n.override_env = BTreeMap::from([("CARGO_PKG_NAME".to_string(), "overridden".to_string())]);
        let inv = plan("/rustc", "/cargo", &n, "/out", &[], None);
        assert_eq!(inv.env["CARGO_PKG_NAME"], "overridden");
    }
}
