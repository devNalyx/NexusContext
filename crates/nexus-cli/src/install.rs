use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Source of the plug, embedded at build time so `nexus install opencode`
/// works from a released binary without an `npm install` step.
const PLUGIN_INDEX: &str = include_str!("../../../integrations/opencode-nexuscontext/src/index.js");
const PLUGIN_CONTROL: &str =
    include_str!("../../../integrations/opencode-nexuscontext/src/control.js");
const PLUGIN_TUI: &str = include_str!("../../../integrations/opencode-nexuscontext/src/tui.tsx");
const MANIFEST: &str = ".nexus/install-manifest.json";
/// Project-local Claude Code settings: private to this checkout, never shared
/// through git, matching the "local-first, per-repo" constraint.
const CLAUDE_SETTINGS_LOCAL: &str = ".claude/settings.local.json";
const CLAUDE_HOOK_SCRIPT: &str = ".claude/hooks/nexus-session-end.sh";
/// `./tui` is what makes OpenCode load the status slot. Peer declarations are
/// documentation for npm users; OpenCode resolves its own TUI runtime for
/// local plugins, so there is no `npm install` step here.
const PLUGIN_PACKAGE: &str = r#"{
  "name": "nexuscontext-plugin",
  "private": true,
  "type": "module",
  "exports": {
    ".": "./index.js",
    "./tui": "./tui.tsx"
  },
  "peerDependencies": {
    "@opentui/core": ">=0.6.6",
    "@opentui/solid": ">=0.6.6",
    "solid-js": ">=1.9.0"
  }
}
"#;

/// Wire `nexusd mcp` into an agent. **Nothing is written without `--yes`.**
/// Config files are merged, never replaced: an installer that silently
/// clobbers a user's agent settings is worse than no installer at all.
pub fn run(target: Option<&str>, yes: bool) -> Result<()> {
    match target {
        Some("opencode") => install_opencode(yes),
        Some("claude") | None => install_claude(yes),
        Some(other) => bail!("unknown install target '{other}' — expected 'opencode' or 'claude'"),
    }
}

fn manifest_path() -> PathBuf {
    PathBuf::from(MANIFEST)
}

#[derive(Default)]
struct Manifest {
    created: Vec<PathBuf>,
    /// True when `opencode.json` existed before install and we merged into
    /// it: uninstall must remove just our server entry, not the user's file.
    merged_config: bool,
    /// Whatever `mcp.servers.nexuscontext` held before we replaced it, so
    /// uninstall can restore it instead of silently dropping user config.
    previous_server: Option<Value>,
}

fn read_manifest() -> Manifest {
    let Some(value) = std::fs::read_to_string(manifest_path())
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
    else {
        return Manifest::default();
    };
    Manifest {
        created: value["created"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(PathBuf::from))
                    .collect()
            })
            .unwrap_or_default(),
        merged_config: value["merged_config"].as_bool().unwrap_or(false),
        previous_server: value
            .get("previous_server")
            .filter(|v| !v.is_null())
            .cloned(),
    }
}

fn write_manifest(
    paths: &[PathBuf],
    merged_config: bool,
    previous_server: Option<&Value>,
) -> Result<()> {
    if let Some(parent) = manifest_path().parent() {
        std::fs::create_dir_all(parent)?;
    }
    let created: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    std::fs::write(
        manifest_path(),
        serde_json::to_string_pretty(&json!({
            "created": created,
            "merged_config": merged_config,
            "previous_server": previous_server,
        }))?,
    )?;
    Ok(())
}

fn install_opencode(yes: bool) -> Result<()> {
    let plugin_dir = Path::new(".opencode").join("plugins").join("nexuscontext");
    let index = plugin_dir.join("index.js");
    let control = plugin_dir.join("control.js");
    let tui = plugin_dir.join("tui.tsx");
    let package = plugin_dir.join("package.json");
    let config = PathBuf::from("opencode.json");

    let manifest = read_manifest();
    if !manifest.created.is_empty() {
        println!(
            "Existing NexusContext install manifest found at {}",
            manifest_path().display()
        );
        println!("Run `nexus install opencode --uninstall --yes` first to remove it cleanly.");
        return Ok(());
    }

    println!("OpenCode V2 install plan (project-local, no account, no cloud):");
    println!("  plugin   : {}", index.display());
    println!("  plugin   : {}", control.display());
    println!("  TUI      : {}", tui.display());
    println!(
        "  MCP      : mcp.servers.nexuscontext -> nexusd mcp in {}",
        config.display()
    );
    println!("  manifest : {}", manifest_path().display());
    if !yes {
        println!("\nDRY RUN — nothing written. Re-run with --yes to apply.");
        print_generic_snippet();
        return Ok(());
    }

    std::fs::create_dir_all(&plugin_dir)?;
    // Only write files we own; an existing user plugin of the same name is a
    // conflict, not something to overwrite.
    for (path, contents) in [
        (&index, PLUGIN_INDEX),
        (&control, PLUGIN_CONTROL),
        (&tui, PLUGIN_TUI),
        (&package, PLUGIN_PACKAGE),
    ] {
        if path.exists() {
            bail!("refusing to overwrite existing {}", path.display());
        }
        std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    }

    let mut files: Vec<PathBuf> = vec![index, control, tui, package];
    let mut merged_config = false;
    let mut previous_server = None;
    if config.exists() {
        merged_config = true;
        let raw = std::fs::read_to_string(&config)?;
        let mut doc: Value = serde_json::from_str(&raw).with_context(|| {
            format!(
                "{} is not plain JSON (JSONC comments are not merged automatically); edit it by hand",
                config.display()
            )
        })?;
        if !doc.is_object() {
            bail!("{} is not a JSON object", config.display());
        }
        previous_server = doc
            .get("mcp")
            .and_then(|m| m.get("servers"))
            .and_then(|s| s.get("nexuscontext"))
            .cloned();
        doc["mcp"]["servers"]["nexuscontext"] =
            json!({ "type": "local", "command": ["nexusd", "mcp"] });
        std::fs::write(&config, serde_json::to_string_pretty(&doc)?)?;
    } else {
        std::fs::write(
            &config,
            serde_json::to_string_pretty(&json!({
                "$schema": "https://opencode.ai/config.json",
                "mcp": { "servers": {
                    "nexuscontext": { "type": "local", "command": ["nexusd", "mcp"] }
                }}
            }))?,
        )?;
        files.push(config);
    }
    write_manifest(&files, merged_config, previous_server.as_ref())?;
    println!("\nInstalled. Start `nexusd serve` before opening OpenCode so the plug can reach the control socket.");
    Ok(())
}

pub fn uninstall_opencode(yes: bool) -> Result<()> {
    let manifest = read_manifest();
    if manifest.created.is_empty() {
        println!("No NexusContext install manifest found — nothing to remove.");
        return Ok(());
    }
    println!("Will remove exactly these files:");
    for path in &manifest.created {
        println!("  {}", path.display());
    }
    if manifest.merged_config {
        println!(
            "  and undo the nexuscontext entry in opencode.json (keeping your other settings)"
        );
    }
    if !yes {
        println!("\nDRY RUN — nothing removed.");
        return Ok(());
    }
    for path in &manifest.created {
        match std::fs::remove_file(path) {
            Ok(()) => println!("removed {}", path.display()),
            Err(err) => println!("could not remove {}: {err}", path.display()),
        }
    }
    if manifest.merged_config {
        let config = PathBuf::from("opencode.json");
        if config.exists() {
            let raw = std::fs::read_to_string(&config)?;
            let mut doc: Value = serde_json::from_str(&raw)?;
            if let Some(servers) = doc
                .get_mut("mcp")
                .and_then(|m| m.get_mut("servers"))
                .and_then(Value::as_object_mut)
            {
                match manifest.previous_server {
                    Some(previous) => {
                        servers.insert("nexuscontext".to_string(), previous);
                    }
                    None => {
                        servers.remove("nexuscontext");
                    }
                }
            }
            // Don't leave empty `mcp` / `servers` shells behind.
            if doc["mcp"]["servers"]
                .as_object()
                .map(|o| o.is_empty())
                .unwrap_or(false)
            {
                doc["mcp"]
                    .as_object_mut()
                    .expect("mcp.servers object implies mcp object")
                    .remove("servers");
            }
            if doc["mcp"]
                .as_object()
                .map(|o| o.is_empty())
                .unwrap_or(false)
            {
                doc.as_object_mut()
                    .expect("mcp object implies root object")
                    .remove("mcp");
            }
            std::fs::write(&config, serde_json::to_string_pretty(&doc)?)?;
            println!("restored {} to its pre-install state", config.display());
        }
    }
    let _ = std::fs::remove_file(manifest_path());
    // `remove_file` leaves the directories behind. Remove only the ones that
    // are now empty — a user's own `.nexus/memory` bundle must survive.
    for dir in [
        Path::new(".opencode/plugins/nexuscontext"),
        Path::new(".opencode/plugins"),
        Path::new(".opencode"),
        Path::new(".nexus"),
    ] {
        let _ = std::fs::remove_dir(dir);
    }
    println!("Your OpenCode settings are otherwise untouched.");
    Ok(())
}

fn install_claude(yes: bool) -> Result<()> {
    let mut configured = 0;
    configure_claude_session_end(yes)?;

    if claude_code_available() {
        println!("Found Claude Code CLI.");
        if yes {
            match configure_claude_code() {
                Ok(()) => {
                    println!("  -> registered via `claude mcp add -s user`\n");
                    configured += 1;
                }
                Err(err) => println!("  -> `claude mcp add` failed: {err}\n"),
            }
        } else {
            println!(
                "  -> would run `claude mcp add -s user nexuscontext -- nexusd mcp` (dry run)\n"
            );
        }
    }

    if let Some(path) = claude_desktop_config_path() {
        if path.parent().map(|p| p.exists()).unwrap_or(false) {
            println!("Found Claude Desktop config directory.");
            if yes {
                match configure_claude_desktop(&path) {
                    Ok(()) => {
                        println!("  -> merged nexuscontext into {}\n", path.display());
                        configured += 1;
                    }
                    Err(err) => println!("  -> failed to update {}: {err}\n", path.display()),
                }
            } else {
                println!(
                    "  -> would merge one mcpServers entry into {} (dry run)\n",
                    path.display()
                );
            }
        }
    }

    if !yes {
        println!("DRY RUN — no Claude settings changed. Re-run with --yes to apply.");
    } else if configured == 0 {
        println!("No auto-configurable Claude agents detected on this machine.");
    }

    println!("Generic MCP config, for any other MCP-compatible agent:\n");
    print_generic_snippet();
    Ok(())
}

/// The `SessionEnd` hook stages only explicitly signposted observations. It
/// lives in `.claude/settings.local.json` so a per-repo install cannot leak
/// into a user's global Claude settings or into a committed repo file.
fn configure_claude_session_end(yes: bool) -> Result<()> {
    let settings = Path::new(CLAUDE_SETTINGS_LOCAL);
    let script = Path::new(CLAUDE_HOOK_SCRIPT);
    let handler = claude_hook_handler();

    println!("Claude Code SessionEnd hook (project-local, signpost-only staging):");
    println!("  script   : {}", script.display());
    println!("  settings : {}", settings.display());

    let mut doc: Value = if settings.exists() {
        serde_json::from_str(&std::fs::read_to_string(settings)?)
            .with_context(|| format!("{} is not valid JSON", settings.display()))?
    } else {
        json!({})
    };
    if !doc.is_object() {
        bail!("{} must be a JSON object", settings.display());
    }
    if has_claude_hook(&doc, &handler) {
        println!("  -> already configured; nothing to add\n");
        return Ok(());
    }
    if !yes {
        println!("  -> would append one SessionEnd hook (dry run)\n");
        return Ok(());
    }

    if script.exists() {
        let existing = std::fs::read_to_string(script)?;
        if !existing.contains("NexusContext SessionEnd hook") {
            bail!(
                "{} already exists and is not a NexusContext hook; refusing to overwrite",
                script.display()
            );
        }
    } else {
        if let Some(parent) = script.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(script, claude_hook_script()?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755))?;
        }
    }

    append_claude_hook(&mut doc, handler);
    if let Some(parent) = settings.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        settings,
        format!("{}\n", serde_json::to_string_pretty(&doc)?),
    )?;
    println!(
        "  -> merged one SessionEnd hook into {}\n",
        settings.display()
    );
    Ok(())
}

fn claude_hook_handler() -> Value {
    json!({
        "type": "command",
        "command": "${CLAUDE_PROJECT_DIR}/.claude/hooks/nexus-session-end.sh",
        "args": [],
        "timeout": 30,
    })
}

fn has_claude_hook(doc: &Value, handler: &Value) -> bool {
    doc["hooks"]["SessionEnd"]
        .as_array()
        .map(|groups| {
            groups.iter().any(|group| {
                group["hooks"]
                    .as_array()
                    .map(|handlers| handlers.iter().any(|h| h == handler))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn append_claude_hook(doc: &mut Value, handler: Value) {
    let groups = doc
        .as_object_mut()
        .expect("validated as an object above")
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("hooks must be an object")
        .entry("SessionEnd")
        .or_insert_with(|| json!([]));
    let groups = groups.as_array_mut().expect("SessionEnd is an array");
    groups.push(json!({ "hooks": [handler] }));
}

fn claude_hook_script() -> Result<String> {
    let binary = std::env::current_exe()?;
    let quoted = binary.display().to_string().replace('\'', "'\\''");
    Ok(format!(
        "#!/bin/sh\n# NexusContext SessionEnd hook — generated by `nexus install claude --yes`.\n# Deterministic signpost-only staging: no LLM, no raw transcript storage.\nset -eu\nNEXUS_BIN='{quoted}'\nif [ -x \"$NEXUS_BIN\" ]; then\n  exec \"$NEXUS_BIN\" session-end\nfi\nexec nexus session-end\n"
    ))
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

fn claude_desktop_config_path() -> Option<PathBuf> {
    Some(
        directories::BaseDirs::new()?
            .config_dir()
            .join("Claude")
            .join("claude_desktop_config.json"),
    )
}

fn configure_claude_desktop(path: &Path) -> Result<()> {
    let mut config: Value = if path.exists() {
        serde_json::from_str(&std::fs::read_to_string(path)?)?
    } else {
        json!({})
    };
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
  "mcp": {{
    "servers": {{
      "nexuscontext": {{ "type": "local", "command": ["nexusd", "mcp"] }}
    }}
  }}
}}"#
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // `nexus install` works on paths relative to the repo root, so these
    // tests swap the process cwd. Serialize them: cwd is global per process.
    static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct TempCwd {
        previous: PathBuf,
        dir: PathBuf,
    }

    impl TempCwd {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("nc-install-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let previous = std::env::current_dir().unwrap();
            std::env::set_current_dir(&dir).unwrap();
            Self { previous, dir }
        }
    }

    impl Drop for TempCwd {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.previous);
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn embedded_plugin_is_not_empty() {
        assert!(PLUGIN_INDEX.contains("Plugin.define"));
        assert!(PLUGIN_CONTROL.contains("NexusControlClient"));
        assert!(PLUGIN_TUI.contains("home.footer.status"));
        assert!(PLUGIN_PACKAGE.contains("\"./tui\""));
    }

    #[test]
    fn dry_run_writes_nothing() {
        let _guard = CWD_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let _cwd = TempCwd::new("dry");
        install_opencode(false).unwrap();
        assert!(!Path::new(".opencode").exists());
        assert!(!Path::new("opencode.json").exists());
        assert!(!Path::new(MANIFEST).exists());
    }

    #[test]
    fn claude_hook_dry_run_writes_nothing() {
        let _guard = CWD_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let _cwd = TempCwd::new("claude-dry");
        configure_claude_session_end(false).unwrap();
        assert!(!Path::new(CLAUDE_SETTINGS_LOCAL).exists());
        assert!(!Path::new(CLAUDE_HOOK_SCRIPT).exists());
    }

    #[test]
    fn claude_hook_merges_without_clobbering_existing_hooks() {
        let _guard = CWD_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let _cwd = TempCwd::new("claude-merge");
        let original = json!({
            "permissions": { "allow": ["Bash(git status)"] },
            "hooks": {
                "SessionEnd": [
                    { "hooks": [{ "type": "command", "command": "echo mine" }] }
                ]
            }
        });
        std::fs::create_dir_all(".claude").unwrap();
        std::fs::write(
            CLAUDE_SETTINGS_LOCAL,
            serde_json::to_string_pretty(&original).unwrap(),
        )
        .unwrap();

        configure_claude_session_end(true).unwrap();
        let installed = read_json(Path::new(CLAUDE_SETTINGS_LOCAL));
        assert_eq!(installed["permissions"], original["permissions"]);
        let groups = installed["hooks"]["SessionEnd"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0], original["hooks"]["SessionEnd"][0]);
        assert_eq!(
            groups[1]["hooks"][0]["command"],
            "${CLAUDE_PROJECT_DIR}/.claude/hooks/nexus-session-end.sh"
        );
        assert!(Path::new(CLAUDE_HOOK_SCRIPT).exists());
        let script = std::fs::read_to_string(CLAUDE_HOOK_SCRIPT).unwrap();
        assert!(script.contains("NexusContext SessionEnd hook"));
        assert!(script.contains("session-end"));

        // Re-running is idempotent: no duplicate hook is appended.
        configure_claude_session_end(true).unwrap();
        let again = read_json(Path::new(CLAUDE_SETTINGS_LOCAL));
        assert_eq!(again, installed);
    }

    #[test]
    fn uninstall_restores_the_users_config_exactly() {
        let _guard = CWD_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let _cwd = TempCwd::new("merge");
        let original = json!({
            "$schema": "https://opencode.ai/config.json",
            "theme": "opencode",
            "mcp": { "servers": {
                "context7": { "type": "local", "command": ["npx", "context7"] },
                "nexuscontext": { "type": "local", "command": ["whatever-was-here"] }
            }}
        });
        std::fs::write(
            "opencode.json",
            serde_json::to_string_pretty(&original).unwrap(),
        )
        .unwrap();

        install_opencode(true).unwrap();
        let installed = read_json(Path::new("opencode.json"));
        assert_eq!(installed["theme"], "opencode");
        assert_eq!(
            installed["mcp"]["servers"]["context7"]["command"][1],
            "context7"
        );
        assert_eq!(
            installed["mcp"]["servers"]["nexuscontext"]["command"][1],
            "mcp"
        );
        assert!(Path::new(".opencode/plugins/nexuscontext/index.js").exists());
        assert!(Path::new(".opencode/plugins/nexuscontext/tui.tsx").exists());

        uninstall_opencode(true).unwrap();
        assert_eq!(read_json(Path::new("opencode.json")), original);
        assert!(!Path::new(".opencode/plugins/nexuscontext/index.js").exists());
        assert!(!Path::new(".opencode/plugins/nexuscontext/tui.tsx").exists());
        assert!(!Path::new(MANIFEST).exists());
    }

    #[test]
    fn uninstall_removes_the_config_it_created() {
        let _guard = CWD_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let _cwd = TempCwd::new("created");
        install_opencode(true).unwrap();
        assert!(Path::new("opencode.json").exists());
        uninstall_opencode(true).unwrap();
        assert!(!Path::new("opencode.json").exists());
        assert!(!Path::new(".opencode").exists());
        assert!(!Path::new(MANIFEST).exists());
    }
}
