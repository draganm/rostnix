//! Which LTO artifacts each unit must produce: a port of cargo's
//! `core/compiler/lto.rs`.
//!
//! A profile says whether the final link does LTO. What that asks of each
//! unit depends on the unit's crate types and on what depends on it: an
//! rlib under an LTO link needs only bitcode, one that is also linked
//! without LTO needs object code too.

use crate::unitgraph::{Unit, UnitGraph};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lto {
    /// rustc performs LTO itself: `-C lto`, or `-C lto=<mode>`.
    Run(Option<String>),
    /// LTO is explicitly off, thin-local LTO included.
    Off,
    /// The unit is only ever used for LTO: `-C linker-plugin-lto`.
    OnlyBitcode,
    /// The unit is linked both with and without LTO: rustc's default.
    ObjectAndBitcode,
    /// No LTO reaches the unit: `-C embed-bitcode=no`.
    OnlyObject,
}

impl Lto {
    /// The flags cargo passes for this setting.
    pub fn args(&self) -> Vec<String> {
        let flags: &[&str] = match self {
            Lto::Run(None) => &["lto"],
            Lto::Run(Some(mode)) => return vec!["-C".to_string(), format!("lto={mode}")],
            Lto::Off => &["lto=off", "embed-bitcode=no"],
            Lto::ObjectAndBitcode => &[],
            Lto::OnlyBitcode => &["linker-plugin-lto"],
            Lto::OnlyObject => &["embed-bitcode=no"],
        };
        flags.iter().flat_map(|flag| ["-C".to_string(), flag.to_string()]).collect()
    }
}

/// The LTO setting of every unit, by unit index.
pub fn generate(graph: &UnitGraph) -> Vec<Lto> {
    let mut map: Vec<Option<Lto>> = vec![None; graph.units.len()];
    for &root in &graph.roots {
        let unit = &graph.units[root];
        let root_lto = match unit.profile.lto.as_str() {
            // LTO not requested, no need for bitcode.
            "false" => Lto::OnlyObject,
            "off" => Lto::Off,
            _ => {
                let crate_types = crate_types(unit);
                if unit.target.for_host() {
                    Lto::OnlyObject
                } else if needs_object(&crate_types) {
                    lto_when_needs_object(&crate_types)
                } else {
                    // This may or may not take part in LTO; `calculate`
                    // widens it if something needs more.
                    Lto::OnlyBitcode
                }
            }
        };
        calculate(graph, &mut map, root, root_lto);
    }
    // Every unit is reachable from a root. One that were not would be
    // linked nowhere, and plain object code is the safe answer.
    map.into_iter().map(|lto| lto.unwrap_or(Lto::OnlyObject)).collect()
}

fn crate_types(unit: &Unit) -> Vec<&str> {
    match unit.mode.as_str() {
        "test" | "doctest" => vec!["bin"],
        _ => unit.target.crate_types.iter().map(String::as_str).collect(),
    }
}

fn can_lto(crate_type: &str) -> bool {
    matches!(crate_type, "bin" | "staticlib" | "cdylib")
}

fn is_dynamic(crate_type: &str) -> bool {
    matches!(crate_type, "dylib" | "cdylib" | "proc-macro")
}

/// Whether any of these crate types needs object code.
fn needs_object(crate_types: &[&str]) -> bool {
    crate_types.iter().any(|ct| can_lto(ct) || is_dynamic(ct))
}

/// The setting for a unit that needs object code while its parent runs LTO.
fn lto_when_needs_object(crate_types: &[&str]) -> Lto {
    if crate_types.iter().all(|ct| *ct == "dylib") {
        // rustc does not do LTO with dylibs, so bitcode is of no use.
        Lto::OnlyObject
    } else {
        // An rlib mixed with a dylib or cdylib: bitcode for the rlib, object
        // code for the dynamic library.
        Lto::ObjectAndBitcode
    }
}

fn calculate(graph: &UnitGraph, map: &mut [Option<Lto>], index: usize, parent_lto: Lto) {
    let unit = &graph.units[index];
    let crate_types = crate_types(unit);
    // LTO can only be performed if all of the crate types support it.
    let all_lto_types = crate_types.iter().all(|ct| can_lto(ct));
    let lto = if unit.target.for_host() {
        // LTO is for the final binary, not for build scripts and proc macros.
        Lto::OnlyObject
    } else if all_lto_types {
        // The parent does not matter: this unit is not embedded in it.
        match unit.profile.lto.as_str() {
            "true" => Lto::Run(None),
            "off" => Lto::Off,
            "false" => Lto::OnlyObject,
            mode => Lto::Run(Some(mode.to_string())),
        }
    } else {
        match (&parent_lto, needs_object(&crate_types)) {
            // An rlib whose parent is running LTO needs only bitcode.
            (Lto::Run(_), false) => Lto::OnlyBitcode,
            // LTO when something needs object code.
            (Lto::Run(_), true) | (Lto::OnlyBitcode, true) => lto_when_needs_object(&crate_types),
            // LTO is off; keep it off.
            (Lto::Off, _) => Lto::Off,
            // No requirement of its own, or one the parent already meets.
            (_, false) | (Lto::OnlyObject, true) | (Lto::ObjectAndBitcode, true) => parent_lto,
        }
    };

    // A unit reached more than once may have its requirements widened.
    let merged = match map[index].take() {
        None => lto,
        Some(seen) => {
            let merged = match (lto, seen.clone()) {
                (Lto::OnlyBitcode, Lto::OnlyBitcode) => Lto::OnlyBitcode,
                (Lto::OnlyObject, Lto::OnlyObject) => Lto::OnlyObject,
                // Once a unit runs LTO it keeps running it.
                (Lto::Run(mode), _) | (_, Lto::Run(mode)) => Lto::Run(mode),
                // Off means off.
                (Lto::Off, _) | (_, Lto::Off) => Lto::Off,
                // Both is the most a unit can be asked for.
                (Lto::ObjectAndBitcode, _) | (_, Lto::ObjectAndBitcode) => Lto::ObjectAndBitcode,
                (Lto::OnlyObject, Lto::OnlyBitcode) | (Lto::OnlyBitcode, Lto::OnlyObject) => {
                    Lto::ObjectAndBitcode
                }
            };
            if merged == seen {
                map[index] = Some(seen);
                return;
            }
            merged
        }
    };
    map[index] = Some(merged.clone());

    for dep in &unit.dependencies {
        calculate(graph, map, dep.index, merged.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A graph from `(kind, crate types, profile lto, dependencies)`, with
    /// unit 0 as the only root.
    fn graph(units: &[(&str, &[&str], &str, &[usize])]) -> UnitGraph {
        let units: Vec<String> = units
            .iter()
            .enumerate()
            .map(|(i, (kind, crate_types, lto, deps))| {
                let deps: Vec<String> = deps
                    .iter()
                    .map(|d| format!(r#"{{"index":{d},"extern_crate_name":"d{d}"}}"#))
                    .collect();
                let types: Vec<String> = crate_types.iter().map(|t| format!("\"{t}\"")).collect();
                format!(
                    r#"{{"pkg_id":"p{i}","target":{{"kind":["{kind}"],"crate_types":[{}],"name":"t{i}",
                        "src_path":"/s","edition":"2021"}},
                        "profile":{{"name":"release","opt_level":"3","lto":"{lto}","codegen_units":null,
                        "debuginfo":0,"split_debuginfo":null,"debug_assertions":false,
                        "overflow_checks":false,"rpath":false,"panic":"unwind","strip":{{"resolved":"None"}}}},
                        "platform":null,"mode":"build","features":[],"dependencies":[{}]}}"#,
                    types.join(","),
                    deps.join(",")
                )
            })
            .collect();
        serde_json::from_str(&format!(r#"{{"version":1,"units":[{}],"roots":[0]}}"#, units.join(",")))
            .unwrap()
    }

    fn thin() -> Lto {
        Lto::Run(Some("thin".to_string()))
    }

    #[test]
    fn thin_binary_asks_its_rlibs_for_bitcode_only() {
        let g = graph(&[("bin", &["bin"], "thin", &[1]), ("lib", &["lib"], "thin", &[2]), ("lib", &["lib"], "thin", &[])]);
        assert_eq!(generate(&g), [thin(), Lto::OnlyBitcode, Lto::OnlyBitcode]);
    }

    #[test]
    fn fat_lto_is_the_bare_flag() {
        let g = graph(&[("bin", &["bin"], "true", &[1]), ("lib", &["lib"], "true", &[])]);
        assert_eq!(generate(&g), [Lto::Run(None), Lto::OnlyBitcode]);
        assert_eq!(Lto::Run(None).args(), ["-C", "lto"]);
        assert_eq!(thin().args(), ["-C", "lto=thin"]);
    }

    #[test]
    fn no_lto_means_no_bitcode() {
        let g = graph(&[("bin", &["bin"], "false", &[1]), ("lib", &["lib"], "false", &[])]);
        assert_eq!(generate(&g), [Lto::OnlyObject, Lto::OnlyObject]);
        assert_eq!(Lto::OnlyObject.args(), ["-C", "embed-bitcode=no"]);
    }

    #[test]
    fn off_stays_off() {
        let g = graph(&[("bin", &["bin"], "off", &[1]), ("lib", &["lib"], "off", &[])]);
        assert_eq!(generate(&g), [Lto::Off, Lto::Off]);
        assert_eq!(Lto::Off.args(), ["-C", "lto=off", "-C", "embed-bitcode=no"]);
    }

    #[test]
    fn host_units_and_their_dependencies_get_object_code() {
        // bin -> proc macro -> lib; bin -> lib -> build-script run -> build script -> lib
        let g = graph(&[
            ("bin", &["bin"], "thin", &[1, 3]),
            ("proc-macro", &["proc-macro"], "thin", &[2]),
            ("lib", &["lib"], "thin", &[]),
            ("lib", &["lib"], "thin", &[4]),
            ("custom-build", &["bin"], "thin", &[5]),
            ("lib", &["lib"], "thin", &[]),
        ]);
        assert_eq!(
            generate(&g),
            [thin(), Lto::OnlyObject, Lto::OnlyObject, Lto::OnlyBitcode, Lto::OnlyObject, Lto::OnlyObject]
        );
    }

    // redb in core-rs: a cdylib that is also an rlib needs both, and so does
    // everything under it.
    #[test]
    fn cdylib_with_rlib_needs_both_and_passes_that_down() {
        let g = graph(&[
            ("bin", &["bin"], "thin", &[1]),
            ("lib", &["lib"], "thin", &[2, 3]),
            ("cdylib", &["cdylib", "rlib"], "thin", &[3]),
            ("lib", &["lib"], "thin", &[]),
        ]);
        let lto = generate(&g);
        assert_eq!(lto[1], Lto::OnlyBitcode);
        assert_eq!(lto[2], Lto::ObjectAndBitcode);
        assert_eq!(lto[3], Lto::ObjectAndBitcode);
        assert!(Lto::ObjectAndBitcode.args().is_empty());
    }

    #[test]
    fn a_library_root_under_lto_makes_bitcode() {
        let g = graph(&[("lib", &["lib"], "thin", &[])]);
        assert_eq!(generate(&g), [Lto::OnlyBitcode]);
        assert_eq!(Lto::OnlyBitcode.args(), ["-C", "linker-plugin-lto"]);
    }
}
