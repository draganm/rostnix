//! Running cargo during evaluation: with a clean environment and a private
//! cargo home, so that the same source plans to the same graph in every
//! shell.

use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use toml::Table;

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
    // How git and ssh find the caller's keys.
    "SSH_AUTH_SOCK",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
];
const CARRIED_PREFIXES: &[&str] = &[
    "CARGO_HTTP_",
    "CARGO_NET_",
    "CARGO_REGISTRIES_",
    "CARGO_REGISTRY_",
];

/// What the private cargo home shares with the caller's: the download
/// caches, and the locks cargo takes on them.
const SHARED: &[(&str, bool)] = &[
    ("registry", true),
    ("git", true),
    (".package-cache", false),
    (".package-cache-mutate", false),
];

/// The caller's tokens, shared when the caller has them.
const CREDENTIALS: &[&str] = &["credentials.toml", "credentials"];

/// The tables of the caller's `config.toml` that say where crates come
/// from and how to reach them. Everything else in it says how to build, and
/// the build must not depend on who evaluates.
const SOURCE_TABLES: &[&str] = &[
    "registries",
    "registry",
    "source",
    "net",
    "http",
    "credential-alias",
];

/// Settings of the kept tables that name a path. In a configuration file
/// a relative path starts at the directory above the one the file is in,
/// and the private home is somewhere else.
const PATHS_IN_SOURCES: &[&str] = &["directory", "local-registry"];

/// The private cargo home's `config.toml` for a caller's, whose cargo home
/// lies in the directory `above_home`: its [`SOURCE_TABLES`] and nothing
/// else, with the relative paths in them made absolute.
///
/// Cargo is also told to fetch git repositories with the git command,
/// unless the caller's file says which git to use. Nix fetches a git
/// dependency with the git command, which reads the caller's git and ssh
/// configuration; cargo's built-in git reads less of it, and would fail
/// where Nix succeeds. Being in this file, the choice gives way to the
/// project's own configuration and to the caller's environment.
pub fn source_config(caller_config: &str, above_home: &str) -> Result<String> {
    let config: Table = toml::from_str(caller_config)?;
    let mut kept: Table = config
        .into_iter()
        .filter(|(table, _)| SOURCE_TABLES.contains(&table.as_str()))
        .collect();
    let absolute = |value: &mut toml::Value| {
        if let Some(path) = value.as_str().filter(|path| !path.starts_with('/')) {
            *value = toml::Value::String(format!("{above_home}/{path}"));
        }
    };
    if let Some(sources) = kept
        .get_mut("source")
        .and_then(|sources| sources.as_table_mut())
    {
        for source in sources
            .iter_mut()
            .filter_map(|(_, source)| source.as_table_mut())
        {
            for key in PATHS_IN_SOURCES {
                if let Some(value) = source.get_mut(*key) {
                    absolute(value);
                }
            }
        }
    }
    if let Some(cainfo) = kept.get_mut("http").and_then(|http| http.get_mut("cainfo")) {
        absolute(cainfo);
    }
    let net = kept
        .entry("net")
        .or_insert_with(|| toml::Value::Table(Table::new()));
    if let Some(net) = net.as_table_mut() {
        net.entry("git-fetch-with-cli")
            .or_insert(toml::Value::Boolean(true));
    }
    Ok(toml::to_string(&kept)?)
}

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
                None => {
                    return Err(
                        "neither CARGO_HOME nor HOME is set, so there is no cargo cache to use"
                            .into(),
                    )
                }
            },
        };
        let scratch = std::env::temp_dir().join(format!(
            "rostnix-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        let this = Cargo {
            cargo: cargo.into(),
            rustc: rustc.into(),
            scratch,
        };
        // The cargo home may come to hold a copy of registry settings, a
        // token among them, so no one else may look into it.
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&this.scratch)
            .map_err(|err| format!("creating {}: {err}", this.scratch.display()))?;
        fs::create_dir_all(this.home())?;
        fs::create_dir_all(this.target_dir())?;

        // Where crates come from is the caller's to say; how they are built
        // is not. `config` is the name cargo used before `config.toml`, and
        // the one it reads when both are there.
        let caller_config = ["config", "config.toml"]
            .iter()
            .map(|name| real_home.join(name))
            .find(|file| file.is_file());
        let above_home = real_home
            .parent()
            .unwrap_or(&real_home)
            .to_string_lossy()
            .into_owned();
        let text = match &caller_config {
            Some(file) => fs::read_to_string(file)
                .map_err(|err| format!("reading {}: {err}", file.display()))?,
            None => String::new(),
        };
        // A file this cannot read is cargo's to complain about, when cargo
        // needs it. Most projects need nothing from it.
        let kept = match source_config(&text, &above_home) {
            Ok(kept) => kept,
            Err(err) => {
                let file = caller_config.unwrap_or_default();
                eprintln!(
                    "rostnix: warning: {} cannot be read ({err}); the registries it may name are not known to this build",
                    file.display()
                );
                source_config("", &above_home)?
            }
        };
        fs::write(this.home().join("config.toml"), kept)?;
        for name in CREDENTIALS {
            let real = real_home.join(name);
            if real.is_file() {
                std::os::unix::fs::symlink(&real, this.home().join(name))
                    .map_err(|err| format!("linking {}: {err}", real.display()))?;
            }
        }

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
            if CARRIED.contains(&name.as_ref())
                || CARRIED_PREFIXES.iter().any(|p| name.starts_with(p))
            {
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

    /// The same, with cargo's own messages kept back unless it fails:
    /// `cargo config get` says on every run which variables it looked at.
    pub fn output_quietly(&self, dir: &Path, args: &[String]) -> Result<Vec<u8>> {
        let output = self
            .command(dir)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|err| format!("running {}: {err}", self.cargo.display()))?;
        if !output.status.success() {
            return Err(format!(
                "cargo {} failed in {}: {}",
                args.join(" "),
                dir.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            )
            .into());
        }
        Ok(output.stdout)
    }

    /// The cfgs of the machine rustc runs on when it is given `flags`, as
    /// `rustc --print=cfg` prints them.
    pub fn print_cfg(&self, flags: &[String]) -> Result<String> {
        let output = Command::new(&self.rustc)
            .arg("--print=cfg")
            .args(flags)
            .env_remove("RUSTFLAGS")
            .output()
            .map_err(|err| format!("running {}: {err}", self.rustc.display()))?;
        if !output.status.success() {
            return Err(format!("{} --print=cfg failed", self.rustc.display()).into());
        }
        Ok(String::from_utf8(output.stdout)?)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(caller_config: &str) -> Table {
        toml::from_str(&source_config(caller_config, "/home/me").unwrap()).unwrap()
    }

    // Where crates come from is carried over; how things are built is not.
    #[test]
    fn only_the_tables_about_sources_are_kept() {
        let kept = kept(
            r#"
[build]
rustflags = ["-C", "target-cpu=native"]
jobs = 2

[env]
FROM_HOME = "1"

[registries.company]
index = "sparse+https://crates.example.com/index/"

[registry]
default = "company"
global-credential-providers = ["cargo:token"]

[source.crates-io]
replace-with = "mirror"

[source.mirror]
registry = "sparse+https://mirror.example.com/"

[net]
retry = 3

[http]
proxy = "proxy.example.com:3128"

[credential-alias]
vault = ["cargo-credential-vault"]

[target.x86_64-unknown-linux-gnu]
linker = "clang"

[alias]
b = "build"
"#,
        );
        let mut tables: Vec<&str> = kept.keys().map(String::as_str).collect();
        tables.sort();
        assert_eq!(
            tables,
            [
                "credential-alias",
                "http",
                "net",
                "registries",
                "registry",
                "source"
            ]
        );
        assert_eq!(
            kept["registries"]["company"]["index"].as_str(),
            Some("sparse+https://crates.example.com/index/")
        );
        assert_eq!(kept["net"]["retry"].as_integer(), Some(3));
    }

    // Cargo fetches with the git command, as Nix does, unless the caller
    // says which git to use.
    #[test]
    fn the_git_command_is_chosen_unless_the_caller_chose() {
        assert_eq!(kept("")["net"]["git-fetch-with-cli"].as_bool(), Some(true));
        assert_eq!(
            kept("[net]\nretry = 3\n")["net"]["git-fetch-with-cli"].as_bool(),
            Some(true)
        );
        assert_eq!(
            kept("[net]\ngit-fetch-with-cli = false\n")["net"]["git-fetch-with-cli"].as_bool(),
            Some(false)
        );
    }

    // A relative path in the caller's file starts above the caller's cargo
    // home, and must still lead there from the private one.
    #[test]
    fn relative_paths_keep_leading_where_they_led() {
        let kept = kept(
            r#"
[source.vendored]
directory = "vendor"

[source.local]
local-registry = "/srv/registry"

[source.mirror]
registry = "sparse+https://mirror.example.com/"

[http]
cainfo = "certs/ca.pem"
"#,
        );
        assert_eq!(
            kept["source"]["vendored"]["directory"].as_str(),
            Some("/home/me/vendor")
        );
        assert_eq!(
            kept["source"]["local"]["local-registry"].as_str(),
            Some("/srv/registry")
        );
        assert_eq!(
            kept["source"]["mirror"]["registry"].as_str(),
            Some("sparse+https://mirror.example.com/")
        );
        assert_eq!(
            kept["http"]["cainfo"].as_str(),
            Some("/home/me/certs/ca.pem")
        );
    }

    #[test]
    fn a_config_that_is_not_toml_is_refused() {
        assert!(source_config("[net", "/home/me").is_err());
    }
}
