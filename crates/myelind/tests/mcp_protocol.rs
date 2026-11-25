//! Drives the real `myelind mcp` binary as a subprocess over its actual
//! stdio JSON-RPC transport - everything so far had only been verified by
//! hand-piping JSON-RPC or via myelin-index's unit tests, never as an
//! automated check that the wire protocol itself behaves.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

struct McpSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl McpSession {
    fn start(data_dir: &std::path::Path, skills_dir: &std::path::Path) -> Self {
        Self::start_with_config(data_dir, skills_dir, None)
    }

    fn start_with_config(
        data_dir: &std::path::Path,
        skills_dir: &std::path::Path,
        config_dir: Option<&std::path::Path>,
    ) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_myelind"));
        cmd.arg("mcp")
            .env("MYELIN_DATA_DIR", data_dir)
            .env("MYELIN_SKILLS_DIR", skills_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(config_dir) = config_dir {
            cmd.env("MYELIN_CONFIG_DIR", config_dir);
        }
        let mut child = cmd.spawn().expect("failed to spawn myelind mcp");

        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn call(&mut self, id: i64, method: &str, params: Value) -> Value {
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.stdin, "{request}").unwrap();
        self.stdin.flush().unwrap();

        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        serde_json::from_str(line.trim()).unwrap()
    }

    fn tool(&mut self, id: i64, name: &str, arguments: Value) -> Value {
        let response = self.call(
            id,
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        response["result"].clone()
    }
}

impl Drop for McpSession {
    fn drop(&mut self) {
        // Dropping stdin closes the pipe, which ends the child's
        // `stdin.lines()` loop so it exits cleanly instead of hanging.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn scratch_dirs(label: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("myelind-it-{label}-{nanos}"));
    (root.join("data"), root.join("skills"))
}

const ALL_ELEVEN_TOOLS: &[&str] = &[
    "record_observation",
    "list_warmup_queue",
    "list_skills",
    "promote_skill",
    "record_skill_feedback",
    "mark_skill_used",
    "list_pending_review",
    "dismiss_pending_review",
    "archive_skill",
    "restore_skill",
    "render_skill_graph",
];

#[test]
fn initialize_and_tools_list_match_the_real_dispatch() {
    let (data_dir, skills_dir) = scratch_dirs("init-list");
    let mut session = McpSession::start(&data_dir, &skills_dir);

    let init = session.call(1, "initialize", json!({}));
    assert_eq!(init["result"]["serverInfo"]["name"], "myelin");

    // No [tools] config present -> default "standard" preset (7 of 11).
    let list = session.call(2, "tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names.len(),
        7,
        "default preset should be the 7-tool 'standard' set"
    );
    for expected in [
        "record_observation",
        "list_skills",
        "list_warmup_queue",
        "record_skill_feedback",
        "mark_skill_used",
        "list_pending_review",
        "dismiss_pending_review",
    ] {
        assert!(names.contains(&expected), "missing tool: {expected}");
    }
    for curation_only in [
        "promote_skill",
        "archive_skill",
        "restore_skill",
        "render_skill_graph",
    ] {
        assert!(
            !names.contains(&curation_only),
            "{curation_only} should not be in the default 'standard' preset"
        );
    }
}

#[test]
fn tools_list_with_full_preset_config_returns_all_eleven() {
    let (data_dir, skills_dir) = scratch_dirs("init-list-full");
    let config_dir = std::env::temp_dir().join(format!(
        "myelind-it-config-full-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        "[tools]\npreset = \"full\"\n",
    )
    .unwrap();

    let mut session = McpSession::start_with_config(&data_dir, &skills_dir, Some(&config_dir));
    let list = session.call(1, "tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names.len(), 11);
    for expected in ALL_ELEVEN_TOOLS {
        assert!(names.contains(expected), "missing tool: {expected}");
    }
}

#[test]
fn full_loop_over_the_real_protocol_promotes_and_accepts_feedback() {
    let (data_dir, skills_dir) = scratch_dirs("full-loop");
    let mut session = McpSession::start(&data_dir, &skills_dir);

    let title = "apply db migration hotfix";
    let summary = "run migrate.sh then restart service then verify health";

    session.tool(
        1,
        "record_observation",
        json!({ "title": title, "summary": summary }),
    );
    session.tool(
        2,
        "record_observation",
        json!({ "title": title, "summary": summary }),
    );
    let third = session.tool(
        3,
        "record_observation",
        json!({ "title": title, "summary": summary }),
    );

    assert_eq!(third["promoted"], true);
    let skill_path = third["skill_path"].as_str().unwrap().to_string();
    assert!(std::path::Path::new(&skill_path).exists());

    let skills = session.tool(4, "list_skills", json!({}));
    let skill_id = skills["skills"][0]["id"].as_i64().unwrap();

    let feedback = session.tool(
        5,
        "record_skill_feedback",
        json!({ "skill_id": skill_id, "kind": "correction", "note": "also check downstream caches" }),
    );
    assert_eq!(feedback["correction_count"], 1);

    let content = std::fs::read_to_string(&skill_path).unwrap();
    assert!(content.contains("also check downstream caches"));

    let queue = session.tool(6, "list_warmup_queue", json!({}));
    assert_eq!(queue["candidates"].as_array().unwrap().len(), 0);

    // Archive/restore round trip, over the same protocol connection.
    let archived = session.tool(7, "archive_skill", json!({ "skill_id": skill_id }));
    assert_eq!(archived["archived"], true);
    let archived_path = archived["path"].as_str().unwrap();
    assert!(!std::path::Path::new(&skill_path).exists());
    assert!(std::path::Path::new(archived_path).exists());

    let skills_after_archive = session.tool(8, "list_skills", json!({}));
    assert_eq!(skills_after_archive["skills"][0]["status"], "archived");

    let restored = session.tool(9, "restore_skill", json!({ "skill_id": skill_id }));
    assert_eq!(restored["restored"], true);
    assert_eq!(restored["path"], skill_path);
    assert!(std::path::Path::new(&skill_path).exists());

    // Graphviz is a Recommends, not a Depends - CI runners may not have
    // it, so this tolerates both outcomes rather than assuming either.
    let graph = session.tool(10, "render_skill_graph", json!({ "skill_id": skill_id }));
    match graph["rendered"].as_bool() {
        Some(true) => {
            let path = graph["path"].as_str().unwrap();
            assert!(std::path::Path::new(path).exists());
        }
        Some(false) => {
            let dot = graph["dot"].as_str().unwrap();
            assert!(dot.starts_with("digraph"));
        }
        _ => panic!("render_skill_graph response missing a boolean 'rendered' field"),
    }
}

#[test]
fn pending_review_can_be_listed_and_dismissed_over_the_protocol() {
    let (data_dir, skills_dir) = scratch_dirs("pending-review");
    let db_path = data_dir.join("myelin.db");

    // Stage a candidate directly, as `myelin ingest-session` (a separate
    // CLI binary, not exercised here) would - this proves the MCP tools
    // read/write the same underlying store, not just their own path.
    {
        let store =
            myelin_index::Store::open(&db_path, myelin_index::StoreConfig::default()).unwrap();
        store
            .stage_pending_review(
                "sess-1",
                Some("myelin"),
                "multi-step-sequence",
                "redacted excerpt",
            )
            .unwrap();
    }

    let mut session = McpSession::start(&data_dir, &skills_dir);
    let listed = session.tool(1, "list_pending_review", json!({}));
    let pending = listed["pending"].as_array().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["heuristic_reason"], "multi-step-sequence");
    let id = pending[0]["id"].as_i64().unwrap();

    let dismissed = session.tool(2, "dismiss_pending_review", json!({ "id": id }));
    assert_eq!(dismissed["dismissed"], true);

    let listed_again = session.tool(3, "list_pending_review", json!({}));
    assert_eq!(listed_again["pending"].as_array().unwrap().len(), 0);
}

#[test]
fn unknown_tool_and_unknown_method_return_json_rpc_errors_not_crashes() {
    let (data_dir, skills_dir) = scratch_dirs("errors");
    let mut session = McpSession::start(&data_dir, &skills_dir);

    let bad_method = session.call(1, "not/a/real/method", json!({}));
    assert!(bad_method.get("error").is_some());

    let bad_tool = session.call(
        2,
        "tools/call",
        json!({ "name": "does_not_exist", "arguments": {} }),
    );
    assert!(bad_tool.get("error").is_some());
}
