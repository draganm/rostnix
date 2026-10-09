//! The `test` subcommand: one test executable, compiled and run where and
//! how `cargo test` would.
//!
//! Under cargo a test is compiled in its package's own directory and runs
//! there, from `target/<profile>/deps`, with its package's binaries and
//! examples in the directories beside. It may write to all of that, and
//! what it was told when it was compiled, `CARGO_MANIFEST_DIR` above all,
//! still holds when it runs. So the derivation makes a writable copy of the
//! source with such a target directory in it, compiles the test into that,
//! and runs it there. A test compiled in the store would find its fixtures
//! read-only, and copies it makes of them too.

use std::collections::BTreeMap;
use std::fs;
use std::io::{PipeReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use crate::compile::{self, Invocation};
use crate::node::{self, Attrs, CompileRecord, RunRecord, TestNode, TestRecord, RUN_RECORD_FILE};
use crate::Result;

/// Where everything of a test is, in the copy of its source.
#[derive(Debug)]
pub struct Layout {
    /// The copy of the source.
    pub source: PathBuf,
    /// The workspace's target directory.
    pub target_dir: PathBuf,
    /// The directory rustc writes the test executable into.
    pub deps_dir: PathBuf,
    /// The test executable.
    pub exe: PathBuf,
    /// The package directory, where the test runs.
    pub cwd: PathBuf,
    /// Each binary and example the test finds beside itself: where its unit
    /// left it, and where it goes.
    pub executables: Vec<Placed>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    pub from: String,
    pub to: PathBuf,
    pub is_example: bool,
}

/// The variable the dynamic linker reads its search path from.
fn dylib_var() -> &'static str {
    if cfg!(target_os = "macos") {
        "DYLD_FALLBACK_LIBRARY_PATH"
    } else {
        "LD_LIBRARY_PATH"
    }
}

/// The directory of a `-L` value, which may start with a kind such as
/// `native=`.
fn search_dir(value: &str) -> &str {
    match value.split_once('=') {
        Some(("native" | "crate" | "dependency" | "framework" | "all", dir)) => dir,
        _ => value,
    }
}

/// The directory `rel` below `dir`, which is `dir` itself for `""`. Joining
/// an empty path would leave a trailing slash, and a test would see it in
/// `CARGO_MANIFEST_DIR`.
fn below(dir: &Path, rel: &str) -> PathBuf {
    if rel.is_empty() {
        dir.to_path_buf()
    } else {
        dir.join(rel)
    }
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Cargo's target directory for the test, at the workspace root of the
/// copy: `target/<profile>/deps/<crate>-<metadata>`, binaries one directory
/// up, examples beside `deps`.
pub fn layout(node: &TestNode, source: &Path, executables: &[CompileRecord]) -> Layout {
    let target_dir = below(source, &node.compile.work_dir).join("target");
    let profile_dir = target_dir.join(&node.profile_dir);
    let deps_dir = profile_dir.join("deps");
    let exe = deps_dir.join(format!(
        "{}-{}",
        node.compile.crate_name, node.compile.metadata
    ));
    let executables = executables
        .iter()
        .map(|record| {
            let name = record.artifact.rsplit('/').next().unwrap_or_default();
            let is_example = record.kind == "example";
            let dir = if is_example {
                profile_dir.join("examples")
            } else {
                profile_dir.clone()
            };
            Placed {
                from: record.artifact.clone(),
                to: dir.join(name),
                is_example,
            }
        })
        .collect();
    Layout {
        source: source.to_path_buf(),
        target_dir,
        deps_dir,
        exe,
        cwd: below(source, &node.compile.manifest_dir),
        executables,
    }
}

/// What an integration test or a bench is told about its package's
/// binaries, when it is compiled and again when it runs.
fn binaries(node: &TestNode, layout: &Layout) -> Vec<(String, String)> {
    if !matches!(node.compile.target_kind.as_str(), "test" | "bench") {
        return Vec::new();
    }
    layout
        .executables
        .iter()
        .filter(|placed| !placed.is_example)
        .map(|placed| {
            let name = placed.to.file_name().unwrap_or_default().to_string_lossy();
            (format!("CARGO_BIN_EXE_{name}"), text(&placed.to))
        })
        .collect()
}

/// The rustc invocation that builds the test, in the copy of its source.
pub fn compile_plan(
    rustc: &str,
    cargo: &str,
    node: &TestNode,
    layout: &Layout,
    deps: &[(String, CompileRecord)],
    script: Option<&RunRecord>,
) -> Invocation {
    let mut inv = compile::plan_at(
        rustc,
        cargo,
        &node.compile,
        &text(&layout.source),
        &text(&layout.deps_dir),
        false,
        deps,
        script,
    );
    if matches!(node.compile.target_kind.as_str(), "test" | "bench") {
        inv.env.extend(binaries(node, layout));
        inv.env.insert(
            "CARGO_TARGET_TMPDIR".to_string(),
            text(&layout.target_dir.join("tmp")),
        );
    }
    inv
}

/// The environment cargo gives a running test. `native` holds the library
/// search paths of the build scripts in the test's closure; `target_libdir`
/// is where the compiler keeps the standard library, which a test of a
/// proc macro links dynamically; `inherited_dylib_path` is what the library
/// path variable holds already.
pub fn run_env(
    cargo: &str,
    node: &TestNode,
    layout: &Layout,
    native: &[String],
    script: Option<&RunRecord>,
    target_libdir: &str,
    inherited_dylib_path: &str,
) -> BTreeMap<String, String> {
    // The package's CARGO_PKG_* variables. What else rustc was given, such
    // as CARGO_CRATE_NAME, is not passed on to the test.
    let mut env: BTreeMap<String, String> = node
        .compile
        .env
        .iter()
        .filter(|(key, _)| key.starts_with("CARGO_PKG_"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    env.insert("CARGO".to_string(), cargo.to_string());
    env.insert("CARGO_MANIFEST_DIR".to_string(), text(&layout.cwd));
    env.insert(
        "CARGO_MANIFEST_PATH".to_string(),
        text(&layout.cwd.join("Cargo.toml")),
    );
    if let Some(script) = script {
        env.insert("OUT_DIR".to_string(), script.out_dir.clone());
        env.extend(script.env.iter().cloned());
    }
    env.extend(binaries(node, layout));

    // Cargo's search path for dynamic libraries: what build scripts built,
    // the target directory, and the compiler's own libraries.
    let profile_dir = layout.deps_dir.parent().unwrap_or(&layout.deps_dir);
    let mut search: Vec<String> = native
        .iter()
        .map(|path| search_dir(path).to_string())
        .collect();
    search.push(text(profile_dir));
    search.push(text(&layout.deps_dir));
    search.push(target_libdir.to_string());
    if !inherited_dylib_path.is_empty() {
        search.push(inherited_dylib_path.to_string());
    } else if cfg!(target_os = "macos") {
        // What the linker falls back to when the variable is not set.
        search.extend(["/usr/local/lib".to_string(), "/usr/lib".to_string()]);
    }
    env.insert(dylib_var().to_string(), search.join(":"));
    env
}

/// Passes on what a child writes to the pipe, to each of `sinks`, until the
/// child has exited, and returns how it exited.
///
/// The pipe is not read to its end. A test may leave a process behind that
/// still holds the pipe, a server it started and did not stop, and the end
/// would not come before that process exits. Cargo returns when the test
/// does, and so does this: once the child is gone, what is still in the
/// pipe is passed on, and whoever holds it is left to the builder's end.
pub fn tee(
    child: &mut Child,
    mut reader: PipeReader,
    sinks: &mut [&mut dyn Write],
) -> std::io::Result<ExitStatus> {
    let (sender, receiver) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buffer) {
            if n == 0 || sender.send(buffer[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut pass = |chunk: Vec<u8>| -> std::io::Result<()> {
        for sink in sinks.iter_mut() {
            sink.write_all(&chunk)?;
            sink.flush()?;
        }
        Ok(())
    };
    let mut exited = None;
    loop {
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => pass(chunk)?,
            // Every holder of the pipe has closed it.
            Err(RecvTimeoutError::Disconnected) => break,
            // Nothing for a while. If the child is gone, nothing it wrote
            // is still on its way.
            Err(RecvTimeoutError::Timeout) => {
                if exited.is_some() {
                    break;
                }
                exited = child.try_wait()?;
            }
        }
    }
    match exited {
        Some(status) => Ok(status),
        None => child.wait(),
    }
}

/// Copies a tree out of the store and makes the copy writable.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(from)?, to)?;
    } else if meta.is_dir() {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
    } else {
        copy_writable(from, to)?;
    }
    Ok(())
}

/// Copies a file, which keeps whether it is executable, and lets its owner
/// write to the copy.
fn copy_writable(from: &Path, to: &Path) -> Result<()> {
    fs::copy(from, to)
        .map_err(|err| format!("copying {} to {}: {err}", from.display(), to.display()))?;
    let mut permissions = fs::metadata(to)?.permissions();
    permissions.set_mode(permissions.mode() | 0o200);
    fs::set_permissions(to, permissions)?;
    Ok(())
}

pub fn run() -> Result<()> {
    let attrs: Attrs<TestNode> = node::load_attrs()?;
    let node = &attrs.node;
    let unit = &node.compile;
    let out = &attrs.outputs.out;
    let what = format!(
        "the test {} of {} {}",
        unit.target_name, unit.pkg.name, unit.pkg.version
    );

    let mut deps = Vec::new();
    for dep in &unit.deps {
        deps.push((
            dep.name.clone(),
            node::read_record::<CompileRecord>(&dep.path)?,
        ));
    }
    let mut executables = Vec::new();
    for path in &node.executables {
        executables.push(node::read_record::<CompileRecord>(path)?);
    }
    let script = unit
        .build_script
        .as_deref()
        .map(node::read_record::<RunRecord>)
        .transpose()?;

    let target_libdir = Command::new(&attrs.rustc)
        .arg("--print=target-libdir")
        .output()
        .map_err(|err| format!("running {} --print=target-libdir: {err}", attrs.rustc))?;
    if !target_libdir.status.success() {
        return Err(format!("{} --print=target-libdir failed", attrs.rustc).into());
    }

    let top = match std::env::var_os("NIX_BUILD_TOP") {
        Some(top) => PathBuf::from(top),
        None => std::env::current_dir()?,
    };
    let source = top.join("source");
    copy_tree(Path::new(&unit.src), &source)
        .map_err(|err| format!("copying {} to {}: {err}", unit.src, source.display()))?;

    let layout = layout(node, &source, &executables);
    fs::create_dir_all(layout.target_dir.join("tmp"))?;
    fs::create_dir_all(&layout.deps_dir)?;
    for placed in &layout.executables {
        if let Some(dir) = placed.to.parent() {
            fs::create_dir_all(dir)?;
        }
        copy_writable(Path::new(&placed.from), &placed.to)?;
    }

    // Compile.
    let inv = compile_plan(
        &attrs.rustc,
        &attrs.cargo,
        node,
        &layout,
        &deps,
        script.as_ref(),
    );
    let status = Command::new(&inv.argv[0])
        .args(&inv.argv[1..])
        .current_dir(&inv.cwd)
        .envs(&inv.env)
        .envs(&unit.override_env)
        .status()
        .map_err(|err| format!("running {}: {err}", inv.argv[0]))?;
    if !status.success() {
        return Err(format!("rustc failed for {what}").into());
    }
    if !layout.exe.is_file() {
        return Err(format!("rustc did not write {}", layout.exe.display()).into());
    }

    // Run.
    let env = run_env(
        &attrs.cargo,
        node,
        &layout,
        &inv.native,
        script.as_ref(),
        String::from_utf8_lossy(&target_libdir.stdout).trim(),
        &std::env::var(dylib_var()).unwrap_or_default(),
    );
    fs::create_dir_all(out)?;
    let mut log = fs::File::create(Path::new(out).join("log"))?;
    let (reader, writer) = std::io::pipe()?;
    let mut child = {
        let mut command = Command::new(&layout.exe);
        command
            .args(&node.args)
            .current_dir(&layout.cwd)
            .envs(&env)
            .envs(&unit.override_env)
            .stdin(Stdio::null())
            .stdout(writer.try_clone()?)
            .stderr(writer);
        // The harness starts as many threads as the machine has cores;
        // the build was given so many.
        let cores = std::env::var("NIX_BUILD_CORES").unwrap_or_default();
        if std::env::var_os("RUST_TEST_THREADS").is_none()
            && !unit.override_env.contains_key("RUST_TEST_THREADS")
            && cores.parse::<usize>().is_ok_and(|n| n > 0)
        {
            command.env("RUST_TEST_THREADS", cores);
        }
        command
            .spawn()
            .map_err(|err| format!("running {what} ({}): {err}", layout.exe.display()))?
        // The command, and with it this process's ends of the pipe, is
        // dropped here, so that reading ends when the test does.
    };

    // Everything the test prints goes to the build log and to the output.
    let mut stdout = std::io::stdout().lock();
    let status = tee(&mut child, reader, &mut [&mut stdout, &mut log])?;
    if !status.success() {
        return Err(format!("{what} failed: {status}").into());
    }

    let mut argv = vec![text(&layout.exe)];
    argv.extend(node.args.iter().cloned());
    node::write_record_as(
        out,
        RUN_RECORD_FILE,
        &TestRecord {
            kind: "test-run".to_string(),
            pkg: unit.pkg.clone(),
            crate_name: unit.crate_name.clone(),
            argv,
            env,
            override_env: unit.override_env.clone(),
            cwd: text(&layout.cwd),
        },
    )?;
    node::write_record(
        out,
        &CompileRecord {
            kind: unit.kind.clone(),
            pkg: unit.pkg.clone(),
            crate_name: unit.crate_name.clone(),
            artifact: text(&layout.exe),
            transitive: inv.transitive,
            native: inv.native,
            native_external: inv.native_external,
            cdylib_link_args: inv.cdylib_link_args,
            argv: inv.argv,
            env: inv.env,
            override_env: unit.override_env.clone(),
            cwd: inv.cwd,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::PkgRef;

    fn node(target_kind: &str) -> TestNode {
        serde_json::from_value(serde_json::json!({
            "kind": "test",
            "targetKind": target_kind,
            "pkg": { "name": "pkg", "version": "1.0.0" },
            "crateName": "cli",
            "targetName": "cli",
            "edition": "2021",
            "src": "/nix/store/src",
            "manifestDir": "ws/crates/pkg",
            "workDir": "ws",
            "srcPath": "tests/cli.rs",
            "local": true,
            "remapTo": "pkg-1.0.0",
            "metadata": "0123456789abcdef",
            "rustcArgs": ["-C", "opt-level=3", "--test"],
            "tailArgs": [],
            "env": { "CARGO_PKG_NAME": "pkg", "CARGO_CRATE_NAME": "cli", "CARGO_PRIMARY_PACKAGE": "1" },
            "deps": [],
            "buildScript": null,
            "passL": false,
            "profileDir": "release",
            "executables": ["/nix/store/tool", "/nix/store/demo"],
            "args": ["--skip", "slow"],
        }))
        .unwrap()
    }

    fn record(kind: &str, artifact: &str) -> CompileRecord {
        CompileRecord {
            kind: kind.to_string(),
            pkg: PkgRef {
                name: "pkg".to_string(),
                version: "1.0.0".to_string(),
            },
            crate_name: "cli".to_string(),
            artifact: artifact.to_string(),
            transitive: vec![],
            native: vec![],
            native_external: vec![],
            cdylib_link_args: vec![],
            argv: vec![],
            env: BTreeMap::new(),
            override_env: BTreeMap::new(),
            cwd: String::new(),
        }
    }

    fn executables() -> Vec<CompileRecord> {
        vec![
            record("bin", "/nix/store/tool/bin/my-tool"),
            record("example", "/nix/store/demo/bin/demo"),
        ]
    }

    fn layout_of(node: &TestNode) -> Layout {
        layout(node, Path::new("/build/source"), &executables())
    }

    // target/<profile>/deps/<test>-<hash>, binaries one directory up,
    // examples beside deps: where cargo puts them, at the workspace root.
    #[test]
    fn layout_is_cargos_target_directory() {
        let layout = layout_of(&node("test"));
        assert_eq!(layout.target_dir, Path::new("/build/source/ws/target"));
        assert_eq!(
            layout.deps_dir,
            Path::new("/build/source/ws/target/release/deps")
        );
        assert_eq!(
            layout.exe,
            Path::new("/build/source/ws/target/release/deps/cli-0123456789abcdef")
        );
        assert_eq!(layout.cwd, Path::new("/build/source/ws/crates/pkg"));
        assert_eq!(
            layout.executables,
            [
                Placed {
                    from: "/nix/store/tool/bin/my-tool".to_string(),
                    to: PathBuf::from("/build/source/ws/target/release/my-tool"),
                    is_example: false,
                },
                Placed {
                    from: "/nix/store/demo/bin/demo".to_string(),
                    to: PathBuf::from("/build/source/ws/target/release/examples/demo"),
                    is_example: true,
                },
            ]
        );
    }

    // The test is compiled in the copy: what it is told then holds when it
    // runs, and what it reads through it can be written to.
    #[test]
    fn integration_test_is_compiled_in_the_copy() {
        let n = node("test");
        let layout = layout_of(&n);
        let inv = compile_plan("/rustc", "/cargo", &n, &layout, &[], None);
        assert_eq!(
            inv.argv.join(" "),
            "/rustc --crate-name cli --edition=2021 crates/pkg/tests/cli.rs -C opt-level=3 --test \
             --emit=link --out-dir /build/source/ws/target/release/deps"
        );
        assert_eq!(inv.cwd, "/build/source/ws");
        assert_eq!(inv.env["CARGO_MANIFEST_DIR"], "/build/source/ws/crates/pkg");
        assert_eq!(
            inv.env["CARGO_BIN_EXE_my-tool"],
            "/build/source/ws/target/release/my-tool"
        );
        assert_eq!(
            inv.env["CARGO_TARGET_TMPDIR"],
            "/build/source/ws/target/tmp"
        );
        assert_eq!(inv.env["CARGO_CRATE_NAME"], "cli");
        assert!(!inv.env.contains_key("CARGO_BIN_EXE_demo"));
    }

    #[test]
    fn integration_test_runs_with_what_cargo_gives_it() {
        let n = node("test");
        let layout = layout_of(&n);
        let env = run_env("/cargo", &n, &layout, &[], None, "/rustc/lib", "");
        let mut names: Vec<&str> = env.keys().map(String::as_str).collect();
        names.retain(|name| *name != dylib_var());
        // Not what only rustc is given: the crate name, the primary package.
        assert_eq!(
            names,
            [
                "CARGO",
                "CARGO_BIN_EXE_my-tool",
                "CARGO_MANIFEST_DIR",
                "CARGO_MANIFEST_PATH",
                "CARGO_PKG_NAME"
            ]
        );
        // The same as it was told when it was compiled.
        let inv = compile_plan("/rustc", "/cargo", &n, &layout, &[], None);
        for name in [
            "CARGO_BIN_EXE_my-tool",
            "CARGO_MANIFEST_DIR",
            "CARGO_MANIFEST_PATH",
        ] {
            assert_eq!(env[name], inv.env[name], "{name}");
        }
        assert_eq!(
            env["CARGO_MANIFEST_PATH"],
            "/build/source/ws/crates/pkg/Cargo.toml"
        );
    }

    // A package at the root of the source: its directory is the copy
    // itself, named without a slash at the end.
    #[test]
    fn package_at_the_root_of_the_source() {
        let mut n = node("test");
        n.compile.manifest_dir = String::new();
        n.compile.work_dir = String::new();
        let layout = layout(&n, Path::new("/build/source"), &[]);
        let env = run_env("/cargo", &n, &layout, &[], None, "/rustc/lib", "");
        assert_eq!(env["CARGO_MANIFEST_DIR"], "/build/source");
        assert_eq!(env["CARGO_MANIFEST_PATH"], "/build/source/Cargo.toml");
        assert_eq!(layout.cwd.to_string_lossy(), "/build/source");
        assert_eq!(layout.target_dir, Path::new("/build/source/target"));
        let inv = compile_plan("/rustc", "/cargo", &n, &layout, &[], None);
        assert_eq!(inv.cwd, "/build/source");
        assert_eq!(inv.argv[4], "tests/cli.rs");
        assert_eq!(inv.env["CARGO_MANIFEST_DIR"], "/build/source");
    }

    // Unit tests are not told where binaries are, and a package with a
    // build script has its OUT_DIR and the script's rustc-env values.
    #[test]
    fn unit_tests_of_a_package_with_a_build_script() {
        let n = node("lib");
        let layout = layout_of(&n);
        let script = RunRecord {
            out_dir: "/nix/store/run/out".to_string(),
            library_paths: vec!["native=/nix/store/run/out".to_string()],
            env: vec![("FROM_SCRIPT".to_string(), "1".to_string())],
            ..RunRecord::default()
        };
        let inv = compile_plan("/rustc", "/cargo", &n, &layout, &[], Some(&script));
        assert!(!inv.env.keys().any(|key| key.starts_with("CARGO_BIN_EXE_")));
        assert!(!inv.env.contains_key("CARGO_TARGET_TMPDIR"));
        assert_eq!(inv.native, ["native=/nix/store/run/out"]);

        let env = run_env(
            "/cargo",
            &n,
            &layout,
            &inv.native,
            Some(&script),
            "/rustc/lib",
            "/inherited",
        );
        assert!(!env.keys().any(|key| key.starts_with("CARGO_BIN_EXE_")));
        assert_eq!(env["OUT_DIR"], "/nix/store/run/out");
        assert_eq!(env["FROM_SCRIPT"], "1");
        // What the script built, the target directory, the compiler's
        // libraries, and what was there before: cargo's order.
        assert_eq!(
            env[dylib_var()],
            "/nix/store/run/out:/build/source/ws/target/release:\
             /build/source/ws/target/release/deps:/rustc/lib:/inherited"
        );
    }

    fn shell(script: &str) -> (Child, PipeReader) {
        let (reader, writer) = std::io::pipe().unwrap();
        let child = Command::new("sh")
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(writer.try_clone().unwrap())
            .stderr(writer)
            .spawn()
            .unwrap();
        (child, reader)
    }

    #[test]
    fn output_and_error_output_are_passed_on_in_order() {
        let (mut child, reader) = shell("echo one; echo two >&2; echo three; exit 3");
        let (mut a, mut b) = (Vec::new(), Vec::new());
        let status = tee(&mut child, reader, &mut [&mut a, &mut b]).unwrap();
        assert_eq!(status.code(), Some(3));
        assert_eq!(String::from_utf8_lossy(&a), "one\ntwo\nthree\n");
        assert_eq!(a, b);
    }

    // A test that starts a server and does not stop it: the process it
    // leaves behind holds the pipe, and the build must not wait for it.
    #[test]
    fn a_process_the_test_leaves_behind_is_not_waited_for() {
        let started = std::time::Instant::now();
        let (mut child, reader) = shell("sleep 20 & echo started; exit 1");
        let mut seen = Vec::new();
        let status = tee(&mut child, reader, &mut [&mut seen]).unwrap();
        assert_eq!(status.code(), Some(1));
        assert_eq!(String::from_utf8_lossy(&seen), "started\n");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "waited {:?} for a process left behind",
            started.elapsed()
        );
    }

    #[test]
    fn a_copied_tree_is_writable() {
        let dir = std::env::temp_dir().join(format!("rostnix-testrun-{}", std::process::id()));
        let (from, to) = (dir.join("from"), dir.join("to"));
        fs::create_dir_all(from.join("sub")).unwrap();
        fs::write(from.join("sub/file"), "data").unwrap();
        std::os::unix::fs::symlink("sub/file", from.join("link")).unwrap();
        // As in the store: nothing may be written.
        for path in [from.join("sub/file"), from.join("sub")] {
            let mut permissions = fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(permissions.mode() & !0o222);
            fs::set_permissions(&path, permissions).unwrap();
        }

        copy_tree(&from, &to).unwrap();
        assert_eq!(fs::read_to_string(to.join("sub/file")).unwrap(), "data");
        assert_eq!(
            fs::read_link(to.join("link")).unwrap(),
            Path::new("sub/file")
        );
        fs::write(to.join("sub/file"), "changed").unwrap();
        fs::write(to.join("sub/new"), "new").unwrap();
        // A copy the test makes of a copied file is writable in its turn.
        fs::copy(to.join("sub/file"), to.join("sub/again")).unwrap();
        fs::write(to.join("sub/again"), "changed").unwrap();

        let mut permissions = fs::metadata(from.join("sub")).unwrap().permissions();
        permissions.set_mode(permissions.mode() | 0o200);
        fs::set_permissions(from.join("sub"), permissions).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }
}
