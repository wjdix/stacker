//! Guard against a mismatch between the installed `jj` binary and the `jj-lib`
//! this tool is compiled against.
//!
//! `jj-lib` reads the on-disk `.jj` format; if you `brew upgrade jj` to a newer
//! version that writes a newer format, our linked library may misread or corrupt
//! it. We pin `jj-lib` to a specific version and refuse to run if the installed
//! `jj` binary's minor version doesn't match.
//!
//! Probing `jj --version` is a one-shot subprocess, not a repo operation, so it
//! doesn't violate the "native, no shelling for repo ops" design.

use anyhow::{Result, anyhow};

/// The `jj` minor version series this binary's `jj-lib` corresponds to.
/// Must be kept in lockstep with the pinned `jj-lib` version in Cargo.toml.
pub const EXPECTED_JJ_MINOR: &str = "0.41";

/// Refuse to proceed if the installed `jj` minor version differs.
/// If `jj` isn't installed, we proceed (we use `jj-lib`, not the binary) but
/// surface a warning.
pub fn check() -> Result<()> {
    let output = std::process::Command::new("jj").arg("--version").output();
    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let version = stdout.split_whitespace().nth(1).unwrap_or("").trim();
            if minor_series(version) == Some(EXPECTED_JJ_MINOR) {
                Ok(())
            } else {
                Err(anyhow!(
                    "installed jj version `{version}` does not match stacker's jj-lib \
                     ({EXPECTED_JJ_MINOR}.x).\n\
                     The on-disk repo format may be incompatible. Rebuild stacker against a \
                     matching jj-lib, or align your jj version to {EXPECTED_JJ_MINOR}.x."
                ))
            }
        }
        _ => {
            eprintln!(
                "warning: could not run `jj --version` to verify version compatibility; \
                 proceeding with jj-lib {EXPECTED_JJ_MINOR}.x"
            );
            Ok(())
        }
    }
}

/// Extract the `major.minor` series from a version string like `0.41.0`.
fn minor_series(version: &str) -> Option<&str> {
    let mut idx = 0;
    let mut dots = 0;
    for (i, c) in version.char_indices() {
        if c == '.' {
            dots += 1;
            if dots == 2 {
                return Some(&version[..i]);
            }
        }
        idx = i;
    }
    let _ = idx;
    // Fewer than two dots: accept the whole thing (e.g. "0.41").
    if dots == 1 { Some(version) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_minor_series() {
        assert_eq!(minor_series("0.41.0"), Some("0.41"));
        assert_eq!(minor_series("0.42.1"), Some("0.42"));
        assert_eq!(minor_series("0.41"), Some("0.41"));
        assert_eq!(minor_series("garbage"), None);
    }
}
