//! What a unit of a local package sees of its package directory.
//!
//! Rust has no list of the files a target reads, so a unit gets the whole
//! package directory less what plainly belongs to other targets. All paths
//! are relative to the source root, and `""` is the root itself.

/// One target of a package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetInfo {
    /// Whether the target is a binary, example, test or bench.
    pub executable: bool,
    /// The directory cargo discovers targets of this kind in: `examples`,
    /// `tests` or `benches`, if the target is of such a kind.
    pub own_dir: Option<&'static str>,
    /// The target's root source file.
    pub src_path: String,
}

/// Joins a package directory and a path inside it.
pub fn join(dir: &str, rel: &str) -> String {
    match (dir.is_empty(), rel.is_empty()) {
        (true, _) => rel.to_string(),
        (false, true) => dir.to_string(),
        (false, false) => format!("{dir}/{rel}"),
    }
}

/// Whether `path` is `dir` or lies under it.
pub fn is_under(path: &str, dir: &str) -> bool {
    dir.is_empty()
        || path == dir
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The files a crate root names, as paths from the root's directory:
/// `name.rs` and `name/mod.rs` for `mod name;`, what a `#[path = "…"]`
/// attribute says, and what `include_str!`, `include_bytes!` and `include!`
/// are given as a literal.
///
/// The text is searched, not parsed. What looks like one of these in a
/// block comment or in a string is taken for one, which only shows a unit
/// a file it does not need. One that a macro writes is missed, and so is
/// one inside an inline module or in a module file rather than in the
/// root, and a path that is put together with `concat!`.
pub fn named_files(source: &str) -> Vec<String> {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut found = Vec::new();
    for line in source.lines() {
        let line = line.split("//").next().unwrap_or_default();
        // `#[path = "…"]`, also inside `#[cfg_attr(…)]`.
        if line.contains("#[") {
            for (at, _) in line.match_indices("path") {
                let before = line[..at].chars().next_back();
                let value = line[at + 4..].trim_start();
                if before.is_some_and(is_ident) || !value.starts_with('=') {
                    continue;
                }
                if let Some(path) = value[1..].trim_start().strip_prefix('"') {
                    found.push(path.split('"').next().unwrap_or_default().to_string());
                }
            }
        }
        // `include_str!("…")` and its like: a library's documentation is
        // often its README, or an example.
        for include in ["include_str!", "include_bytes!", "include!"] {
            for (at, _) in line.match_indices(include) {
                let before = line[..at].chars().next_back();
                let argument = line[at + include.len()..].trim_start();
                let literal = argument
                    .strip_prefix('(')
                    .and_then(|rest| rest.trim_start().strip_prefix('"'));
                if let (false, Some(path)) = (before.is_some_and(is_ident), literal) {
                    found.push(path.split('"').next().unwrap_or_default().to_string());
                }
            }
        }
        for (at, _) in line.match_indices("mod") {
            let before = line[..at].chars().next_back();
            let rest = &line[at + 3..];
            if before.is_some_and(is_ident) || !rest.starts_with(char::is_whitespace) {
                continue;
            }
            let rest = rest.trim_start();
            let rest = rest.strip_prefix("r#").unwrap_or(rest);
            let name: String = rest.chars().take_while(|c| is_ident(*c)).collect();
            if !name.is_empty() && rest[name.len()..].trim_start().starts_with(';') {
                found.push(format!("{name}.rs"));
                found.push(format!("{name}/mod.rs"));
            }
        }
    }
    found
}

/// `rel` as seen from the directory `dir`, with `.` and `..` resolved.
pub fn resolve(dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = dir.split('/').filter(|part| !part.is_empty()).collect();
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

/// The paths left out of the view a unit of `unit_target` gets of the
/// package in `pkg_dir`: sorted, and none under another. A unit built as a
/// test keeps `examples/`, `tests/` and `benches/` whatever its target is,
/// and loses only the root files of other targets in them. `keep` names
/// the files the unit's root names, as modules or to include. They stay
/// even when they are other targets' roots, as `tests/common.rs` is when
/// the tests beside it say `mod common;`, and they keep a directory the
/// unit would not see, as an example does that a library includes in its
/// documentation.
pub fn exclusions(
    pkg_dir: &str,
    other_pkg_dirs: &[String],
    targets: &[TargetInfo],
    unit_target: &TargetInfo,
    as_test: bool,
    keep: &[String],
) -> Vec<String> {
    let mut excluded: Vec<String> = Vec::new();

    // Other packages inside this one are not part of it.
    for other in other_pkg_dirs {
        if other != pkg_dir && is_under(other, pkg_dir) {
            excluded.push(other.clone());
        }
    }

    // Where cargo looks for examples, tests and benches, unless the unit is
    // one of them, or has its root there by a `path` of its own, or names a
    // file there as a module. A test keeps all three: it runs in its
    // package directory and may read whatever lies there, and a test that
    // looks through a directory that is not there finds nothing wrong.
    for dir in ["examples", "tests", "benches"] {
        let dir_path = join(pkg_dir, dir);
        let kept = as_test
            || unit_target.own_dir == Some(dir)
            || is_under(&unit_target.src_path, &dir_path)
            || keep.iter().any(|file| is_under(file, &dir_path));
        if !kept {
            excluded.push(dir_path);
        }
    }

    // The root file of every other executable target, or its directory when
    // cargo found the target as one: `src/bin/<name>/main.rs` and the like.
    // A main.rs anywhere else may share its directory with other targets'
    // modules.
    let target_dirs = ["src/bin", "examples", "tests", "benches"].map(|dir| join(pkg_dir, dir));
    for target in targets {
        if !target.executable || target.src_path == unit_target.src_path {
            continue;
        }
        let (parent, file) = target
            .src_path
            .rsplit_once('/')
            .unwrap_or(("", &target.src_path));
        let grandparent = parent.rsplit_once('/').map_or("", |(dir, _)| dir);
        let own_directory = file == "main.rs" && target_dirs.iter().any(|dir| dir == grandparent);
        let path = if own_directory {
            parent
        } else {
            target.src_path.as_str()
        };
        // Never hide the unit's own root, nor a file it names as a module.
        let needed =
            is_under(&unit_target.src_path, path) || keep.iter().any(|file| is_under(file, path));
        if !needed {
            excluded.push(path.to_string());
        }
    }

    excluded.sort();
    excluded.dedup();
    let all = excluded.clone();
    excluded.retain(|path| {
        !all.iter()
            .any(|other| other != path && is_under(path, other))
    });
    excluded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(kind: &str, src_path: &str) -> TargetInfo {
        TargetInfo {
            executable: matches!(kind, "bin" | "example" | "test" | "bench"),
            own_dir: match kind {
                "example" => Some("examples"),
                "test" => Some("tests"),
                "bench" => Some("benches"),
                _ => None,
            },
            src_path: src_path.to_string(),
        }
    }

    fn core_rs() -> Vec<TargetInfo> {
        vec![
            target("lib", "src/lib.rs"),
            target("example", "examples/amber-bench.rs"),
            target("example", "examples/amber-store.rs"),
            target("example", "examples/repair-interop.rs"),
            target("test", "tests/cbor.rs"),
            target("test", "tests/cli_e2e.rs"),
        ]
    }

    #[test]
    fn library_sees_no_examples_tests_or_benches() {
        let targets = core_rs();
        assert_eq!(
            exclusions("", &[], &targets, &targets[0], false, &[]),
            ["benches", "examples", "tests"]
        );
    }

    #[test]
    fn example_sees_its_directory_without_the_other_examples() {
        let targets = core_rs();
        assert_eq!(
            exclusions("", &[], &targets, &targets[2], false, &[]),
            [
                "benches",
                "examples/amber-bench.rs",
                "examples/repair-interop.rs",
                "tests"
            ]
        );
    }

    #[test]
    fn test_sees_tests_without_the_other_tests() {
        let targets = core_rs();
        assert_eq!(
            exclusions("", &[], &targets, &targets[4], false, &[]),
            ["benches", "examples", "tests/cli_e2e.rs"]
        );
    }

    // Whatever is built as a test may read whatever lies in its package:
    // it keeps examples/, tests/ and benches/, with their data and helper
    // modules, and loses only the other targets' own files.
    #[test]
    fn a_target_built_as_a_test_keeps_the_three_directories() {
        let mut targets = core_rs();
        targets.push(target("bin", "src/main.rs"));
        let examples = [
            "examples/amber-bench.rs",
            "examples/amber-store.rs",
            "examples/repair-interop.rs",
        ];
        // The library's unit tests.
        assert_eq!(
            exclusions("", &[], &targets, &targets[0], true, &[]),
            [
                examples[0],
                examples[1],
                examples[2],
                "src/main.rs",
                "tests/cbor.rs",
                "tests/cli_e2e.rs"
            ]
        );
        // The binary's.
        assert_eq!(
            exclusions("", &[], &targets, &targets[6], true, &[]),
            [
                examples[0],
                examples[1],
                examples[2],
                "tests/cbor.rs",
                "tests/cli_e2e.rs"
            ]
        );
        // An integration test.
        assert_eq!(
            exclusions("", &[], &targets, &targets[4], true, &[]),
            [
                examples[0],
                examples[1],
                examples[2],
                "src/main.rs",
                "tests/cli_e2e.rs"
            ]
        );
    }

    #[test]
    fn module_declarations_are_found_in_the_text() {
        let source = r#"
//! mod not_this;
use std::fmt;
mod common;
pub mod helpers ;
pub(crate) mod inner;
#[cfg(unix)] mod unix_only;
#[path = "../shared/support.rs"]
mod support;
mod inline { fn f() {} }
fn modify() { let model = 1; } // mod neither;
mod r#async;
#[cfg_attr(windows, path = "sys/windows.rs")]
mod sys;
fn f() { let path = "not/this.rs"; }
"#;
        assert_eq!(
            named_files(source),
            [
                "common.rs",
                "common/mod.rs",
                "helpers.rs",
                "helpers/mod.rs",
                "inner.rs",
                "inner/mod.rs",
                "unix_only.rs",
                "unix_only/mod.rs",
                "../shared/support.rs",
                "support.rs",
                "support/mod.rs",
                "async.rs",
                "async/mod.rs",
                "sys/windows.rs",
                "sys.rs",
                "sys/mod.rs",
            ]
        );
        assert!(named_files("fn main() {}").is_empty());
    }

    // ruff's ruff_annotate_snippets does this: its documentation is an
    // example and a picture of what the example prints.
    #[test]
    fn included_files_are_found_in_the_text() {
        let source = r#"
#![doc = include_str!("../examples/expected_type.rs")]
#![doc = include_str!( "../examples/expected_type.svg" )]
const DATA: &[u8] = include_bytes!("data.bin");
include!("generated.rs");
const NOT_A_LITERAL: &str = include_str!(concat!(env!("OUT_DIR"), "/x"));
fn reinclude_str!() {}
"#;
        assert_eq!(
            named_files(source),
            [
                "../examples/expected_type.rs",
                "../examples/expected_type.svg",
                "data.bin",
                "generated.rs",
            ]
        );
    }

    // A library whose documentation includes one of its examples sees the
    // examples directory, without the other examples.
    #[test]
    fn a_file_the_root_includes_keeps_its_directory() {
        let targets = vec![
            target("lib", "src/lib.rs"),
            target("example", "examples/expected_type.rs"),
            target("example", "examples/other.rs"),
        ];
        let keep = [
            "examples/expected_type.rs".to_string(),
            "examples/expected_type.svg".to_string(),
        ];
        assert_eq!(
            exclusions("", &[], &targets, &targets[0], false, &keep),
            ["benches", "examples/other.rs", "tests"]
        );
    }

    #[test]
    fn paths_are_resolved_from_a_directory() {
        assert_eq!(resolve("tests", "common.rs"), "tests/common.rs");
        assert_eq!(
            resolve("app/tests", "../shared/support.rs"),
            "app/shared/support.rs"
        );
        assert_eq!(resolve("", "./a/b.rs"), "a/b.rs");
        assert_eq!(resolve("tests", "../../outside.rs"), "outside.rs");
    }

    // `tests/common.rs` is a test of its own to cargo, and a module to the
    // tests that say `mod common;`.
    #[test]
    fn a_root_the_unit_names_as_a_module_stays() {
        let targets = vec![
            target("lib", "src/lib.rs"),
            target("test", "tests/a.rs"),
            target("test", "tests/b.rs"),
            target("test", "tests/common.rs"),
            target("test", "tests/suite/main.rs"),
        ];
        let view = |keep: &[&str]| {
            let keep: Vec<String> = keep.iter().map(|s| s.to_string()).collect();
            exclusions("", &[], &targets, &targets[1], false, &keep)
        };
        assert_eq!(
            view(&[]),
            [
                "benches",
                "examples",
                "tests/b.rs",
                "tests/common.rs",
                "tests/suite"
            ]
        );
        assert_eq!(
            view(&["tests/common.rs", "tests/common/mod.rs"]),
            ["benches", "examples", "tests/b.rs", "tests/suite"]
        );
        // A file inside a target that is a directory keeps the directory.
        assert_eq!(
            view(&["tests/suite/helpers.rs"]),
            ["benches", "examples", "tests/b.rs", "tests/common.rs"]
        );
        // `#[path = "../examples/demo.rs"] mod demo;` keeps the directory
        // the file is in.
        assert_eq!(
            view(&["examples/demo.rs"]),
            ["benches", "tests/b.rs", "tests/common.rs", "tests/suite"]
        );
    }

    #[test]
    fn binaries_do_not_see_each_other() {
        let targets = vec![
            target("lib", "app/src/lib.rs"),
            target("bin", "app/src/main.rs"),
            target("bin", "app/src/bin/quick.rs"),
            target("bin", "app/src/bin/tool/main.rs"),
            target("build-script", "app/build.rs"),
        ];
        let common = ["app/benches", "app/examples"];
        let view = |i: usize| exclusions("app", &[], &targets, &targets[i], false, &[]);
        assert_eq!(
            view(0),
            [
                common[0],
                common[1],
                "app/src/bin/quick.rs",
                "app/src/bin/tool",
                "app/src/main.rs",
                "app/tests"
            ]
        );
        assert_eq!(
            view(1),
            [
                common[0],
                common[1],
                "app/src/bin/quick.rs",
                "app/src/bin/tool",
                "app/tests"
            ]
        );
        assert_eq!(
            view(2),
            [
                common[0],
                common[1],
                "app/src/bin/tool",
                "app/src/main.rs",
                "app/tests"
            ]
        );
        assert_eq!(
            view(3),
            [
                common[0],
                common[1],
                "app/src/bin/quick.rs",
                "app/src/main.rs",
                "app/tests"
            ]
        );
        // The build script sees what the library sees.
        assert_eq!(view(4), view(0));
    }

    // `[[bin]] path = "src/cli/main.rs"`: the library may have `mod cli;`
    // with its other files in that directory.
    #[test]
    fn a_main_rs_outside_cargos_target_directories_hides_only_itself() {
        let targets = vec![
            target("lib", "src/lib.rs"),
            target("bin", "src/cli/main.rs"),
        ];
        assert_eq!(
            exclusions("", &[], &targets, &targets[0], false, &[]),
            ["benches", "examples", "src/cli/main.rs", "tests"]
        );
    }

    // `[[bin]] path = "examples/tool.rs"`, or a library rooted under tests/.
    #[test]
    fn a_unit_rooted_in_another_kinds_directory_keeps_it() {
        let targets = vec![
            target("lib", "tests/support/lib.rs"),
            target("bin", "examples/tool.rs"),
            target("example", "examples/demo.rs"),
        ];
        assert_eq!(
            exclusions("", &[], &targets, &targets[0], false, &[]),
            ["benches", "examples"]
        );
        assert_eq!(
            exclusions("", &[], &targets, &targets[1], false, &[]),
            ["benches", "examples/demo.rs", "tests"]
        );
    }

    #[test]
    fn nested_packages_are_left_out() {
        let targets = vec![target("lib", "crates/core/src/lib.rs")];
        let others = [
            "crates/core/nested".to_string(),
            "crates/macros".to_string(),
            "app".to_string(),
        ];
        assert_eq!(
            exclusions("crates/core", &others, &targets, &targets[0], false, &[]),
            [
                "crates/core/benches",
                "crates/core/examples",
                "crates/core/nested",
                "crates/core/tests"
            ]
        );
        // From the root package every other package is nested, and one
        // inside another is covered by the outer one.
        let root = vec![target("lib", "src/lib.rs")];
        let others = [others.to_vec(), vec!["crates/core".to_string()]].concat();
        assert_eq!(
            exclusions("", &others, &root, &root[0], false, &[]),
            [
                "app",
                "benches",
                "crates/core",
                "crates/macros",
                "examples",
                "tests"
            ]
        );
    }

    #[test]
    fn paths() {
        assert_eq!(join("", "src"), "src");
        assert_eq!(join("a/b", "src"), "a/b/src");
        assert_eq!(join("a/b", ""), "a/b");
        assert!(is_under("a/b/c", "a/b"));
        assert!(is_under("a/b", "a/b"));
        assert!(!is_under("a/bc", "a/b"));
        assert!(is_under("anything", ""));
    }
}
