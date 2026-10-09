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
    /// Library search paths of the build scripts in the unit's closure:
    /// those inside the `OUT_DIR` of the script that printed them, and the
    /// others.
    pub native: Vec<String>,
    pub native_external: Vec<String>,
    /// Linker arguments the build scripts in the closure ask of any cdylib.
    pub cdylib_link_args: Vec<String>,
}

pub fn run() -> Result<()> {
    let attrs: Attrs<CompileNode> = node::load_attrs()?;
    let node = &attrs.node;
    let out = &attrs.outputs.out;

    let mut deps = Vec::new();
    for dep in &node.deps {
        deps.push((
            dep.name.clone(),
            node::read_record::<CompileRecord>(&dep.path)?,
        ));
    }
    let script = node
        .build_script
        .as_deref()
        .map(node::read_record::<RunRecord>)
        .transpose()?;
    let inv = plan(
        &attrs.rustc,
        &attrs.cargo,
        node,
        out,
        &deps,
        script.as_ref(),
    );

    fs::create_dir_all(&inv.out_dir)?;
    let status = Command::new(&inv.argv[0])
        .args(&inv.argv[1..])
        .current_dir(&inv.cwd)
        .envs(&inv.env)
        .envs(&node.override_env)
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
            native_external: inv.native_external,
            cdylib_link_args: inv.cdylib_link_args,
            argv: inv.argv,
            env: inv.env,
            override_env: node.override_env.clone(),
            cwd: inv.cwd,
        },
    )
}

fn is_executable(node: &CompileNode) -> bool {
    matches!(
        node.kind.as_str(),
        "bin" | "example" | "build-script" | "test"
    )
}

fn has_crate_type(node: &CompileNode, crate_type: &str) -> bool {
    node.rustc_args
        .windows(2)
        .any(|pair| pair[0] == "--crate-type" && pair[1] == crate_type)
}

fn is_cdylib(node: &CompileNode) -> bool {
    node.kind == "lib" && has_crate_type(node, "cdylib")
}

/// The directory of a `-L` value, which may start with a kind such as
/// `native=`.
fn search_dir(value: &str) -> &str {
    match value.split_once('=') {
        Some(("native" | "crate" | "dependency" | "framework" | "all", dir)) => dir,
        _ => value,
    }
}

/// Whether a linker argument a build script asked for applies to this unit.
/// Cargo goes by what the target is: a binary built as a test still takes
/// what was asked of binaries.
fn link_arg_applies(target: &str, node: &CompileNode) -> bool {
    match target {
        "all" => true,
        "cdylib" => is_cdylib(node),
        "bins" => node.target_kind == "bin",
        "examples" => node.target_kind == "example",
        "tests" => node.target_kind == "test",
        "benches" => node.target_kind == "bench",
        other => other
            .strip_prefix("bin:")
            .is_some_and(|name| node.target_kind == "bin" && node.target_name == name),
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
    plan_at(rustc, cargo, node, &node.src, &out_dir, true, deps, script)
}

/// The same for a source tree and an output directory of the caller's
/// choosing. A test is compiled in a writable copy of its source, into the
/// directory cargo would use, and without the remapping that keeps store
/// paths out of what is installed: nothing of a test is installed, and its
/// paths should be the real ones.
#[allow(clippy::too_many_arguments)]
pub fn plan_at(
    rustc: &str,
    cargo: &str,
    node: &CompileNode,
    src: &str,
    out_dir: &str,
    remap: bool,
    deps: &[(String, CompileRecord)],
    script: Option<&RunRecord>,
) -> Invocation {
    let out_dir = out_dir.to_string();
    let pkg_root = join(src, &node.manifest_dir);

    // Cargo names a local package's source relative to the workspace root
    // and runs rustc there; anything else gets an absolute path and runs in
    // its package directory.
    let (src_arg, cwd) = if node.local && is_under(&node.manifest_dir, &node.work_dir) {
        let from_src = join(&node.manifest_dir, &node.src_path);
        let rel = from_src
            .strip_prefix(&node.work_dir)
            .unwrap_or(&from_src)
            .trim_start_matches('/');
        (rel.to_string(), join(src, &node.work_dir))
    } else {
        (join(&pkg_root, &node.src_path), pkg_root.clone())
    };

    // rustc must find every transitive dependency, not only the direct ones.
    let mut search: Vec<String> = Vec::new();
    for (_, record) in deps {
        push_unique(&mut search, record.transitive.iter().cloned());
    }

    // Library search paths in cargo's order: the unit's own build script,
    // then its dependencies' by package; and across all of them the paths
    // inside a script's own OUT_DIR before the others, so that a library a
    // script built wins over one of the same name found elsewhere.
    let (mut native, mut native_external) = (Vec::new(), Vec::new());
    let mut cdylib_link_args: Vec<String> = Vec::new();
    if let Some(script) = script {
        for path in &script.library_paths {
            let inside = Path::new(search_dir(path)).starts_with(&script.out_dir);
            (if inside {
                &mut native
            } else {
                &mut native_external
            })
            .push(path.clone());
        }
        cdylib_link_args.extend(
            script
                .link_args
                .iter()
                .filter(|a| a.target == "cdylib")
                .map(|a| a.arg.clone()),
        );
    }
    let own_cdylib_link_args = cdylib_link_args.len();
    let mut by_package: Vec<&CompileRecord> = deps.iter().map(|(_, record)| record).collect();
    by_package.sort_by(|a, b| (&a.pkg.name, &a.pkg.version).cmp(&(&b.pkg.name, &b.pkg.version)));
    for record in by_package {
        push_unique(&mut native, record.native.iter().cloned());
        push_unique(&mut native_external, record.native_external.iter().cloned());
        push_unique(
            &mut cdylib_link_args,
            record.cdylib_link_args.iter().cloned(),
        );
    }

    let mut argv: Vec<String> = vec![
        rustc.to_string(),
        "--crate-name".to_string(),
        node.crate_name.clone(),
        format!("--edition={}", node.edition),
        src_arg,
    ];
    argv.extend(node.rustc_args.iter().cloned());
    argv.extend([
        "--emit=link".to_string(),
        "--out-dir".to_string(),
        out_dir.clone(),
    ]);
    for dir in &search {
        argv.extend(["-L".to_string(), format!("dependency={dir}")]);
    }
    for (name, record) in deps {
        argv.extend([
            "--extern".to_string(),
            format!("{name}={}", record.artifact),
        ]);
    }
    argv.extend(node.tail_args.iter().cloned());

    for path in native.iter().chain(&native_external) {
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
    }
    // What a build script asks of cdylibs also reaches a cdylib that only
    // depends on the script's package. Cargo keeps that on purpose.
    if is_cdylib(node) {
        for arg in &cdylib_link_args[own_cdylib_link_args..] {
            argv.extend(["-C".to_string(), format!("link-arg={arg}")]);
        }
    }
    if let Some(script) = script {
        for cfg in &script.cfgs {
            argv.extend(["--cfg".to_string(), cfg.clone()]);
        }
        for check_cfg in &script.check_cfgs {
            argv.extend(["--check-cfg".to_string(), check_cfg.clone()]);
        }
    }
    // Keeps the store out of panic messages and debug information, and with
    // it the sources out of the closure of what is built.
    if remap {
        argv.extend([
            "--remap-path-prefix".to_string(),
            format!("{src}={}", node.remap_to),
        ]);
    }
    if let Some(script) = script.filter(|_| remap) {
        // Code a build script generated is compiled from OUT_DIR, and a
        // panic in it would otherwise name the script's run, which refers
        // to the script, the compiler and everything the script was built
        // from.
        argv.extend([
            "--remap-path-prefix".to_string(),
            format!("{}={}/out", script.out_dir, node.remap_to),
        ]);
    }

    let mut env = node.env.clone();
    env.insert("CARGO".to_string(), cargo.to_string());
    env.insert("CARGO_MANIFEST_DIR".to_string(), pkg_root.clone());
    env.insert(
        "CARGO_MANIFEST_PATH".to_string(),
        join(&pkg_root, "Cargo.toml"),
    );
    if let Some(script) = script {
        env.insert("OUT_DIR".to_string(), script.out_dir.clone());
        env.extend(script.env.iter().cloned());
    }

    // What dependents need. A proc macro is loaded by rustc on its own, and
    // nothing links an executable.
    let lib_dir = out_dir.clone();
    let transitive = match node.kind.as_str() {
        "lib" => {
            let mut all = vec![lib_dir];
            push_unique(&mut all, search);
            all
        }
        "proc-macro" => vec![lib_dir],
        _ => Vec::new(),
    };
    // A test keeps its search paths for its run: cargo lets a test find
    // the dynamic libraries build scripts made.
    if !matches!(node.kind.as_str(), "lib" | "test") {
        native.clear();
        native_external.clear();
    }
    if node.kind != "lib" {
        cdylib_link_args.clear();
    }

    Invocation {
        argv,
        env,
        cwd,
        out_dir,
        transitive,
        native,
        native_external,
        cdylib_link_args,
    }
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
            "targetKind": kind,
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
            pkg: PkgRef {
                name: name.to_string(),
                version: "1.0.0".to_string(),
            },
            crate_name: name.to_string(),
            artifact: format!("/nix/store/{name}/lib/lib{name}-x.rlib"),
            transitive: transitive.iter().map(|s| s.to_string()).collect(),
            native: native.iter().map(|s| s.to_string()).collect(),
            native_external: vec![],
            cdylib_link_args: vec![],
            argv: vec![],
            env: BTreeMap::new(),
            override_env: BTreeMap::new(),
            cwd: String::new(),
        }
    }

    #[test]
    fn registry_library_with_dependencies() {
        let deps = vec![
            (
                "a".to_string(),
                record(
                    "lib",
                    "a",
                    &["/nix/store/a/lib", "/nix/store/shared/lib"],
                    &["native=/n/a"],
                ),
            ),
            (
                "renamed".to_string(),
                record(
                    "lib",
                    "b",
                    &["/nix/store/b/lib", "/nix/store/shared/lib"],
                    &[],
                ),
            ),
            (
                "mac".to_string(),
                record("proc-macro", "mac", &["/nix/store/mac/lib"], &[]),
            ),
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
            [
                "/out/lib",
                "/nix/store/a/lib",
                "/nix/store/shared/lib",
                "/nix/store/b/lib",
                "/nix/store/mac/lib"
            ]
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
                LinkArg {
                    target: "all".to_string(),
                    arg: "-Wl,-all".to_string(),
                },
                LinkArg {
                    target: "bins".to_string(),
                    arg: "-Wl,-bins".to_string(),
                },
                LinkArg {
                    target: "bin:the-crate".to_string(),
                    arg: "-Wl,-mine".to_string(),
                },
                LinkArg {
                    target: "bin:other".to_string(),
                    arg: "-Wl,-other".to_string(),
                },
                LinkArg {
                    target: "examples".to_string(),
                    arg: "-Wl,-examples".to_string(),
                },
                LinkArg {
                    target: "cdylib".to_string(),
                    arg: "-Wl,-cdylib".to_string(),
                },
            ],
            cfgs: vec!["has_foo".to_string()],
            check_cfgs: vec!["cfg(has_foo)".to_string()],
            env: vec![("FROM_SCRIPT".to_string(), "1".to_string())],
            ..RunRecord::default()
        }
    }

    #[test]
    fn library_takes_what_its_build_script_printed() {
        let deps = vec![(
            "a".to_string(),
            record("lib", "a", &["/nix/store/a/lib"], &["native=/n/a"]),
        )];
        let inv = plan(
            "/rustc",
            "/cargo",
            &node("lib", false),
            "/out",
            &deps,
            Some(&script()),
        );
        let tail = inv.argv.join(" ");
        let tail = tail.split("--cap-lints allow ").nth(1).unwrap();
        assert_eq!(
            tail,
            "-L native=/nix/store/run/out -L native=/n/a -l static=foo -C link-arg=-Wl,-all \
             --cfg has_foo --check-cfg cfg(has_foo) --remap-path-prefix /nix/store/src=pkg-1.0.0 \
             --remap-path-prefix /nix/store/run/out=pkg-1.0.0/out"
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
            args.contains(
                "-C link-arg=-Wl,-all -C link-arg=-Wl,-bins -C link-arg=-Wl,-mine --cfg has_foo"
            ),
            "{args}"
        );
        assert_eq!(inv.out_dir, "/out/bin");
        // Nothing links an executable.
        assert!(inv.transitive.is_empty() && inv.native.is_empty());
    }

    // Cargo puts what a script built ahead of what it found elsewhere, for
    // the unit's own script and for its dependencies' alike, and takes the
    // dependencies by package.
    #[test]
    fn search_paths_inside_out_dir_come_first() {
        let mut own = script();
        own.library_paths = vec![
            "/usr/lib/found".to_string(),
            "native=/nix/store/run/out".to_string(),
            "/nix/store/run/out/sub".to_string(),
            "framework=/Frameworks".to_string(),
        ];
        let mut zed = record("lib", "zed", &["/nix/store/zed/lib"], &["native=/z/out"]);
        zed.native_external = vec!["/z/found".to_string()];
        let mut abc = record("lib", "abc", &["/nix/store/abc/lib"], &["native=/a/out"]);
        abc.native_external = vec!["/a/found".to_string(), "/z/found".to_string()];
        let deps = vec![("zed".to_string(), zed), ("abc".to_string(), abc)];
        let inv = plan(
            "/rustc",
            "/cargo",
            &node("lib", false),
            "/out",
            &deps,
            Some(&own),
        );
        let searched: Vec<&str> = inv
            .argv
            .windows(2)
            .filter(|pair| pair[0] == "-L" && !pair[1].starts_with("dependency="))
            .map(|pair| pair[1].as_str())
            .collect();
        assert_eq!(
            searched,
            [
                "native=/nix/store/run/out",
                "/nix/store/run/out/sub",
                "native=/a/out",
                "native=/z/out",
                "/usr/lib/found",
                "framework=/Frameworks",
                "/a/found",
                "/z/found"
            ]
        );
        // Dependents get the two classes apart, so they can keep the order.
        assert_eq!(
            inv.native,
            [
                "native=/nix/store/run/out",
                "/nix/store/run/out/sub",
                "native=/a/out",
                "native=/z/out"
            ]
        );
        assert_eq!(
            inv.native_external,
            [
                "/usr/lib/found",
                "framework=/Frameworks",
                "/a/found",
                "/z/found"
            ]
        );
    }

    #[test]
    fn cdylib_arguments_reach_cdylibs_that_depend_on_the_package() {
        let mut dep = record("lib", "a", &["/nix/store/a/lib"], &[]);
        dep.cdylib_link_args = vec!["-Wl,-from-a".to_string()];
        let deps = vec![("a".to_string(), dep)];

        // A plain library passes them on and does not use them.
        let inv = plan(
            "/rustc",
            "/cargo",
            &node("lib", false),
            "/out",
            &deps,
            Some(&script()),
        );
        assert!(!inv.argv.join(" ").contains("-cdylib"), "{:?}", inv.argv);
        assert!(!inv.argv.join(" ").contains("-from-a"));
        assert_eq!(inv.cdylib_link_args, ["-Wl,-cdylib", "-Wl,-from-a"]);

        // A cdylib takes its own script's once and its dependencies' too.
        let mut cdylib = node("lib", false);
        cdylib.rustc_args = vec![
            "--crate-type".into(),
            "cdylib".into(),
            "--crate-type".into(),
            "rlib".into(),
        ];
        let inv = plan("/rustc", "/cargo", &cdylib, "/out", &deps, Some(&script()));
        let args = inv.argv.join(" ");
        assert!(
            args.contains(
                "-C link-arg=-Wl,-all -C link-arg=-Wl,-cdylib -C link-arg=-Wl,-from-a --cfg"
            ),
            "{args}"
        );

        // Nothing links an executable, so it hands nothing on.
        let inv = plan(
            "/rustc",
            "/cargo",
            &node("bin", true),
            "/out",
            &deps,
            Some(&script()),
        );
        assert!(inv.cdylib_link_args.is_empty());
        assert!(!inv.argv.join(" ").contains("-from-a"));
    }

    // A test is compiled where the caller says: in a copy of the source,
    // into cargo's directory, with the paths left as they are.
    #[test]
    fn a_test_is_planned_in_a_copy_of_its_source() {
        let deps = vec![(
            "pkg".to_string(),
            record("lib", "pkg", &["/nix/store/pkg/lib"], &[]),
        )];
        let mut n = node("test", true);
        n.src_path = "tests/cli.rs".to_string();
        let inv = plan_at(
            "/rustc",
            "/cargo",
            &n,
            "/build/source",
            "/build/source/target/release/deps",
            false,
            &deps,
            Some(&script()),
        );
        let args = inv.argv.join(" ");
        assert_eq!(inv.cwd, "/build/source");
        assert_eq!(inv.argv[4], "crates/pkg/tests/cli.rs");
        assert!(
            args.contains("--out-dir /build/source/target/release/deps"),
            "{args}"
        );
        assert!(!args.contains("--remap-path-prefix"), "{args}");
        assert!(!args.contains("/nix/store/src"), "{args}");
        assert_eq!(inv.env["CARGO_MANIFEST_DIR"], "/build/source/crates/pkg");
        assert_eq!(inv.env["OUT_DIR"], "/nix/store/run/out");
        assert!(inv.transitive.is_empty());
    }

    // What a script asks of tests, binaries or examples goes by the target,
    // whatever it is built as.
    #[test]
    fn link_arguments_follow_the_target_of_a_test() {
        let mut own = script();
        own.link_args.push(LinkArg {
            target: "tests".to_string(),
            arg: "-Wl,-tests".to_string(),
        });
        let link_args = |target_kind: &str| -> Vec<String> {
            let mut n = node("test", true);
            n.target_kind = target_kind.to_string();
            plan("/rustc", "/cargo", &n, "/out", &[], Some(&own))
                .argv
                .iter()
                .filter_map(|arg| arg.strip_prefix("link-arg=-Wl,-").map(String::from))
                .collect()
        };
        assert_eq!(link_args("test"), ["all", "tests"]);
        assert_eq!(link_args("lib"), ["all"]);
        assert_eq!(link_args("bin"), ["all", "bins", "mine"]);
        assert_eq!(link_args("example"), ["all", "examples"]);
        // A test keeps the search paths for its run.
        let inv = plan(
            "/rustc",
            "/cargo",
            &node("test", true),
            "/out",
            &[],
            Some(&own),
        );
        assert_eq!(inv.native, ["native=/nix/store/run/out"]);
        assert!(inv.cdylib_link_args.is_empty());
    }

    // The planned environment is cargo's. An override is laid over it when
    // rustc is started, and recorded apart.
    #[test]
    fn override_env_is_not_part_of_what_cargo_would_set() {
        let mut n = node("lib", false);
        n.override_env = BTreeMap::from([("EXTRA".to_string(), "1".to_string())]);
        let inv = plan("/rustc", "/cargo", &n, "/out", &[], None);
        assert!(!inv.env.contains_key("EXTRA"));
        assert_eq!(inv.env["CARGO_PKG_NAME"], "pkg");
    }
}
