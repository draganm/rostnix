fn main() {
    // Which platform this build of the package is for. The package is
    // needed twice, by a build script and by a library, and when those are
    // for different machines it is built once for each.
    let target = std::env::var("TARGET").expect("TARGET not set");
    println!("cargo::metadata=side={target}");
    println!("cargo::rustc-env=ROSTNIX_TABLES_SIDE={target}");
    println!("cargo::rerun-if-changed=build.rs");
}
