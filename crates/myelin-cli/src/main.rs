mod install;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use myelin_index::{NewObservation, Store, StoreConfig};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "myelin", about = "Myelin CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Ping the running myelind daemon over its control socket.
    Status,
    /// Auto-detect MCP-capable agents on this machine, register `myelind
    /// mcp` using this machine's actual binary paths, and wire up the
    /// SessionEnd ingestion hook in ~/.claude/settings.json.
    Install,
    /// Record an observation directly (bypasses the daemon — for debugging
    /// the same store the MCP `record_observation` tool writes to).
    Observe {
        #[arg(long)]
        title: String,
        #[arg(long)]
        summary: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        context_signal: Option<String>,
        #[arg(long)]
        high_stakes: bool,
    },
    /// List warmup-queue candidates (not yet promoted).
    Queue,
    /// List promoted skills.
    Skills,
    /// Force-promote a candidate now.
    Promote { candidate_id: i64 },
    /// Record feedback on a promoted skill (correction appends into the
    /// live SKILL.md; confirmation just logs).
    Feedback {
        skill_id: i64,
        #[arg(long, value_parser = ["correction", "confirmation"])]
        kind: String,
        #[arg(long)]
        note: String,
    },
    /// Mark a skill as just used (what the `stale` flag is judged against).
    MarkUsed { skill_id: i64 },
    /// Meant to be invoked by a Claude Code SessionEnd hook, fed the
    /// hook's JSON payload on stdin. Redacts and heuristically stages
    /// candidates from the session transcript for later review - never
    /// fails loudly, since a hook has no decision control anyway.
    IngestSession,
    /// List staged candidates awaiting review (from ingest-session).
    PendingReview,
    /// Clear a staged candidate - whether or not it was acted on.
    DismissReview { id: i64 },
    /// Move a skill's SKILL.md out of the live skills directory - it
    /// stops being loadable, but nothing is deleted. Explicit only.
    ArchiveSkill { skill_id: i64 },
    /// Reverse archive-skill.
    RestoreSkill { skill_id: i64 },
    /// Render one skill's bounded neighborhood (its candidate, backing
    /// observations, corrections/confirmations) as a PNG via Graphviz.
    /// Falls back to writing the .dot source if `dot` isn't installed.
    Graph {
        skill_id: i64,
        #[arg(long)]
        output: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Status => status(),
        Command::Install => install::run(),
        Command::Observe {
            title,
            summary,
            project,
            context_signal,
            high_stakes,
        } => observe(title, summary, project, context_signal, high_stakes),
        Command::Queue => queue(),
        Command::Skills => skills(),
        Command::Promote { candidate_id } => promote(candidate_id),
        Command::Feedback {
            skill_id,
            kind,
            note,
        } => feedback(skill_id, kind, note),
        Command::MarkUsed { skill_id } => mark_used(skill_id),
        Command::IngestSession => ingest_session(),
        Command::PendingReview => pending_review(),
        Command::DismissReview { id } => dismiss_review(id),
        Command::ArchiveSkill { skill_id } => archive_skill(skill_id),
        Command::RestoreSkill { skill_id } => restore_skill(skill_id),
        Command::Graph { skill_id, output } => graph(skill_id, output),
    }
}

fn open_store() -> Result<Store> {
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

fn observe(
    title: String,
    summary: String,
    project: Option<String>,
    context_signal: Option<String>,
    high_stakes: bool,
) -> Result<()> {
    let store = open_store()?;
    let skills_dir = myelin_core::Paths::resolve().skills_dir();
    let result = store.record_observation(
        NewObservation {
            title,
            summary,
            project,
            context_signal,
            high_stakes,
        },
        &skills_dir,
    )?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn queue() -> Result<()> {
    let store = open_store()?;
    let candidates = store.list_candidates(200)?;
    println!("{}", serde_json::to_string_pretty(&candidates)?);
    Ok(())
}

fn skills() -> Result<()> {
    let store = open_store()?;
    println!("{}", serde_json::to_string_pretty(&store.list_skills()?)?);
    Ok(())
}

fn promote(candidate_id: i64) -> Result<()> {
    let store = open_store()?;
    let skills_dir = myelin_core::Paths::resolve().skills_dir();
    let outcome = store.promote_candidate(candidate_id, &skills_dir)?;
    println!("promoted -> {}", outcome.path);
    if !outcome.evicted.is_empty() {
        let slugs: Vec<_> = outcome.evicted.iter().map(|e| e.slug.as_str()).collect();
        println!(
            "auto-archived {} skill(s) to stay under the cap: {}",
            outcome.evicted.len(),
            slugs.join(", ")
        );
    }
    Ok(())
}

fn feedback(skill_id: i64, kind: String, note: String) -> Result<()> {
    let store = open_store()?;
    let result = store.record_skill_feedback(skill_id, &kind, &note)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn mark_used(skill_id: i64) -> Result<()> {
    let store = open_store()?;
    store.mark_skill_used(skill_id)?;
    println!("marked used");
    Ok(())
}

/// Never returns an error - a SessionEnd hook has no decision control
/// anyway (per Claude Code's docs), so failing loudly here would only
/// ever surprise a user who isn't watching, never actually block or fix
/// anything. Best-effort at every step; silently does nothing on any
/// unexpected shape.
fn ingest_session() -> Result<()> {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return Ok(());
    }
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&input) else {
        return Ok(());
    };
    let Some(transcript_path) = payload.get("transcript_path").and_then(|v| v.as_str()) else {
        return Ok(());
    };
    let session_id = payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let project = payload.get("cwd").and_then(|v| v.as_str());

    let Ok(turns) = myelin_index::parse_transcript(std::path::Path::new(transcript_path)) else {
        return Ok(());
    };
    let candidates = myelin_index::stage_candidates(&turns);
    if candidates.is_empty() {
        return Ok(());
    }

    let Ok(store) = open_store() else {
        return Ok(());
    };
    for candidate in candidates {
        let _ = store.stage_pending_review(
            session_id,
            project,
            &candidate.heuristic_reason,
            &candidate.excerpt,
        );
    }
    Ok(())
}

fn pending_review() -> Result<()> {
    let store = open_store()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&store.list_pending_review(200)?)?
    );
    Ok(())
}

fn dismiss_review(id: i64) -> Result<()> {
    let store = open_store()?;
    store.dismiss_pending_review(id)?;
    println!("dismissed");
    Ok(())
}

fn archive_skill(skill_id: i64) -> Result<()> {
    let store = open_store()?;
    let skills_dir = myelin_core::Paths::resolve().skills_dir();
    let path = store.archive_skill(skill_id, &skills_dir)?;
    println!("archived -> {path}");
    Ok(())
}

fn restore_skill(skill_id: i64) -> Result<()> {
    let store = open_store()?;
    let skills_dir = myelin_core::Paths::resolve().skills_dir();
    let path = store.restore_skill(skill_id, &skills_dir)?;
    println!("restored -> {path}");
    Ok(())
}

fn graph(skill_id: i64, output: Option<PathBuf>) -> Result<()> {
    let store = open_store()?;
    let neighborhood = store.skill_neighborhood(skill_id)?;
    let dot = myelin_index::graph::to_dot(&neighborhood);
    let output = output.unwrap_or_else(|| PathBuf::from(format!("myelin-skill-{skill_id}.png")));

    match myelin_index::graph::render_png(&dot, &output) {
        Ok(()) => println!("rendered -> {}", output.display()),
        Err(err) => {
            let dot_path = output.with_extension("dot");
            std::fs::write(&dot_path, &dot)?;
            println!(
                "could not render PNG ({err}) - wrote DOT source instead -> {}",
                dot_path.display()
            );
        }
    }
    Ok(())
}

fn status() -> Result<()> {
    let paths = myelin_core::Paths::resolve();
    let socket_path = paths.control_socket();
    let mut stream = UnixStream::connect(&socket_path).with_context(|| {
        format!(
            "could not connect to {} — is `myelind serve` running?",
            socket_path.display()
        )
    })?;

    writeln!(stream, r#"{{"method":"status.get"}}"#)?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let value: serde_json::Value = serde_json::from_str(line.trim())?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
