use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    cc::Build::new().file("c/add.c").compile("rostnixadd");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR not set"));
    fs::write(
        out_dir.join("generated.rs"),
        // The indexing can panic, so the compiled library names this file.
        "pub const GENERATED: &str = \"from-build-script\";\n\
         pub fn pick(i: usize) -> u8 {\n    [10u8, 20, 30][i]\n}\n",
    )
    .expect("failed to write generated.rs");

    let note = env::var("ROSTNIX_FIXTURE_NOTE").unwrap_or_else(|_| "unset".to_string());

    println!("cargo::rustc-check-cfg=cfg(has_native)");
    println!("cargo::rustc-cfg=has_native");
    println!("cargo::rustc-env=NATIVE_NOTE={note}");
    println!("cargo::metadata=answer=42");
    println!("cargo::rerun-if-changed=c/add.c");
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=ROSTNIX_FIXTURE_NOTE");
}
