fn yes_no(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

fn main() {
    let zlib = if unsafe { libz_sys::zlibVersion() }.is_null() {
        "null"
    } else {
        "ok"
    };
    // Code from OUT_DIR that can panic: its location must not make the
    // program refer to the build script's run.
    assert_eq!(bs_native::pick(std::hint::black_box(1)), 20);
    println!(
        "add={} answer={} note={} generated={} cfg={} old={} msg={} zlib={} pc={}",
        bs_native::add(2, 3),
        env!("NATIVE_ANSWER"),
        bs_native::note(),
        bs_native::GENERATED,
        yes_no(bs_native::has_native()),
        yes_no(cfg!(consumer_old_style)),
        include_str!("../../shared/message.txt").trim(),
        zlib,
        env!("ZLIB_PC"),
    );
}
