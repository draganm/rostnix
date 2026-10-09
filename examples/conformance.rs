//! Compares what rostnix ran with what cargo runs.
//!
//!     conformance [--compile-only] [--without <crate>]... [--root <dir>] <cargo-log> <unit-records-dir>
//!
//! `cargo-log` is the stderr of `cargo build -vv` or `cargo test -vv`, which
//! prints every rustc command line, every build-script run and every test
//! run with the environment cargo set. `unit-records-dir` holds the
//! `unit.json` of every unit rostnix built and of every test it ran. Both
//! are reduced to sets of normalised invocations, which must be equal.
//! Exits 1 and prints the differences when they are not.
//!
//! With `--compile-only` test runs are left out on both sides, for a log
//! made with `cargo test --no-run`. `--without` leaves out what is done for
//! one crate, for a test that rostnix was told to skip and so did not
//! compile either.
//!
//! `--root` names the source root cargo ran in. A variable the cargo
//! configuration sets to a path relative to its own directory is then
//! compared as a path from that root: cargo names it in the source, rostnix
//! in a copy of what the unit sees. And a variable rostnix withholds from
//! crates that are not local, because it names the source itself, is taken
//! out of what cargo gave those crates.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::process::ExitCode;

use serde_json::Value;

/// One rustc invocation or build-script run, before normalisation.
#[derive(Debug, Clone)]
struct Raw {
    env: BTreeMap<String, String>,
    program: String,
    args: Vec<String>,
    /// From a record: the variables set from relative values of the cargo
    /// configuration, with the path each names from the source root.
    relative_env: BTreeMap<String, String>,
    /// From a record: the names of such variables the unit was not given.
    withheld_env: Vec<String>,
}

/// The same, with everything that legitimately differs taken out.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Invocation {
    /// `rustc`, `build-script` or `test`.
    program: String,
    package: String,
    crate_name: String,
    /// Every flag with its value, sorted; the order carries no meaning.
    flags: Vec<String>,
    /// Lint flags in order: later ones override earlier ones.
    lints: Vec<String>,
    /// Library search paths and libraries in order: the first match wins.
    link_order: Vec<String>,
    env: BTreeMap<String, String>,
}

/// Variables that describe the machine or cargo's own bookkeeping.
const IGNORED_ENV: &[&str] = &[
    "NUM_JOBS",
    "CARGO_MAKEFLAGS",
    "DYLD_FALLBACK_LIBRARY_PATH",
    "LD_LIBRARY_PATH",
    "CARGO_TARGET_TMPDIR",
    "CARGO_SBOM_PATH",
    "CARGO_RUSTC_CURRENT_DIR",
    "CARGO_INCREMENTAL",
];

/// Flags that take their value as the next argument.
const TWO_PART: &[&str] = &[
    "-C",
    "--cfg",
    "--check-cfg",
    "--extern",
    "-L",
    "-l",
    "--crate-type",
    "--crate-name",
    "--out-dir",
    "--cap-lints",
    "--target",
    "-Z",
    "--remap-path-prefix",
    "-A",
    "-W",
    "-D",
    "-F",
];

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut compile_only = false;
    let mut without: Vec<String> = Vec::new();
    let mut root: Option<String> = None;
    loop {
        match args.first().map(String::as_str) {
            Some("--compile-only") => {
                compile_only = true;
                args.remove(0);
            }
            Some("--without") if args.len() > 1 => {
                without.push(args.remove(1));
                args.remove(0);
            }
            Some("--root") if args.len() > 1 => {
                root = Some(args.remove(1).trim_end_matches('/').to_string());
                args.remove(0);
            }
            _ => break,
        }
    }
    let [log, records] = &args[..] else {
        eprintln!(
            "usage: conformance [--compile-only] [--without <crate>]... [--root <dir>] <cargo-log> <unit-records-dir>"
        );
        return ExitCode::from(2);
    };
    let cargo = parse_cargo_log(&fs::read_to_string(log).expect("reading the cargo log"));
    let rostnix = read_records(records);
    if cargo.is_empty() {
        eprintln!("the cargo log has no `Running` lines; was it made with `cargo build -vv` from a clean target directory?");
        return ExitCode::from(2);
    }

    // What rostnix says of the relative variables tells which of cargo's
    // are meant.
    let relative: BTreeSet<&str> = rostnix
        .iter()
        .flat_map(|raw| raw.relative_env.keys().map(String::as_str))
        .collect();
    let withheld: BTreeSet<&str> = rostnix
        .iter()
        .flat_map(|raw| raw.withheld_env.iter().map(String::as_str))
        .collect();
    let mut cargo = normalise(
        &cargo,
        &Relative {
            root: root.as_deref(),
            relative: &relative,
            withheld: &withheld,
        },
    );
    let mut rostnix = normalise(
        &rostnix,
        &Relative {
            root: None,
            relative: &relative,
            withheld: &BTreeSet::new(),
        },
    );
    for side in [&mut cargo, &mut rostnix] {
        side.retain(|inv| !(compile_only && inv.program == "test"));
        side.retain(|inv| !without.contains(&inv.crate_name));
    }
    // As multisets: two invocations that normalise alike on one side need
    // two on the other.
    let only_cargo = unmatched_in(&cargo, &rostnix);
    let only_rostnix = unmatched_in(&rostnix, &cargo);
    if only_cargo.is_empty() && only_rostnix.is_empty() {
        println!(
            "{} invocations, the same under cargo and rostnix",
            cargo.len()
        );
        return ExitCode::SUCCESS;
    }

    println!(
        "cargo ran {} invocations, rostnix {}",
        cargo.len(),
        rostnix.len()
    );
    let mut unpaired: Vec<&Invocation> = only_rostnix.clone();
    for theirs in &only_cargo {
        // The likeliest counterpart: same program, package and crate, and
        // the most flags in common.
        let best = unpaired
            .iter()
            .enumerate()
            .filter(|(_, ours)| {
                ours.program == theirs.program
                    && ours.package == theirs.package
                    && ours.crate_name == theirs.crate_name
            })
            .max_by_key(|(_, ours)| {
                ours.flags
                    .iter()
                    .filter(|f| theirs.flags.contains(f))
                    .count()
            })
            .map(|(i, _)| i);
        match best {
            Some(i) => report_difference(theirs, unpaired.remove(i)),
            None => println!(
                "\nonly cargo runs {} for {} ({})",
                theirs.program, theirs.package, theirs.crate_name
            ),
        }
    }
    for ours in unpaired {
        println!(
            "\nonly rostnix runs {} for {} ({})",
            ours.program, ours.package, ours.crate_name
        );
    }
    ExitCode::FAILURE
}

/// The invocations of `all` that `other` has no counterpart for, counting
/// each of `other` once.
fn unmatched_in<'a>(all: &'a [Invocation], other: &[Invocation]) -> Vec<&'a Invocation> {
    let mut unmatched: Vec<&Invocation> = other.iter().collect();
    all.iter()
        .filter(|inv| match unmatched.iter().position(|o| o == inv) {
            Some(i) => {
                unmatched.swap_remove(i);
                false
            }
            None => true,
        })
        .collect()
}

fn report_difference(cargo: &Invocation, rostnix: &Invocation) {
    println!(
        "\n{} for {} ({}) differs:",
        cargo.program, cargo.package, cargo.crate_name
    );
    for flag in cargo.flags.iter().filter(|f| !rostnix.flags.contains(f)) {
        println!("  only cargo:   {flag}");
    }
    for flag in rostnix.flags.iter().filter(|f| !cargo.flags.contains(f)) {
        println!("  only rostnix: {flag}");
    }
    if cargo.lints != rostnix.lints {
        println!("  cargo lints:   {}", cargo.lints.join(" "));
        println!("  rostnix lints: {}", rostnix.lints.join(" "));
    }
    if cargo.link_order != rostnix.link_order {
        println!(
            "  cargo searches and links:   {}",
            cargo.link_order.join(" ")
        );
        println!(
            "  rostnix searches and links: {}",
            rostnix.link_order.join(" ")
        );
    }
    let keys: BTreeSet<&String> = cargo.env.keys().chain(rostnix.env.keys()).collect();
    for key in keys {
        let (theirs, ours) = (cargo.env.get(key), rostnix.env.get(key));
        if theirs != ours {
            println!("  env {key}: cargo {theirs:?}, rostnix {ours:?}");
        }
    }
}

/// Finds every `Running `…`` in cargo's output. A command can span lines,
/// because a quoted value may contain newlines.
fn parse_cargo_log(log: &str) -> Vec<Raw> {
    const MARKER: &str = "Running `";
    let mut raws = Vec::new();
    let mut rest = log;
    while let Some(start) = rest.find(MARKER) {
        // Only at the start of a line, after cargo's indentation.
        let line_start = rest[..start].rfind('\n').map_or(0, |i| i + 1);
        let at_line_start = rest[line_start..start].trim().is_empty();
        let (words, consumed) = shell_words(&rest[start + MARKER.len()..]);
        rest = &rest[start + MARKER.len() + consumed..];
        if !at_line_start {
            continue;
        }
        let mut env = BTreeMap::new();
        let mut words = words.into_iter().peekable();
        while let Some((key, value)) = words.peek().and_then(|word| assignment(word)) {
            env.insert(key, value);
            words.next();
        }
        let Some(program) = words.next() else {
            continue;
        };
        // Doc tests are not run.
        if basename(&program) == "rustdoc" {
            continue;
        }
        raws.push(Raw {
            env,
            program,
            args: words.collect(),
            relative_env: BTreeMap::new(),
            withheld_env: Vec::new(),
        });
    }
    raws
}

/// `KEY=VALUE` with a variable name as the key. A name may hold a hyphen:
/// `CARGO_BIN_EXE_<name>` carries the binary's name as it is.
fn assignment(word: &str) -> Option<(String, String)> {
    let (key, value) = word.split_once('=')?;
    let mut chars = key.chars();
    let first = chars.next()?;
    let is_name = (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    is_name.then(|| (key.to_string(), value.to_string()))
}

/// Splits what cargo quoted for the shell, up to the closing backtick.
/// Returns the words and how many bytes were read.
fn shell_words(text: &str) -> (Vec<String>, usize) {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '`' => {
                if in_word {
                    words.push(word);
                }
                return (words, i + 1);
            }
            ' ' => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                for (_, q) in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    word.push(q);
                }
            }
            '\\' => {
                in_word = true;
                if let Some((_, escaped)) = chars.next() {
                    word.push(escaped);
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    (words, text.len())
}

fn read_records(dir: &str) -> Vec<Raw> {
    let mut raws = Vec::new();
    let mut files: Vec<_> = fs::read_dir(dir)
        .expect("reading the records directory")
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    for file in files {
        let record: Value =
            serde_json::from_str(&fs::read_to_string(&file).expect("reading a record"))
                .expect("parsing a record");
        let strings = |value: &Value| -> Vec<String> {
            value
                .as_array()
                .map(|a| a.iter().map(|v| v.as_str().unwrap().to_string()).collect())
                .unwrap_or_default()
        };
        let map = |value: &Value| -> BTreeMap<String, String> {
            value
                .as_object()
                .map(|o| {
                    o.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
                        .collect()
                })
                .unwrap_or_default()
        };
        let argv = strings(&record["argv"]);
        // A build-script run keeps its environment under another name,
        // because `env` is what the script asked rustc to be given.
        let env = if record["kind"] == "run-build-script" {
            map(&record["envRecorded"])
        } else {
            map(&record["env"])
        };
        raws.push(Raw {
            env,
            program: argv[0].clone(),
            args: argv[1..].to_vec(),
            relative_env: map(&record["relativeEnv"]),
            withheld_env: strings(&record["withheldEnv"]),
        });
    }
    raws
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The crate a test executable was built from: its file name is the crate
/// name and a hash of sixteen digits.
fn test_crate(program: &str) -> &str {
    let name = basename(program);
    match name.rsplit_once('-') {
        Some((stem, hash)) if hash.len() == 16 && hash.chars().all(|c| c.is_ascii_hexdigit()) => {
            stem
        }
        _ => name,
    }
}

/// How the variables that the cargo configuration sets to relative paths
/// are brought to one form.
struct Relative<'a> {
    /// The source root on cargo's side. A record of rostnix's names the
    /// paths itself.
    root: Option<&'a str>,
    /// The names of all such variables.
    relative: &'a BTreeSet<&'a str>,
    /// Those that are taken out of invocations for packages outside the
    /// root.
    withheld: &'a BTreeSet<&'a str>,
}

/// A path below the root as `ROOT/…`.
fn from_root(path: &str) -> String {
    if path.is_empty() {
        "ROOT".to_string()
    } else {
        format!("ROOT/{path}")
    }
}

fn normalise(raws: &[Raw], relative: &Relative) -> Vec<Invocation> {
    // Each side names the same things by its own paths. An invocation says
    // which package its directories belong to.
    let mut paths: Vec<(String, String)> = Vec::new();
    for raw in raws {
        let package = format!(
            "{}-{}",
            raw.env.get("CARGO_PKG_NAME").map_or("", String::as_str),
            raw.env.get("CARGO_PKG_VERSION").map_or("", String::as_str)
        );
        if let Some(dir) = raw.env.get("CARGO_MANIFEST_DIR") {
            paths.push((dir.clone(), format!("SRC({package})")));
        }
        if let Some(dir) = raw.env.get("OUT_DIR") {
            paths.push((dir.clone(), format!("OUT({package})")));
        }
    }
    // Longest first, so that a package inside another is not mistaken for
    // its parent.
    paths.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.cmp(b)));
    paths.dedup();
    let symbolic = |text: &str| -> String {
        let mut text = text.to_string();
        for (path, symbol) in &paths {
            text = text.replace(path.as_str(), symbol);
        }
        text
    };

    let mut invocations: Vec<Invocation> = raws
        .iter()
        .map(|raw| {
            let package = format!(
                "{}-{}",
                raw.env.get("CARGO_PKG_NAME").map_or("", String::as_str),
                raw.env.get("CARGO_PKG_VERSION").map_or("", String::as_str)
            );
            let is_rustc = basename(&raw.program) == "rustc";
            let is_build_script = basename(&raw.program).starts_with("build-script-");
            let mut flags = Vec::new();
            let mut lints = Vec::new();
            let mut link_order = Vec::new();
            let mut crate_name = if is_rustc || is_build_script {
                String::new()
            } else {
                test_crate(&raw.program).to_string()
            };
            let mut args = raw.args.iter();
            while let Some(arg) = args.next() {
                if !is_rustc {
                    flags.push(symbolic(arg));
                    continue;
                }
                let value = if TWO_PART.contains(&arg.as_str()) {
                    args.next().cloned().unwrap_or_default()
                } else {
                    String::new()
                };
                match arg.as_str() {
                    "--crate-name" => crate_name = value,
                    // Where things are, and cargo's own bookkeeping.
                    "--out-dir" | "--remap-path-prefix" => {}
                    "-C" if ["metadata=", "extra-filename=", "incremental="]
                        .iter()
                        .any(|p| value.starts_with(p)) => {}
                    "-L" if value.starts_with("dependency=") => {}
                    "-L" | "-l" => link_order.push(format!("{arg} {}", symbolic(&value))),
                    // Cargo names an .rmeta where it pipelines; which crate is
                    // meant is what matters.
                    "--extern" => flags.push(format!(
                        "--extern {}",
                        value.split('=').next().unwrap_or("")
                    )),
                    // `-vv` makes cargo show warnings of foreign crates.
                    "--cap-lints" => flags.push("--cap-lints allow".to_string()),
                    "-A" | "-W" | "-D" | "-F" => lints.push(format!("{arg} {value}")),
                    a if a.starts_with("--emit")
                        || a.starts_with("--error-format")
                        || a.starts_with("--json")
                        || a.starts_with("--diagnostic-width") => {}
                    a if ["--allow", "--warn", "--deny", "--forbid", "--force-warn"]
                        .iter()
                        .any(|p| a.starts_with(p)) =>
                    {
                        lints.push(a.to_string())
                    }
                    a if TWO_PART.contains(&a) => flags.push(format!("{a} {}", symbolic(&value))),
                    a if a.starts_with('-') => flags.push(a.to_string()),
                    source => flags.push(format!("source {}", symbolic(source))),
                }
            }
            flags.sort();

            let under_root = |path: &str| {
                relative
                    .root
                    .and_then(|root| path.strip_prefix(root))
                    .filter(|rest| rest.is_empty() || rest.starts_with('/'))
                    .map(|rest| rest.trim_start_matches('/').to_string())
            };
            let is_local = raw
                .env
                .get("CARGO_MANIFEST_DIR")
                .is_some_and(|dir| under_root(dir).is_some());
            let env = raw
                .env
                .iter()
                .filter(|(key, _)| !IGNORED_ENV.contains(&key.as_str()))
                .filter(|(key, _)| is_local || !relative.withheld.contains(key.as_str()))
                .map(|(key, value)| {
                    let value = match key.as_str() {
                        // Cargo's log has the flags without the separator
                        // that is between them, a control character.
                        "CARGO_ENCODED_RUSTFLAGS" => value.replace('\x1f', ""),
                        // A relative variable of the cargo configuration:
                        // the path it names from the source root.
                        key if raw.relative_env.contains_key(key) => {
                            from_root(&raw.relative_env[key])
                        }
                        key if relative.relative.contains(key) && under_root(value).is_some() => {
                            from_root(&under_root(value).unwrap_or_default())
                        }
                        "CARGO" | "RUSTC" | "RUSTDOC" => basename(value).to_string(),
                        // Where a binary is; which one is what matters.
                        key if key.starts_with("CARGO_BIN_EXE_") => basename(value).to_string(),
                        _ => symbolic(value),
                    };
                    (key.clone(), value)
                })
                .collect();

            Invocation {
                program: if is_rustc {
                    "rustc"
                } else if is_build_script {
                    "build-script"
                } else {
                    "test"
                }
                .to_string(),
                package,
                crate_name,
                flags,
                lints,
                link_order,
                env,
            }
        })
        .collect();
    invocations.sort();
    invocations
}
