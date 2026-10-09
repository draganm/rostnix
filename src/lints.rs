//! The rustc flags of a manifest's `[lints]` table, as cargo derives them.

use std::cmp::Reverse;

use toml::{Table, Value};

use crate::Result;

/// The lint flags for a package. `workspace` is the manifest of the
/// workspace root, consulted when the package says `lints.workspace = true`.
pub fn rustflags(manifest: &Table, workspace: Option<&Table>) -> Result<Vec<String>> {
    let Some(lints) = manifest.get("lints").and_then(Value::as_table) else {
        return Ok(Vec::new());
    };
    let inherited = lints.get("workspace").and_then(Value::as_bool) == Some(true);
    let lints = if inherited {
        let table = workspace
            .and_then(|ws| ws.get("workspace"))
            .and_then(|ws| ws.get("lints"))
            .and_then(Value::as_table);
        match table {
            Some(table) => table,
            None => return Ok(Vec::new()),
        }
    } else {
        lints
    };

    let mut flags = Vec::new();
    for (tool, tool_lints) in lints {
        // Cargo's own lints are not rustc's business.
        if tool == "cargo" {
            continue;
        }
        let Some(tool_lints) = tool_lints.as_table() else {
            continue;
        };
        for (name, config) in tool_lints {
            let (level, priority) = match config {
                Value::String(level) => (level.as_str(), 0),
                Value::Table(table) => (
                    table.get("level").and_then(Value::as_str).unwrap_or(""),
                    table
                        .get("priority")
                        .and_then(Value::as_integer)
                        .unwrap_or(0),
                ),
                _ => ("", 0),
            };
            let flag = match level {
                "forbid" => "--forbid",
                "deny" => "--deny",
                "warn" => "--warn",
                "allow" => "--allow",
                other => {
                    return Err(
                        format!("lint {tool}::{name} has the unknown level '{other}'").into(),
                    )
                }
            };
            let option = if tool == "rust" {
                format!("{flag}={name}")
            } else {
                format!("{flag}={tool}::{name}")
            };
            flags.push((priority, Reverse(name.clone()), option));
        }
    }
    // Lower priorities come first so that higher ones override them; among
    // equals cargo puts groups such as `all` first by reversing the names.
    flags.sort();
    let mut flags: Vec<String> = flags.into_iter().map(|(_, _, option)| option).collect();

    let check_cfg = lints
        .get("rust")
        .and_then(|rust| rust.get("unexpected_cfgs"))
        .and_then(|config| config.get("check-cfg"))
        .and_then(Value::as_array);
    for value in check_cfg.into_iter().flatten() {
        let Some(value) = value.as_str() else {
            return Err("lints.rust.unexpected_cfgs.check-cfg must be a list of strings".into());
        };
        flags.push("--check-cfg".to_string());
        flags.push(value.to_string());
    }
    Ok(flags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> Table {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn no_table_no_flags() {
        assert!(rustflags(&table("[package]\nname = \"a\""), None)
            .unwrap()
            .is_empty());
    }

    // libc 0.2.190's table, and the order cargo 1.86 passed its flags in.
    #[test]
    fn orders_by_priority_then_reversed_name_across_tools() {
        let manifest = table(
            r#"
            [lints.rust]
            unused_qualifications = "allow"

            [lints.clippy]
            explicit_iter_loop = "warn"
            identity_op = "allow"
            manual_assert = "warn"
            map_unwrap_or = "warn"
            missing_safety_doc = "allow"
            non_minimal_cfg = "allow"
            ptr_as_ptr = "warn"
            unnecessary_cast = "allow"
            unnecessary_semicolon = "warn"
            "#,
        );
        assert_eq!(
            rustflags(&manifest, None).unwrap(),
            [
                "--allow=unused_qualifications",
                "--warn=clippy::unnecessary_semicolon",
                "--allow=clippy::unnecessary_cast",
                "--warn=clippy::ptr_as_ptr",
                "--allow=clippy::non_minimal_cfg",
                "--allow=clippy::missing_safety_doc",
                "--warn=clippy::map_unwrap_or",
                "--warn=clippy::manual_assert",
                "--allow=clippy::identity_op",
                "--warn=clippy::explicit_iter_loop",
            ]
        );
    }

    #[test]
    fn tables_priorities_check_cfg_and_cargo_lints() {
        let manifest = table(
            r#"
            [lints.rust]
            unsafe_code = "forbid"
            warnings = { level = "deny", priority = -1 }
            unexpected_cfgs = { level = "warn", check-cfg = ["cfg(fuzzing)", "cfg(has_x, values(\"a\"))"] }

            [lints.cargo]
            unknown_lints = "deny"
            "#,
        );
        assert_eq!(
            rustflags(&manifest, None).unwrap(),
            [
                "--deny=warnings",
                "--forbid=unsafe_code",
                "--warn=unexpected_cfgs",
                "--check-cfg",
                "cfg(fuzzing)",
                "--check-cfg",
                "cfg(has_x, values(\"a\"))",
            ]
        );
    }

    #[test]
    fn inherits_from_the_workspace() {
        let manifest = table("[lints]\nworkspace = true");
        let workspace = table(
            r#"
            [workspace.lints.rust]
            unsafe_code = "forbid"
            unused_must_use = { level = "deny", priority = -1 }
            "#,
        );
        assert_eq!(
            rustflags(&manifest, Some(&workspace)).unwrap(),
            ["--deny=unused_must_use", "--forbid=unsafe_code"]
        );
        assert!(rustflags(&manifest, None).unwrap().is_empty());
    }

    #[test]
    fn rejects_an_unknown_level() {
        let manifest = table("[lints.rust]\nunsafe_code = \"loud\"");
        let err = rustflags(&manifest, None).unwrap_err().to_string();
        assert!(
            err.contains("rust::unsafe_code") && err.contains("loud"),
            "{err}"
        );
    }
}
