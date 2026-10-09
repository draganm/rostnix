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
