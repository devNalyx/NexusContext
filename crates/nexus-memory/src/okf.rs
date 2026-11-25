//! OKF v0.3 producer/consumer for curated facts (ADR 0020).
//!
//! A bundle is a directory of Markdown with YAML frontmatter. Only `type` is
//! required by OKF; everything else is producer-defined. NC uses the standard
//! provenance/trust/lifecycle families and reserves `x-nexus-*` keys for its
//! own anchors and identity. Emitted links are standard Markdown links:
//! `[[wikilinks]]` are explicitly not OKF links (§6.1).

use crate::fact::{Anchor, Fact, FactKind, FactStatus, Source, OKF_VERSION};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use serde::{de, Deserialize, Deserializer, Serialize};
use serde_yaml::Value as Yaml;
use std::path::{Path, PathBuf};

pub fn bundle_dir(repo_path: &Path) -> PathBuf {
    repo_path.join(".nexus").join("memory")
}

/// One selectable bundle, as advertised by its root `index.md`. This is the
/// whole point of OKF's progressive disclosure: an agent reads a one-line
/// `description` to choose a bundle, then loads facts from only that repo.
#[derive(Debug, Clone, Serialize)]
pub struct BundleRoute {
    pub repo_path: String,
    pub bundle_dir: String,
    pub title: String,
    pub description: String,
    pub okf_version: Option<String>,
    pub fact_files: usize,
    pub active: bool,
}

fn yaml_text(value: &Yaml, key: &str) -> Option<String> {
    match value.get(Yaml::String(key.into()))? {
        Yaml::String(s) => Some(s.clone()),
        Yaml::Number(n) => Some(n.to_string()),
        Yaml::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Discover OKF bundles one directory below `root`, without reading any fact
/// bodies. The active repo is marked for the caller (usually the current
/// project) but no selection is made here: choosing is agent/human judgment,
/// and injection remains scoped to one repo under the same hard cap.
pub fn discover_bundles(root: &Path, active: &Path) -> Result<Vec<BundleRoute>> {
    let active_canon = active
        .canonicalize()
        .unwrap_or_else(|_| active.to_path_buf());
    let mut candidates = Vec::new();

    let root_canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let root_bundle = bundle_dir(&root_canon);
    if root_bundle.join("index.md").is_file() {
        candidates.push((root_canon.clone(), root_bundle));
    }
    if let Ok(entries) = std::fs::read_dir(&root_canon) {
        for entry in entries.flatten() {
            let child = entry.path();
            let dir = bundle_dir(&child);
            if dir.join("index.md").is_file() {
                candidates.push((child, dir));
            }
        }
    }

    let mut routes = Vec::new();
    for (repo, dir) in candidates {
        let text = std::fs::read_to_string(dir.join("index.md"))?;
        let (front, _) = split_frontmatter(&text).unwrap_or(("", text.as_str()));
        let meta: Yaml = serde_yaml::from_str(front).unwrap_or(Yaml::Null);
        let declared_repo = yaml_text(&meta, "x-nexus-repo");
        let repo_display = declared_repo
            .clone()
            .unwrap_or_else(|| repo.to_string_lossy().to_string());
        let fact_files = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.ends_with(".md")
                    && name != "index.md"
                    && name != "log.md"
                    && !name.starts_with('.')
            })
            .count();
        let repo_canon = repo.canonicalize().unwrap_or(repo.clone());
        routes.push(BundleRoute {
            repo_path: repo_display,
            bundle_dir: dir.to_string_lossy().to_string(),
            title: yaml_text(&meta, "title")
                .unwrap_or_else(|| "Untitled NexusContext bundle".into()),
            description: yaml_text(&meta, "description").unwrap_or_default(),
            okf_version: yaml_text(&meta, "okf_version"),
            fact_files,
            active: repo_canon == active_canon
                || declared_repo.as_deref().is_some_and(|p| {
                    Path::new(p)
                        .canonicalize()
                        .unwrap_or_else(|_| PathBuf::from(p))
                        == active_canon
                }),
        });
    }
    routes.sort_by(|a, b| a.repo_path.cmp(&b.repo_path));
    Ok(routes)
}

/// Parse an OKF instant (RFC 3339, or date-only meaning UTC midnight).
/// Exposed so the CLI and MCP layers can accept the same timestamp shape
/// the bundle writer emits, instead of inventing a second date dialect.
pub fn parse_instant(raw: &str) -> Result<i64> {
    let trimmed = raw.trim();
    // OKF §5: a date-only value means the start of that day in UTC.
    let normalised = if trimmed.len() == 10 && trimmed.as_bytes().get(4) == Some(&b'-') {
        format!("{trimmed}T00:00:00Z")
    } else {
        trimmed.to_string()
    };
    Ok(DateTime::parse_from_rfc3339(&normalised)
        .with_context(|| format!("timestamp is not RFC 3339: {raw}"))?
        .timestamp())
}

fn format_instant(secs: i64) -> String {
    Utc.timestamp_opt(secs, 0)
        .single()
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).single().unwrap())
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// OKF §11.2: consumers accept one item where a list is declared.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

// Manual impl: deriving would require `T: Default`, but the meaningful
// absent value is always an empty list regardless of `T`.
impl<T> Default for OneOrMany<T> {
    fn default() -> Self {
        Self::Many(Vec::new())
    }
}

impl<T> OneOrMany<T> {
    fn into_vec(self) -> Vec<T> {
        match self {
            Self::One(v) => vec![v],
            Self::Many(v) => v,
        }
    }
}

#[derive(Debug, Deserialize)]
struct GeneratedIn {
    by: String,
    #[serde(default, deserialize_with = "de_instant")]
    at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct VerifiedIn {
    by: String,
    #[serde(default, deserialize_with = "de_instant")]
    at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct SourceIn {
    #[serde(default)]
    id: String,
    resource: String,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnchorIn {
    path: String,
    #[serde(default)]
    symbol: Option<String>,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default)]
    hash: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
enum StaleAfter {
    #[default]
    Absent,
    At(i64),
    /// OKF §5.5: an unparseable value must never be treated as fresh.
    Malformed,
}

fn de_instant<'de, D>(deserializer: D) -> std::result::Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    match Yaml::deserialize(deserializer)? {
        Yaml::Null => Ok(None),
        Yaml::String(raw) => parse_instant(&raw).map(Some).map_err(de::Error::custom),
        _ => Err(de::Error::custom("timestamp must be a string or null")),
    }
}

fn de_stale_after<'de, D>(deserializer: D) -> std::result::Result<StaleAfter, D::Error>
where
    D: Deserializer<'de>,
{
    match Yaml::deserialize(deserializer)? {
        Yaml::Null => Ok(StaleAfter::Absent),
        Yaml::String(raw) => Ok(match parse_instant(&raw) {
            Ok(instant) => StaleAfter::At(instant),
            Err(_) => StaleAfter::Malformed,
        }),
        _ => Ok(StaleAfter::Malformed),
    }
}

#[derive(Debug, Deserialize)]
struct ConceptIn {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    superseded_by: Option<String>,
    #[serde(default, rename = "x-nexus-id")]
    id: Option<String>,
    #[serde(default, rename = "x-nexus-repo")]
    repo: Option<String>,
    #[serde(default, rename = "x-nexus-anchors")]
    anchors: OneOrMany<AnchorIn>,
    #[serde(default)]
    sources: OneOrMany<SourceIn>,
    #[serde(default)]
    generated: Option<GeneratedIn>,
    #[serde(default)]
    verified: Option<OneOrMany<VerifiedIn>>,
    #[serde(default, deserialize_with = "de_stale_after")]
    stale_after: StaleAfter,
}

fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let mut lines = text.split_inclusive('\n');
    let first = lines.next()?;
    if first.trim_end() != "---" {
        return None;
    }
    let mut consumed = first.len();
    for line in lines.by_ref() {
        consumed += line.len();
        if line.trim_end() == "---" {
            return Some((&text[first.len()..consumed - line.len()], &text[consumed..]));
        }
    }
    None
}

/// Import one OKF concept. `None` means "not a concept we own": no `type`, an
/// unknown `type`, or a malformed frontmatter block. OKF §11 requires those
/// to be tolerated, never rejected as bundle-invalid.
fn import_concept(path: &Path, text: &str, fallback_repo: &str) -> Result<Option<Fact>> {
    let Some((front, body)) = split_frontmatter(text) else {
        return Ok(None);
    };
    let parsed: ConceptIn = match serde_yaml::from_str(front) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let Some(kind) = FactKind::from_okf_type(&parsed.kind) else {
        return Ok(None);
    };

    let id = parsed.id.unwrap_or_else(|| {
        path.to_string_lossy()
            .trim_end_matches(".md")
            .replace(['/', '\\'], "_")
    });
    let title = parsed.title.unwrap_or_else(|| id.clone());
    let generated = parsed.generated;
    let generated_at = generated.as_ref().and_then(|g| g.at).unwrap_or(0);
    let generated_by = generated.map(|g| g.by).unwrap_or_else(|| "unknown".into());

    let verified = parsed.verified.map(OneOrMany::into_vec).unwrap_or_default();
    // OKF §5.3: only current events count; the most recent current one wins.
    let current = verified
        .into_iter()
        .filter(|v| v.at.is_some_and(|at| at >= generated_at))
        .max_by_key(|v| v.at.unwrap_or(0));

    let stale_after = match parsed.stale_after {
        StaleAfter::At(instant) => Some(instant),
        // A malformed value can never be fresh. Zero is in the past.
        StaleAfter::Malformed => Some(0),
        StaleAfter::Absent => None,
    };

    Ok(Some(Fact {
        id,
        repo_path: parsed.repo.unwrap_or_else(|| fallback_repo.to_string()),
        kind,
        title,
        body: body.trim_start_matches('\n').to_string(),
        status: FactStatus::from_okf(parsed.status.as_deref().unwrap_or("stable")),
        superseded_by: parsed.superseded_by,
        anchors: parsed
            .anchors
            .into_vec()
            .into_iter()
            .map(|a| Anchor {
                path: a.path,
                symbol: a.symbol,
                line: a.line,
                hash: a.hash,
            })
            .collect(),
        sources: parsed
            .sources
            .into_vec()
            .into_iter()
            .map(|s| Source {
                id: s.id,
                resource: s.resource,
                title: s.title,
            })
            .collect(),
        generated_by,
        generated_at,
        verified_by: current.as_ref().map(|v| v.by.clone()),
        verified_at: current.and_then(|v| v.at),
        stale_after,
        // An imported bundle cannot override SQLite's watcher truth. The
        // caller merges into the canonical store, which keeps its own flag.
        anchor_stale: false,
    }))
}

/// Read every concept in a bundle. `index.md`/`log.md` and dot-files are
/// skipped per OKF §3.1/§3.2; broken links are never followed.
pub fn import_bundle(root: &Path) -> Result<Vec<Fact>> {
    if !root.is_dir() {
        bail!("OKF bundle not found: {}", root.display());
    }
    let fallback_repo = root
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(root)
        .to_string_lossy()
        .to_string();
    let mut facts = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(root)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            path.extension().and_then(|e| e.to_str()) == Some("md")
                && !path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n == "index.md" || n == "log.md" || n.starts_with('.'))
        })
        .collect();
    entries.sort();
    for path in entries {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        if let Some(fact) = import_concept(Path::new(&relative), &text, &fallback_repo)? {
            facts.push(fact);
        }
    }
    Ok(facts)
}

#[derive(Serialize)]
struct GeneratedOut<'a> {
    by: &'a str,
    at: String,
}

#[derive(Serialize)]
struct VerifiedOut {
    by: String,
    at: String,
}

#[derive(Serialize)]
struct SourceOut<'a> {
    id: &'a str,
    resource: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
}

#[derive(Serialize)]
struct AnchorOut<'a> {
    path: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    symbol: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hash: Option<&'a str>,
}

#[derive(Serialize)]
struct ConceptOut<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    title: &'a str,
    description: &'a str,
    tags: Vec<&'static str>,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    superseded_by: Option<&'a str>,
    generated: GeneratedOut<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verified: Option<Vec<VerifiedOut>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stale_after: Option<String>,
    sources: Vec<SourceOut<'a>>,
    #[serde(rename = "x-nexus-id")]
    id: &'a str,
    #[serde(rename = "x-nexus-repo")]
    repo: &'a str,
    #[serde(rename = "x-nexus-anchors")]
    anchors: Vec<AnchorOut<'a>>,
}

fn frontmatter<T: Serialize>(value: &T) -> Result<String> {
    let yaml = serde_yaml::to_string(value)?;
    Ok(format!("---\n{}---\n", yaml.trim_start_matches("---\n")))
}

fn concept_body(fact: &Fact) -> String {
    let mut body = String::new();
    let first_line = fact.body.lines().next().unwrap_or("");
    body.push_str(first_line);
    body.push('\n');
    if fact.body.lines().count() > 1 {
        body.push('\n');
        body.push_str(
            fact.body
                .strip_prefix(first_line)
                .unwrap_or(&fact.body)
                .trim_start_matches('\n'),
        );
        body.push('\n');
    }
    // Standard Markdown links, never [[wikilinks]] (OKF §6.1).
    for source in &fact.sources {
        body.push_str(&format!(
            "\n[^{}]: {}\n",
            source.id,
            source.title.as_deref().unwrap_or(&source.resource)
        ));
    }
    body
}

fn filename_for(fact: &Fact) -> String {
    let slug: String = fact
        .title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let slug = slug.trim_matches('-').to_string();
    let slug = if slug.is_empty() { "fact" } else { &slug };
    let short_id: String = fact.id.chars().take(8).collect();
    format!("{slug}-{short_id}.md")
}

/// Write a bundle. `index.md` frontmatter is preserved across regeneration
/// per OKF §8.1: never clobber keys a previous producer (or a human) added.
pub fn export_bundle(repo_path: &Path, facts: &[Fact]) -> Result<PathBuf> {
    let dir = bundle_dir(repo_path);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    nexus_core::paths::harden_dir_owner_only(&dir);

    for fact in facts {
        let out = ConceptOut {
            kind: fact.kind.as_okf_type(),
            title: &fact.title,
            description: fact.body.lines().next().unwrap_or(""),
            tags: vec!["nexuscontext", "curated"],
            status: fact.status.as_okf(),
            superseded_by: fact.superseded_by.as_deref(),
            generated: GeneratedOut {
                by: &fact.generated_by,
                at: format_instant(fact.generated_at),
            },
            verified: fact.verified_at.map(|at| {
                vec![VerifiedOut {
                    by: fact.verified_by.clone().unwrap_or_default(),
                    at: format_instant(at),
                }]
            }),
            stale_after: fact.stale_after.map(format_instant),
            sources: fact
                .sources
                .iter()
                .map(|s| SourceOut {
                    id: &s.id,
                    resource: &s.resource,
                    title: s.title.as_deref(),
                })
                .collect(),
            id: &fact.id,
            repo: &fact.repo_path,
            anchors: fact
                .anchors
                .iter()
                .map(|a| AnchorOut {
                    path: &a.path,
                    symbol: a.symbol.as_deref(),
                    line: a.line,
                    hash: a.hash.as_deref(),
                })
                .collect(),
        };
        std::fs::write(
            dir.join(filename_for(fact)),
            format!("{}{}", frontmatter(&out)?, concept_body(fact)),
        )?;
    }

    let index = dir.join("index.md");
    let mut meta: Yaml = if index.exists() {
        let existing = std::fs::read_to_string(&index)?;
        match split_frontmatter(&existing) {
            Some((front, _)) => serde_yaml::from_str(front).unwrap_or(Yaml::Null),
            None => Yaml::Null,
        }
    } else {
        Yaml::Null
    };
    if !matches!(meta, Yaml::Mapping(_)) {
        meta = Yaml::Mapping(serde_yaml::Mapping::new());
    }
    if let Yaml::Mapping(ref mut map) = meta {
        for (key, value) in [
            ("okf_version", Yaml::String(OKF_VERSION.into())),
            ("title", Yaml::String(format!("NexusContext facts — {}", repo_path.display()))),
            ("description", Yaml::String("Curated, code-anchored facts for this repo. Use it for decisions, incidents and lessons; stale facts are excluded.".into())),
            ("license", Yaml::String("MIT".into())),
            ("x-nexus-repo", Yaml::String(repo_path.to_string_lossy().to_string())),
        ] {
            map.entry(Yaml::String(key.into())).or_insert(value);
        }
    }

    let mut index_body = String::from("# Curated facts\n\n");
    let mut names: Vec<(String, String)> = facts
        .iter()
        .map(|f| {
            (
                filename_for(f).trim_end_matches(".md").to_string(),
                format!("{} — {}", f.title, f.kind.as_okf_type()),
            )
        })
        .collect();
    names.sort();
    for (file, label) in names {
        index_body.push_str(&format!("* [{label}]({file}.md)\n"));
    }
    std::fs::write(&index, format!("{}{}", frontmatter(&meta)?, index_body))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    /// Assert the parsed value, not a quote style: `'0.3'` and `"0.3"` both
    /// mean the string the spec asks for, while bare `0.3` would be a float.
    fn index_okf_version(path: &Path) -> Option<String> {
        let text = std::fs::read_to_string(path).ok()?;
        let (front, _) = split_frontmatter(&text)?;
        let doc: Yaml = serde_yaml::from_str(front).ok()?;
        match doc.get("okf_version")? {
            Yaml::String(value) => Some(value.clone()),
            _ => None,
        }
    }

    fn fact(id: &str) -> Fact {
        Fact {
            id: id.into(),
            repo_path: "/repo".into(),
            kind: FactKind::Decision,
            title: "Use presets".into(),
            body: "We cap schema tax.\nIt is measured twice.".into(),
            status: FactStatus::Stable,
            superseded_by: None,
            anchors: vec![Anchor {
                path: "src/tools.rs".into(),
                symbol: Some("get_session_usage".into()),
                line: Some(12),
                hash: None,
            }],
            sources: vec![Source {
                id: "adr5".into(),
                resource: "docs/ADRs/0005.md".into(),
                title: Some("ADR 0005".into()),
            }],
            generated_by: "agent/test".into(),
            generated_at: now() - 20,
            verified_by: Some("human:alice".into()),
            verified_at: Some(now() - 10),
            stale_after: Some(now() + 3600),
            anchor_stale: false,
        }
    }

    #[test]
    fn export_import_round_trip_preserves_trust_and_anchors() {
        let dir = std::env::temp_dir().join(format!("nc-okf-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let original = fact("abc123");
        export_bundle(&dir, std::slice::from_ref(&original)).unwrap();

        let contents =
            std::fs::read_to_string(bundle_dir(&dir).join("use-presets-abc123.md")).unwrap();
        assert!(contents.contains("type: Decision"));
        assert!(contents.contains("verified:"));
        assert!(contents.contains("x-nexus-anchors:"));
        assert!(!contents.contains("[["));
        assert_eq!(
            index_okf_version(&bundle_dir(&dir).join("index.md")).as_deref(),
            Some(OKF_VERSION)
        );

        let imported = import_bundle(&bundle_dir(&dir)).unwrap();
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].title, original.title);
        assert_eq!(imported[0].anchors, original.anchors);
        assert_eq!(imported[0].sources, original.sources);
        assert_eq!(
            crate::fact::trust_tier(&imported[0]),
            crate::fact::TrustTier::HumanReviewed
        );
        assert_eq!(imported[0].stale_after, original.stale_after);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn files_without_type_and_unknown_types_are_not_concepts() {
        let dir = std::env::temp_dir().join(format!("nc-okf-skip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bundle = bundle_dir(&dir);
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(bundle.join("README.md"), "# not a concept\n").unwrap();
        std::fs::write(
            bundle.join("other.md"),
            "---\ntype: Unknown Type\n---\nbody\n",
        )
        .unwrap();
        assert!(import_bundle(&bundle).unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn single_item_lists_are_accepted_and_broken_links_tolerated() {
        let dir = std::env::temp_dir().join(format!("nc-okf-one-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bundle = bundle_dir(&dir);
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(bundle.join("f.md"), "---\ntype: Decision\nsources: { id: a, resource: missing.md }\nx-nexus-anchors: { path: src/a.rs }\n---\nbody\n").unwrap();
        let facts = import_bundle(&bundle).unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].sources.len(), 1);
        assert_eq!(facts[0].anchors.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_stale_after_is_never_fresh() {
        let dir = std::env::temp_dir().join(format!("nc-okf-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bundle = bundle_dir(&dir);
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(
            bundle.join("f.md"),
            "---\ntype: Decision\nstale_after: tomorrow-ish\n---\nbody\n",
        )
        .unwrap();
        let facts = import_bundle(&bundle).unwrap();
        assert_eq!(facts.len(), 1);
        assert!(facts[0].is_stale(0));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn index_frontmatter_survives_regeneration() {
        let dir = std::env::temp_dir().join(format!("nc-okf-index-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bundle = bundle_dir(&dir);
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(
            bundle.join("index.md"),
            "---\nx-team-owner: alice\n---\n# old\n",
        )
        .unwrap();
        export_bundle(&dir, &[]).unwrap();
        let index = std::fs::read_to_string(bundle.join("index.md")).unwrap();
        assert!(index.contains("x-team-owner: alice"));
        assert_eq!(
            index_okf_version(&bundle.join("index.md")).as_deref(),
            Some(OKF_VERSION)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bundle_routes_expose_only_index_metadata_and_mark_the_active_repo() {
        let root = std::env::temp_dir().join(format!("nc-okf-routes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo_a = root.join("repo-a");
        let repo_b = root.join("repo-b");
        std::fs::create_dir_all(&repo_a).unwrap();
        std::fs::create_dir_all(&repo_b).unwrap();
        let mut f = fact("route1");
        f.repo_path = repo_a.to_string_lossy().to_string();
        export_bundle(&repo_a, std::slice::from_ref(&f)).unwrap();
        export_bundle(&repo_b, &[]).unwrap();

        let index = bundle_dir(&repo_a).join("index.md");
        let text = std::fs::read_to_string(&index).unwrap();
        let text = text.replace(
            "Curated, code-anchored facts for this repo. Use it for decisions, incidents and lessons; stale facts are excluded.",
            "Ledger ownership and budget decisions for repo A.",
        );
        std::fs::write(&index, text).unwrap();

        let routes = discover_bundles(&root, &repo_a).unwrap();
        assert_eq!(routes.len(), 2);
        let a = routes
            .iter()
            .find(|r| r.repo_path == repo_a.to_string_lossy())
            .unwrap();
        assert!(a.active);
        assert_eq!(
            a.description,
            "Ledger ownership and budget decisions for repo A."
        );
        assert_eq!(a.fact_files, 1);
        assert_eq!(a.okf_version.as_deref(), Some(OKF_VERSION));
        let b = routes.iter().find(|r| !r.active).unwrap();
        assert_eq!(b.fact_files, 0);
        std::fs::remove_dir_all(root).unwrap();
    }
}
