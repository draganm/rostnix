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

/// The flags cargo gives every rustc invocation for the machine `host`.
///
/// When the table of the triple or a matching `cfg(…)` table has flags,
/// those are the flags: the triple's first, then the `cfg` tables' in the
/// order of their keys. Only when none has any does `build.rustflags`
/// count.
pub fn rustflags(config: &Value, host: &str, cfgs: &[Cfg]) -> Vec<String> {
    let mut flags = Vec::new();
    if let Some(tables) = config.get("target").and_then(Value::as_object) {
        if let Some(of_triple) = tables.get(host).and_then(|table| table.get("rustflags")) {
            flags.extend(string_list(of_triple));
        }
        let mut keys: Vec<&String> = tables
            .keys()
            .filter(|key| key.starts_with("cfg("))
            .collect();
        keys.sort();
        for key in keys {
            if !CfgExpr::matches_key(key, cfgs) {
                continue;
            }
            if let Some(of_cfg) = tables[key].get("rustflags") {
                flags.extend(string_list(of_cfg));
            }
        }
    }
    if flags.is_empty() {
        if let Some(of_build) = config.get("build").and_then(|build| build.get("rustflags")) {
            flags.extend(string_list(of_build));
        }
    }
    flags
}

/// Whether the configuration has a `cfg(…)` table with flags, so that the
/// cfgs of the machine are needed to read it.
pub fn has_cfg_rustflags(config: &Value) -> bool {
    config
        .get("target")
        .and_then(Value::as_object)
        .is_some_and(|tables| {
            tables
                .iter()
                .any(|(key, table)| key.starts_with("cfg(") && table.get("rustflags").is_some())
        })
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
        if !relative {
            entries.push(EnvEntry {
                name: name.clone(),
                value,
                force,
                relative: None,
                slash: false,
            });
            continue;
        }
        // <base>/.cargo/config.toml
        let base = origins
            .get(name)
            .and_then(|file| file.rsplit_once('/'))
            .and_then(|(cargo_dir, _)| cargo_dir.strip_suffix("/.cargo"))
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
        if let Some(path) = &entry.relative {
            relative.insert(entry.name.clone(), format!("{path}{slash}"));
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
                ("DATA".to_string(), "data/message.txt".to_string()),
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
        assert_eq!(relative["DATA"], "data/message.txt");
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
            rustflags(&config, HOST, &cfgs()),
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
        assert!(has_cfg_rustflags(&config));
    }

    #[test]
    fn build_rustflags_count_when_no_target_table_has_any() {
        let config = config(
            r#"{"build":{"rustflags":"-C target-cpu=native  --cfg from_build"},
                "target":{"cfg(windows)":{"rustflags":["--cfg","from_windows"]},
                          "aarch64-apple-darwin":{"linker":"clang"}}}"#,
        );
        assert_eq!(
            rustflags(&config, HOST, &cfgs()),
            ["-C", "target-cpu=native", "--cfg", "from_build"]
        );
        assert!(rustflags(&serde_json::json!({}), HOST, &cfgs()).is_empty());
        assert!(!has_cfg_rustflags(
            &serde_json::json!({"target":{"cfg(unix)":{"runner":"x"}}})
        ));
    }

    const SHOW_ORIGIN: &str = r#"env.FIXTURE_DATA.relative = true # /src/.cargo/config.toml
env.FIXTURE_DATA.value = "data/message.txt" # /src/.cargo/config.toml
env.FIXTURE_PLAIN = "plain" # /src/ws/.cargo/config.toml
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
        assert_eq!(origins.len(), 4);
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
