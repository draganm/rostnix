//! Adding `.crate` files from cargo's cache to the Nix store, under the
//! paths their `Cargo.lock` checksums dictate.
//!
//! The download derivation of each crate then finds its output present and
//! never runs on the machine that evaluated.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::storepath;

/// One `.crate` file to have in the store.
#[derive(Debug, Clone)]
pub struct Crate {
    /// The store name: `<name>-<version>.crate`.
    pub name: String,
    pub sha256: String,
    /// The file in cargo's download cache.
    pub cache_file: PathBuf,
}

/// Adds every crate that is not in the store yet. A crate whose cached file
/// is not what `Cargo.lock` names is an error; a `nix` that cannot add is
/// only worth a warning, because the download derivation covers it.
pub fn seed(store_dir: &str, crates: &[Crate]) -> crate::Result<()> {
    let next = AtomicUsize::new(0);
    let nix_broken = AtomicBool::new(false);
    let errors: Mutex<Vec<String>> = Mutex::new(Vec::new());

    std::thread::scope(|scope| {
        for _ in 0..crates.len().min(8) {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(krate) = crates.get(index) else { break };
                let expected = storepath::fixed_flat_sha256(store_dir, &krate.name, &krate.sha256);
                if Path::new(&expected).exists() || nix_broken.load(Ordering::Relaxed) {
                    continue;
                }
                match add(krate) {
                    Ok(added) if added == expected => {}
                    Ok(added) => errors.lock().unwrap().push(format!(
                        "{} does not match Cargo.lock: the store path of {} is {added}, the checksum gives {expected}. Remove the file so that cargo downloads it again",
                        krate.name,
                        krate.cache_file.display()
                    )),
                    Err(err) => {
                        if !nix_broken.swap(true, Ordering::Relaxed) {
                            eprintln!(
                                "rostnix: warning: could not add {} to the store ({err}); crates will be downloaded at build time",
                                krate.name
                            );
                        }
                    }
                }
            });
        }
    });

    let errors = errors.into_inner().unwrap();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n").into())
    }
}

/// Adds the file with `nix store add` and returns the path it printed.
fn add(krate: &Crate) -> Result<String, String> {
    if !krate.cache_file.exists() {
        return Err(format!(
            "{} is not in cargo's cache",
            krate.cache_file.display()
        ));
    }
    let output = Command::new("nix")
        .args([
            "--extra-experimental-features",
            "nix-command",
            "store",
            "add",
        ])
        .args([
            "--mode",
            "flat",
            "--hash-algo",
            "sha256",
            "--name",
            &krate.name,
        ])
        .arg(&krate.cache_file)
        .output()
        .map_err(|err| format!("running nix: {err}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
