//! Reads what the cargo configuration gives a build script: the variables
//! of its [env] table, and the flags, in two forms.

use std::env;
use std::fs;

fn main() {
    let data = env::var("FIXTURE_DATA").expect("FIXTURE_DATA");
    let message = fs::read_to_string(&data).unwrap_or_else(|err| panic!("reading {data}: {err}"));
    println!("cargo::rustc-env=MESSAGE={}", message.trim());

    let flags = env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default();
    let told = flags.split('\x1f').any(|flag| flag == "from_unix");
    let cfg = env::var_os("CARGO_CFG_FROM_UNIX").is_some();
    #[allow(unexpected_cfgs)]
    let compiled = cfg!(from_unix);
    println!(
        "cargo::rustc-env=SCRIPT_SAW={}",
        if told && cfg && compiled { "flags" } else { "nothing" }
    );
}
