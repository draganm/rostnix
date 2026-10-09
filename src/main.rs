use std::process::ExitCode;

const USAGE: &str = "usage: rostnix <command>

commands:
  resolve <json>      print the build graph of a project as Nix (runs during evaluation)
  compile             run rustc for the unit of the derivation being built
  run-build-script    run the build script of the derivation being built
  test                compile the test of the derivation being built and run it";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["resolve", request] => rostnix::resolve::run(request).map(|nix| print!("{nix}")),
        ["compile"] => rostnix::compile::run(),
        ["run-build-script"] => rostnix::buildscript::run(),
        ["test"] => rostnix::testrun::run(),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("rostnix: {err}");
            ExitCode::FAILURE
        }
    }
}
