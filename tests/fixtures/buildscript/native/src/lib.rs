include!(concat!(env!("OUT_DIR"), "/generated.rs"));

extern "C" {
    fn rostnix_add(a: i32, b: i32) -> i32;
}

pub fn add(a: i32, b: i32) -> i32 {
    unsafe { rostnix_add(a, b) }
}

pub fn note() -> &'static str {
    env!("NATIVE_NOTE")
}

pub fn has_native() -> bool {
    cfg!(has_native)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_through_c() {
        assert_eq!(add(2, 3), 5);
    }

    // Cargo gives a test its package's OUT_DIR and what the build script set
    // with rustc-env, as it gave them to rustc.
    #[test]
    fn build_script_values_are_there_at_run_time() {
        let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
        assert!(std::path::Path::new(&out_dir).join("generated.rs").exists());
        assert_eq!(std::env::var("NATIVE_NOTE").expect("NATIVE_NOTE"), note());
    }
}
