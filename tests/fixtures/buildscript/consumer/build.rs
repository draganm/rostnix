use std::env;
use std::process::Command;

fn main() {
    let answer = env::var("DEP_ROSTNIXNATIVE_ANSWER").expect("DEP_ROSTNIXNATIVE_ANSWER not set");
    println!("cargo::rustc-env=NATIVE_ANSWER={answer}");

    let pc = match Command::new("pkg-config")
        .args(["--modversion", "zlib"])
        .output()
    {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => "none".to_string(),
    };
    println!("cargo::rustc-env=ZLIB_PC={pc}");

    println!("cargo:rustc-check-cfg=cfg(consumer_old_style)");
    println!("cargo:rustc-cfg=consumer_old_style");
}
