#![allow(unexpected_cfgs)]

fn main() {
    let mut buffer = itoa::Buffer::new();
    println!(
        "config ok: plain={} message={} unix={} expression={} build={} script={} n={}",
        env!("FIXTURE_PLAIN"),
        env!("MESSAGE"),
        cfg!(from_unix),
        cfg!(from_expression),
        cfg!(from_build),
        env!("SCRIPT_SAW"),
        buffer.format(7),
    );
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    // A test is given the variables too, when it runs.
    #[test]
    fn variables_are_set_when_the_test_runs() {
        assert_eq!(std::env::var("FIXTURE_PLAIN").unwrap(), "plain");
        let data = std::env::var("FIXTURE_DATA").unwrap();
        assert_eq!(
            std::fs::read_to_string(&data).unwrap().trim(),
            "hello-from-data"
        );
        assert_eq!(std::env::var("TERM").unwrap(), "dumb");
        assert_ne!(std::env::var("HOME").unwrap_or_default(), "/not-applied");
    }

    // The workspace a relative variable names is the one the test was
    // compiled in and runs in, with its package in it.
    #[test]
    fn the_workspace_is_where_the_test_runs() {
        let workspace = std::env::var("FIXTURE_WORKSPACE").unwrap();
        assert_eq!(workspace, env!("FIXTURE_WORKSPACE"));
        assert!(Path::new(&workspace).join("app/src/main.rs").is_file());
        assert_eq!(
            Path::new(&workspace).join("app").canonicalize().unwrap(),
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
    }
}
