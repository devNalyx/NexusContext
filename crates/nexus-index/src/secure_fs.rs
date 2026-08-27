//! Defense-in-depth file reads for the paths that touch the filesystem
//! *after* a `canonicalize()` + `allowed_roots` check has already passed
//! (see `queries::canonicalize_and_authorize` / `get_file_context`,
//! `ingest::read_source_capped`).
//!
//! This is **not** full TOCTOU-proofing. See issue #72, ADR 0015, and the
//! "TOCTOU vs. confused deputy" section of Security-Model.md for the full
//! reasoning and the gap this deliberately does not close.
//!
//! What this module does: on Unix, opens the file with `O_NOFOLLOW`, so if
//! the path was swapped for a symlink *after* the canonicalize/allowed-roots
//! check ran but *before* this open executes, the open fails (ELOOP) instead
//! of silently following the attacker-controlled symlink target. That closes
//! the cheapest/most common TOCTOU attack shape: symlink substitution.
//!
//! What this module explicitly does NOT do: it does not defend against the
//! path being replaced with a *different regular file or directory* at the
//! same name between check and use - `O_NOFOLLOW` only rejects symlinks, not
//! substituted non-symlink content. Closing that fully would need atomic
//! check-and-open (e.g. Linux `openat2(RESOLVE_NO_SYMLINKS)` plus re-deriving
//! the path from an already-open directory fd, all the way down), which is
//! out of scope here - see issue #72's remaining checklist.
//!
//! Windows and macOS: unchanged, `#[cfg(unix)]` gates all of the above.
//! `O_NOFOLLOW` isn't a thing on Windows, and macOS's equivalent mechanisms
//! (e.g. `O_NOFOLLOW_ANY` on newer Darwin) aren't targeted here either - both
//! are a documented known gap, not attempted.

use anyhow::{Context, Result};
use std::path::Path;

/// User-facing error for when the O_NOFOLLOW-guarded open rejects a path -
/// deliberately doesn't leak the raw OS errno/message to the MCP caller,
/// matching the clean-message style the other checks in this crate already
/// use (e.g. `canonicalize_and_authorize`'s "repo_path does not exist").
const UNEXPECTED_PATH_MSG: &str =
    "path resolved to something unexpected mid-check (possible symlink substitution) - refusing to read";

/// Reads a file's raw bytes, using `O_NOFOLLOW` on Unix as defense-in-depth
/// against the path being swapped for a symlink between an earlier
/// `canonicalize()`/`allowed_roots` check and this read. On non-Unix
/// platforms this is a plain `std::fs::read` - no equivalent guard exists
/// there today (documented gap, see module docs).
pub fn read_verified(path: &Path) -> Result<Vec<u8>> {
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::io::Read;
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|e| {
                if e.raw_os_error() == Some(libc::ELOOP) {
                    anyhow::anyhow!(UNEXPECTED_PATH_MSG)
                } else {
                    anyhow::Error::from(e).context(format!("failed to open {}", path.display()))
                }
            })?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Ok(buf)
    }
    #[cfg(not(unix))]
    {
        std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))
    }
}

/// Same as [`read_verified`] but decodes the result as UTF-8, matching what
/// `get_file_context` needs `std::fs::read_to_string` for. Non-UTF-8 content
/// produces a clean error rather than the raw `FromUtf8Error`.
pub fn read_to_string_verified(path: &Path) -> Result<String> {
    let bytes = read_verified(path)?;
    String::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!("file is not valid UTF-8: {}", path.display()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nexus_secure_fs_test_{name}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_a_plain_file_normally() {
        let dir = temp_dir("plain");
        let file_path = dir.join("real.txt");
        fs::write(&file_path, b"hello").unwrap();

        let content = read_to_string_verified(&file_path).unwrap();
        assert_eq!(content, "hello");
        let _ = fs::remove_dir_all(&dir);
    }

    /// Simulates the TOCTOU window from issue #72: a path that was a
    /// regular file at "canonicalize-and-check" time gets swapped out for a
    /// symlink pointing outside the allowed root before the actual read
    /// happens. `O_NOFOLLOW` must cause the read to fail cleanly instead of
    /// silently following the attacker's substituted symlink target.
    #[test]
    fn rejects_a_file_swapped_for_a_symlink_between_check_and_read() {
        let allowed_dir = temp_dir("race_allowed");
        let outside_dir = temp_dir("race_outside");

        let secret_path = outside_dir.join("secret.txt");
        fs::write(&secret_path, b"outside-secret").unwrap();

        let target_path = allowed_dir.join("target.txt");
        fs::write(&target_path, b"original-content").unwrap();

        // Step 1: the "canonicalize + allowed_roots check" step runs against
        // a genuine regular file - this is what a real caller does before
        // ever reading content.
        let canonical = target_path.canonicalize().unwrap();
        assert!(canonical.starts_with(allowed_dir.canonicalize().unwrap()));

        // Step 2 (the race): the file at that exact path is replaced with a
        // symlink pointing outside the allowed root, simulating a co-resident
        // attacker winning the TOCTOU window.
        fs::remove_file(&target_path).unwrap();
        symlink(&secret_path, &target_path).unwrap();

        // Step 3: the actual read - must NOT follow the symlink and must NOT
        // return the outside secret.
        let result = read_to_string_verified(&target_path);
        assert!(
            result.is_err(),
            "expected O_NOFOLLOW to reject the swapped symlink"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("unexpected") || msg.contains("symlink"),
            "expected a clean 'unexpected path' error, got: {msg}"
        );

        let _ = fs::remove_dir_all(&allowed_dir);
        let _ = fs::remove_dir_all(&outside_dir);
    }
}
