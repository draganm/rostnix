fn main() {
    // C that is compiled for the platform the library is for.
    cc::Build::new().file("c/twice.c").compile("rostnixtwice");

    // What rostnix-tables says of itself: through its build script, the
    // one built for the library's platform, and here, the one built for
    // the machine this script runs on.
    let library =
        std::env::var("DEP_ROSTNIXTABLES_SIDE").expect("DEP_ROSTNIXTABLES_SIDE not set");
    let script = rostnix_tables::side();
    println!("cargo::rustc-env=ROSTNIX_MATH_SIDES=library:{library} script:{script}");

    println!("cargo::rerun-if-changed=c/twice.c");
    println!("cargo::rerun-if-changed=build.rs");
}
