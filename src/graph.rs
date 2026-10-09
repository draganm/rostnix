//! The build graph: cargo's units joined with what `cargo metadata` knows
//! about their packages, ready to be printed as Nix.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

use sha2::{Digest, Sha256};
use toml::Table;

use crate::flags::{self, UnitFlags};
use crate::localsrc::{self, TargetInfo};
use crate::lockfile::Checksums;
use crate::lto;
use crate::metadata::{MetaTarget, Metadata, Package};
use crate::storepath::sanitize_name;
use crate::unitgraph::{Unit, UnitGraph};
use crate::{lints, Result};

pub const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";

#[derive(Debug, Default)]
pub struct Graph {
    pub cargo_version: String,
    pub host: String,
    pub sources: BTreeMap<String, Source>,
    pub packages: BTreeMap<String, PackageNode>,
    pub units: BTreeMap<String, UnitNode>,
    /// Target name of each binary and example the selection builds, and its
    /// unit.
    pub bins: BTreeMap<String, String>,
    pub roots: Vec<String>,
    /// The units that are tests: each compiles a test executable and runs
    /// it.
    pub tests: Vec<String>,
    /// What `cargo test` builds without running it, to see that it
    /// compiles: the examples.
    pub test_builds: Vec<String>,
    /// The units `cargo build` plans and the units `cargo test` plans. A
    /// unit both plan alike is in both.
    pub build_units: Vec<String>,
    pub test_units: Vec<String>,
}

/// A registry crate.
#[derive(Debug, Clone)]
pub struct Source {
    pub pname: String,
    pub version: String,
    pub sha256: String,
    pub url: String,
    /// Where cargo unpacked the crate. Not part of the generated graph.
    pub cargo_src_dir: String,
}

#[derive(Debug, Clone)]
pub struct PackageNode {
    pub name: String,
    pub version: String,
    pub local: bool,
    /// The package directory and the working directory of its rustc runs,
    /// relative to the source root; `""` is the root.
    pub manifest_dir: String,
    pub work_dir: String,
    pub links: Option<String>,
    /// The `crateOverrides` key that names this package, if one does.
    pub override_key: Option<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SrcRef {
    /// A key of `Graph::sources`.
    Registry(String),
    /// A view of the local source: `dir` without `exclude`.
    Local {
        name: String,
        dir: String,
        exclude: Vec<String>,
    },
}

#[derive(Debug, Clone)]
// A graph holds a few hundred nodes; boxing the larger kind would buy
// nothing.
#[allow(clippy::large_enum_variant)]
pub enum UnitNode {
    Compile(CompileUnit),
    Run(RunUnit),
}

#[derive(Debug, Clone)]
pub struct CompileUnit {
    pub name: String,
    pub package: String,
    pub src: SrcRef,
    /// What is built: `lib`, `proc-macro`, `bin`, `example`, `build-script`,
    /// or `test` for any target built as a test.
    pub kind: &'static str,
    /// What the target is, whatever it is built as: `lib`, `proc-macro`,
    /// `bin`, `example`, `test`, `bench` or `custom-build`.
    pub target_kind: &'static str,
    pub crate_name: String,
    pub target_name: String,
    pub edition: String,
    /// The root source file, relative to the package directory.
    pub src_path: String,
    pub metadata: String,
    /// Whether rustc runs the linker for this unit.
    pub linked: bool,
    pub pass_l: bool,
    pub rustc_args: Vec<String>,
    pub tail_args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Extern name and unit key of each direct dependency.
    pub deps: Vec<(String, String)>,
    /// The run of the package's build script.
    pub build_script: Option<String>,
    /// The overridden packages this unit links.
    pub overrides: Vec<String>,
    /// What running the test needs, for a unit of kind `test`.
    pub test: Option<TestInfo>,
}

/// What a test is run with, beside what it is compiled from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestInfo {
    /// The profile's directory in cargo's target directory.
    pub profile_dir: String,
    /// The binaries and examples the test finds beside itself: those of
    /// its package, for an integration test or a bench.
    pub executables: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RunUnit {
    pub name: String,
    pub package: String,
    pub src: SrcRef,
    /// The unit that compiles the script.
    pub script: String,
    pub features: Vec<String>,
    pub debug_assertions: bool,
    pub env: BTreeMap<String, String>,
    /// `links` name and build-script run of each dependency that has one.
    pub links_deps: Vec<(String, String)>,
}

pub struct Inputs<'a> {
    /// What `cargo build` plans.
    pub units: &'a UnitGraph,
    /// What `cargo test --no-run` plans, when tests are wanted.
    pub test_units: Option<&'a UnitGraph>,
    pub metadata: &'a Metadata,
    pub checksums: &'a Checksums,
    /// The source root as cargo saw it.
    pub src: &'a str,
    /// The workspace directory relative to it; `""` is the root.
    pub cargo_root: &'a str,
    pub host: &'a str,
    pub cargo_version: &'a str,
    pub override_keys: &'a [String],
    /// Reads the manifest at a path.
    pub read_manifest: &'a dyn Fn(&str) -> Result<Table>,
    /// Reads a source file of the tree, if it is there to be read.
    pub read_source: &'a dyn Fn(&str) -> Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Lib,
    ProcMacro,
    Bin,
    Example,
    BuildScript,
    Run,
    /// Any target built as a test executable.
    Test,
    /// Planned by cargo and not built: doc tests, which rustdoc compiles
    /// and runs, and what `cargo test` builds only to see that it compiles.
    Skipped,
}

impl Kind {
    fn of(unit: &Unit) -> Option<Kind> {
        let has = |kind: &str| unit.target.kind.iter().any(|k| k == kind);
        Some(if unit.mode == "run-custom-build" {
            Kind::Run
        } else if unit.mode == "test" {
            Kind::Test
        } else if unit.mode == "doctest" {
            Kind::Skipped
        } else if has("custom-build") {
            Kind::BuildScript
        } else if has("proc-macro") {
            Kind::ProcMacro
        } else if has("bin") {
            Kind::Bin
        } else if has("example") {
            Kind::Example
        } else if ["lib", "rlib", "dylib", "cdylib", "staticlib"]
            .iter()
            .any(|k| has(k))
        {
            Kind::Lib
        } else {
            return None;
        })
    }

    /// The word for this kind in unit keys and nodes.
    fn word(self) -> &'static str {
        match self {
            Kind::Lib => "lib",
            Kind::ProcMacro => "proc-macro",
            Kind::Bin => "bin",
            Kind::Example => "example",
            Kind::BuildScript => "build-script",
            Kind::Run => "run-build-script",
            Kind::Test => "test",
            Kind::Skipped => "skipped",
        }
    }

    fn name_prefix(self) -> &'static str {
        match self {
            Kind::Lib => "rustlib",
            Kind::ProcMacro => "rustmacro",
            Kind::Bin | Kind::Example => "rustbin",
            Kind::BuildScript => "rustbs",
            Kind::Run => "rustbsrun",
            Kind::Test => "rusttest",
            Kind::Skipped => "skipped",
        }
    }
}

/// What the graph needs to know about a package that owns a unit.
struct PkgInfo<'a> {
    pkg: &'a Package,
    key: String,
    local: bool,
    /// The package directory relative to the source root, for a local
    /// package.
    rel_dir: String,
    declared_features: Vec<String>,
    lint_flags: Vec<String>,
    manifest: Table,
}

/// What the units of every plan are built from.
struct Ctx<'a> {
    inp: &'a Inputs<'a>,
    src_root: &'a str,
    infos: &'a HashMap<&'a str, PkgInfo<'a>>,
    local_dirs: &'a [String],
    workspace_manifest: &'a Table,
}

/// The word for what a target is, whatever it is built as.
fn target_kind(unit: &Unit) -> &'static str {
    let has = |kind: &str| unit.target.kind.iter().any(|k| k == kind);
    if has("custom-build") {
        "custom-build"
    } else if has("proc-macro") {
        "proc-macro"
    } else if has("bin") {
        "bin"
    } else if has("example") {
        "example"
    } else if has("test") {
        "test"
    } else if has("bench") {
        "bench"
    } else {
        "lib"
    }
}

/// Whether a test of this unit's target is run by the test harness: true
/// unless the target's entry in the manifest says `harness = false`.
fn harness(manifest: &Table, unit: &Unit) -> bool {
    let entry = match (target_kind(unit), manifest) {
        ("lib" | "proc-macro", manifest) => manifest.get("lib").and_then(|lib| lib.as_table()),
        (table, manifest) => manifest
            .get(table)
            .and_then(|entries| entries.as_array())
            .and_then(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| entry.as_table())
                    .find(|entry| {
                        entry.get("name").and_then(|name| name.as_str())
                            == Some(unit.target.name.as_str())
                    })
            }),
    };
    entry
        .and_then(|entry| entry.get("harness"))
        .and_then(|harness| harness.as_bool())
        .unwrap_or(true)
}

/// The directory of a profile in cargo's target directory.
fn profile_dir(profile: &str) -> String {
    match profile {
        "dev" | "test" => "debug".to_string(),
        "bench" => "release".to_string(),
        other => other.to_string(),
    }
}

/// Says what each unit of a plan is, and refuses by name what this version
/// does not build.
fn classify(
    graph: &UnitGraph,
    by_id: &HashMap<&str, &Package>,
    test_plan: bool,
) -> Result<Vec<Kind>> {
    let mut kinds = Vec::with_capacity(graph.units.len());
    for unit in &graph.units {
        let pkg = by_id.get(unit.pkg_id.as_str()).ok_or_else(|| {
            format!(
                "cargo metadata does not know the package {} of the unit graph",
                unit.pkg_id
            )
        })?;
        let what = format!("{} of {} {}", unit.target.name, pkg.name, pkg.version);
        if let Some(platform) = &unit.platform {
            return Err(format!("{what} is planned for the target {platform}; cross-compilation is not supported yet").into());
        }
        let built = matches!(unit.mode.as_str(), "build" | "run-custom-build")
            || (test_plan && matches!(unit.mode.as_str(), "test" | "doctest"));
        if !built {
            return Err(format!(
                "{what} has the mode '{}', which this version does not build",
                unit.mode
            )
            .into());
        }
        let is_example = unit.target.kind.iter().any(|k| k == "example");
        if is_example && !unit.target.crate_types.iter().any(|ct| ct == "bin") {
            // `cargo test` builds every example to see that it compiles.
            // Nothing can use a library example yet, so it is left out
            // there, and refused where someone asked for it.
            if test_plan && unit.mode == "build" {
                kinds.push(Kind::Skipped);
                continue;
            }
            return Err(format!(
                "{what} is an example of crate type {}; only executable examples are built yet",
                unit.target.crate_types.join(", ")
            )
            .into());
        }
        let kind = Kind::of(unit).ok_or_else(|| {
            format!(
                "{what} is a target of kind {:?}, which this version does not build",
                unit.target.kind
            )
        })?;
        kinds.push(kind);
    }
    Ok(kinds)
}

pub fn build(inp: &Inputs) -> Result<Graph> {
    let src_root = inp.src.trim_end_matches('/');
    let by_id: HashMap<&str, &Package> = inp
        .metadata
        .packages
        .iter()
        .map(|p| (p.id.as_str(), p))
        .collect();

    // The build plan, and the test plan when tests are wanted.
    let build_kinds = classify(inp.units, &by_id, false)?;
    let test_plan = match inp.test_units {
        Some(tests) => Some((tests, classify(tests, &by_id, true)?)),
        None => None,
    };

    // Package keys: name and version, told apart by a hash of the id in the
    // rare case that two sources provide the same name and version.
    let mut used: Vec<&Package> = Vec::new();
    let planned = inp
        .units
        .units
        .iter()
        .chain(test_plan.iter().flat_map(|(tests, _)| tests.units.iter()));
    for unit in planned {
        let pkg = by_id[unit.pkg_id.as_str()];
        if !used.iter().any(|p| p.id == pkg.id) {
            used.push(pkg);
        }
    }
    let mut key_count: HashMap<String, usize> = HashMap::new();
    for pkg in &used {
        *key_count
            .entry(format!("{}-{}", pkg.name, pkg.version))
            .or_default() += 1;
    }

    let has_local = used.iter().any(|p| p.source.is_none());
    let workspace_manifest = if has_local {
        (inp.read_manifest)(&format!("{}/Cargo.toml", inp.metadata.workspace_root))?
    } else {
        Table::new()
    };
    let local_dirs: Vec<String> = inp
        .metadata
        .packages
        .iter()
        .filter(|p| p.source.is_none())
        .filter_map(|p| relative_to(p.manifest_dir(), src_root))
        .collect();

    let mut out = Graph {
        cargo_version: inp.cargo_version.to_string(),
        host: inp.host.to_string(),
        ..Graph::default()
    };
    let mut infos: HashMap<&str, PkgInfo> = HashMap::new();
    for pkg in used {
        let base = format!("{}-{}", pkg.name, pkg.version);
        let key = if key_count[&base] > 1 {
            format!("{base}-{}", &hash(&[&pkg.id])[..8])
        } else {
            base
        };
        let local = pkg.source.is_none();
        let rel_dir = if local {
            relative_to(pkg.manifest_dir(), src_root).ok_or_else(|| {
                format!(
                    "{} {} is a path dependency at {}, outside the source tree {src_root}",
                    pkg.name,
                    pkg.version,
                    pkg.manifest_dir()
                )
            })?
        } else {
            let source = pkg.source.as_deref().unwrap_or_default();
            if source != CRATES_IO {
                return Err(format!(
                    "{} {} comes from {source}; this version builds packages from crates.io and from the source tree only",
                    pkg.name, pkg.version
                )
                .into());
            }
            let sha256 = inp
                .checksums
                .get(&pkg.name, &pkg.version, source)
                .ok_or_else(|| {
                    format!(
                        "Cargo.lock has no checksum for {} {}",
                        pkg.name, pkg.version
                    )
                })?;
            out.sources.insert(
                key.clone(),
                Source {
                    pname: pkg.name.clone(),
                    version: pkg.version.clone(),
                    sha256: sha256.to_string(),
                    url: format!(
                        "https://static.crates.io/crates/{0}/{0}-{1}.crate",
                        pkg.name, pkg.version
                    ),
                    cargo_src_dir: pkg.manifest_dir().to_string(),
                },
            );
            String::new()
        };

        let manifest = (inp.read_manifest)(&pkg.manifest_path)?;
        let lint_flags = lints::rustflags(&manifest, local.then_some(&workspace_manifest))
            .map_err(|err| format!("{}: {err}", pkg.manifest_path))?;

        out.packages.insert(
            key.clone(),
            PackageNode {
                name: pkg.name.clone(),
                version: pkg.version.clone(),
                local,
                manifest_dir: rel_dir.clone(),
                work_dir: if local {
                    inp.cargo_root.to_string()
                } else {
                    String::new()
                },
                links: pkg.links.clone(),
                override_key: inp
                    .override_keys
                    .contains(&pkg.name)
                    .then(|| pkg.name.clone()),
                env: pkg.env(),
            },
        );
        infos.insert(
            pkg.id.as_str(),
            PkgInfo {
                pkg,
                key,
                local,
                rel_dir,
                declared_features: pkg.features.keys().cloned().collect(),
                lint_flags,
                manifest,
            },
        );
    }

    let ctx = Ctx {
        inp,
        src_root,
        infos: &infos,
        local_dirs: &local_dirs,
        workspace_manifest: &workspace_manifest,
    };
    let built = |keys: &[String], kinds: &[Kind]| -> Vec<String> {
        let mut keys: Vec<String> = keys
            .iter()
            .zip(kinds)
            .filter(|(_, kind)| **kind != Kind::Skipped)
            .map(|(key, _)| key.clone())
            .collect();
        keys.sort();
        keys.dedup();
        keys
    };

    let graph = inp.units;
    let keys = add_plan(&ctx, graph, &build_kinds, &mut out)?;
    out.build_units = built(&keys, &build_kinds);
    for &root in &graph.roots {
        out.roots.push(keys[root].clone());
        if matches!(build_kinds[root], Kind::Bin | Kind::Example) {
            let name = &graph.units[root].target.name;
            match out.bins.insert(name.clone(), keys[root].clone()) {
                Some(other) if other != keys[root] => {
                    return Err(format!(
                        "the selection builds two executables named {name} ({other} and {}), which would be installed under one name; select one of them with `packages`, `bins` or `examples`",
                        keys[root]
                    )
                    .into());
                }
                _ => {}
            }
        }
    }

    if let Some((graph, kinds)) = &test_plan {
        let keys = add_plan(&ctx, graph, kinds, &mut out)?;
        out.test_units = built(&keys, kinds);
        out.tests = keys
            .iter()
            .zip(kinds)
            .filter(|(_, kind)| **kind == Kind::Test)
            .map(|(key, _)| key.clone())
            .collect();
        out.tests.sort();
        out.tests.dedup();
        out.test_builds = graph
            .roots
            .iter()
            .filter(|&&root| !matches!(kinds[root], Kind::Test | Kind::Skipped))
            .map(|&root| keys[root].clone())
            .collect();
        out.test_builds.sort();
        out.test_builds.dedup();
    }
    Ok(out)
}

/// Adds the units of one of cargo's plans to the graph and returns the key
/// of each, by unit index. A unit an earlier plan described the same way
/// has the same key and is the same node.
fn add_plan(ctx: &Ctx, graph: &UnitGraph, kinds: &[Kind], out: &mut Graph) -> Result<Vec<String>> {
    let inp = ctx.inp;
    let info = |unit: &Unit| &ctx.infos[unit.pkg_id.as_str()];

    let ltos = lto::generate(graph);
    let primary: BTreeSet<&str> = graph
        .roots
        .iter()
        .map(|&r| graph.units[r].pkg_id.as_str())
        .collect();

    // Metadata hashes, dependencies first.
    let mut hashes: Vec<Option<String>> = vec![None; graph.units.len()];
    for index in 0..graph.units.len() {
        unit_hash(graph, ctx.infos, &ltos, &primary, &mut hashes, index);
    }
    let hashes: Vec<String> = hashes.into_iter().map(Option::unwrap).collect();

    let keys: Vec<String> = graph
        .units
        .iter()
        .enumerate()
        .map(|(i, unit)| {
            let target = match kinds[i] {
                Kind::Bin | Kind::Example | Kind::Test => format!("-{}", unit.target.name),
                _ => String::new(),
            };
            format!(
                "{}-{}{target}-{}",
                info(unit).key,
                kinds[i].word(),
                &hashes[i][..8]
            )
        })
        .collect();

    let mut closures: Vec<Option<Rc<BTreeSet<String>>>> = vec![None; graph.units.len()];
    for (i, unit) in graph.units.iter().enumerate() {
        let kind = kinds[i];
        if kind == Kind::Skipped || out.units.contains_key(&keys[i]) {
            continue;
        }
        let pkg_info = info(unit);
        let pkg = pkg_info.pkg;
        let name = sanitize_name(&match kind {
            Kind::Bin | Kind::Example | Kind::Test => {
                format!("{}-{}", kind.name_prefix(), unit.target.name)
            }
            _ => format!("{}-{}", kind.name_prefix(), pkg_info.key),
        });
        let src = if pkg_info.local {
            let view_of = if kind == Kind::Run {
                script_of(graph, unit).unwrap_or(unit)
            } else {
                unit
            };
            local_src(
                ctx,
                pkg_info,
                view_of,
                kind == Kind::Run,
                kind == Kind::Test,
            )
        } else {
            SrcRef::Registry(pkg_info.key.clone())
        };

        if kind == Kind::Run {
            let script = unit
                .dependencies
                .iter()
                .find(|d| kinds[d.index] == Kind::BuildScript)
                .ok_or_else(|| {
                    format!(
                        "the build-script run of {} {} has no build script",
                        pkg.name, pkg.version
                    )
                })?;
            let mut links_deps = Vec::new();
            for dep in unit
                .dependencies
                .iter()
                .filter(|d| kinds[d.index] == Kind::Run)
            {
                let dep_pkg = info(&graph.units[dep.index]).pkg;
                let links = dep_pkg.links.clone().ok_or_else(|| {
                    format!(
                        "{} depends on the build script of {}, which sets no `links`",
                        pkg.name, dep_pkg.name
                    )
                })?;
                links_deps.push((links, keys[dep.index].clone()));
            }
            let mut env = BTreeMap::from([
                ("OPT_LEVEL".to_string(), unit.profile.opt_level.clone()),
                (
                    "DEBUG".to_string(),
                    unit.profile.debuginfo().is_some().to_string(),
                ),
                (
                    "PROFILE".to_string(),
                    profile_root(&unit.profile.name, ctx.workspace_manifest).to_string(),
                ),
                ("TARGET".to_string(), inp.host.to_string()),
                ("HOST".to_string(), inp.host.to_string()),
            ]);
            if let Some(links) = &pkg.links {
                env.insert("CARGO_MANIFEST_LINKS".to_string(), links.clone());
            }
            out.units.insert(
                keys[i].clone(),
                UnitNode::Run(RunUnit {
                    name,
                    package: pkg_info.key.clone(),
                    src,
                    script: keys[script.index].clone(),
                    features: unit.features.clone(),
                    debug_assertions: unit.profile.debug_assertions,
                    env,
                    links_deps,
                }),
            );
            continue;
        }

        let manifest_dir = pkg.manifest_dir();
        // Cargo names a root as the manifest wrote it: `path = "../x.rs"`
        // comes out with the `..` in it.
        let root = normalized(&unit.target.src_path);
        let src_path = relative_to(&root, manifest_dir).ok_or_else(|| {
            format!(
                "the target {} of {} {} has its root at {}, outside the package directory",
                unit.target.name, pkg.name, pkg.version, root
            )
        })?;
        let is_primary = primary.contains(unit.pkg_id.as_str());
        let rustc_args = flags::base_args(&UnitFlags {
            unit,
            lto: &ltos[i],
            declared_features: &pkg_info.declared_features,
            lint_flags: &pkg_info.lint_flags,
            metadata: &hashes[i],
            primary: is_primary,
            harness: harness(&pkg_info.manifest, unit),
        });

        let target_kind = target_kind(unit);
        let mut env = BTreeMap::from([("CARGO_CRATE_NAME".to_string(), unit.target.crate_name())]);
        if matches!(target_kind, "bin" | "example") {
            env.insert("CARGO_BIN_NAME".to_string(), unit.target.name.clone());
        }
        if is_primary {
            env.insert("CARGO_PRIMARY_PACKAGE".to_string(), "1".to_string());
        }

        // An integration test or a bench finds its package's binaries and
        // examples in the target directory: `cargo test` builds them before
        // it runs anything. A test depends on the binaries so that they are
        // built, not to link them. Unit tests are promised none of this.
        let test = (kind == Kind::Test).then(|| {
            let mut executables: Vec<String> = Vec::new();
            if matches!(target_kind, "test" | "bench") {
                executables.extend(
                    unit.dependencies
                        .iter()
                        .filter(|d| kinds[d.index] == Kind::Bin)
                        .map(|d| keys[d.index].clone()),
                );
                executables.extend(
                    graph
                        .units
                        .iter()
                        .enumerate()
                        .filter(|(j, other)| {
                            kinds[*j] == Kind::Example && other.pkg_id == unit.pkg_id
                        })
                        .map(|(j, _)| keys[j].clone()),
                );
                executables.sort();
                executables.dedup();
            }
            TestInfo {
                profile_dir: profile_dir(&unit.profile.name),
                executables,
            }
        });
        let binary_of_a_test =
            |d: &crate::unitgraph::UnitDep| kind == Kind::Test && kinds[d.index] == Kind::Bin;

        let linked = kind == Kind::Test
            || unit
                .target
                .crate_types
                .iter()
                .any(|ct| matches!(ct.as_str(), "bin" | "proc-macro" | "cdylib" | "dylib"));
        let overrides = if linked {
            link_closure(graph, ctx.infos, kinds, &mut closures, i)
                .iter()
                .filter(|name| inp.override_keys.contains(name))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };

        out.units.insert(
            keys[i].clone(),
            UnitNode::Compile(CompileUnit {
                name,
                package: pkg_info.key.clone(),
                src,
                kind: kind.word(),
                target_kind,
                crate_name: unit.target.crate_name(),
                target_name: unit.target.name.clone(),
                edition: unit.target.edition.clone(),
                src_path,
                metadata: hashes[i].clone(),
                linked,
                // The script's `-l` flags go to the package's library, or to
                // its executables when it has none.
                pass_l: matches!(target_kind, "lib" | "proc-macro") || !pkg.has_lib(),
                rustc_args,
                tail_args: flags::tail_args(unit, pkg_info.local),
                env,
                deps: unit
                    .dependencies
                    .iter()
                    .filter(|d| kinds[d.index] != Kind::Run && !binary_of_a_test(d))
                    .map(|d| (d.extern_crate_name.clone(), keys[d.index].clone()))
                    .collect(),
                build_script: unit
                    .dependencies
                    .iter()
                    .find(|d| kinds[d.index] == Kind::Run)
                    .map(|d| keys[d.index].clone()),
                overrides,
                test,
            }),
        );
    }
    Ok(keys)
}

/// An absolute path with its `.` and `..` resolved.
fn normalized(path: &str) -> String {
    format!("/{}", localsrc::resolve("", path))
}

/// `path` relative to `dir`, when it is `dir` or under it. `""` is `dir`
/// itself.
fn relative_to(path: &str, dir: &str) -> Option<String> {
    let rest = path.strip_prefix(dir)?;
    match rest.strip_prefix('/') {
        Some(rel) => Some(rel.to_string()),
        None if rest.is_empty() => Some(String::new()),
        None => None,
    }
}

fn hash(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    hasher
        .finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The metadata hash of a unit: what it is and what it is built from, with
/// no path in it, so that it is the same on every machine.
fn unit_hash(
    graph: &UnitGraph,
    infos: &HashMap<&str, PkgInfo>,
    ltos: &[lto::Lto],
    primary: &BTreeSet<&str>,
    hashes: &mut Vec<Option<String>>,
    index: usize,
) -> String {
    if let Some(done) = &hashes[index] {
        return done.clone();
    }
    let unit = &graph.units[index];
    let info = &infos[unit.pkg_id.as_str()];
    let source = match &info.pkg.source {
        Some(source) => source.clone(),
        None => format!("path+{}", info.rel_dir),
    };
    let profile = &unit.profile;
    let mut parts = vec![
        "rostnix-unit-2".to_string(),
        info.pkg.name.clone(),
        info.pkg.version.clone(),
        source,
        unit.target.kind.join(","),
        unit.target.name.clone(),
        unit.target.crate_types.join(","),
        unit.mode.clone(),
        unit.features.join(","),
        format!(
            "{}|{}|{}|{:?}|{:?}|{:?}|{}|{}|{}|{}|{:?}",
            profile.name,
            profile.opt_level,
            profile.lto,
            profile.codegen_units,
            profile.debuginfo(),
            profile.split_debuginfo,
            profile.debug_assertions,
            profile.overflow_checks,
            profile.rpath,
            profile.panic,
            profile.strip()
        ),
        format!("{:?}", ltos[index]),
        // Whether the package is one the plan was asked for reaches the
        // unit's environment, and plans differ in what they were asked for.
        format!("primary={}", primary.contains(unit.pkg_id.as_str())),
    ];
    let mut deps: Vec<String> = unit
        .dependencies
        .iter()
        .map(|dep| {
            format!(
                "{}={}",
                dep.extern_crate_name,
                unit_hash(graph, infos, ltos, primary, hashes, dep.index)
            )
        })
        .collect();
    deps.sort();
    parts.extend(deps);

    let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
    let digest = hash(&refs);
    hashes[index] = Some(digest.clone());
    digest
}

/// The build-script unit a run executes.
fn script_of<'a>(graph: &'a UnitGraph, run: &Unit) -> Option<&'a Unit> {
    run.dependencies
        .iter()
        .map(|d| &graph.units[d.index])
        .find(|u| u.target.is_custom_build() && u.mode == "build")
}

/// The names of the packages whose code a unit links: its own, unless it is
/// a build script, and those of the libraries it depends on. A proc macro or
/// a build script is linked by itself, so the walk does not enter them.
fn link_closure(
    graph: &UnitGraph,
    infos: &HashMap<&str, PkgInfo>,
    kinds: &[Kind],
    closures: &mut Vec<Option<Rc<BTreeSet<String>>>>,
    index: usize,
) -> Rc<BTreeSet<String>> {
    if let Some(done) = &closures[index] {
        return done.clone();
    }
    let unit = &graph.units[index];
    // A build script is built before its package's native library exists;
    // it links what it depends on and nothing of its own package.
    let mut names = BTreeSet::new();
    if kinds[index] != Kind::BuildScript {
        names.insert(infos[unit.pkg_id.as_str()].pkg.name.clone());
    }
    for dep in &unit.dependencies {
        if kinds[dep.index] == Kind::Lib {
            names.extend(
                link_closure(graph, infos, kinds, closures, dep.index)
                    .iter()
                    .cloned(),
            );
        }
    }
    let names = Rc::new(names);
    closures[index] = Some(names.clone());
    names
}

fn target_info(target_kind: &[String], executable: bool, src_path: String) -> TargetInfo {
    let has = |kind: &str| target_kind.iter().any(|k| k == kind);
    TargetInfo {
        executable,
        own_dir: if has("example") {
            Some("examples")
        } else if has("test") {
            Some("tests")
        } else if has("bench") {
            Some("benches")
        } else {
            None
        },
        src_path,
    }
}

/// The view a unit gets of its local package. A build script reads files
/// rustc never names, the roots of the package's executables among them, so
/// its run is shown those too.
fn local_src(
    ctx: &Ctx,
    info: &PkgInfo,
    unit: &Unit,
    sees_every_root: bool,
    as_test: bool,
) -> SrcRef {
    let (local_dirs, src_root) = (ctx.local_dirs, ctx.src_root);
    let targets: Vec<TargetInfo> = info
        .pkg
        .targets
        .iter()
        .filter(|_| !sees_every_root)
        .filter_map(|t: &MetaTarget| {
            Some(target_info(
                &t.kind,
                t.is_executable(),
                relative_to(&normalized(&t.src_path), src_root)?,
            ))
        })
        .collect();
    let unit_target = target_info(
        &unit.target.kind,
        false,
        relative_to(&normalized(&unit.target.src_path), src_root).unwrap_or_default(),
    );
    // Another target's root that this unit's root names as a module is
    // part of this unit: `mod common;` beside `tests/common.rs`.
    let root_dir = unit_target
        .src_path
        .rsplit_once('/')
        .map_or("", |(dir, _)| dir);
    let keep: Vec<String> = (ctx.inp.read_source)(&unit.target.src_path)
        .map(|text| {
            localsrc::declared_modules(&text)
                .iter()
                .map(|module| localsrc::resolve(root_dir, module))
                .collect()
        })
        .unwrap_or_default();
    SrcRef::Local {
        name: sanitize_name(&format!("rustsrc-{}", info.key)),
        dir: info.rel_dir.clone(),
        exclude: localsrc::exclusions(
            &info.rel_dir,
            local_dirs,
            &targets,
            &unit_target,
            as_test,
            &keep,
        ),
    }
}

/// Whether a profile descends from `release` or from `dev`, which is what
/// build scripts are told in `PROFILE`.
fn profile_root(name: &str, workspace_manifest: &Table) -> &'static str {
    let mut name = name.to_string();
    for _ in 0..16 {
        match name.as_str() {
            "release" | "bench" => return "release",
            "dev" | "test" => return "debug",
            _ => {}
        }
        let inherits = workspace_manifest
            .get("profile")
            .and_then(|profiles| profiles.get(&name))
            .and_then(|profile| profile.get("inherits"))
            .and_then(|inherits| inherits.as_str());
        match inherits {
            Some(parent) => name = parent.to_string(),
            None => break,
        }
    }
    // A profile defined outside the manifest; release is the likelier root
    // for a profile someone builds with Nix.
    "release"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core_rs(override_keys: &[String]) -> Graph {
        core_rs_with(override_keys, |_, _, _| {}).unwrap()
    }

    /// The graph of core-rs as recorded, after `change` has had its way
    /// with cargo's output and the lockfile.
    fn core_rs_with(
        override_keys: &[String],
        change: impl FnOnce(&mut UnitGraph, &mut Metadata, &mut String),
    ) -> Result<Graph> {
        let mut units: UnitGraph =
            serde_json::from_str(include_str!("../testdata/core-rs/unit-graph.json")).unwrap();
        let mut metadata: Metadata =
            serde_json::from_str(include_str!("../testdata/core-rs/metadata.json")).unwrap();
        // Every registry package gets a checksum made of its name.
        let mut lock = String::new();
        for pkg in metadata.packages.iter().filter(|p| p.source.is_some()) {
            lock.push_str(&format!(
                "[[package]]\nname = \"{}\"\nversion = \"{}\"\nsource = \"{CRATES_IO}\"\nchecksum = \"sum-{}\"\n\n",
                pkg.name, pkg.version, pkg.name
            ));
        }
        change(&mut units, &mut metadata, &mut lock);
        let checksums = Checksums::parse(&lock).unwrap();
        build(&Inputs {
            units: &units,
            test_units: None,
            metadata: &metadata,
            checksums: &checksums,
            src: "/src",
            cargo_root: "",
            host: "aarch64-apple-darwin",
            cargo_version: "1.95.0",
            override_keys,
            read_manifest: &|_| Ok(Table::new()),
            read_source: &|_| None,
        })
    }

    fn rejection(change: impl FnOnce(&mut UnitGraph, &mut Metadata, &mut String)) -> String {
        core_rs_with(&[], change).unwrap_err().to_string()
    }

    fn package_mut<'a>(metadata: &'a mut Metadata, name: &str) -> &'a mut Package {
        metadata
            .packages
            .iter_mut()
            .find(|p| p.name == name)
            .unwrap()
    }

    #[test]
    fn packages_from_git_or_another_registry_are_refused_by_name() {
        for source in [
            "git+https://github.com/KokaKiwi/rust-hex#abcdef",
            "registry+https://example.com/index",
        ] {
            let err = rejection(|_, metadata, _| {
                package_mut(metadata, "hex").source = Some(source.to_string())
            });
            assert!(
                err.contains("hex 0.4.3") && err.contains(source) && err.contains("crates.io"),
                "{err}"
            );
        }
    }

    #[test]
    fn a_path_dependency_outside_the_source_is_refused() {
        let err = rejection(|_, metadata, _| {
            let hex = package_mut(metadata, "hex");
            hex.source = None;
            hex.manifest_path = "/elsewhere/hex/Cargo.toml".to_string();
        });
        assert!(
            err.contains("hex 0.4.3")
                && err.contains("/elsewhere/hex")
                && err.contains("outside the source tree /src"),
            "{err}"
        );
    }

    #[test]
    fn a_unit_for_another_target_is_refused() {
        let err = rejection(|units, _, _| {
            units.units[0].platform = Some("x86_64-unknown-linux-gnu".to_string())
        });
        assert!(
            err.contains("x86_64-unknown-linux-gnu") && err.contains("cross-compilation"),
            "{err}"
        );
    }

    #[test]
    fn a_mode_that_is_not_built_is_refused() {
        let err = rejection(|units, _, _| {
            let root = units.roots[0];
            units.units[root].mode = "test".to_string();
        });
        assert!(
            err.contains("amber-store of amber-store-core 0.10.0") && err.contains("'test'"),
            "{err}"
        );
    }

    #[test]
    fn an_example_that_is_a_library_is_refused() {
        let err = rejection(|units, _, _| {
            let root = units.roots[0];
            units.units[root].target.crate_types = vec!["cdylib".to_string()];
        });
        assert!(
            err.contains("amber-store of amber-store-core 0.10.0")
                && err.contains("crate type cdylib"),
            "{err}"
        );
    }

    // A binary and an example of one name, or binaries of one name in two
    // packages, cannot both be installed.
    #[test]
    fn two_executables_of_one_name_are_refused() {
        let err = rejection(|units, _, _| {
            let mut twin = units.units[units.roots[0]].clone();
            twin.features = vec!["other".to_string()];
            units.units.push(twin);
            units.roots.push(units.units.len() - 1);
        });
        assert!(
            err.contains("two executables named amber-store")
                && err.contains("`packages`, `bins` or `examples`"),
            "{err}"
        );
    }

    // A build script reads what it likes of its package: its run sees the
    // roots of the package's executables, which a library does not.
    #[test]
    fn a_build_script_run_sees_every_root_in_src() {
        let graph = core_rs_with(&[], |units, metadata, _| {
            // Give the local package a build script and a binary.
            let lib = units
                .units
                .iter()
                .position(|u| u.pkg_id.contains("amber-store-core") && u.target.kind == ["lib"])
                .unwrap();
            let mut script = units.units[lib].clone();
            script.target.kind = vec!["custom-build".to_string()];
            script.target.crate_types = vec!["bin".to_string()];
            script.target.name = "build-script-build".to_string();
            script.target.src_path = "/src/build.rs".to_string();
            script.dependencies.clear();
            let mut run = script.clone();
            run.mode = "run-custom-build".to_string();
            units.units.push(script);
            run.dependencies = vec![crate::unitgraph::UnitDep {
                index: units.units.len() - 1,
                extern_crate_name: "build_script_build".to_string(),
            }];
            units.units.push(run);
            let run_index = units.units.len() - 1;
            units.units[lib]
                .dependencies
                .push(crate::unitgraph::UnitDep {
                    index: run_index,
                    extern_crate_name: "build_script_build".to_string(),
                });
            let pkg = package_mut(metadata, "amber-store-core");
            pkg.targets.push(MetaTarget {
                kind: vec!["bin".to_string()],
                name: "tool".to_string(),
                src_path: "/src/src/main.rs".to_string(),
            });
        })
        .unwrap();
        let view = |prefix: &str| {
            let (_, unit) = graph
                .units
                .iter()
                .find(|(key, _)| key.starts_with(prefix))
                .unwrap();
            let src = match unit {
                UnitNode::Compile(unit) => &unit.src,
                UnitNode::Run(unit) => &unit.src,
            };
            let SrcRef::Local { exclude, .. } = src else {
                panic!()
            };
            exclude.clone()
        };
        assert_eq!(
            view("amber-store-core-0.10.0-lib-"),
            ["benches", "examples", "src/main.rs", "tests"]
        );
        assert_eq!(
            view("amber-store-core-0.10.0-build-script-"),
            ["benches", "examples", "src/main.rs", "tests"]
        );
        assert_eq!(
            view("amber-store-core-0.10.0-run-build-script-"),
            ["benches", "examples", "tests"]
        );
    }

    #[test]
    fn a_registry_package_without_checksum_is_refused() {
        let err =
            rejection(|_, _, lock| *lock = lock.replace("name = \"hex\"", "name = \"other\""));
        assert_eq!(err, "Cargo.lock has no checksum for hex 0.4.3");
    }

    fn compile<'a>(graph: &'a Graph, key_prefix: &str) -> Vec<&'a CompileUnit> {
        graph
            .units
            .iter()
            .filter(|(key, _)| key.starts_with(key_prefix))
            .filter_map(|(_, unit)| match unit {
                UnitNode::Compile(unit) => Some(unit),
                UnitNode::Run(_) => None,
            })
            .collect()
    }

    #[test]
    fn every_unit_of_core_rs_becomes_a_node() {
        let graph = core_rs(&[]);
        assert_eq!(graph.units.len(), 103);
        assert_eq!(graph.roots.len(), 1);
        assert_eq!(graph.bins.keys().collect::<Vec<_>>(), ["amber-store"]);
        assert_eq!(graph.bins["amber-store"], graph.roots[0]);
        assert!(
            graph.roots[0].starts_with("amber-store-core-0.10.0-example-amber-store-"),
            "{}",
            graph.roots[0]
        );
        // Only registry packages have a source to fetch.
        assert!(!graph.sources.contains_key("amber-store-core-0.10.0"));
        assert_eq!(
            graph.sources["zstd-sys-2.0.16+zstd.1.5.7"].sha256,
            "sum-zstd-sys"
        );
        assert_eq!(
            graph.sources["lz4-sys-1.11.1+lz4-1.10.0"].url,
            "https://static.crates.io/crates/lz4-sys/lz4-sys-1.11.1+lz4-1.10.0.crate"
        );
    }

    // libc is a normal dependency and a build dependency with other features.
    #[test]
    fn a_package_planned_twice_gives_two_units() {
        let graph = core_rs(&[]);
        let libs: Vec<_> = compile(&graph, "libc-")
            .into_iter()
            .filter(|u| u.kind == "lib")
            .collect();
        assert_eq!(libs.len(), 2);
        assert_ne!(libs[0].metadata, libs[1].metadata);
        assert_eq!(libs[0].name, libs[1].name);
        assert!(libs
            .iter()
            .all(|u| !u.linked && u.name.starts_with("rustlib-libc-")));
    }

    #[test]
    fn metadata_names_no_path_and_is_stable() {
        let (a, b) = (core_rs(&[]), core_rs(&[]));
        assert_eq!(a.roots, b.roots);
        for unit in compile(&a, "") {
            assert_eq!(unit.metadata.len(), 16);
            assert!(unit
                .rustc_args
                .contains(&format!("metadata={}", unit.metadata)));
        }
    }

    #[test]
    fn build_scripts_are_compiled_run_and_consumed() {
        let graph = core_rs(&[]);
        let run_key = graph
            .units
            .keys()
            .find(|k| k.starts_with("zstd-safe-7.2.4-run-build-script-"))
            .unwrap();
        let UnitNode::Run(run) = &graph.units[run_key] else {
            panic!()
        };
        assert!(run.script.starts_with("zstd-safe-7.2.4-build-script-"));
        assert_eq!(run.links_deps.len(), 1);
        assert_eq!(run.links_deps[0].0, "zstd");
        assert!(run.links_deps[0]
            .1
            .starts_with("zstd-sys-2.0.16+zstd.1.5.7-run-build-script-"));
        assert_eq!(run.env["PROFILE"], "release");
        assert_eq!(run.env["OPT_LEVEL"], "3");
        assert_eq!(run.env["DEBUG"], "false");
        assert_eq!(run.env["TARGET"], "aarch64-apple-darwin");

        let lib = compile(&graph, "zstd-safe-7.2.4-lib-")[0];
        assert_eq!(lib.build_script.as_deref(), Some(run_key.as_str()));
        assert!(lib.pass_l);
        assert!(lib
            .deps
            .iter()
            .all(|(_, key)| !key.contains("-run-build-script-")));

        let script = compile(&graph, "zstd-sys-2.0.16+zstd.1.5.7-build-script-")[0];
        assert!(script.linked && !script.pass_l);
        assert_eq!(script.crate_name, "build_script_build");
        assert_eq!(script.src_path, "build.rs");
        let UnitNode::Run(sys_run) = &graph.units[&run.links_deps[0].1] else {
            panic!()
        };
        assert_eq!(sys_run.env["CARGO_MANIFEST_LINKS"], "zstd");
    }

    #[test]
    fn local_units_get_views_and_registry_units_sources() {
        let graph = core_rs(&[]);
        let example = compile(&graph, "amber-store-core-0.10.0-example-")[0];
        assert_eq!(
            example.src,
            SrcRef::Local {
                name: "rustsrc-amber-store-core-0.10.0".to_string(),
                dir: String::new(),
                exclude: [
                    "benches",
                    "examples/amber-bench.rs",
                    "examples/repair-interop.rs",
                    "tests"
                ]
                .map(String::from)
                .to_vec(),
            }
        );
        assert_eq!(example.src_path, "examples/amber-store.rs");
        assert_eq!(example.env["CARGO_BIN_NAME"], "amber-store");
        assert_eq!(example.env["CARGO_PRIMARY_PACKAGE"], "1");
        assert!(example.tail_args.is_empty());

        let lib = compile(&graph, "amber-store-core-0.10.0-lib-")[0];
        let SrcRef::Local { exclude, .. } = &lib.src else {
            panic!()
        };
        assert_eq!(exclude, &["benches", "examples", "tests"]);

        let redb = compile(&graph, "redb-")[0];
        assert!(matches!(&redb.src, SrcRef::Registry(key) if key.starts_with("redb-")));
        assert_eq!(redb.tail_args, ["--cap-lints", "allow"]);
        assert!(!redb.env.contains_key("CARGO_PRIMARY_PACKAGE"));
    }

    // redb's library is also a cdylib, so rustc links it.
    #[test]
    fn linking_units_know_the_overrides_in_their_closure() {
        let keys = [
            "libsqlite3-sys".to_string(),
            "syn".to_string(),
            "absent".to_string(),
        ];
        let graph = core_rs(&keys);
        let example = compile(&graph, "amber-store-core-0.10.0-example-")[0];
        // syn is only inside proc macros, which link themselves.
        assert_eq!(example.overrides, ["libsqlite3-sys"]);
        let derive = compile(&graph, "serde_derive-")[0];
        assert_eq!(derive.kind, "proc-macro");
        assert_eq!(derive.overrides, ["syn"]);
        let redb = compile(&graph, "redb-")
            .into_iter()
            .find(|u| u.kind == "lib")
            .unwrap();
        assert!(redb.linked);
        assert!(redb.overrides.is_empty());
        // A package's build script links nothing of the package itself.
        let script = compile(&graph, "libsqlite3-sys-0.38.2-build-script-")[0];
        assert!(script.linked);
        assert!(script.overrides.is_empty());
        assert_eq!(
            graph.packages["libsqlite3-sys-0.38.2"]
                .override_key
                .as_deref(),
            Some("libsqlite3-sys")
        );
        assert_eq!(graph.packages["redb-2.6.3"].override_key, None);
    }

    /// The graph of the hello fixture from what cargo 1.95 printed for it:
    /// the plan of `cargo build` and, with tests, that of `cargo test`.
    fn hello_with(with_tests: bool, change: impl FnOnce(&mut UnitGraph)) -> Result<Graph> {
        hello_reading(with_tests, change, &|_| None)
    }

    /// The same, with the source files `read_source` holds.
    fn hello_reading(
        with_tests: bool,
        change: impl FnOnce(&mut UnitGraph),
        read_source: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Graph> {
        let units: UnitGraph =
            serde_json::from_str(include_str!("../testdata/hello/build-graph.json")).unwrap();
        let mut tests: UnitGraph =
            serde_json::from_str(include_str!("../testdata/hello/test-graph.json")).unwrap();
        let metadata: Metadata =
            serde_json::from_str(include_str!("../testdata/hello/metadata.json")).unwrap();
        let mut lock = String::new();
        for pkg in metadata.packages.iter().filter(|p| p.source.is_some()) {
            lock.push_str(&format!(
                "[[package]]\nname = \"{}\"\nversion = \"{}\"\nsource = \"{CRATES_IO}\"\nchecksum = \"sum\"\n\n",
                pkg.name, pkg.version
            ));
        }
        change(&mut tests);
        let checksums = Checksums::parse(&lock).unwrap();
        build(&Inputs {
            units: &units,
            test_units: with_tests.then_some(&tests),
            metadata: &metadata,
            checksums: &checksums,
            src: "/src",
            cargo_root: "",
            host: "aarch64-apple-darwin",
            cargo_version: "1.95.0",
            override_keys: &[],
            read_manifest: &|_| Ok(Table::new()),
            read_source,
        })
    }

    fn hello(with_tests: bool) -> Graph {
        hello_with(with_tests, |_| {}).unwrap()
    }

    /// A key without the hash at its end.
    fn unhashed(key: &str) -> &str {
        key.rsplit_once('-').unwrap().0
    }

    /// The unit that builds a target of hello as a test.
    fn hello_test<'a>(graph: &'a Graph, target_kind: &str, name: &str) -> &'a CompileUnit {
        compile(graph, &format!("hello-0.1.0-test-{name}-"))
            .into_iter()
            .find(|unit| unit.target_kind == target_kind)
            .unwrap()
    }

    fn excluded(src: &SrcRef) -> Vec<&str> {
        let SrcRef::Local { exclude, .. } = src else {
            panic!("not a local source")
        };
        exclude.iter().map(String::as_str).collect()
    }

    // Everything `cargo build` plans for hello, `cargo test` plans the same
    // way: tests add units and take none away.
    #[test]
    fn a_unit_both_plans_describe_alike_is_one_node() {
        let (plain, graph) = (hello(false), hello(true));
        assert!(plain.tests.is_empty() && plain.test_units.is_empty());
        assert_eq!(plain.build_units, graph.build_units);
        assert_eq!(plain.build_units.len(), plain.units.len());
        assert_eq!(plain.roots, graph.roots);
        for key in &graph.build_units {
            assert!(graph.test_units.contains(key), "{key} is planned twice");
        }
        let added: Vec<&str> = graph
            .test_units
            .iter()
            .filter(|key| !graph.build_units.contains(key))
            .map(|key| unhashed(key))
            .collect();
        assert_eq!(
            added,
            [
                "hello-0.1.0-example-extra",
                "hello-0.1.0-test-cli",
                "hello-0.1.0-test-hello",
                "hello-0.1.0-test-hello",
                "hello-0.1.0-test-smoke"
            ]
        );
        // The doc test cargo plans is not built.
        assert_eq!(graph.units.len(), graph.test_units.len());
        assert!(!graph.units.keys().any(|key| key.contains("skipped")));
        // Only what is installed counts as a binary of the application.
        assert_eq!(graph.bins.keys().collect::<Vec<_>>(), ["hello"]);
    }

    #[test]
    fn a_test_is_an_executable_built_with_the_harness() {
        let graph = hello(true);
        let cli = hello_test(&graph, "test", "cli");
        assert_eq!(cli.kind, "test");
        assert_eq!(cli.name, "rusttest-cli");
        assert!(cli.linked);
        assert!(cli.rustc_args.contains(&"--test".to_string()));
        assert!(!cli.rustc_args.contains(&"--crate-type".to_string()));
        assert_eq!(cli.src_path, "tests/cli.rs");
        // It links the library. The binary it depends on is something to
        // run, and so is the example.
        let deps: Vec<(&str, &str)> = cli
            .deps
            .iter()
            .filter(|(name, _)| name == "hello")
            .map(|(name, key)| (name.as_str(), unhashed(key)))
            .collect();
        assert_eq!(deps, [("hello", "hello-0.1.0-lib")]);
        let info = cli.test.as_ref().unwrap();
        assert_eq!(info.profile_dir, "release");
        let executables: Vec<&str> = info.executables.iter().map(|key| unhashed(key)).collect();
        assert_eq!(
            executables,
            ["hello-0.1.0-bin-hello", "hello-0.1.0-example-extra"]
        );
        // The binary the test runs is the one that is installed.
        assert_eq!(info.executables[0], graph.bins["hello"]);
        assert!(!cli.env.contains_key("CARGO_BIN_NAME"));

        // Unit tests: the library and the binary, each built as a test.
        let lib = hello_test(&graph, "lib", "hello");
        assert_eq!(lib.src_path, "src/lib.rs");
        assert!(lib.linked && lib.pass_l);
        assert!(!lib.env.contains_key("CARGO_BIN_NAME"));
        // Unit tests are promised no binary and no example.
        assert!(lib.test.as_ref().unwrap().executables.is_empty());
        // What is not a test has nothing to run.
        assert_eq!(compile(&graph, "hello-0.1.0-lib-")[0].test, None);
        let bin = hello_test(&graph, "bin", "hello");
        assert_eq!(bin.src_path, "src/main.rs");
        assert_eq!(bin.env["CARGO_BIN_NAME"], "hello");
        assert_ne!(lib.metadata, bin.metadata);
    }

    #[test]
    fn a_test_sees_tests_without_the_other_tests() {
        let graph = hello(true);
        assert_eq!(
            excluded(&hello_test(&graph, "test", "cli").src),
            ["examples/extra.rs", "src/main.rs", "tests/smoke.rs"]
        );
        assert_eq!(
            excluded(&hello_test(&graph, "lib", "hello").src),
            [
                "examples/extra.rs",
                "src/main.rs",
                "tests/cli.rs",
                "tests/smoke.rs"
            ]
        );
        assert_eq!(
            excluded(&hello_test(&graph, "bin", "hello").src),
            ["examples/extra.rs", "tests/cli.rs", "tests/smoke.rs"]
        );
        // What is installed sees no test at all.
        let lib = compile(&graph, "hello-0.1.0-lib-")[0];
        assert_eq!(
            excluded(&lib.src),
            ["benches", "examples", "src/main.rs", "tests"]
        );
    }

    // A test that says `mod smoke;` reads tests/smoke.rs, which cargo also
    // builds as a test of its own. The file stays in that test's view and
    // in no other's.
    #[test]
    fn a_test_keeps_the_root_it_names_as_a_module() {
        let graph = hello_reading(true, |_| {}, &|path| {
            (path == "/src/tests/cli.rs").then(|| "mod smoke;\nfn helper() {}\n".to_string())
        })
        .unwrap();
        assert_eq!(
            excluded(&hello_test(&graph, "test", "cli").src),
            ["examples/extra.rs", "src/main.rs"]
        );
        assert_eq!(
            excluded(&hello_test(&graph, "lib", "hello").src),
            [
                "examples/extra.rs",
                "src/main.rs",
                "tests/cli.rs",
                "tests/smoke.rs"
            ]
        );
    }

    #[test]
    fn tests_are_the_units_built_as_tests() {
        let graph = hello(true);
        let names: Vec<&str> = graph.tests.iter().map(|key| unhashed(key)).collect();
        assert_eq!(
            names,
            [
                "hello-0.1.0-test-cli",
                "hello-0.1.0-test-hello",
                "hello-0.1.0-test-hello",
                "hello-0.1.0-test-smoke"
            ]
        );
        for key in &graph.tests {
            assert!(
                matches!(&graph.units[key], UnitNode::Compile(unit) if unit.test.is_some()),
                "{key}"
            );
        }
        // `cargo test` also builds the example, to see that it compiles.
        let builds: Vec<&str> = graph.test_builds.iter().map(|key| unhashed(key)).collect();
        assert_eq!(builds, ["hello-0.1.0-example-extra"]);
        assert!(hello(false).test_builds.is_empty());
    }

    // `cargo test` builds every example to see that it compiles. One that
    // is a library cannot be built yet, and nothing would use it.
    #[test]
    fn a_library_example_in_the_test_plan_is_left_out() {
        let graph = hello_with(true, |tests| {
            let example = tests
                .units
                .iter_mut()
                .find(|unit| unit.target.kind == ["example"])
                .unwrap();
            example.target.crate_types = vec!["cdylib".to_string()];
        })
        .unwrap();
        assert!(!graph.units.keys().any(|key| key.contains("-example-")));
        let cli = hello_test(&graph, "test", "cli");
        assert_eq!(cli.test.as_ref().unwrap().executables.len(), 1);
    }

    // `[[test]] path = "../shared.rs"`: cargo reports the path with the
    // `..` in it, which must not pass for a path inside the package.
    #[test]
    fn a_test_rooted_outside_its_package_is_refused() {
        let set_root = |tests: &mut UnitGraph, root: &str| {
            let cli = tests
                .units
                .iter_mut()
                .find(|unit| unit.target.name == "cli")
                .unwrap();
            cli.target.src_path = root.to_string();
        };
        let err = hello_with(true, |tests| set_root(tests, "/src/../shared.rs"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("cli of hello 0.1.0")
                && err.contains("/shared.rs")
                && err.contains("outside the package directory"),
            "{err}"
        );
        // A detour that stays inside is the file it leads to.
        let graph =
            hello_with(true, |tests| set_root(tests, "/src/tests/../tests/cli.rs")).unwrap();
        assert_eq!(hello_test(&graph, "test", "cli").src_path, "tests/cli.rs");
    }

    #[test]
    fn a_mode_the_test_plan_cannot_have_is_refused() {
        let err = hello_with(true, |tests| tests.units[0].mode = "check".to_string())
            .unwrap_err()
            .to_string();
        assert!(err.contains("'check'"), "{err}");
    }

    #[test]
    fn harness_is_read_from_the_targets_entry() {
        let manifest: Table = toml::from_str(
            "[lib]\nharness = false\n\n[[test]]\nname = \"plain\"\nharness = false\n\n\
             [[test]]\nname = \"other\"\n\n[[bin]]\nname = \"tool\"\nharness = false\n",
        )
        .unwrap();
        let tests: UnitGraph =
            serde_json::from_str(include_str!("../testdata/hello/test-graph.json")).unwrap();
        let unit = |kind: &str, name: &str| {
            let mut unit = tests.units[0].clone();
            unit.target.kind = vec![kind.to_string()];
            unit.target.name = name.to_string();
            unit
        };
        assert!(!harness(&manifest, &unit("lib", "anything")));
        assert!(!harness(&manifest, &unit("proc-macro", "anything")));
        assert!(!harness(&manifest, &unit("test", "plain")));
        assert!(harness(&manifest, &unit("test", "other")));
        assert!(harness(&manifest, &unit("test", "found-by-cargo")));
        assert!(!harness(&manifest, &unit("bin", "tool")));
        assert!(harness(&manifest, &unit("example", "tool")));
        assert!(harness(&Table::new(), &unit("lib", "anything")));
    }

    #[test]
    fn profile_directories() {
        assert_eq!(profile_dir("dev"), "debug");
        assert_eq!(profile_dir("test"), "debug");
        assert_eq!(profile_dir("release"), "release");
        assert_eq!(profile_dir("bench"), "release");
        assert_eq!(profile_dir("thin"), "thin");
    }

    #[test]
    fn profile_roots() {
        let manifest: Table = toml::from_str(
            "[profile.thin]\ninherits = \"release\"\n[profile.quick]\ninherits = \"dev\"\n[profile.deep]\ninherits = \"thin\"",
        )
        .unwrap();
        assert_eq!(profile_root("release", &manifest), "release");
        assert_eq!(profile_root("dev", &manifest), "debug");
        assert_eq!(profile_root("test", &manifest), "debug");
        assert_eq!(profile_root("thin", &manifest), "release");
        assert_eq!(profile_root("deep", &manifest), "release");
        assert_eq!(profile_root("quick", &manifest), "debug");
        assert_eq!(profile_root("elsewhere", &manifest), "release");
    }

    #[test]
    fn relative_paths() {
        assert_eq!(
            relative_to("/src/a/b.rs", "/src").as_deref(),
            Some("a/b.rs")
        );
        assert_eq!(relative_to("/src", "/src").as_deref(), Some(""));
        assert_eq!(relative_to("/srcs/a", "/src"), None);
        assert_eq!(relative_to("/other", "/src"), None);
    }
}
