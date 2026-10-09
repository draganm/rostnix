//! The rustc flags of a unit that do not depend on where anything is
//! stored, in the order cargo 1.95 passes them.

use crate::lto::Lto;
use crate::unitgraph::Unit;

/// What the flags of one unit are computed from.
pub struct UnitFlags<'a> {
    pub unit: &'a Unit,
    pub lto: &'a Lto,
    /// Every feature the package declares, sorted.
    pub declared_features: &'a [String],
    /// The flags of the package's `[lints]` table.
    pub lint_flags: &'a [String],
    /// The unit's metadata hash.
    pub metadata: &'a str,
    /// Whether the package is one the build was asked for.
    pub primary: bool,
}

/// The flags from `--crate-type` up to where cargo names the output
/// directory and the dependencies.
pub fn base_args(flags: &UnitFlags) -> Vec<String> {
    let unit = flags.unit;
    let profile = &unit.profile;
    let mut args: Vec<String> = Vec::new();
    let mut push = |parts: &[&str]| args.extend(parts.iter().map(|p| p.to_string()));

    for crate_type in &unit.target.crate_types {
        push(&["--crate-type", crate_type]);
    }

    let has_dylib = unit.target.crate_types.iter().any(|ct| ct == "dylib");
    if (unit.target.for_host() && !unit.target.is_custom_build()) || (has_dylib && !flags.primary) {
        push(&["-C", "prefer-dynamic"]);
    }
    if profile.opt_level != "0" {
        push(&["-C", &format!("opt-level={}", profile.opt_level)]);
    }
    if profile.panic != "unwind" {
        push(&["-C", &format!("panic={}", profile.panic)]);
    }
    for arg in flags.lto.args() {
        push(&[&arg]);
    }
    if let Some(n) = profile.codegen_units {
        push(&["-C", &format!("codegen-units={n}")]);
    }
    if let Some(debuginfo) = profile.debuginfo() {
        push(&["-C", &format!("debuginfo={debuginfo}")]);
        if let Some(split) = &profile.split_debuginfo {
            push(&["-C", &format!("split-debuginfo={split}")]);
        }
    }
    for flag in flags.lint_flags {
        push(&[flag]);
    }

    // Both checks default to on without optimisation and off with it, and
    // overflow checks follow debug assertions unless said otherwise.
    if profile.opt_level != "0" {
        if profile.debug_assertions {
            push(&["-C", "debug-assertions=on"]);
            if !profile.overflow_checks {
                push(&["-C", "overflow-checks=off"]);
            }
        } else if profile.overflow_checks {
            push(&["-C", "overflow-checks=on"]);
        }
    } else if !profile.debug_assertions {
        push(&["-C", "debug-assertions=off"]);
        if profile.overflow_checks {
            push(&["-C", "overflow-checks=on"]);
        }
    } else if !profile.overflow_checks {
        push(&["-C", "overflow-checks=off"]);
    }

    for feature in &unit.features {
        push(&["--cfg", &format!("feature=\"{feature}\"")]);
    }
    let declared: Vec<String> = flags.declared_features.iter().map(|f| format!("\"{f}\"")).collect();
    push(&["--check-cfg", "cfg(docsrs,test)"]);
    push(&["--check-cfg", &format!("cfg(feature, values({}))", declared.join(", "))]);

    push(&["-C", &format!("metadata={}", flags.metadata)]);
    push(&["-C", &format!("extra-filename=-{}", flags.metadata)]);
    if profile.rpath {
        push(&["-C", "rpath"]);
    }
    if let Some(strip) = profile.strip() {
        push(&["-C", &format!("strip={strip}")]);
    }
    args
}

/// The flags cargo puts after the dependencies.
pub fn tail_args(unit: &Unit, local: bool) -> Vec<String> {
    let mut args = Vec::new();
    if unit.target.is_proc_macro() {
        args.extend(["--extern".to_string(), "proc_macro".to_string()]);
    }
    // Warnings in other people's crates are not the builder's to fix.
    if !local {
        args.extend(["--cap-lints".to_string(), "allow".to_string()]);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASE: &str = r#"{"name":"release","opt_level":"3","lto":"thin","codegen_units":null,
        "debuginfo":0,"split_debuginfo":null,"debug_assertions":false,"overflow_checks":false,
        "rpath":false,"panic":"abort","strip":{"resolved":{"Named":"debuginfo"}}}"#;
    // The profile cargo gives build scripts, proc macros and what they use.
    const HOST: &str = r#"{"name":"release","opt_level":"0","lto":"thin","codegen_units":null,
        "debuginfo":0,"split_debuginfo":null,"debug_assertions":false,"overflow_checks":false,
        "rpath":false,"panic":"unwind","strip":{"resolved":{"Named":"debuginfo"}}}"#;
    const DEV: &str = r#"{"name":"dev","opt_level":"0","lto":"false","codegen_units":null,
        "debuginfo":2,"split_debuginfo":"unpacked","debug_assertions":true,"overflow_checks":true,
        "rpath":false,"panic":"unwind","strip":{"deferred":"None"}}"#;

    fn unit(kind: &str, crate_type: &str, name: &str, profile: &str, features: &[&str]) -> Unit {
        let features: Vec<String> = features.iter().map(|f| format!("\"{f}\"")).collect();
        serde_json::from_str(&format!(
            r#"{{"pkg_id":"p","target":{{"kind":["{kind}"],"crate_types":["{crate_type}"],"name":"{name}",
                "src_path":"/s","edition":"2021"}},"profile":{profile},"platform":null,"mode":"build",
                "features":[{}],"dependencies":[]}}"#,
            features.join(",")
        ))
        .unwrap()
    }

    fn args(unit: &Unit, lto: Lto, declared: &[&str], lints: &[&str]) -> String {
        let declared: Vec<String> = declared.iter().map(|s| s.to_string()).collect();
        let lints: Vec<String> = lints.iter().map(|s| s.to_string()).collect();
        base_args(&UnitFlags {
            unit,
            lto: &lto,
            declared_features: &declared,
            lint_flags: &lints,
            metadata: "M",
            primary: false,
        })
        .join(" ")
    }

    // The expectations are the flags cargo printed with `build -v --release`
    // for a project with `lto = "thin"` and `panic = "abort"`.

    #[test]
    fn build_script() {
        let unit = unit("custom-build", "bin", "build-script-build", HOST, &["default", "std"]);
        assert_eq!(
            args(&unit, Lto::OnlyObject, &["backtrace", "default", "std"], &[]),
            "--crate-type bin -C embed-bitcode=no -C debug-assertions=off \
             --cfg feature=\"default\" --cfg feature=\"std\" --check-cfg cfg(docsrs,test) \
             --check-cfg cfg(feature, values(\"backtrace\", \"default\", \"std\")) \
             -C metadata=M -C extra-filename=-M -C strip=debuginfo"
        );
        assert_eq!(tail_args(&unit, false), ["--cap-lints", "allow"]);
    }

    #[test]
    fn proc_macro() {
        let unit = unit("proc-macro", "proc-macro", "serde_derive", HOST, &["default"]);
        assert_eq!(
            args(&unit, Lto::OnlyObject, &["default", "deserialize_in_place"], &[]),
            "--crate-type proc-macro -C prefer-dynamic -C embed-bitcode=no -C debug-assertions=off \
             --cfg feature=\"default\" --check-cfg cfg(docsrs,test) \
             --check-cfg cfg(feature, values(\"default\", \"deserialize_in_place\")) \
             -C metadata=M -C extra-filename=-M -C strip=debuginfo"
        );
        assert_eq!(tail_args(&unit, false), ["--extern", "proc_macro", "--cap-lints", "allow"]);
    }

    #[test]
    fn library_under_thin_lto_with_lints() {
        let unit = unit("lib", "lib", "libc", RELEASE, &["default", "std"]);
        assert_eq!(
            args(&unit, Lto::OnlyBitcode, &["default", "std"], &["--allow=unused_qualifications"]),
            "--crate-type lib -C opt-level=3 -C panic=abort -C linker-plugin-lto \
             --allow=unused_qualifications --cfg feature=\"default\" --cfg feature=\"std\" \
             --check-cfg cfg(docsrs,test) --check-cfg cfg(feature, values(\"default\", \"std\")) \
             -C metadata=M -C extra-filename=-M -C strip=debuginfo"
        );
    }

    #[test]
    fn local_binary() {
        let unit = unit("bin", "bin", "probe", RELEASE, &[]);
        assert_eq!(
            args(&unit, Lto::Run(Some("thin".to_string())), &[], &[]),
            "--crate-type bin -C opt-level=3 -C panic=abort -C lto=thin \
             --check-cfg cfg(docsrs,test) --check-cfg cfg(feature, values()) \
             -C metadata=M -C extra-filename=-M -C strip=debuginfo"
        );
        assert!(tail_args(&unit, true).is_empty());
    }

    #[test]
    fn dev_profile_has_debuginfo_and_no_assertion_flags() {
        let unit = unit("lib", "lib", "a", DEV, &[]);
        assert_eq!(
            args(&unit, Lto::OnlyObject, &[], &[]),
            "--crate-type lib -C embed-bitcode=no -C debuginfo=2 -C split-debuginfo=unpacked \
             --check-cfg cfg(docsrs,test) --check-cfg cfg(feature, values()) \
             -C metadata=M -C extra-filename=-M"
        );
    }
}
