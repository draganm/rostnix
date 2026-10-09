pub fn greet(who: &str, n: u32) -> String {
    let s = format!("hello from {who}: {n}");
    if cfg!(feature = "loud") {
        s.to_uppercase()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greets() {
        assert_eq!(greet("x", 1).to_lowercase(), "hello from x: 1");
    }
}
