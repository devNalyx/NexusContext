use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Auto-detects MCP-capable agents and wires up `myelind mcp` for each,
/// plus (Claude Code only) the `SessionEnd` ingestion hook - instead of
/// requiring hand-edited config per tool. Mirrors NexusContext's own
/// `nexus install`, with two deliberate differences: this resolves the
/// actual running binaries' absolute paths rather than bare command names
/// (bare names only resolve once installed system-wide, e.g. via the
/// .deb), and refuses to silently reset a malformed config root rather
/// than blowing away existing content.
pub fn run() -> Result<()> {
    let myelin_bin = std::env::current_exe().context("could not determine this binary's path")?;
    let myelin_bin = myelin_bin.canonicalize().unwrap_or(myelin_bin);
    let myelind_bin = sibling_binary(&myelin_bin, "myelind").ok();

    let mut configured = 0;

    match &myelind_bin {
        Some(myelind_bin) => {
            if claude_code_available() {
                println!("Found Claude Code CLI.");
                match configure_claude_code(myelind_bin) {
                    Ok(()) => {
                        println!("  -> registered via `claude mcp add -s user`\n");
                        configured += 1;
                    }
                    Err(err) => println!("  -> `claude mcp add` failed: {err}\n"),
                }
            }

            if let Some(path) = claude_desktop_config_path() {
                if path.parent().is_some_and(|p| p.exists()) {
                    println!("Found Claude Desktop config directory.");
                    match configure_claude_desktop(&path, myelind_bin) {
                        Ok(()) => {
                            println!("  -> added myelin to {}\n", path.display());
                            configured += 1;
                        }
                        Err(err) => println!("  -> failed to update {}: {err}\n", path.display()),
                    }
                }
            }

            if configured == 0 {
                println!("No auto-configurable agents detected on this machine.");
            }
            println!("Generic MCP config, for any other MCP-compatible agent:\n");
            print_generic_snippet(myelind_bin);
        }
        None => {
            println!(
                "Could not find a `myelind` binary next to {} - skipping MCP registration.",
                myelin_bin.display()
            );
        }
    }

    if claude_code_available() {
        let settings_path = myelin_core::Paths::resolve().claude_settings_file();
        match write_session_hook(&settings_path, &myelin_bin) {
            Ok(HookOutcome::Added) => {
                println!("Wired SessionEnd hook into {}", settings_path.display())
            }
            Ok(HookOutcome::AlreadyConfigured) => println!(
                "SessionEnd hook already configured in {}",
                settings_path.display()
            ),
            Err(err) => println!("Could not update {}: {err}", settings_path.display()),
        }
    }

    Ok(())
}

/// Looks for a binary named `name` next to `reference` (e.g. `myelind`
/// alongside the running `myelin` binary) - true in both `cargo build`/
/// `--release` (both land in the same `target/{debug,release}/`) and
/// post-`.deb`-install (both installed to the same `usr/bin/`).
fn sibling_binary(reference: &Path, name: &str) -> Result<PathBuf> {
    let dir = reference
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", reference.display()))?;
    let candidate = dir.join(name);
    if !candidate.is_file() {
        anyhow::bail!(
            "expected {} next to {}",
            candidate.display(),
            reference.display()
        );
    }
    Ok(candidate)
}

fn claude_code_available() -> bool {
    std::process::Command::new("claude")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn configure_claude_code(myelind_bin: &Path) -> Result<()> {
    let status = std::process::Command::new("claude")
        .args(["mcp", "add", "-s", "user", "myelin", "--"])
        .arg(myelind_bin)
        .arg("mcp")
        .status()?;
    if !status.success() {
        anyhow::bail!("exit code {status} - it may already be registered");
    }
    Ok(())
}

/// Linux-only path (`~/.config/Claude/claude_desktop_config.json`) -
/// consistent with NexusContext's own equivalent.
fn claude_desktop_config_path() -> Option<PathBuf> {
    let config_home = if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(xdg)
    } else {
        PathBuf::from(std::env::var("HOME").ok()?).join(".config")
    };
    Some(
        config_home
            .join("Claude")
            .join("claude_desktop_config.json"),
    )
}

fn configure_claude_desktop(path: &Path, myelind_bin: &Path) -> Result<()> {
    let mut config: Value = if path.exists() {
        serde_json::from_str(&std::fs::read_to_string(path)?)
            .with_context(|| format!("{} is not valid JSON", path.display()))?
    } else {
        json!({})
    };

    if !config.is_object() {
        anyhow::bail!(
            "top-level content isn't a JSON object - refusing to modify it automatically"
        );
    }
    let obj = config.as_object_mut().unwrap();
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("existing 'mcpServers' key isn't a JSON object"))?;
    servers.insert(
        "myelin".to_string(),
        json!({ "command": myelind_bin.to_string_lossy(), "args": ["mcp"] }),
    );

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&config)?)?;
    Ok(())
}

fn print_generic_snippet(myelind_bin: &Path) {
    println!(
        "{{\n  \"mcpServers\": {{\n    \"myelin\": {{\n      \"command\": \"{}\",\n      \"args\": [\"mcp\"]\n    }}\n  }}\n}}",
        myelind_bin.display()
    );
}

#[derive(Debug, PartialEq, Eq)]
enum HookOutcome {
    Added,
    AlreadyConfigured,
}

/// Substring, not exact match, so a re-run after a dev->release->installed
/// path change still recognizes an existing entry instead of appending a
/// duplicate that would double-fire `ingest-session` on every session end.
const HOOK_MARKER: &str = "ingest-session";

/// Pure - no I/O, so this is what tests exercise directly. Refuses to
/// touch anything shaped unexpectedly rather than guessing: `settings.json`
/// is a shared file (attribution/statusLine/enabledPlugins/theme alongside
/// hooks, in a real example), not something safe to reset on a hunch.
fn merge_session_hook(mut root: Value, command: &str) -> Result<(Value, HookOutcome)> {
    if !root.is_object() {
        anyhow::bail!(
            "top-level content isn't a JSON object - refusing to modify it automatically"
        );
    }
    let hooks = root
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| json!({}));
    if !hooks.is_object() {
        anyhow::bail!(
            "existing 'hooks' key isn't a JSON object - refusing to modify it automatically"
        );
    }
    let session_end = hooks
        .as_object_mut()
        .unwrap()
        .entry("SessionEnd")
        .or_insert_with(|| json!([]));
    if !session_end.is_array() {
        anyhow::bail!(
            "existing 'hooks.SessionEnd' key isn't an array - refusing to modify it automatically"
        );
    }
    let arr = session_end.as_array_mut().unwrap();

    let already = arr.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|hs| {
                hs.iter().any(|h| {
                    h.get("command")
                        .and_then(Value::as_str)
                        .is_some_and(|c| c.contains(HOOK_MARKER) || c == command)
                })
            })
    });
    if already {
        return Ok((root, HookOutcome::AlreadyConfigured));
    }

    arr.push(json!({ "matcher": "*", "hooks": [{ "type": "command", "command": command }] }));
    Ok((root, HookOutcome::Added))
}

/// Thin I/O wrapper around `merge_session_hook` - explicit `&Path` args, no
/// internal `Paths::resolve()`, so tests can point this at a scratch file.
fn write_session_hook(settings_path: &Path, myelin_bin: &Path) -> Result<HookOutcome> {
    let root: Value = if settings_path.exists() {
        let raw = std::fs::read_to_string(settings_path)
            .with_context(|| format!("reading {}", settings_path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("{} is not valid JSON", settings_path.display()))?
    } else {
        json!({})
    };

    let command = format!("{} ingest-session", myelin_bin.display());
    let (merged, outcome) = merge_session_hook(root, &command)?;

    if outcome == HookOutcome::Added {
        if let Some(parent) = settings_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(settings_path, serde_json::to_string_pretty(&merged)?)
            .with_context(|| format!("writing {}", settings_path.display()))?;
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch_path(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("myelin-install-test-{label}-{nanos}"))
    }

    #[test]
    fn missing_file_adds_the_hook() {
        let path = scratch_path("missing-file");
        let outcome = write_session_hook(&path, Path::new("/usr/bin/myelin")).unwrap();
        assert_eq!(outcome, HookOutcome::Added);
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            written["hooks"]["SessionEnd"][0]["hooks"][0]["command"],
            "/usr/bin/myelin ingest-session"
        );
    }

    #[test]
    fn unrelated_content_and_existing_hooks_are_preserved() {
        let path = scratch_path("preserve-unrelated");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "theme": "auto",
                "hooks": {
                    "SessionEnd": [
                        { "matcher": "*", "hooks": [{ "type": "command", "command": "some-other-tool" }] }
                    ]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let outcome = write_session_hook(&path, Path::new("/usr/bin/myelin")).unwrap();
        assert_eq!(outcome, HookOutcome::Added);

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["theme"], "auto");
        let arr = written["hooks"]["SessionEnd"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["hooks"][0]["command"], "some-other-tool");
        assert_eq!(
            arr[1]["hooks"][0]["command"],
            "/usr/bin/myelin ingest-session"
        );
    }

    #[test]
    fn malformed_root_is_rejected_and_file_left_untouched() {
        let path = scratch_path("malformed-root");
        std::fs::write(&path, "[1, 2, 3]").unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let err = write_session_hook(&path, Path::new("/usr/bin/myelin")).unwrap_err();
        assert!(err.to_string().contains("JSON object"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn unparseable_json_is_rejected() {
        let path = scratch_path("unparseable");
        std::fs::write(&path, "{ not valid json").unwrap();
        let err = write_session_hook(&path, Path::new("/usr/bin/myelin")).unwrap_err();
        assert!(err.to_string().contains("not valid JSON"));
    }

    #[test]
    fn wrong_shaped_hooks_key_is_rejected() {
        let root = json!({ "hooks": "not an object" });
        let err = merge_session_hook(root, "/usr/bin/myelin ingest-session").unwrap_err();
        assert!(err.to_string().contains("'hooks' key"));
    }

    #[test]
    fn wrong_shaped_session_end_key_is_rejected() {
        let root = json!({ "hooks": { "SessionEnd": "not an array" } });
        let err = merge_session_hook(root, "/usr/bin/myelin ingest-session").unwrap_err();
        assert!(err.to_string().contains("'hooks.SessionEnd' key"));
    }

    #[test]
    fn rerun_with_identical_inputs_does_not_duplicate() {
        let path = scratch_path("rerun-identical");
        write_session_hook(&path, Path::new("/usr/bin/myelin")).unwrap();
        let second = write_session_hook(&path, Path::new("/usr/bin/myelin")).unwrap();
        assert_eq!(second, HookOutcome::AlreadyConfigured);

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["hooks"]["SessionEnd"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn rerun_with_different_path_but_same_marker_is_recognized() {
        let path = scratch_path("rerun-path-drift");
        write_session_hook(&path, Path::new("/home/user/repo/target/release/myelin")).unwrap();
        // Simulates dev -> installed path drift: different absolute path,
        // same "ingest-session" marker.
        let second = write_session_hook(&path, Path::new("/usr/bin/myelin")).unwrap();
        assert_eq!(second, HookOutcome::AlreadyConfigured);

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["hooks"]["SessionEnd"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn sibling_binary_finds_a_real_sibling() {
        let path = scratch_path("sibling-found");
        std::fs::create_dir_all(&path).unwrap();
        let reference = path.join("myelin");
        std::fs::write(&reference, "fake binary").unwrap();
        let sibling = path.join("myelind");
        std::fs::write(&sibling, "fake binary").unwrap();

        let found = sibling_binary(&reference, "myelind").unwrap();
        assert_eq!(found, sibling);
    }

    #[test]
    fn sibling_binary_errors_clearly_when_missing() {
        let path = scratch_path("sibling-missing");
        std::fs::create_dir_all(&path).unwrap();
        let reference = path.join("myelin");
        std::fs::write(&reference, "fake binary").unwrap();

        let err = sibling_binary(&reference, "myelind").unwrap_err();
        assert!(err.to_string().contains("myelind"));
    }
}
