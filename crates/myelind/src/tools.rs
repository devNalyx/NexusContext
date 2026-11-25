use myelin_index::{NewObservation, Store, StoreConfig};
use serde_json::{json, Value};

/// Hard ceiling on any caller-supplied `limit`, independent of each tool's
/// own default - a single bad call can't request an unbounded response
/// regardless of what it asked for. See change_proposal.md.
const SERVER_MAX_LIMIT: i64 = 200;

fn clamp_limit(requested: i64) -> i64 {
    requested.min(SERVER_MAX_LIMIT)
}

fn open_store() -> anyhow::Result<Store> {
    let paths = myelin_core::Paths::resolve();
    let config = myelin_core::Config::load(&paths.config_file())?;
    Store::open(
        &paths.db_file(),
        StoreConfig {
            promotion_reps: config.promotion.reps,
            similarity_threshold: config.promotion.similarity_threshold,
            stale_after_secs: config.atrophy.stale_after_secs,
            max_active_skills: config.pruning.max_active_skills,
        },
    )
}

fn skills_dir() -> std::path::PathBuf {
    myelin_core::Paths::resolve().skills_dir()
}

pub fn tool_definitions() -> Value {
    json!([
        {
            "name": "record_observation",
            "description": "Record a noteworthy procedure you just performed - see README.md for when to call this. Similar observations accumulate reps and auto-promote into a real Skill, or promote immediately if high_stakes.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "Short name for the procedure, e.g. 'apply DB migration hotfix across services'." },
                    "summary": { "type": "string", "description": "The goal and the steps taken, generalized away from this specific repo's particulars." },
                    "project": { "type": "string", "description": "Optional: which project/repo this was observed in." },
                    "context_signal": { "type": "string", "description": "Optional: why this looks reusable beyond this one instance, e.g. a ticket saying it needs to be applied fleet-wide." },
                    "high_stakes": { "type": "boolean", "description": "Set true to fast-track promotion off a single observation, when context_signal makes the future reuse obvious." }
                },
                "required": ["title", "summary"]
            }
        },
        {
            "name": "list_warmup_queue",
            "description": "List skill candidates still accumulating reps (not yet promoted to a real skill), with their rep counts.",
            "inputSchema": {
                "type": "object",
                "properties": { "limit": { "type": "integer", "default": 50 } }
            }
        },
        {
            "name": "list_skills",
            "description": "List skills that have been promoted (auto or manual), with provenance. Each includes a `stale` flag (informational - no automatic effect except feeding the max_active_skills pruning cap, see README.md).",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "promote_skill",
            "description": "Force-promote a warmup-queue candidate into a real skill immediately, bypassing the reps/high-stakes gate.",
            "inputSchema": {
                "type": "object",
                "properties": { "candidate_id": { "type": "integer" } },
                "required": ["candidate_id"]
            }
        },
        {
            "name": "record_skill_feedback",
            "description": "Report feedback on a promoted skill after using it. kind='correction' appends your note into the live SKILL.md; kind='confirmation' just logs. See README.md for when to call this.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "skill_id": { "type": "integer", "description": "From list_skills." },
                    "kind": { "type": "string", "enum": ["correction", "confirmation"] },
                    "note": { "type": "string", "description": "For a correction: what was wrong and what actually worked. For a confirmation: brief context on what was done." }
                },
                "required": ["skill_id", "kind", "note"]
            }
        },
        {
            "name": "mark_skill_used",
            "description": "Record that a promoted skill was just followed/invoked, independent of whether you also have feedback to give. Call it whenever you use an existing skill, even if it worked perfectly.",
            "inputSchema": {
                "type": "object",
                "properties": { "skill_id": { "type": "integer", "description": "From list_skills." } },
                "required": ["skill_id"]
            }
        },
        {
            "name": "list_pending_review",
            "description": "List redacted, heuristically-flagged excerpts staged automatically from past session transcripts (via a SessionEnd hook) - candidates that MIGHT be worth an observation, not yet judged by any agent. Review each one: if it's genuinely worth capturing, call record_observation yourself based on it, then dismiss_pending_review it. If it's not worth it, just dismiss it.",
            "inputSchema": {
                "type": "object",
                "properties": { "limit": { "type": "integer", "default": 50 } }
            }
        },
        {
            "name": "dismiss_pending_review",
            "description": "Clear a staged item from the pending-review queue, whether or not you turned it into an observation. Keeps the queue from accumulating things already handled.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "integer", "description": "From list_pending_review." } },
                "required": ["id"]
            }
        },
        {
            "name": "archive_skill",
            "description": "Move a skill's SKILL.md out of the live skills directory so it stops being loadable, without deleting it. Reversible via restore_skill. See README.md for when to call this vs. letting the pruning cap handle it.",
            "inputSchema": {
                "type": "object",
                "properties": { "skill_id": { "type": "integer", "description": "From list_skills." } },
                "required": ["skill_id"]
            }
        },
        {
            "name": "restore_skill",
            "description": "Reverse archive_skill - moves the file back into the live skills directory and marks it active again.",
            "inputSchema": {
                "type": "object",
                "properties": { "skill_id": { "type": "integer", "description": "From list_skills." } },
                "required": ["skill_id"]
            }
        },
        {
            "name": "render_skill_graph",
            "description": "Render one skill's bounded neighborhood - the candidate that produced it, the observations that backed it, any corrections/confirmations - as a PNG you can view directly (e.g. with your Read tool). Never the whole graph, always scoped to one skill. Falls back to returning the raw DOT source if Graphviz isn't installed.",
            "inputSchema": {
                "type": "object",
                "properties": { "skill_id": { "type": "integer", "description": "From list_skills." } },
                "required": ["skill_id"]
            }
        }
    ])
}

/// Source of truth for every tool `tool_definitions()` can return - kept in
/// sync via a drift-guard test below, so a 12th tool added to
/// `tool_definitions()` without also being added to a preset fails a test
/// instead of silently vanishing from every preset.
const ALL_TOOL_NAMES: &[&str] = &[
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

/// Pure consumption of already-promoted skills - no curation, no learning.
const MINIMAL_TOOLS: &[&str] = &["list_skills", "mark_skill_used"];

/// Rounds out `MINIMAL_TOOLS` with the core observe-and-review loop:
/// capturing new observations, reporting feedback, and working the
/// warmup/pending-review queues. Still no active curation (promote/
/// archive/restore/graph).
const STANDARD_EXTRA_TOOLS: &[&str] = &[
    "record_observation",
    "record_skill_feedback",
    "list_warmup_queue",
    "list_pending_review",
    "dismiss_pending_review",
];

/// Manual curation tools - force-promote, archive/restore, and the
/// graphviz-rendered neighborhood view. Opt-in via `preset = "full"` or an
/// explicit `enabled` list, not advertised by default.
const FULL_EXTRA_TOOLS: &[&str] = &[
    "promote_skill",
    "archive_skill",
    "restore_skill",
    "render_skill_graph",
];

fn resolved_tool_names(config: &myelin_core::Config) -> std::collections::HashSet<&'static str> {
    if let Some(explicit) = &config.tools.enabled {
        return ALL_TOOL_NAMES
            .iter()
            .copied()
            .filter(|name| explicit.iter().any(|e| e == name))
            .collect();
    }
    match config.tools.preset {
        myelin_core::ToolsPreset::Minimal => MINIMAL_TOOLS.iter().copied().collect(),
        myelin_core::ToolsPreset::Standard => MINIMAL_TOOLS
            .iter()
            .chain(STANDARD_EXTRA_TOOLS)
            .copied()
            .collect(),
        myelin_core::ToolsPreset::Full => MINIMAL_TOOLS
            .iter()
            .chain(STANDARD_EXTRA_TOOLS)
            .chain(FULL_EXTRA_TOOLS)
            .copied()
            .collect(),
    }
}

/// `tools/list`'s entry point - filters `tool_definitions()` down to the
/// resolved enabled-set so a session only pays the schema-token cost for
/// tools it can actually use. See change_proposal.md.
pub fn enabled_tool_definitions(config: &myelin_core::Config) -> Value {
    let enabled = resolved_tool_names(config);
    let all = tool_definitions();
    Value::Array(
        all.as_array()
            .expect("tool_definitions() always returns a JSON array")
            .iter()
            .filter(|t| t["name"].as_str().is_some_and(|n| enabled.contains(n)))
            .cloned()
            .collect(),
    )
}

pub fn call(params: Value) -> anyhow::Result<Value> {
    let name = params
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing tool name"))?;
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);

    match name {
        "record_observation" => {
            let store = open_store()?;
            let input = NewObservation {
                title: field_str(&args, "title")?,
                summary: field_str(&args, "summary")?,
                project: args
                    .get("project")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                context_signal: args
                    .get("context_signal")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                high_stakes: args
                    .get("high_stakes")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            };
            let result = store.record_observation(input, &skills_dir())?;
            for ev in &result.evicted_skills {
                tracing::info!(
                    skill_id = ev.skill_id,
                    slug = %ev.slug,
                    "auto-archived skill to stay under max_active_skills cap"
                );
            }
            Ok(serde_json::to_value(result)?)
        }
        "list_warmup_queue" => {
            let store = open_store()?;
            let limit = clamp_limit(args.get("limit").and_then(|v| v.as_i64()).unwrap_or(50));
            let candidates = store.list_candidates(limit)?;
            Ok(json!({ "candidates": candidates }))
        }
        "list_skills" => {
            let store = open_store()?;
            Ok(json!({ "skills": store.list_skills()? }))
        }
        "promote_skill" => {
            let store = open_store()?;
            let candidate_id = args
                .get("candidate_id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow::anyhow!("missing candidate_id"))?;
            let outcome = store.promote_candidate(candidate_id, &skills_dir())?;
            for ev in &outcome.evicted {
                tracing::info!(
                    skill_id = ev.skill_id,
                    slug = %ev.slug,
                    "auto-archived skill to stay under max_active_skills cap"
                );
            }
            Ok(json!({
                "promoted": true,
                "skill_path": outcome.path,
                "evicted_skills": outcome.evicted
            }))
        }
        "record_skill_feedback" => {
            let store = open_store()?;
            let skill_id = args
                .get("skill_id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow::anyhow!("missing skill_id"))?;
            let kind = field_str(&args, "kind")?;
            let note = field_str(&args, "note")?;
            let result = store.record_skill_feedback(skill_id, &kind, &note)?;
            Ok(serde_json::to_value(result)?)
        }
        "mark_skill_used" => {
            let store = open_store()?;
            let skill_id = args
                .get("skill_id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow::anyhow!("missing skill_id"))?;
            store.mark_skill_used(skill_id)?;
            Ok(json!({ "marked_used": true }))
        }
        "list_pending_review" => {
            let store = open_store()?;
            let limit = clamp_limit(args.get("limit").and_then(|v| v.as_i64()).unwrap_or(50));
            Ok(json!({ "pending": store.list_pending_review(limit)? }))
        }
        "dismiss_pending_review" => {
            let store = open_store()?;
            let id = args
                .get("id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow::anyhow!("missing id"))?;
            store.dismiss_pending_review(id)?;
            Ok(json!({ "dismissed": true }))
        }
        "archive_skill" => {
            let store = open_store()?;
            let skill_id = args
                .get("skill_id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow::anyhow!("missing skill_id"))?;
            let path = store.archive_skill(skill_id, &skills_dir())?;
            Ok(json!({ "archived": true, "path": path }))
        }
        "restore_skill" => {
            let store = open_store()?;
            let skill_id = args
                .get("skill_id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow::anyhow!("missing skill_id"))?;
            let path = store.restore_skill(skill_id, &skills_dir())?;
            Ok(json!({ "restored": true, "path": path }))
        }
        "render_skill_graph" => {
            let store = open_store()?;
            let skill_id = args
                .get("skill_id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow::anyhow!("missing skill_id"))?;
            let neighborhood = store.skill_neighborhood(skill_id)?;
            let dot = myelin_index::graph::to_dot(&neighborhood);

            let graphs_dir = myelin_core::Paths::resolve().data_dir.join("graphs");
            std::fs::create_dir_all(&graphs_dir)?;
            let output_path = graphs_dir.join(format!("skill-{skill_id}.png"));

            match myelin_index::graph::render_png(&dot, &output_path) {
                Ok(()) => Ok(json!({ "rendered": true, "path": output_path.to_string_lossy() })),
                Err(err) => Ok(json!({ "rendered": false, "error": err.to_string(), "dot": dot })),
            }
        }
        other => anyhow::bail!("unknown tool: {other}"),
    }
}

fn field_str(args: &Value, key: &str) -> anyhow::Result<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("missing required field: {key}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use myelin_core::{Config, ToolsConfig, ToolsPreset};

    fn tool_names(defs: &Value) -> std::collections::HashSet<String> {
        defs.as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn clamp_limit_passes_through_requests_at_or_below_the_max() {
        assert_eq!(clamp_limit(1), 1);
        assert_eq!(clamp_limit(SERVER_MAX_LIMIT), SERVER_MAX_LIMIT);
    }

    #[test]
    fn clamp_limit_caps_requests_above_the_max() {
        assert_eq!(clamp_limit(SERVER_MAX_LIMIT + 1), SERVER_MAX_LIMIT);
        assert_eq!(clamp_limit(1_000_000), SERVER_MAX_LIMIT);
    }

    #[test]
    fn full_preset_matches_all_tool_definitions() {
        let config = Config {
            tools: ToolsConfig {
                preset: ToolsPreset::Full,
                enabled: None,
            },
            ..Default::default()
        };
        let filtered = tool_names(&enabled_tool_definitions(&config));
        let all = tool_names(&tool_definitions());
        assert_eq!(filtered, all);
        assert_eq!(all.len(), ALL_TOOL_NAMES.len());
    }

    #[test]
    fn minimal_standard_extra_and_full_extra_partition_all_tool_names_exactly() {
        let reconstructed: std::collections::HashSet<_> = MINIMAL_TOOLS
            .iter()
            .chain(STANDARD_EXTRA_TOOLS)
            .chain(FULL_EXTRA_TOOLS)
            .copied()
            .collect();
        let all: std::collections::HashSet<_> = ALL_TOOL_NAMES.iter().copied().collect();
        assert_eq!(
            reconstructed, all,
            "a tool was added to tool_definitions() without being added to a preset, or vice versa"
        );
    }

    #[test]
    fn minimal_and_standard_presets_are_subsets_of_full() {
        let all: std::collections::HashSet<_> = ALL_TOOL_NAMES.iter().copied().collect();
        let minimal: std::collections::HashSet<_> = MINIMAL_TOOLS.iter().copied().collect();
        let standard: std::collections::HashSet<_> = MINIMAL_TOOLS
            .iter()
            .chain(STANDARD_EXTRA_TOOLS)
            .copied()
            .collect();
        assert!(minimal.is_subset(&standard));
        assert!(standard.is_subset(&all));
    }

    #[test]
    fn default_config_resolves_to_standard_seven_tools() {
        let config = Config::default();
        let filtered = tool_names(&enabled_tool_definitions(&config));
        assert_eq!(filtered.len(), 7);
        assert!(filtered.contains("record_observation"));
        assert!(!filtered.contains("promote_skill"));
        assert!(!filtered.contains("render_skill_graph"));
    }

    #[test]
    fn minimal_preset_resolves_to_exactly_two_tools() {
        let config = Config {
            tools: ToolsConfig {
                preset: ToolsPreset::Minimal,
                enabled: None,
            },
            ..Default::default()
        };
        let filtered = tool_names(&enabled_tool_definitions(&config));
        assert_eq!(
            filtered,
            MINIMAL_TOOLS
                .iter()
                .map(|s| s.to_string())
                .collect::<std::collections::HashSet<_>>()
        );
    }

    #[test]
    fn explicit_enabled_list_overrides_preset() {
        let config = Config {
            tools: ToolsConfig {
                preset: ToolsPreset::Standard,
                enabled: Some(vec![
                    "archive_skill".to_string(),
                    "restore_skill".to_string(),
                ]),
            },
            ..Default::default()
        };
        let filtered = tool_names(&enabled_tool_definitions(&config));
        assert_eq!(filtered.len(), 2);
        assert!(filtered.contains("archive_skill"));
        assert!(filtered.contains("restore_skill"));
        assert!(!filtered.contains("list_skills"));
    }

    #[test]
    fn unknown_name_in_enabled_list_is_silently_dropped() {
        let config = Config {
            tools: ToolsConfig {
                preset: ToolsPreset::Standard,
                enabled: Some(vec![
                    "list_skills".to_string(),
                    "not_a_real_tool".to_string(),
                ]),
            },
            ..Default::default()
        };
        let filtered = tool_names(&enabled_tool_definitions(&config));
        assert_eq!(filtered.len(), 1);
        assert!(filtered.contains("list_skills"));
    }
}
