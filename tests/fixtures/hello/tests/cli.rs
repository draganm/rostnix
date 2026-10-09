//! What an integration test may rely on under `cargo test`: where it runs,
//! what it can find and what it is told.

use std::path::{Path, PathBuf};
use std::process::Command;

const GREETING: &str = r#"{"greeting":"hello","n":42}"#;

fn stdout_of(program: impl AsRef<Path>) -> String {
    let program = program.as_ref();
    let out = Command::new(program)
        .output()
        .unwrap_or_else(|err| panic!("running {}: {err}", program.display()));
    assert!(out.status.success(), "{} failed", program.display());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn binary_is_named_when_compiling_and_when_running() {
    assert_eq!(stdout_of(env!("CARGO_BIN_EXE_hello")), GREETING);
    let at_run_time = std::env::var("CARGO_BIN_EXE_hello").expect("CARGO_BIN_EXE_hello");
    assert_eq!(stdout_of(at_run_time), GREETING);
}

// target/<profile>/deps/<test>-<hash>, with the package's binaries one
// directory up and its examples in `examples` beside `deps`.
#[test]
fn binaries_and_examples_are_beside_the_test_executable() {
    let mut dir = std::env::current_exe().unwrap();
    dir.pop();
    assert!(dir.ends_with("deps"), "{}", dir.display());
    dir.pop();
    assert_eq!(stdout_of(dir.join("examples/extra")), "extra");
    assert_eq!(stdout_of(dir.join("hello")), GREETING);
}

#[test]
fn data_is_found_from_the_working_directory_and_the_manifest_directory() {
    let read = |path: PathBuf| {
        std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()))
            .trim()
            .to_string()
    };
    let data = "tests/data/expected.json";
    assert_eq!(read(data.into()), GREETING);
    assert_eq!(read(Path::new(env!("CARGO_MANIFEST_DIR")).join(data)), GREETING);
    let at_run_time = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    assert_eq!(read(at_run_time.join(data)), GREETING);
    // What the test was told when it was compiled still holds.
    assert_eq!(Path::new(env!("CARGO_MANIFEST_DIR")), at_run_time);
    // The test runs in its package directory.
    assert_eq!(
        std::env::current_dir().unwrap().canonicalize().unwrap(),
        at_run_time.canonicalize().unwrap()
    );
}

#[test]
fn working_directory_and_target_tmpdir_are_writable() {
    let tmp = Path::new(env!("CARGO_TARGET_TMPDIR"));
    assert!(tmp.is_dir(), "{} is not a directory", tmp.display());
    let probe = tmp.join("hello-cli-probe");
    std::fs::write(&probe, "x").unwrap();
    std::fs::remove_file(&probe).unwrap();

    let probe = Path::new("written-by-the-cli-test");
    std::fs::write(probe, "x").unwrap();
    std::fs::remove_file(probe).unwrap();
}

// The source is an ordinary checkout: a fixture can be copied out of it and
// the copy changed. `fs::copy` keeps the permissions of the original.
#[test]
fn a_copy_of_a_fixture_can_be_changed() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/expected.json");
    let copy = Path::new(env!("CARGO_TARGET_TMPDIR")).join("copied-fixture.json");
    std::fs::copy(&fixture, &copy).unwrap();
    std::fs::write(&copy, "changed").unwrap();
    std::fs::remove_file(&copy).unwrap();
}

#[test]
fn environment_is_what_cargo_gives_a_test() {
    assert_eq!(std::env::var("CARGO_PKG_NAME").unwrap(), "hello");
    assert_eq!(std::env::var("CARGO_PKG_VERSION").unwrap(), "0.1.0");
    for name in ["CARGO", "CARGO_MANIFEST_PATH"] {
        assert!(std::env::var(name).is_ok(), "{name} is not set");
    }
    // What rustc was given to compile the test is not passed on to it.
    for name in ["CARGO_CRATE_NAME", "CARGO_PRIMARY_PACKAGE", "CARGO_TARGET_TMPDIR"] {
        assert!(std::env::var(name).is_err(), "{name} is set");
    }
}
