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
