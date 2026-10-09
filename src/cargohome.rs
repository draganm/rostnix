//! Running cargo during evaluation: with a clean environment and a private
//! cargo home, so that the same source plans to the same graph in every
//! shell.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::Result;

/// Variables cargo may take from the caller: where crates come from, never
/// how they are built.
const CARRIED: &[&str] = &[
    "HOME",
    "PATH",
    "TMPDIR",
    "USER",
    "LOGNAME",
    "SSL_CERT_FILE",
    "NIX_SSL_CERT_FILE",
    "CURL_CA_BUNDLE",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
];
const CARRIED_PREFIXES: &[&str] = &["CARGO_HTTP_", "CARGO_NET_"];

/// What the private cargo home shares with the caller's: the download
/// cache, and the locks cargo takes on it.
const SHARED: &[(&str, bool)] = &[("registry", true), (".package-cache", false), (".package-cache-mutate", false)];

/// A cargo set up to plan a build. Its temporary directories are removed
/// when it is dropped.
pub struct Cargo {
    cargo: PathBuf,
    rustc: PathBuf,
    scratch: PathBuf,
}

impl Cargo {
    pub fn new(cargo: &str, rustc: &str) -> Result<Cargo> {
        let real_home = match std::env::var_os("CARGO_HOME") {
            Some(home) if !home.is_empty() => PathBuf::from(home),
            _ => match std::env::var_os("HOME") {
                Some(home) => PathBuf::from(home).join(".cargo"),
                None => return Err("neither CARGO_HOME nor HOME is set, so there is no cargo cache to use".into()),
            },
        };
        let scratch = std::env::temp_dir().join(format!(
            "rostnix-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos()
        ));
        let this = Cargo { cargo: cargo.into(), rustc: rustc.into(), scratch };
        fs::create_dir_all(this.home())?;
        fs::create_dir_all(this.target_dir())?;

        for (name, is_dir) in SHARED {
            let real = real_home.join(name);
            if !real.exists() {
                // A cargo home that has never been used.
                if *is_dir {
                    fs::create_dir_all(&real)
                } else {
                    fs::create_dir_all(&real_home).and_then(|()| fs::File::create(&real).map(drop))
                }
                .map_err(|err| format!("creating {}: {err}", real.display()))?;
            }
            std::os::unix::fs::symlink(&real, this.home().join(name))
                .map_err(|err| format!("linking {}: {err}", real.display()))?;
        }
        Ok(this)
    }

    fn home(&self) -> PathBuf {
        self.scratch.join("cargo-home")
    }

    fn target_dir(&self) -> PathBuf {
        self.scratch.join("target")
    }

    /// A cargo command in `dir` that sees nothing of the caller's build
    /// configuration.
    fn command(&self, dir: &Path) -> Command {
        let mut cmd = Command::new(&self.cargo);
        cmd.current_dir(dir).env_clear();
        for (key, value) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if CARRIED.contains(&name.as_ref()) || CARRIED_PREFIXES.iter().any(|p| name.starts_with(p)) {
                cmd.env(&key, value);
            }
        }
        cmd.env("CARGO_HOME", self.home())
            .env("CARGO_TARGET_DIR", self.target_dir())
            .env("RUSTC", &self.rustc)
            // Lets stable cargo print the unit graph. It is set for cargo's
            // planning only and never reaches a derivation.
            .env("RUSTC_BOOTSTRAP", "1")
            // The private home has no record of what was used when.
            .env("CARGO_GC_AUTO_FREQUENCY", "never")
            .env("CARGO_TERM_PROGRESS_WHEN", "never");
        cmd
    }

    /// Runs cargo in `dir` and returns what it printed. Cargo's own messages
    /// go to stderr, which Nix shows.
    pub fn output(&self, dir: &Path, args: &[String]) -> Result<Vec<u8>> {
        let output = self
            .command(dir)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output()
            .map_err(|err| format!("running {}: {err}", self.cargo.display()))?;
        if !output.status.success() {
            return Err(format!(
                "cargo {} failed in {}; if it reports a problem with the lock file, commit a Cargo.lock that matches Cargo.toml",
                args.first().map_or("", String::as_str),
                dir.display()
            )
            .into());
        }
        Ok(output.stdout)
    }

    /// `cargo --version`'s number.
    pub fn version(&self, dir: &Path) -> Result<String> {
        let out = String::from_utf8(self.output(dir, &["--version".to_string()])?)?;
        Ok(out.split_whitespace().nth(1).unwrap_or("").to_string())
    }

    /// The triple of the machine rustc runs on.
    pub fn host(&self) -> Result<String> {
        let output = Command::new(&self.rustc)
            .arg("-vV")
            .env_remove("RUSTFLAGS")
            .output()
            .map_err(|err| format!("running {}: {err}", self.rustc.display()))?;
        let text = String::from_utf8(output.stdout)?;
        text.lines()
            .find_map(|line| line.strip_prefix("host: "))
            .map(str::to_string)
            .ok_or_else(|| format!("{} -vV names no host", self.rustc.display()).into())
    }
}

impl Drop for Cargo {
    fn drop(&mut self) {
        // The cargo home holds only links, so this removes nothing of the
        // caller's.
        let _ = fs::remove_dir_all(&self.scratch);
    }
}
