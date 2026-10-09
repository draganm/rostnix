// Helpers kept in a file of their own beside the tests.
mod common;

fn main() {
    assert!(cfg!(test), "a test without the harness is still built with cfg(test)");
    assert_eq!(ws_core::greet("a", 2).to_lowercase(), common::expected("a", 2));
    println!("plain test ran");
}
