//! Store paths of fixed-output files, computed as Nix computes them.

use sha2::{Digest, Sha256};

const BASE32: &[u8; 32] = b"0123456789abcdfghijklmnpqrsvwxyz";

/// The store path of a flat file with the given SHA-256, the path
/// `pkgs.fetchurl { name; sha256; }` and `nix store add --mode flat` give.
pub fn fixed_flat_sha256(store_dir: &str, name: &str, sha256_hex: &str) -> String {
    let inner = hex(&Sha256::digest(format!("fixed:out:sha256:{sha256_hex}:")));
    let digest = Sha256::digest(format!("output:out:sha256:{inner}:{store_dir}:{name}"));
    let mut folded = [0u8; 20];
    for (i, byte) in digest.iter().enumerate() {
        folded[i % 20] ^= byte;
    }
    format!("{store_dir}/{}-{name}", base32(&folded))
}

/// Replaces every character a store path name may not contain with `-`.
pub fn sanitize_name(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '+' | '.' | '_' | '?' | '=' | '-' => c,
            _ => '-',
        })
        .collect();
    // A name may not start with a period.
    match clean.strip_prefix('.') {
        Some(rest) => format!("_{rest}"),
        None => clean,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Nix's base32: its own alphabet, least significant digit last.
fn base32(bytes: &[u8]) -> String {
    let len = (bytes.len() * 8 - 1) / 5 + 1;
    (0..len)
        .rev()
        .map(|n| {
            let (i, j) = (n * 5 / 8, n * 5 % 8);
            let low = bytes[i] >> j;
            let high = if j > 3 { bytes.get(i + 1).map_or(0, |b| b << (8 - j)) } else { 0 };
            BASE32[((low | high) & 0x1f) as usize] as char
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The path `nix store add --mode flat` printed for this file, and the
    // outPath of pkgs.fetchurl with the same name and hash.
    #[test]
    fn fixed_path_matches_nix() {
        let path = fixed_flat_sha256(
            "/nix/store",
            "anyhow-1.0.104.crate",
            "330a5ed07fa54e4702c9d6c4174f74427fc0ef6e214bbd677ae50a5099946470",
        );
        assert_eq!(path, "/nix/store/7xybb3ddg063d2g44a5sc5rf6rdz77dc-anyhow-1.0.104.crate");
    }

    #[test]
    fn sanitize_keeps_semver_and_replaces_the_rest() {
        assert_eq!(sanitize_name("lz4-sys-1.11.1+lz4-1.10.0.crate"), "lz4-sys-1.11.1+lz4-1.10.0.crate");
        assert_eq!(sanitize_name("a b@c/d"), "a-b-c-d");
        assert_eq!(sanitize_name(".hidden"), "_hidden");
    }
}
