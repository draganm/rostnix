fn main() {
    println!(
        "{}",
        core_renamed::greet(ws_macros::app_name!(), ws_nested::three())
    );
}
