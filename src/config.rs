//! What the project's cargo configuration says about how to build:
//! `rustflags` and `[env]`.
//!
//! Cargo merges its configuration files itself, and `cargo config get`
//! prints the result. What is read here is that result, by cargo's rules
//! for which flags apply and what a variable's value is.

use std::collections::BTreeMap;
use std::str::FromStr;

use cargo_platform::{Cfg, CfgExpr};
use serde_json::Value;

use crate::localsrc::{join, resolve};
use crate::node::ConfigEnv;

/// A variable of the `[env]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvEntry {
    pub name: String,
    /// The value, for a variable that is not relative.
    pub value: String,
    /// Whether the variable is set although the environment has it.
    pub force: bool,
    /// For a value with `relative = true`: the path it names, from the
    /// source root, with `""` for the root itself.
    pub relative: Option<String>,
    /// Whether cargo ends the path with a slash, which it does when the
    /// value is empty or ends with one: it joins the value to the
    /// directory as it is. Code that appends to the variable counts on it.
    pub slash: bool,
}

/// A value that is a list of strings, or one string to split at whitespace.
fn string_list(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item.as_str().map(String::from))
            .collect(),
        Value::String(text) => text.split_whitespace().map(String::from).collect(),
        _ => Vec::new(),
    }
}

/// The cfgs of `rustc --print=cfg`.
pub fn parse_cfgs(print_cfg: &str) -> Vec<Cfg> {
    print_cfg
        .lines()
        .filter_map(|line| Cfg::from_str(line.trim()).ok())
        .collect()
}

/// The `[target]` tables that apply to the machine `host`: the triple's,
/// then the `cfg(…)` tables that match, in the order of their keys.
/// Without cfgs only the triple's is known to apply.
fn target_tables<'a>(
    config: &'a Value,
    host: &str,
    cfgs: Option<&[Cfg]>,
) -> Vec<(&'a str, &'a Value)> {
    let Some(tables) = config.get("target").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut applying = Vec::new();
    if let Some((key, table)) = tables.get_key_value(host) {
        applying.push((key.as_str(), table));
    }
    if let Some(cfgs) = cfgs {
        let mut keys: Vec<&String> = tables
            .keys()
            .filter(|key| key.starts_with("cfg("))
            .collect();
        keys.sort();
        for key in keys {
            if CfgExpr::matches_key(key, cfgs) {
                applying.push((key.as_str(), &tables[key]));
            }
        }
    }
    applying
}

/// The flags cargo gives every rustc invocation for the machine `host`,
/// when the machine has the cfgs `cfgs`.
///
/// When the table of the triple or a matching `cfg(…)` table has flags,
/// those are the flags: the triple's first, then the `cfg` tables' in the
/// order of their keys. Only when none has any does `build.rustflags`
/// count. Without cfgs no `cfg(…)` table is looked at.
pub fn rustflags(config: &Value, host: &str, cfgs: Option<&[Cfg]>) -> Vec<String> {
    let mut flags = Vec::new();
    for (_, table) in target_tables(config, host, cfgs) {
        if let Some(of_table) = table.get("rustflags") {
            flags.extend(string_list(of_table));
        }
    }
    if flags.is_empty() {
        if let Some(of_build) = config.get("build").and_then(|build| build.get("rustflags")) {
            flags.extend(string_list(of_build));
        }
    }
    flags
}

/// The flags as cargo settles them. Which `cfg(…)` tables match depends on
/// the machine's cfgs, and those depend on the flags: a table of the triple
/// may turn a target feature on that a `cfg(…)` table asks about.
///
/// Cargo starts from the flags it knows without cfgs, asks rustc for the
/// cfgs with those flags, and reads the tables again. If that gives other
/// flags it asks once more with them, and keeps them whether or not they
/// hold, with a warning when they do not. `print_cfg` runs
/// `rustc --print=cfg` with the flags it is given.
pub fn settled_rustflags(
    config: &Value,
    host: &str,
    print_cfg: &dyn Fn(&[String]) -> crate::Result<String>,
) -> crate::Result<(Vec<String>, Vec<Cfg>)> {
    let first = rustflags(config, host, None);
    let cfgs = parse_cfgs(&print_cfg(&first)?);
    let flags = rustflags(config, host, Some(&cfgs));
    if flags == first {
        return Ok((flags, cfgs));
    }
    let cfgs = parse_cfgs(&print_cfg(&flags)?);
    if rustflags(config, host, Some(&cfgs)) != flags {
        // Cargo's own words for it.
        eprintln!(
            "rostnix: warning: non-trivial mutual dependency between target-specific configuration and RUSTFLAGS"
        );
    }
    Ok((flags, cfgs))
}

/// What the configuration sets for this machine that acts when things are
/// built and that rostnix does not apply: each as the key that sets it.
pub fn unapplied(config: &Value, host: &str, cfgs: &[Cfg]) -> Vec<String> {
    let mut keys = Vec::new();
    for (name, table) in target_tables(config, host, Some(cfgs)) {
        for setting in ["linker", "runner"] {
            if table.get(setting).is_some() {
                keys.push(format!("target.'{name}'.{setting}"));
            }
        }
    }
    for setting in ["rustc-wrapper", "rustc-workspace-wrapper"] {
        if config
            .get("build")
            .and_then(|build| build.get(setting))
            .is_some()
        {
            keys.push(format!("build.{setting}"));
        }
    }
    keys
}

/// Whether the configuration has a `cfg(…)` table, so that the cfgs of the
/// machine are needed to read it.
pub fn has_cfg_tables(config: &Value) -> bool {
    config
        .get("target")
        .and_then(Value::as_object)
        .is_some_and(|tables| tables.keys().any(|key| key.starts_with("cfg(")))
}

/// The file each variable of `[env]` is set in, from what
/// `cargo config get --show-origin env` prints:
/// `env.NAME.value = "…" # /path/.cargo/config.toml`.
pub fn origins(show_origin: &str) -> BTreeMap<String, String> {
    let mut origins = BTreeMap::new();
    for line in show_origin.lines() {
        let Some((assignment, origin)) = line.rsplit_once(" # ") else {
            continue;
        };
        let Some(key) = assignment
            .split(" = ")
            .next()
            .and_then(|key| key.trim().strip_prefix("env."))
        else {
            continue;
        };
        // The value's line names the file a relative value is relative to.
        let name = match key.strip_suffix(".value") {
            Some(name) => name,
            None if key.ends_with(".force") || key.ends_with(".relative") => continue,
            None => key,
        };
        origins.insert(
            name.trim_matches('"').to_string(),
            origin.trim().to_string(),
        );
    }
    origins
}

/// The variables of `[env]`. A relative value is relative to the directory
/// that holds the `.cargo` directory of the file that sets it; `origins`
/// says which file that is, and `workspace` is taken where it does not.
/// One that names a path outside `src_root` cannot be given to a
/// derivation, and is left out with a warning.
pub fn env(
    config: &Value,
    origins: &BTreeMap<String, String>,
    src_root: &str,
    workspace: &str,
) -> Vec<EnvEntry> {
    let Some(table) = config.get("env").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for (name, setting) in table {
        let (value, force, relative) = match setting {
            Value::String(value) => (value.clone(), false, false),
            Value::Object(setting) => (
                setting
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                setting.get("force").and_then(Value::as_bool) == Some(true),
                setting.get("relative").and_then(Value::as_bool) == Some(true),
            ),
            _ => continue,
        };
        // Cargo joins the value to the directory, and joining an absolute
        // path gives that path.
        if !relative || value.starts_with('/') {
            entries.push(EnvEntry {
                name: name.clone(),
                value,
                force,
                relative: None,
                slash: false,
            });
            continue;
        }
        // The directory above the one the file is in: the one that holds
        // `.cargo` for a `.cargo/config.toml`, and by the same rule for a
        // file that one includes. An origin that is no file, a command
        // line say, has no directory.
        let base = origins
            .get(name)
            .filter(|origin| origin.starts_with('/'))
            .and_then(|file| file.rsplit_once('/'))
            .and_then(|(dir, _)| dir.rsplit_once('/'))
            .map(|(base, _)| base)
            .unwrap_or(workspace);
        let path = format!("/{}", resolve(base, &value));
        let from_root = match path.strip_prefix(src_root) {
            Some("") => Some(String::new()),
            Some(rest) => rest.strip_prefix('/').map(String::from),
            None => None,
        };
        match from_root {
            Some(relative) => entries.push(EnvEntry {
                name: name.clone(),
                value: String::new(),
                force,
                relative: Some(relative),
                slash: value.is_empty() || value.ends_with('/'),
            }),
            None => eprintln!(
                "rostnix: warning: the cargo configuration sets {name} to the path {path}, which is outside the source tree {src_root}; it is not set"
            ),
        }
    }
    entries
}

/// Gives a process the variables of the cargo configuration, by cargo's
/// rules: never in place of one cargo itself sets, which are those `env`
/// holds already, and in place of one the environment has only when the
/// configuration says `force`. A unit of a local package finds the path of
/// a relative variable below `src`, its own tree.
///
/// Returns the relative variables that were set, each with the path it
/// names from the source root.
pub fn apply(
    env: &mut BTreeMap<String, String>,
    entries: &[ConfigEnv],
    local: bool,
    src: &str,
    in_environment: &dyn Fn(&str) -> bool,
) -> BTreeMap<String, String> {
    let mut relative = BTreeMap::new();
    for entry in entries {
        if env.contains_key(&entry.name) || (!entry.force && in_environment(&entry.name)) {
            continue;
        }
        let slash = if entry.slash { "/" } else { "" };
        let value = match &entry.relative {
            Some(path) if local => format!("{}{slash}", join(src, path)),
            _ => entry.value.clone(),
        };
        env.insert(entry.name.clone(), value);
        // Recorded as what follows the source root in the value.
        if let Some(path) = &entry.relative {
            let below_root = match path.as_str() {
                "" => slash.to_string(),
                path => format!("/{path}{slash}"),
            };
            relative.insert(entry.name.clone(), below_root);
        }
    }
    relative
}

/// Whether the builder's own environment has a variable.
pub fn in_environment(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn given(name: &str, value: &str, force: bool, relative: Option<&str>) -> ConfigEnv {
        ConfigEnv {
            name: name.to_string(),
            value: value.to_string(),
            force,
            relative: relative.map(String::from),
            slash: relative == Some(""),
        }
    }

    // What cargo did with these in a shell that had HOME and TERM: it set
    // PLAIN, DATA and TERM, and left HOME and CARGO_MANIFEST_DIR alone.
    #[test]
    fn variables_go_under_cargos_and_over_the_environment_only_when_forced() {
        let entries = [
            given("PLAIN", "plain", false, None),
            given("HOME", "/not-applied", false, None),
            given("TERM", "dumb", true, None),
            given("CARGO_MANIFEST_DIR", "/not-applied", true, None),
            given("DATA", "", false, Some("data/message.txt")),
            given("ROOT", "", false, Some("")),
        ];
        let mut env = BTreeMap::from([("CARGO_MANIFEST_DIR".to_string(), "/pkg".to_string())]);
        let ambient = |name: &str| matches!(name, "HOME" | "TERM");
        let relative = apply(&mut env, &entries, true, "/nix/store/view", &ambient);
        assert_eq!(env["PLAIN"], "plain");
        assert_eq!(env["TERM"], "dumb");
        assert!(!env.contains_key("HOME"));
        assert_eq!(env["CARGO_MANIFEST_DIR"], "/pkg");
        // A local unit finds a relative path in its own tree.
        assert_eq!(env["DATA"], "/nix/store/view/data/message.txt");
        // Cargo ends the directory an empty value names with a slash.
        assert_eq!(env["ROOT"], "/nix/store/view/");
        assert_eq!(
            relative,
            BTreeMap::from([
                ("DATA".to_string(), "/data/message.txt".to_string()),
                ("ROOT".to_string(), "/".to_string())
            ])
        );
    }

    // Any other unit is handed the path of a copy that holds the file.
    #[test]
    fn a_unit_that_is_not_local_takes_a_relative_value_as_it_is_given() {
        let entries = [given(
            "DATA",
            "/nix/store/copy/data/message.txt",
            false,
            Some("data/message.txt"),
        )];
        let mut env = BTreeMap::new();
        let relative = apply(&mut env, &entries, false, "/nix/store/crate", &|_| false);
        assert_eq!(env["DATA"], "/nix/store/copy/data/message.txt");
        assert_eq!(relative["DATA"], "/data/message.txt");
    }

    const HOST: &str = "aarch64-apple-darwin";

    fn cfgs() -> Vec<Cfg> {
        parse_cfgs(
            "debug_assertions\npanic=\"unwind\"\ntarget_arch=\"aarch64\"\ntarget_family=\"unix\"\n\
             target_os=\"macos\"\nunix\n",
        )
    }

    fn config(toml_as_json: &str) -> Value {
        serde_json::from_str(toml_as_json).unwrap()
    }

    // What cargo printed for a project with all of these, on this host:
    // --cfg from_triple --cfg from_aarch64 -A dead_code --cfg from_unix.
    #[test]
    fn target_tables_take_the_place_of_build_rustflags() {
        let config = config(
            r#"{"build":{"rustflags":["--cfg","from_build","-W","unused_qualifications"]},
                "target":{
                  "aarch64-apple-darwin":{"rustflags":["--cfg","from_triple"]},
                  "cfg(all(unix, target_arch = \"aarch64\"))":{"rustflags":"--cfg from_aarch64 -A dead_code"},
                  "cfg(unix)":{"rustflags":["--cfg","from_unix"]},
                  "cfg(windows)":{"rustflags":["--cfg","from_windows"]},
                  "x86_64-unknown-linux-gnu":{"rustflags":["--cfg","from_other_triple"]}}}"#,
        );
        assert_eq!(
            rustflags(&config, HOST, Some(&cfgs())),
            [
                "--cfg",
                "from_triple",
                "--cfg",
                "from_aarch64",
                "-A",
                "dead_code",
                "--cfg",
                "from_unix"
            ]
        );
        assert!(has_cfg_tables(&config));
        // Before the cfgs are known, only the triple's table counts.
        assert_eq!(rustflags(&config, HOST, None), ["--cfg", "from_triple"]);
    }

    /// A rustc that prints the cfgs of the test machine and one for every
    /// `--cfg` it is given.
    fn print_cfg(flags: &[String]) -> crate::Result<String> {
        let mut out = String::from("target_arch=\"aarch64\"\ntarget_family=\"unix\"\nunix\n");
        for pair in flags.windows(2) {
            if pair[0] == "--cfg" {
                out.push_str(&format!("{}\n", pair[1]));
            }
        }
        Ok(out)
    }

    // A table may ask about a cfg that the triple's flags turn on. Cargo
    // ran rustc with `--cfg foo --cfg bar` for this.
    #[test]
    fn flags_are_settled_against_the_cfgs_they_bring_about() {
        let chained = config(
            r#"{"target":{"aarch64-apple-darwin":{"rustflags":["--cfg","foo"]},
                          "cfg(foo)":{"rustflags":["--cfg","bar"]}}}"#,
        );
        let (flags, cfgs) = settled_rustflags(&chained, HOST, &print_cfg).unwrap();
        assert_eq!(flags, ["--cfg", "foo", "--cfg", "bar"]);
        assert!(CfgExpr::matches_key("cfg(bar)", &cfgs));

        // Nothing but `[build]`: rustc is asked once and the flags stay.
        let plain = config(r#"{"build":{"rustflags":["-C","target-cpu=native"]}}"#);
        let (flags, _) = settled_rustflags(&plain, HOST, &print_cfg).unwrap();
        assert_eq!(flags, ["-C", "target-cpu=native"]);
    }

    // Cargo asks rustc twice and no more. A table that would match only
    // because of a flag from the second reading is not used: cargo ran
    // rustc without `--cfg from_flag` for this, and said that the
    // configuration depends on itself.
    #[test]
    fn flags_that_do_not_settle_are_those_of_the_second_reading() {
        let unsettled = config(
            r#"{"build":{"rustflags":["--cfg","from_build"]},
                "target":{"cfg(unix)":{"rustflags":["--cfg","from_unix"]},
                          "cfg(from_unix)":{"rustflags":["--cfg","from_flag"]}}}"#,
        );
        let (flags, cfgs) = settled_rustflags(&unsettled, HOST, &print_cfg).unwrap();
        assert_eq!(flags, ["--cfg", "from_unix"]);
        // The cfgs are those of the flags that are used.
        assert!(CfgExpr::matches_key("cfg(from_unix)", &cfgs));
        assert!(!CfgExpr::matches_key("cfg(from_build)", &cfgs));
    }

    #[test]
    fn settings_that_are_not_applied_are_named() {
        let config = config(
            r#"{"build":{"rustc-wrapper":"sccache","rustflags":["-C","link-arg=-fuse-ld=mold"]},
                "target":{"aarch64-apple-darwin":{"linker":"clang"},
                          "cfg(unix)":{"runner":"valgrind"},
                          "cfg(windows)":{"linker":"lld-link"},
                          "x86_64-unknown-linux-gnu":{"linker":"clang"}}}"#,
        );
        assert_eq!(
            unapplied(&config, HOST, &cfgs()),
            [
                "target.'aarch64-apple-darwin'.linker",
                "target.'cfg(unix)'.runner",
                "build.rustc-wrapper"
            ]
        );
        assert!(unapplied(&serde_json::json!({}), HOST, &cfgs()).is_empty());
    }

    #[test]
    fn build_rustflags_count_when_no_target_table_has_any() {
        let config = config(
            r#"{"build":{"rustflags":"-C target-cpu=native  --cfg from_build"},
                "target":{"cfg(windows)":{"rustflags":["--cfg","from_windows"]},
                          "aarch64-apple-darwin":{"linker":"clang"}}}"#,
        );
        assert_eq!(
            rustflags(&config, HOST, Some(&cfgs())),
            ["-C", "target-cpu=native", "--cfg", "from_build"]
        );
        assert!(rustflags(&serde_json::json!({}), HOST, Some(&cfgs())).is_empty());
        assert!(!has_cfg_tables(
            &serde_json::json!({"target":{"aarch64-apple-darwin":{"linker":"x"}}})
        ));
    }

    const SHOW_ORIGIN: &str = r#"env.FIXTURE_DATA.relative = true # /src/.cargo/config.toml
env.FIXTURE_DATA.value = "data/message.txt" # /src/.cargo/config.toml
env.FIXTURE_PLAIN = "plain" # /src/ws/.cargo/config.toml
env.INCLUDED.relative = true # /src/ws/.cargo/extra/env.toml
env.INCLUDED.value = "x" # /src/ws/.cargo/extra/env.toml
env.FROM_CLI.relative = true # --config cli option
env.FROM_CLI.value = "y" # --config cli option
env.TERM.force = true # /src/ws/.cargo/config.toml
env.TERM.value = "dumb" # /src/ws/.cargo/config.toml
env.WS_ROOT.relative = true # /src/ws/.cargo/config.toml
env.WS_ROOT.value = "" # /src/ws/.cargo/config.toml
"#;

    #[test]
    fn origins_are_read_from_what_cargo_prints() {
        let origins = origins(SHOW_ORIGIN);
        assert_eq!(origins["FIXTURE_DATA"], "/src/.cargo/config.toml");
        assert_eq!(origins["FIXTURE_PLAIN"], "/src/ws/.cargo/config.toml");
        assert_eq!(origins["WS_ROOT"], "/src/ws/.cargo/config.toml");
        assert_eq!(origins.len(), 6);
    }

    #[test]
    fn env_entries_are_plain_forced_or_relative_to_their_file() {
        let config = config(
            r#"{"env":{
                "FIXTURE_DATA":{"relative":true,"value":"data/message.txt"},
                "FIXTURE_PLAIN":"plain",
                "TERM":{"force":true,"value":"dumb"},
                "WS_ROOT":{"relative":true,"value":""},
                "UP":{"relative":true,"value":"../shared"},
                "INCLUDED":{"relative":true,"value":"x"},
                "FROM_CLI":{"relative":true,"value":"y"},
                "ABSOLUTE":{"relative":true,"value":"/opt/data"},
                "OUTSIDE":{"relative":true,"value":"../../elsewhere"}}}"#,
        );
        let entries = env(&config, &origins(SHOW_ORIGIN), "/src", "/src/ws");
        let entry = |name: &str| entries.iter().find(|entry| entry.name == name).cloned();
        assert_eq!(
            entry("FIXTURE_PLAIN"),
            Some(EnvEntry {
                name: "FIXTURE_PLAIN".to_string(),
                value: "plain".to_string(),
                force: false,
                relative: None,
                slash: false,
            })
        );
        assert!(entry("TERM").unwrap().force);
        // Relative to the directory of the file that sets it: the source
        // root for one, the workspace for the other.
        assert_eq!(
            entry("FIXTURE_DATA").unwrap().relative.as_deref(),
            Some("data/message.txt")
        );
        assert_eq!(entry("WS_ROOT").unwrap().relative.as_deref(), Some("ws"));
        // An empty value is the directory itself, which cargo writes with
        // a slash at its end; a file has none.
        assert!(entry("WS_ROOT").unwrap().slash);
        assert!(!entry("FIXTURE_DATA").unwrap().slash);
        // No origin is known for these two: the workspace is the base.
        assert_eq!(entry("UP").unwrap().relative.as_deref(), Some("shared"));
        // The directory above the file's, whatever that directory is
        // called: a file that `.cargo/config.toml` includes from
        // `.cargo/extra/` has `.cargo` for its base.
        assert_eq!(
            entry("INCLUDED").unwrap().relative.as_deref(),
            Some("ws/.cargo/x")
        );
        // An origin that is no file has no directory.
        assert_eq!(entry("FROM_CLI").unwrap().relative.as_deref(), Some("ws/y"));
        // Joined to a directory, an absolute path stays what it is.
        let absolute = entry("ABSOLUTE").unwrap();
        assert_eq!(
            (absolute.value.as_str(), absolute.relative),
            ("/opt/data", None)
        );
        // A path outside the source cannot be given to a derivation.
        assert_eq!(entry("OUTSIDE"), None);
        assert!(env(&serde_json::json!({}), &BTreeMap::new(), "/src", "/src").is_empty());
    }

    // A workspace at the source root: its own directory is the root.
    #[test]
    fn the_source_root_itself_is_the_empty_path() {
        let config = config(r#"{"env":{"ROOT":{"relative":true,"value":""}}}"#);
        let entries = env(&config, &BTreeMap::new(), "/src", "/src");
        assert_eq!(entries[0].relative.as_deref(), Some(""));
    }
}
