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

/// The paths left out of the view a unit of `unit_target` gets of the
/// package in `pkg_dir`: sorted, and none under another.
pub fn exclusions(
    pkg_dir: &str,
    other_pkg_dirs: &[String],
    targets: &[TargetInfo],
    unit_target: &TargetInfo,
) -> Vec<String> {
    let mut excluded: Vec<String> = Vec::new();

    // Other packages inside this one are not part of it.
    for other in other_pkg_dirs {
        if other != pkg_dir && is_under(other, pkg_dir) {
            excluded.push(other.clone());
        }
    }

    // Where cargo looks for examples, tests and benches, unless the unit is
    // one of them, or has its root there by a `path` of its own.
    for dir in ["examples", "tests", "benches"] {
        let dir_path = join(pkg_dir, dir);
        if unit_target.own_dir != Some(dir) && !is_under(&unit_target.src_path, &dir_path) {
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
        // Never hide the unit's own root.
        if !is_under(&unit_target.src_path, path) {
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
            exclusions("", &[], &targets, &targets[0]),
            ["benches", "examples", "tests"]
        );
    }

    #[test]
    fn example_sees_its_directory_without_the_other_examples() {
        let targets = core_rs();
        assert_eq!(
            exclusions("", &[], &targets, &targets[2]),
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
            exclusions("", &[], &targets, &targets[4]),
            ["benches", "examples", "tests/cli_e2e.rs"]
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
        let view = |i: usize| exclusions("app", &[], &targets, &targets[i]);
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
            exclusions("", &[], &targets, &targets[0]),
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
            exclusions("", &[], &targets, &targets[0]),
            ["benches", "examples"]
        );
        assert_eq!(
            exclusions("", &[], &targets, &targets[1]),
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
            exclusions("crates/core", &others, &targets, &targets[0]),
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
            exclusions("", &others, &root, &root[0]),
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
