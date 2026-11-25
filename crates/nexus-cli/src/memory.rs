//! `nexus memory …` — the human/CLI side of curated memory and the one-release
//! front end for the absorbed Myelin skill tools. Deliberately a separate
//! binary surface from the MCP catalog: reviewing is a human act, not a tool
//! an agent should be able to invoke on itself.

use anyhow::{anyhow, bail, Result};
use clap::{Args, Subcommand};
use nexus_core::{project_hash, Config, Paths};
use nexus_memory::fact::{Anchor, Fact, FactKind, Source};
use nexus_memory::{okf, recall, recall_with_lower_trust, trust_tier, Store};
use serde_json::json;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct Common {
    /// Repository whose curated memory is being operated on.
    #[arg(long, default_value = ".")]
    project: PathBuf,
}

#[derive(Subcommand)]
pub enum Command {
    /// Stage a code-anchored fact (unverified until a human approves it).
    Add {
        #[command(flatten)]
        common: Common,
        /// Decision, Incident, Lesson, Dead End or Owner.
        kind: String,
        title: String,
        /// Fact text. Redaction is applied before storage.
        #[arg(long)]
        body: String,
        /// Repo-relative anchored file. Repeat for multiple anchors.
        #[arg(long = "anchor")]
        anchors: Vec<String>,
        /// Optional anchor symbol (paired with the first anchor).
        #[arg(long)]
        symbol: Option<String>,
        /// Optional RFC 3339 instant after which this fact must be re-checked.
        #[arg(long)]
        stale_after: Option<String>,
        /// Who produced the staging record (default: agent/cli).
        #[arg(long, default_value = "agent/cli")]
        actor: String,
    },
    /// Review queue: all facts with trust, staleness and lifecycle labels.
    List {
        #[command(flatten)]
        common: Common,
    },
    /// Show one fact in full.
    Show {
        #[command(flatten)]
        common: Common,
        id: String,
    },
    /// Human approval: only a `human:` actor can make a fact injectable.
    Approve {
        #[command(flatten)]
        common: Common,
        id: String,
        #[arg(long)]
        actor: String,
    },
    /// Reject a fact and clear any earlier approval.
    Reject {
        #[command(flatten)]
        common: Common,
        id: String,
        #[arg(long)]
        actor: String,
    },
    /// Budgeted recall of verified, fresh facts.
    Recall {
        #[command(flatten)]
        common: Common,
        query: String,
        #[arg(long, default_value_t = 800)]
        budget: u64,
        /// Deliberately include labelled machine-confirmed/unverified facts.
        #[arg(long)]
        lower_trust: bool,
    },
    /// Mark facts anchored to a file as stale (manual invalidation).
    Stale {
        #[command(flatten)]
        common: Common,
        file: String,
    },
    /// Trust/staleness audit summary for a repo.
    Audit {
        #[command(flatten)]
        common: Common,
    },
    /// Export an OKF v0.3 bundle to `<project>/.nexus/memory/`.
    Export {
        #[command(flatten)]
        common: Common,
    },
    /// Import an OKF v0.3 bundle into the canonical SQLite store.
    Import {
        #[command(flatten)]
        common: Common,
        #[arg(long)]
        bundle: Option<PathBuf>,
    },
    /// List OKF bundle roots and their one-line routing descriptions.
    Bundles {
        #[command(flatten)]
        common: Common,
        /// Parent directory to scan. Defaults to the project's parent.
        #[arg(long)]
        root: Option<PathBuf>,
    },
    /// List promoted procedural skills (absorbed Myelin).
    Skills {
        #[command(flatten)]
        common: Common,
    },
    /// List promotable skill candidates and staged review excerpts, labelled.
    SkillQueue {
        #[command(flatten)]
        common: Common,
    },
    /// Force-promote a reviewed skill candidate.
    SkillPromote {
        #[command(flatten)]
        common: Common,
        /// Numeric candidate id from `skill-queue`.
        id: i64,
    },
}

fn config() -> Config {
    Config::load(&Paths::resolve().config_file()).unwrap_or_default()
}

fn open(project: &Path, cfg: &Config) -> Result<(Store, String)> {
    if !cfg.is_path_allowed(project) {
        bail!("project path is outside allowed_roots");
    }
    let repo = project.to_string_lossy().to_string();
    let store = Store::open(&Paths::resolve().memory_db_file(&project_hash(project)))?;
    Ok((store, repo))
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn print_fact(f: &Fact, unix: i64) {
    println!(
        "{} [{}] {} — {}{}",
        f.id,
        f.kind.as_okf_type(),
        f.title,
        trust_tier(f).label(),
        if f.is_stale(unix) { " STALE" } else { "" }
    );
    if !f.anchors.is_empty() {
        println!(
            "  anchors: {}",
            f.anchors
                .iter()
                .map(|a| a.path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some(reason) = &f.superseded_by {
        println!("  superseded by: {reason}");
    }
    println!("  {}", f.body.replace('\n', "\n  "));
}

fn myelin_call(name: &str, args: serde_json::Value) -> Result<()> {
    let value = myelind::tools::call(json!({ "name": name, "arguments": args }))?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

pub fn run(command: Command) -> Result<()> {
    let cfg = config();
    let unix = now();
    match command {
        Command::Add {
            common,
            kind,
            title,
            body,
            anchors,
            symbol,
            stale_after,
            actor,
        } => {
            let (mut store, repo) = open(&common.project, &cfg)?;
            let Some(kind) = FactKind::from_okf_type(&kind) else {
                bail!("unknown fact kind: {kind}");
            };
            if anchors.is_empty() {
                bail!("at least one --anchor is required: curated facts must be code-anchored");
            }
            let anchors: Vec<Anchor> = anchors
                .into_iter()
                .enumerate()
                .map(|(i, path)| Anchor {
                    path,
                    symbol: if i == 0 { symbol.clone() } else { None },
                    line: None,
                    hash: None,
                })
                .collect();
            let stale_after = match stale_after {
                Some(raw) => Some(okf::parse_instant(&raw)?),
                None => None,
            };
            let id = format!("f{:016x}", {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                repo.hash(&mut h);
                title.hash(&mut h);
                for a in &anchors {
                    a.path.hash(&mut h);
                }
                h.finish()
            });
            let fact = Fact {
                id: id.clone(),
                repo_path: repo,
                kind,
                title,
                body,
                status: nexus_memory::FactStatus::Draft,
                superseded_by: None,
                anchors,
                sources: Vec::<Source>::new(),
                generated_by: actor,
                generated_at: unix,
                verified_by: None,
                verified_at: None,
                stale_after,
                anchor_stale: false,
            };
            store.put(&fact)?;
            println!("staged {id} (unverified) — approve with: nexus memory approve {id} --actor human:<name>");
        }
        Command::List { common } => {
            let (store, repo) = open(&common.project, &cfg)?;
            let facts = store.list(&repo)?;
            if facts.is_empty() {
                println!("(no curated facts)");
            }
            for f in &facts {
                print_fact(f, unix);
                println!();
            }
        }
        Command::Show { common, id } => {
            let (store, _) = open(&common.project, &cfg)?;
            let Some(fact) = store.get(&id)? else {
                bail!("no such fact: {id}");
            };
            print_fact(&fact, unix);
        }
        Command::Approve { common, id, actor } => {
            let (mut store, _) = open(&common.project, &cfg)?;
            if !actor.starts_with("human:") || actor.len() <= "human:".len() {
                bail!("only a real 'human:<name>' actor can approve a fact");
            }
            store.record_review(&id, &actor, "approve", unix)?;
            let fact = store
                .get(&id)?
                .ok_or_else(|| anyhow!("no such fact: {id}"))?;
            println!("approved {id} — trust: {}", trust_tier(&fact).label());
        }
        Command::Reject { common, id, actor } => {
            let (mut store, _) = open(&common.project, &cfg)?;
            store.record_review(&id, &actor, "reject", unix)?;
            println!("rejected {id} — any earlier verification was cleared");
        }
        Command::Recall {
            common,
            query,
            budget,
            lower_trust,
        } => {
            let (store, repo) = open(&common.project, &cfg)?;
            let budget = budget.min(nexus_memory::DEFAULT_INJECTION_BUDGET_TOKENS);
            let result = if lower_trust {
                recall_with_lower_trust(&store, &cfg, &repo, &query, budget, unix)?
            } else {
                recall(&store, &cfg, &repo, &query, budget, unix)?
            };
            println!("{}", serde_json::to_string_pretty(&result.to_json())?);
        }
        Command::Stale { common, file } => {
            let (store, repo) = open(&common.project, &cfg)?;
            let n = store.mark_anchors_stale(&repo, &file)?;
            println!("{n} fact(s) anchored to {file} marked stale");
        }
        Command::Audit { common } => {
            let (store, repo) = open(&common.project, &cfg)?;
            let facts = store.list(&repo)?;
            let mut by_trust = std::collections::BTreeMap::new();
            let mut stale = 0usize;
            for f in &facts {
                *by_trust.entry(trust_tier(f).label()).or_insert(0usize) += 1;
                if f.is_stale(unix) {
                    stale += 1;
                }
            }
            println!("facts: {}", facts.len());
            for (tier, count) in by_trust {
                println!("  {tier}: {count}");
            }
            println!("  stale: {stale}");
            println!(
                "  injectable: {}",
                facts
                    .iter()
                    .filter(|f| { nexus_memory::admit(f, &cfg, unix).include })
                    .count()
            );
        }
        Command::Export { common } => {
            let (store, _) = open(&common.project, &cfg)?;
            let facts = store.list(&common.project.to_string_lossy())?;
            let dir = okf::export_bundle(&common.project, &facts)?;
            println!(
                "exported {} fact(s) to {} (OKF v{})",
                facts.len(),
                dir.display(),
                nexus_memory::OKF_VERSION
            );
        }
        Command::Bundles { common, root } => {
            let root = root.unwrap_or_else(|| {
                common
                    .project
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."))
                    .to_path_buf()
            });
            let bundles = nexus_memory::discover_bundles(&root, &common.project)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "root": root,
                    "active_project": common.project,
                    "bundles": bundles,
                    "note": "index.md descriptions are routing metadata only; no fact bodies were read and no cross-repo memory was loaded",
                }))?
            );
        }
        Command::Import { common, bundle } => {
            let (mut store, repo) = open(&common.project, &cfg)?;
            let dir = bundle.unwrap_or_else(|| okf::bundle_dir(&common.project));
            let incoming = okf::import_bundle(&dir)?;
            let mut count = 0usize;
            for mut f in incoming {
                if f.repo_path != repo {
                    f.repo_path = repo.clone();
                }
                if let Some(existing) = store.get(&f.id)? {
                    if existing.anchor_stale {
                        f.anchor_stale = true;
                    }
                    if trust_tier(&existing) == nexus_memory::TrustTier::HumanReviewed
                        && trust_tier(&f) != nexus_memory::TrustTier::HumanReviewed
                    {
                        f.verified_by = existing.verified_by;
                        f.verified_at = existing.verified_at;
                    }
                }
                store.put(&f)?;
                count += 1;
            }
            println!("imported {count} OKF concept(s) into canonical SQLite");
        }
        Command::Skills { common } => {
            myelin_call(
                "list_skills",
                json!({ "repo_path": common.project.to_string_lossy() }),
            )?;
        }
        Command::SkillQueue { common } => {
            // Two distinct id spaces: warming *skill candidates* (promotable)
            // and staged *review excerpts* from a harness SessionEnd hook
            // (not promotable until judgment turns one into an observation).
            // Show both with explicit labels so an id is never ambiguous.
            let repo = common.project.to_string_lossy();
            let candidates = myelind::tools::call(json!({
                "name": "list_warmup_queue",
                "arguments": { "repo_path": repo, "limit": 200 },
            }))?;
            let pending = myelind::tools::call(json!({
                "name": "list_pending_review",
                "arguments": { "repo_path": repo, "limit": 200 },
            }))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "skill_candidates_promotable": candidates,
                    "pending_review_excerpts": pending,
                    "note": "use `skill-promote <id>` only with an id from skill_candidates_promotable; a pending excerpt must first become an observation via the agent",
                }))?
            );
        }
        Command::SkillPromote { common, id } => {
            myelin_call(
                "promote_skill",
                json!({ "repo_path": common.project.to_string_lossy(), "candidate_id": id }),
            )?;
        }
    }
    Ok(())
}
