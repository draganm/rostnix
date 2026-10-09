use std::process::Command;

fn stdout_of(program: &str) -> String {
    let out = Command::new(program).output().expect("running the binary");
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// The dev-dependency turns `loud` on for everything `cargo test` builds,
// the binary under test included.
#[test]
fn the_binary_under_test_is_built_with_the_dev_dependencys_features() {
    assert_eq!(stdout_of(env!("CARGO_BIN_EXE_ws-app")), "HELLO FROM WS-APP: 3");
    assert_eq!(core_renamed::greet("t", 1), "HELLO FROM T: 1");
}

#[test]
fn every_binary_of_the_package_is_named() {
    assert_eq!(stdout_of(env!("CARGO_BIN_EXE_ws-tool")), "ws-tool ok 7");
}
