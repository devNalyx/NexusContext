use anyhow::Result;
use serde_json::{json, Value};
use std::path::PathBuf;

/// Auto-detects MCP-capable agents and wires up `nexusd mcp` for each,
/// instead of requiring hand-edited config per tool. Deliberately scoped to
/// what can be verified rather than guessing at config formats: Claude
/// Code has its own CLI for this (so we shell out to the exact mechanism
/// already proven to work, rather than reverse-engineering its config
/// file), and Claude Desktop's `claude_desktop_config.json` format is
/// stable and well-documented. Anything else just gets the generic snippet
/// printed - better than silently corrupting a config file whose shape
/// isn't actually confirmed.
pub fn run() -> Result<()> {
    let mut configured = 0;

    if claude_code_available() {
        println!("Found Claude Code CLI.");
        match configure_claude_code() {
            Ok(()) => {
                println!("  -> registered via `claude mcp add -s user`\n");
                configured += 1;
            }
            Err(err) => println!("  -> `claude mcp add` failed: {err}\n"),
        }
    }

    if let Some(path) = claude_desktop_config_path() {
        if path.parent().map(|p| p.exists()).unwrap_or(false) {
            println!("Found Claude Desktop config directory.");
            match configure_claude_desktop(&path) {
                Ok(()) => {
                    println!("  -> added nexuscontext to {}\n", path.display());
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
    print_generic_snippet();

    Ok(())
}

fn claude_code_available() -> bool {
    std::process::Command::new("claude")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn configure_claude_code() -> Result<()> {
    let status = std::process::Command::new("claude")
        .args([
            "mcp",
            "add",
            "-s",
            "user",
            "nexuscontext",
            "--",
            "nexusd",
            "mcp",
        ])
        .status()?;
    if !status.success() {
        anyhow::bail!("exit code {status} - it may already be registered");
    }
    Ok(())
}

/// Cross-platform: `directories::BaseDirs::config_dir()` already resolves
/// to exactly the base Claude Desktop itself uses on each OS - `~/.config`
/// (respecting `$XDG_CONFIG_HOME`) on Linux, `~/Library/Application
/// Support` on macOS, `%APPDATA%` on Windows - so this is the same
/// `Claude/claude_desktop_config.json` join on all three, not three
/// separately-maintained hand-rolled paths. Previously Linux-only (a
/// hardcoded `$HOME/.config` join) - real bug on both other platforms,
/// not just an unimplemented one, since it silently resolved to a path
/// Claude Desktop never reads there.
fn claude_desktop_config_path() -> Option<PathBuf> {
    Some(
        directories::BaseDirs::new()?
            .config_dir()
            .join("Claude")
            .join("claude_desktop_config.json"),
    )
}

fn configure_claude_desktop(path: &PathBuf) -> Result<()> {
    let mut config: Value = if path.exists() {
        serde_json::from_str(&std::fs::read_to_string(path)?)?
    } else {
        json!({})
    };

    // Merge rather than overwrite - this file is shared with whatever else
    // the user has already configured, so clobbering it would be a real
    // problem, not just a style choice.
    if !config.is_object() {
        config = json!({});
    }
    let obj = config.as_object_mut().unwrap();
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("existing 'mcpServers' key isn't a JSON object"))?;
    servers.insert(
        "nexuscontext".to_string(),
        json!({ "command": "nexusd", "args": ["mcp"] }),
    );

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&config)?)?;
    Ok(())
}

fn print_generic_snippet() {
    println!(
        r#"{{
  "mcpServers": {{
    "nexuscontext": {{
      "command": "nexusd",
      "args": ["mcp"]
    }}
  }}
}}"#
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `BaseDirs::config_dir()`'s own per-OS correctness is `directories`'
    /// job, already documented upstream, not re-verified here (and not
    /// mockable - it reads real env/OS state, not a parameter). What this
    /// project's own code controls, and is worth locking in, is the join:
    /// every platform ends up pointed at the same relative
    /// `Claude/claude_desktop_config.json`, not three separately-typed
    /// path literals that could drift apart again.
    #[test]
    fn resolves_to_the_same_claude_desktop_relative_path_on_this_platform() {
        let path = claude_desktop_config_path().expect("a home directory exists in test envs");
        // Path::ends_with compares components, not raw string separators,
        // so this holds regardless of the platform's own separator.
        assert!(path.ends_with(std::path::Path::new("Claude").join("claude_desktop_config.json")));
    }
}
