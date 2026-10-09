//! The `run-build-script` subcommand: runs one build script as cargo runs
//! it and records what it printed.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::localsrc::join;
use crate::node::{self, Attrs, CompileRecord, LinkArg, RunNode, RunRecord};
use crate::Result;

/// The directives a build script printed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ScriptOutput {
    pub library_paths: Vec<String>,
    pub library_links: Vec<String>,
    pub link_args: Vec<LinkArg>,
    pub cfgs: Vec<String>,
    pub check_cfgs: Vec<String>,
    pub env: Vec<(String, String)>,
    pub metadata: Vec<(String, String)>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

/// The keys the one-colon form reserves. Anything else in that form is
/// metadata for dependents.
const OLD_RESERVED: &[&str] = &[
    "rustc-flags",
    "rustc-link-lib",
    "rustc-link-search",
    "rustc-link-arg-cdylib",
    "rustc-cdylib-link-arg",
    "rustc-link-arg-bins",
    "rustc-link-arg-bin",
    "rustc-link-arg-tests",
    "rustc-link-arg-benches",
    "rustc-link-arg-examples",
    "rustc-link-arg",
    "rustc-cfg",
    "rustc-check-cfg",
    "rustc-env",
    "warning",
    "rerun-if-changed",
    "rerun-if-env-changed",
];

fn key_value<'a>(line: &str, data: &'a str, form: &str) -> Result<(&'a str, &'a str)> {
    match data.split_once('=') {
        Some((key, value)) => Ok((key, value.trim_end())),
        None => {
            Err(format!("invalid build script output `{line}`: expected `{form}KEY=VALUE`").into())
        }
    }
}

/// Reads the `cargo::` and `cargo:` lines of a build script's stdout, as
/// cargo reads them.
pub fn parse_output(stdout: &str) -> Result<ScriptOutput> {
    let mut out = ScriptOutput::default();
    // Cargo trims each line before it looks for the prefix.
    for line in stdout.lines().map(str::trim) {
        let (key, value) = if let Some(data) = line.strip_prefix("cargo::") {
            key_value(line, data, "cargo::")?
        } else if let Some(data) = line.strip_prefix("cargo:") {
            if OLD_RESERVED.iter().any(|key| {
                data.strip_prefix(key)
                    .is_some_and(|rest| rest.starts_with('='))
            }) {
                key_value(line, data, "cargo:")?
            } else {
                ("metadata", data)
            }
        } else {
            continue;
        };

        let mut link_arg = |target: &str, arg: &str| {
            out.link_args.push(LinkArg {
                target: target.to_string(),
                arg: arg.to_string(),
            })
        };
        match key {
            "rustc-flags" => {
                let (paths, links) =
                    parse_rustc_flags(value).map_err(|err| format!("{err} in `{line}`"))?;
                out.library_paths.extend(paths);
                out.library_links.extend(links);
            }
            "rustc-link-lib" => out.library_links.push(value.to_string()),
            "rustc-link-search" => out.library_paths.push(value.to_string()),
            "rustc-link-arg-cdylib" | "rustc-cdylib-link-arg" => link_arg("cdylib", value),
            "rustc-link-arg-bins" => link_arg("bins", value),
            "rustc-link-arg-bin" => {
                let (bin, arg) = value.split_once('=').ok_or_else(|| {
                    format!("invalid build script output `{line}`: expected `BIN=ARG`")
                })?;
                link_arg(&format!("bin:{bin}"), arg);
            }
            "rustc-link-arg-tests" => link_arg("tests", value),
            "rustc-link-arg-benches" => link_arg("benches", value),
            "rustc-link-arg-examples" => link_arg("examples", value),
            "rustc-link-arg" => link_arg("all", value),
            "rustc-cfg" => out.cfgs.push(value.to_string()),
            "rustc-check-cfg" => out.check_cfgs.push(value.to_string()),
            "rustc-env" => {
                let (name, val) = value.split_once('=').ok_or_else(|| {
                    format!("invalid build script output `{line}`: expected `VAR=VALUE`")
                })?;
                out.env.push((name.to_string(), val.to_string()));
            }
            "warning" => out.warnings.push(value.to_string()),
            "error" => out.errors.push(value.to_string()),
            // Nix decides when a derivation is rebuilt.
            "rerun-if-changed" | "rerun-if-env-changed" => {}
            "metadata" => {
                let (name, val) = value.split_once('=').ok_or_else(|| {
                    format!("invalid build script output `{line}`: expected `KEY=VALUE` metadata")
                })?;
                out.metadata
                    .push((name.to_string(), val.trim_end().to_string()));
            }
            other => {
                return Err(
                    format!("unknown build script instruction `{other}` in `{line}`").into(),
                )
            }
        }
    }
    Ok(out)
}

/// `rustc-flags` takes only `-l` and `-L`, attached to their value or not.
fn parse_rustc_flags(value: &str) -> std::result::Result<(Vec<String>, Vec<String>), String> {
    let (mut paths, mut links) = (Vec::new(), Vec::new());
    let mut words = value.split_whitespace();
    while let Some(word) = words.next() {
        let (flag, attached) = match word {
            w if w.starts_with("-l") => ("-l", &w[2..]),
            w if w.starts_with("-L") => ("-L", &w[2..]),
            other => {
                return Err(format!(
                    "only -l and -L are allowed in rustc-flags, found `{other}`"
                ))
            }
        };
        let arg = if attached.is_empty() {
            words
                .next()
                .ok_or_else(|| format!("`{flag}` has no value"))?
        } else {
            attached
        };
        if flag == "-l" {
            links.push(arg.to_string())
        } else {
            paths.push(arg.to_string())
        }
    }
    Ok((paths, links))
}

/// A feature or cfg name as cargo puts it in a variable name.
pub fn envify(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c == '-' {
                '_'
            } else {
                c.to_ascii_uppercase()
            }
        })
        .collect()
}

/// The `CARGO_CFG_*` variables: what `rustc --print=cfg` printed, plus the
/// features and the profile's debug assertions, which rustc cannot know.
pub fn cfg_env(
    print_cfg: &str,
    features: &[String],
    debug_assertions: bool,
) -> BTreeMap<String, String> {
    let mut cfgs: BTreeMap<String, Vec<String>> = BTreeMap::new();
    cfgs.insert("feature".to_string(), features.to_vec());
    for line in print_cfg
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        match line.split_once('=') {
            Some((key, value)) => {
                let value = value
                    .strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .unwrap_or(value);
                cfgs.entry(key.to_string())
                    .or_default()
                    .push(value.to_string());
            }
            // rustc reports its own default here, not the profile's setting.
            None if line == "debug_assertions" => {}
            None => {
                cfgs.entry(line.to_string()).or_default();
            }
        }
    }
    if debug_assertions {
        cfgs.insert("debug_assertions".to_string(), Vec::new());
    }
    cfgs.into_iter()
        .map(|(key, values)| (format!("CARGO_CFG_{}", envify(&key)), values.join(",")))
        .collect()
}

pub fn run() -> Result<()> {
    let attrs: Attrs<RunNode> = node::load_attrs()?;
    let node = &attrs.node;
    let out = &attrs.outputs.out;
    let what = format!("the build script of {} {}", node.pkg.name, node.pkg.version);

    let script: CompileRecord = node::read_record(&node.script)?;
    let out_dir = format!("{out}/out");
    fs::create_dir_all(&out_dir)?;
    let pkg_root = join(&node.src, &node.manifest_dir);

    let print_cfg = Command::new(&attrs.rustc)
        .arg("--print=cfg")
        .output()
        .map_err(|err| format!("running {} --print=cfg: {err}", attrs.rustc))?;
    if !print_cfg.status.success() {
        return Err(format!("{} --print=cfg failed", attrs.rustc).into());
    }

    let jobs = match std::env::var("NIX_BUILD_CORES")
        .ok()
        .and_then(|n| n.parse::<usize>().ok())
    {
        Some(n) if n > 0 => n,
        _ => std::thread::available_parallelism().map_or(1, usize::from),
    };

    let mut env = node.env.clone();
    env.extend(cfg_env(
        &String::from_utf8_lossy(&print_cfg.stdout),
        &node.features,
        node.debug_assertions,
    ));
    for feature in &node.features {
        env.insert(
            format!("CARGO_FEATURE_{}", envify(feature)),
            "1".to_string(),
        );
    }
    for dep in &node.links_deps {
        let record: RunRecord = node::read_record(&dep.path)?;
        for (key, value) in &record.metadata {
            env.insert(
                format!("DEP_{}_{}", envify(&dep.links), envify(key)),
                value.clone(),
            );
        }
    }
    env.insert("OUT_DIR".to_string(), out_dir.clone());
    env.insert("CARGO_MANIFEST_DIR".to_string(), pkg_root.clone());
    env.insert(
        "CARGO_MANIFEST_PATH".to_string(),
        join(&pkg_root, "Cargo.toml"),
    );
    env.insert("NUM_JOBS".to_string(), jobs.to_string());
    env.insert("RUSTC".to_string(), attrs.rustc.clone());
    env.insert(
        "RUSTDOC".to_string(),
        Path::new(&attrs.rustc)
            .with_file_name("rustdoc")
            .to_string_lossy()
            .into_owned(),
    );
    env.insert("CARGO".to_string(), attrs.cargo.clone());
    env.insert("CARGO_ENCODED_RUSTFLAGS".to_string(), String::new());

    let output = Command::new(&script.artifact)
        .current_dir(&pkg_root)
        .envs(&env)
        .envs(&node.override_env)
        .env_remove("RUSTFLAGS")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|err| format!("running {what} ({}): {err}", script.artifact))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    fs::write(format!("{out}/output"), &stdout)?;
    if !output.status.success() {
        eprint!("{stdout}");
        return Err(format!("{what} failed: {}", output.status).into());
    }

    let parsed = parse_output(&stdout).map_err(|err| format!("{what}: {err}"))?;
    // As under cargo, a foreign package's warnings are not the builder's
    // concern; they stay in `output`.
    if node.local {
        for warning in &parsed.warnings {
            eprintln!("warning: {}@{}: {warning}", node.pkg.name, node.pkg.version);
        }
    }
    if !parsed.errors.is_empty() {
        for error in &parsed.errors {
            eprintln!("error: {}@{}: {error}", node.pkg.name, node.pkg.version);
        }
        return Err(format!("{what} reported an error").into());
    }

    node::write_record(
        out,
        &RunRecord {
            kind: "run-build-script".to_string(),
            pkg: Some(node.pkg.clone()),
            out_dir,
            library_paths: parsed.library_paths,
            library_links: parsed.library_links,
            link_args: parsed.link_args,
            cfgs: parsed.cfgs,
            check_cfgs: parsed.check_cfgs,
            env: parsed.env,
            metadata: parsed.metadata,
            argv: vec![script.artifact],
            env_recorded: env,
            override_env: node.override_env.clone(),
            cwd: pkg_root,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn both_forms_of_every_directive() {
        for prefix in ["cargo::", "cargo:"] {
            let stdout = [
                "rustc-link-lib=static=foo",
                "rustc-link-search=native=/out",
                "rustc-flags=-l bar -L /flags -lbaz -L/attached",
                "rustc-cfg=has_foo",
                "rustc-check-cfg=cfg(has_foo)",
                "rustc-env=NOTE=a=b",
                "rustc-link-arg=-all",
                "rustc-link-arg-bins=-bins",
                "rustc-link-arg-bin=tool=-one=1",
                "rustc-link-arg-cdylib=-cdylib",
                "rustc-cdylib-link-arg=-cdylib-old",
                "rustc-link-arg-tests=-tests",
                "rustc-link-arg-benches=-benches",
                "rustc-link-arg-examples=-examples",
                "warning=careful",
                "rerun-if-changed=build.rs",
                "rerun-if-env-changed=CC",
            ]
            .map(|line| format!("{prefix}{line}"))
            .join("\n");
            let out = parse_output(&stdout).unwrap();
            assert_eq!(out.library_links, ["static=foo", "bar", "baz"], "{prefix}");
            assert_eq!(out.library_paths, ["native=/out", "/flags", "/attached"]);
            assert_eq!(out.cfgs, ["has_foo"]);
            assert_eq!(out.check_cfgs, ["cfg(has_foo)"]);
            assert_eq!(out.env, pairs(&[("NOTE", "a=b")]));
            let args: Vec<String> = out
                .link_args
                .iter()
                .map(|a| format!("{}={}", a.target, a.arg))
                .collect();
            assert_eq!(
                args,
                [
                    "all=-all",
                    "bins=-bins",
                    "bin:tool=-one=1",
                    "cdylib=-cdylib",
                    "cdylib=-cdylib-old",
                    "tests=-tests",
                    "benches=-benches",
                    "examples=-examples"
                ]
            );
            assert_eq!(out.warnings, ["careful"]);
            assert!(out.metadata.is_empty() && out.errors.is_empty());
        }
    }

    #[test]
    fn metadata_in_both_forms() {
        let out =
            parse_output("cargo::metadata=answer=42\ncargo:include=/some/dir \ncargo:root=/r=x")
                .unwrap();
        assert_eq!(
            out.metadata,
            pairs(&[("answer", "42"), ("include", "/some/dir"), ("root", "/r=x")])
        );
    }

    #[test]
    fn error_is_a_directive_only_in_the_new_form() {
        assert_eq!(
            parse_output("cargo::error=broken").unwrap().errors,
            ["broken"]
        );
        let old = parse_output("cargo:error=broken").unwrap();
        assert!(old.errors.is_empty());
        assert_eq!(old.metadata, pairs(&[("error", "broken")]));
    }

    #[test]
    fn noise_is_ignored() {
        let out = parse_output(
            "compiling foo.c\nsee cargo:rustc-cfg=inline\ncargo is nice\n\nTARGET = Some(x)",
        )
        .unwrap();
        assert_eq!(out, ScriptOutput::default());
    }

    // Cargo honours a directive that is indented.
    #[test]
    fn indented_directives_count() {
        let out =
            parse_output("  cargo:rustc-cfg=spaces\n\tcargo::rustc-env=K=V  \r\n cargo:key=value")
                .unwrap();
        assert_eq!(out.cfgs, ["spaces"]);
        assert_eq!(out.env, pairs(&[("K", "V")]));
        assert_eq!(out.metadata, pairs(&[("key", "value")]));
    }

    #[test]
    fn a_reserved_key_must_be_whole() {
        // `rustc-cfgs` is not `rustc-cfg`: in the old form it is metadata.
        let out = parse_output("cargo:rustc-cfgs=x").unwrap();
        assert!(out.cfgs.is_empty());
        assert_eq!(out.metadata, pairs(&[("rustc-cfgs", "x")]));
    }

    #[test]
    fn malformed_directives_are_errors() {
        for bad in [
            "cargo::unknown-thing=1",
            "cargo::rustc-cfg",
            "cargo::rustc-env=NOVALUE",
            "cargo::rustc-link-arg-bin=noarg",
            "cargo::rustc-flags=-O2",
            "cargo::rustc-flags=-l",
            "cargo::metadata=novalue",
            "cargo:novalue",
        ] {
            assert!(parse_output(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn cfg_variables() {
        let print = "debug_assertions\npanic=\"unwind\"\ntarget_arch=\"aarch64\"\ntarget_feature=\"aes\"\n\
                     target_feature=\"neon\"\ntarget_has_atomic=\"8\"\ntarget_has_atomic=\"ptr\"\nunix\n";
        let env = cfg_env(print, &["default".to_string(), "std".to_string()], false);
        assert_eq!(env["CARGO_CFG_TARGET_ARCH"], "aarch64");
        assert_eq!(env["CARGO_CFG_TARGET_FEATURE"], "aes,neon");
        assert_eq!(env["CARGO_CFG_TARGET_HAS_ATOMIC"], "8,ptr");
        assert_eq!(env["CARGO_CFG_UNIX"], "");
        assert_eq!(env["CARGO_CFG_PANIC"], "unwind");
        assert_eq!(env["CARGO_CFG_FEATURE"], "default,std");
        // rustc's own answer is not the profile's.
        assert!(!env.contains_key("CARGO_CFG_DEBUG_ASSERTIONS"));
        assert_eq!(cfg_env(print, &[], true)["CARGO_CFG_DEBUG_ASSERTIONS"], "");
    }

    #[test]
    fn names_in_variables() {
        assert_eq!(envify("zdict_builder"), "ZDICT_BUILDER");
        assert_eq!(envify("my-feature"), "MY_FEATURE");
        assert_eq!(envify("rostnixnative"), "ROSTNIXNATIVE");
    }
}
