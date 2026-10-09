fn main() {
    assert!(cfg!(test), "a test without the harness is still built with cfg(test)");
    assert_eq!(ws_core::greet("a", 2).to_lowercase(), "hello from a: 2");
    println!("plain test ran");
}
