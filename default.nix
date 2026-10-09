# Non-flake entry point: `import ./. { inherit pkgs; }` is mkRustEnv.
args: import ./nix/mk-rust-env.nix args
