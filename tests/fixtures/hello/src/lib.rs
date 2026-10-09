#[derive(serde::Serialize)]
struct Greeting {
    greeting: &'static str,
    n: u32,
}

pub fn greeting() -> anyhow::Result<String> {
    let g = Greeting {
        greeting: "hello",
        n: 42,
    };
    Ok(serde_json::to_string(&g)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greeting_names_the_number() {
        assert!(greeting().unwrap().contains("42"));
    }

    // Run with `--include-ignored` to see what a failing test looks like.
    #[test]
    #[ignore = "fails on purpose"]
    fn fails_on_purpose() {
        panic!("this test fails on purpose");
    }
}
