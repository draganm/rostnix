pub fn greet(who: &str, n: u32) -> String {
    let s = format!("hello from {who}: {n}");
    if cfg!(feature = "loud") {
        s.to_uppercase()
    } else {
        s
    }
}
