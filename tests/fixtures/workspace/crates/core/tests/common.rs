//! Shared by the tests beside it through `mod common;`. Cargo also builds
//! this file as an integration test of its own, with no test in it.

#[allow(dead_code)]
pub fn expected(who: &str, n: u32) -> String {
    format!("hello from {who}: {n}")
}
