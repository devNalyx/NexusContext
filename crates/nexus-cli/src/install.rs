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
        .args(["mcp", "add", "-s", "user", "nexuscontext", "--", "nexusd", "mcp"])
        .status()?;
    if !status.success() {
        anyhow::bail!("exit code {status} - it may already be registered");
    }
    Ok(())
}

/// Linux-only path (`~/.config/Claude/claude_desktop_config.json`) -
/// consistent with this project's overall Linux/GNOME scope.
fn claude_desktop_config_path() -> Option<PathBuf> {
    let config_home = if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(xdg)
    } else {
        PathBuf::from(std::env::var("HOME").ok()?).join(".config")
    };
    Some(config_home.join("Claude").join("claude_desktop_config.json"))
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
