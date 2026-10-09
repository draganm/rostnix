# The integration fixtures, built with the rustEnv under test.
{ rustEnv, pkgs }:
{
  hello = rustEnv.buildRustApplication {
    pname = "hello";
    src = ./fixtures/hello;
  };

  # Patient zero: a library whose CLI is an example, with C-compiling build
  # scripts, thin LTO and a dependency that is also a cdylib.
  core-rs = rustEnv.buildRustApplication {
    pname = "amber-store";
    version = "0.10.0";
    src = builtins.fetchTree {
      type = "github";
      owner = "amber-store";
      repo = "core-rs";
      rev = "e6e900b7a0f41b3540a319c1367fd167921d5d8c";
    };
    examples = [ "amber-store" ];
  };
}
