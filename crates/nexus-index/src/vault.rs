//! Read-side queries over the vault metadata produced by the Markdown
//! ingest pass: resolved wikilinks, backlinks, tags and a bounded document
//! map. Everything here is structural/lexical over the existing SQLite
//! graph - no LLM, no network, no Obsidian process required.
//!
//! This is deliberately *not* an Obsidian query-language implementation.
//! It answers the small set of questions an agent actually needs when
//! following a vault's MOC: what does this note link to, what links here,
//! which notes carry a tag, and what is reachable within N hops.

use crate::graph::{GraphStore, NodeRecord, VaultLinkRecord};
use crate::project::{graph_db_path, require_path_allowed};
use anyhow::{anyhow, bail, Result};
use nexus_core::Paths;
use serde::Serialize;
use std::collections::{HashSet, VecDeque};
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct VaultHit {
    /// Destination note for outbound links, source note for backlinks.
    /// Unresolved links keep the raw target text here.
    pub file: String,
    pub line: u32,
    pub target: String,
    pub resolution: String,
    pub confidence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    pub embed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct VaultTagHit {
    pub tag: String,
    pub notes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct VaultMapHit {
    pub file: String,
    pub depth: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum VaultContext {
    Links {
        note: String,
        links: Vec<VaultHit>,
    },
    Backlinks {
        note: String,
        links: Vec<VaultHit>,
    },
    Tags {
        tags: Vec<VaultTagHit>,
    },
    NotesWithTag {
        tag: String,
        notes: Vec<String>,
    },
    Map {
        root: String,
        notes: Vec<VaultMapHit>,
    },
}

fn open_vault_store(repo_path: &Path) -> Result<GraphStore> {
    let repo_path = repo_path
        .canonicalize()
        .map_err(|_| anyhow!("repo_path does not exist: {}", repo_path.display()))?;
    require_path_allowed(&Paths::resolve(), &repo_path)?;
    let db_path = graph_db_path(&repo_path);
    if !db_path.exists() {
        bail!(
            "no index found for {} - run index_project first",
            repo_path.display()
        );
    }
    GraphStore::open(&db_path)
}

/// One canonical key for both wikilink targets and stored `file_path`s.
/// Obsidian links always use `/`; Windows paths from the walker use `\\`.
/// Without this swap, `[[docs/Plan]]` cannot resolve on Windows even though
/// the note exists at `docs\\Plan.md`.
pub(crate) fn normalized_path_key(path: &str) -> String {
    path.trim()
        .replace('\\', "/")
        .trim_matches('/')
        .to_lowercase()
}

fn normalized_note_keys(note: &str) -> Vec<String> {
    let lower = normalized_path_key(note);
    let mut keys = vec![lower.clone()];
    if let Some(stem) = lower
        .strip_suffix(".markdown")
        .or_else(|| lower.strip_suffix(".md"))
    {
        keys.push(stem.to_string());
    } else {
        keys.push(format!("{lower}.md"));
    }
    keys
}

/// Resolves a caller-supplied note name the same way `[[wikilinks]]` are
/// resolved during indexing: path first, then alias, then unique basename.
fn resolve_note(store: &GraphStore, note: &str) -> Result<NodeRecord> {
    let files = store.all_file_nodes()?;
    let aliases = store.all_vault_aliases()?;
    let keys = normalized_note_keys(note);

    // Match the ingest pass's documented precedence: explicit path, then
    // alias, then unique basename. A bare basename must never beat an alias.
    for key in &keys {
        let mut matches: Vec<&NodeRecord> = files
            .iter()
            .filter(|file| normalized_path_key(&file.file_path) == *key)
            .collect();
        matches.sort_by_key(|file| &file.file_path);
        if matches.len() == 1 {
            return Ok(matches[0].clone());
        }
    }

    for key in &keys {
        let mut ids: Vec<i64> = aliases
            .iter()
            .filter(|(alias, _)| alias.to_lowercase() == *key)
            .map(|(_, id)| *id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        if ids.len() == 1 {
            return files
                .iter()
                .find(|file| file.id == ids[0])
                .cloned()
                .ok_or_else(|| anyhow!("alias points at a missing note"));
        }
    }

    for key in &keys {
        let basename = key.rsplit('/').next().unwrap_or(key);
        let mut matches: Vec<&NodeRecord> = files
            .iter()
            .filter(|file| {
                normalized_path_key(&file.file_path)
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    == basename
            })
            .collect();
        matches.sort_by_key(|file| &file.file_path);
        if matches.len() == 1 {
            return Ok(matches[0].clone());
        }
    }

    bail!("no unique note matching '{note}' - use its full path or fix the ambiguous name")
}

fn link_to_hit(link: &VaultLinkRecord, outbound: bool) -> VaultHit {
    VaultHit {
        file: if outbound {
            link.dst_path.clone().unwrap_or_else(|| link.target.clone())
        } else {
            link.src_path.clone()
        },
        line: link.line,
        target: link.target.clone(),
        resolution: link.resolution.clone(),
        confidence: link.confidence.clone(),
        heading: link.heading.clone(),
        display: link.display.clone(),
        embed: link.embed,
    }
}

/// Bounded view over a vault's link graph. `depth` only affects `map` and is
/// clamped by the caller (nexusd's `SERVER_MAX_DEPTH`); `limit` bounds every
/// mode's response independently.
pub fn vault_context(
    repo_path: &Path,
    mode: &str,
    note: Option<&str>,
    depth: u32,
    limit: u32,
) -> Result<VaultContext> {
    let store = open_vault_store(repo_path)?;

    match mode {
        "links" => {
            let note = note.ok_or_else(|| anyhow!("'links' mode requires 'note'"))?;
            let resolved = resolve_note(&store, note)?;
            let links = store
                .vault_links_from(resolved.id, limit)?
                .into_iter()
                .map(|link| link_to_hit(&link, true))
                .collect();
            Ok(VaultContext::Links {
                note: resolved.file_path,
                links,
            })
        }
        "backlinks" => {
            let note = note.ok_or_else(|| anyhow!("'backlinks' mode requires 'note'"))?;
            let resolved = resolve_note(&store, note)?;
            let mut seen = HashSet::new();
            let mut links = Vec::new();
            // Resolved edges + raw-target matches: the latter catches links
            // that failed to resolve (e.g. an ambiguous `[[README]]`) without
            // pretending they were confident.
            for link in store
                .vault_links_to(resolved.id, limit)?
                .into_iter()
                .chain(store.vault_links_by_target(note, limit)?)
                .chain(store.vault_links_by_target(&resolved.file_path, limit)?)
            {
                let key = (link.src_id, link.target.clone(), link.line);
                if seen.insert(key) {
                    links.push(link_to_hit(&link, false));
                    if links.len() as u32 >= limit {
                        break;
                    }
                }
            }
            Ok(VaultContext::Backlinks {
                note: resolved.file_path,
                links,
            })
        }
        "tags" => Ok(VaultContext::Tags {
            tags: store
                .tags(limit)?
                .into_iter()
                .map(|(tag, notes)| VaultTagHit { tag, notes })
                .collect(),
        }),
        "notes_with_tag" => {
            let tag = note.ok_or_else(|| anyhow!("'notes_with_tag' mode requires 'note'"))?;
            let notes = store
                .notes_with_tag(tag.trim_start_matches('#'), limit)?
                .into_iter()
                .map(|node| node.file_path)
                .collect();
            Ok(VaultContext::NotesWithTag {
                tag: tag.trim_start_matches('#').to_string(),
                notes,
            })
        }
        "map" => {
            let note = note.ok_or_else(|| anyhow!("'map' mode requires 'note'"))?;
            let root = resolve_note(&store, note)?;
            let mut visited = HashSet::new();
            visited.insert(root.id);
            let mut queue = VecDeque::from([(root.id, 0u32)]);
            let mut notes = Vec::new();

            while let Some((id, current_depth)) = queue.pop_front() {
                if notes.len() as u32 >= limit {
                    break;
                }
                if current_depth >= depth {
                    continue;
                }
                for target in store.vault_link_targets(id)? {
                    // The outer check only runs once per frontier node; a
                    // single MOC note can have dozens of links, so the cap
                    // must be enforced inside this loop too.
                    if notes.len() as u32 >= limit {
                        break;
                    }
                    if visited.insert(target) {
                        if let Some(node) = store.node_by_id(target)? {
                            notes.push(VaultMapHit {
                                file: node.file_path,
                                depth: current_depth + 1,
                            });
                            if current_depth + 1 < depth {
                                queue.push_back((target, current_depth + 1));
                            }
                        }
                    }
                }
            }

            Ok(VaultContext::Map {
                root: root.file_path,
                notes,
            })
        }
        other => bail!(
            "unknown vault mode '{other}' - expected links, backlinks, tags, notes_with_tag or map"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::normalized_note_keys;
    use crate::graph::GraphStore;
    use crate::ingest::index_directory;
    use crate::project::index_project;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::MutexGuard;

    /// `vault_context` resolves `Paths` from the environment, so tests that
    /// call it need an isolated config/data dir. The lock is shared with all
    /// other env-mutating tests in this test binary, not just this module.
    struct FakeEnv {
        prev_config: Option<std::ffi::OsString>,
        prev_cache: Option<std::ffi::OsString>,
        _guard: MutexGuard<'static, ()>,
    }

    impl FakeEnv {
        fn set(label: &str) -> Self {
            let guard = crate::TEST_ENV_LOCK
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            let prev_config = std::env::var_os("NEXUS_CONFIG_DIR");
            let prev_cache = std::env::var_os("NEXUS_CACHE_DIR");
            let dir = std::env::temp_dir().join(format!(
                "nexus_vault_env_test_{label}_{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(dir.join("config")).unwrap();
            fs::create_dir_all(dir.join("data")).unwrap();
            std::env::set_var("NEXUS_CONFIG_DIR", dir.join("config"));
            std::env::set_var("NEXUS_CACHE_DIR", dir.join("data"));
            Self {
                prev_config,
                prev_cache,
                _guard: guard,
            }
        }
    }

    impl Drop for FakeEnv {
        fn drop(&mut self) {
            match &self.prev_config {
                Some(value) => std::env::set_var("NEXUS_CONFIG_DIR", value),
                None => std::env::remove_var("NEXUS_CONFIG_DIR"),
            }
            match &self.prev_cache {
                Some(value) => std::env::set_var("NEXUS_CACHE_DIR", value),
                None => std::env::remove_var("NEXUS_CACHE_DIR"),
            }
        }
    }

    /// Vault extraction is off by default. Tests that expect links/tags must
    /// opt in explicitly through an isolated config dir, just like a user
    /// does in `~/.config/nexuscontext/config.toml`.
    fn enable_vault_index() {
        let paths = nexus_core::Paths::resolve();
        fs::write(paths.config_file(), "[vault_index]\nenabled = true\n").unwrap();
    }

    fn temp_vault(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nexus_vault_graph_test_{name}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_note(dir: &std::path::Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn indexed_store(dir: &std::path::Path) -> GraphStore {
        let store = GraphStore::open(&dir.join("graph.db")).unwrap();
        index_directory(dir, &store).unwrap();
        store
    }

    fn file_id(store: &GraphStore, rel: &str) -> i64 {
        store
            .all_file_nodes()
            .unwrap()
            .into_iter()
            .find(|node| node.file_path == rel)
            .unwrap_or_else(|| panic!("missing file node {rel}"))
            .id
    }

    #[test]
    fn note_keys_cover_path_with_and_without_extension() {
        assert_eq!(
            normalized_note_keys("docs/Home.md"),
            vec!["docs/home.md", "docs/home"]
        );
        assert_eq!(
            normalized_note_keys("docs\\Home.md"),
            vec!["docs/home.md", "docs/home"]
        );
        assert_eq!(normalized_note_keys("Status"), vec!["status", "status.md"]);
    }

    #[test]
    fn vault_metadata_is_absent_when_the_index_gate_is_off() {
        let _env = FakeEnv::set("disabled");
        let dir = temp_vault("disabled");
        write_note(
            &dir,
            "Home.md",
            "---\ntags: [moc]\naliases: [Start]\n---\n# Home\n[[Missing]] #fresh\n",
        );

        let store = indexed_store(&dir);
        assert!(store.all_vault_aliases().unwrap().is_empty());
        assert!(store.tags(20).unwrap().is_empty());
        assert!(store
            .vault_links_from(file_id(&store, "Home.md"), 20)
            .unwrap()
            .is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolves_alias_path_basename_and_records_unresolved_links() {
        let _env = FakeEnv::set("resolution");
        enable_vault_index();
        let dir = temp_vault("resolution");
        write_note(
            &dir,
            "Home.md",
            "---\ntags: [moc]\n---\n# Home\n[[State]] [[docs/Plan|the plan]] [[Notes]] [[Missing]] ![[Notes#Detail]] #fresh\n",
        );
        write_note(
            &dir,
            "Status.md",
            "---\naliases: [State]\ntags: [current]\n---\n# Status\n",
        );
        write_note(&dir, "docs/Plan.md", "# Plan\n");
        write_note(&dir, "Notes.md", "# Notes\n## Detail\n");

        let store = indexed_store(&dir);
        let links = store
            .vault_links_from(file_id(&store, "Home.md"), 50)
            .unwrap();
        assert_eq!(links.len(), 5);

        let by_target: std::collections::HashMap<&str, &crate::VaultLinkRecord> = links
            .iter()
            .map(|link| (link.target.as_str(), link))
            .collect();
        assert_eq!(by_target["State"].resolution, "alias");
        assert_eq!(by_target["State"].confidence, "exact");
        assert_eq!(by_target["docs/Plan"].resolution, "exact-path");
        assert_eq!(by_target["Notes"].resolution, "unique-basename");
        assert_eq!(by_target["Notes"].confidence, "heuristic");
        assert_eq!(by_target["Missing"].resolution, "unresolved");
        assert!(by_target["Missing"].dst_id.is_none());

        let embed = links.iter().find(|link| link.embed).unwrap();
        assert_eq!(embed.heading.as_deref(), Some("Detail"));

        // Four resolved links: alias, exact path, and two links to Notes
        // (one of them with a heading anchor). The unresolved link is absent.
        assert_eq!(
            store
                .vault_link_targets(file_id(&store, "Home.md"))
                .unwrap()
                .len(),
            4
        );
        assert_eq!(
            store
                .vault_links_to(file_id(&store, "Status.md"), 20)
                .unwrap()
                .len(),
            1
        );

        let moc = store.notes_with_tag("moc", 20).unwrap();
        assert_eq!(moc.len(), 1);
        assert_eq!(moc[0].file_path, "Home.md");
        let current = store.notes_with_tag("current", 20).unwrap();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].file_path, "Status.md");
        assert!(store
            .tags(20)
            .unwrap()
            .iter()
            .any(|(tag, _)| tag == "fresh"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn aliases_beat_a_colliding_basename_when_resolving_notes() {
        let _env = FakeEnv::set("alias_precedence");
        enable_vault_index();
        let dir = temp_vault("alias_precedence");
        write_note(&dir, "Start.md", "# Start\n");
        write_note(
            &dir,
            "Other.md",
            "---\naliases: [Kickoff]\n---\n# Other\n[[Start]]\n",
        );

        // Use the real project index path so `vault_context` can open it.
        index_project(&dir).unwrap();
        let result = super::vault_context(&dir, "links", Some("Kickoff"), 2, 20).unwrap();
        match result {
            super::VaultContext::Links { note, links } => {
                assert_eq!(note, "Other.md");
                assert_eq!(links.len(), 1);
                assert_eq!(links[0].resolution, "unique-basename");
                assert_eq!(links[0].file, "Start.md");
            }
            other => panic!("expected links result, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ambiguous_basename_is_not_guessed() {
        let _env = FakeEnv::set("ambiguous");
        enable_vault_index();
        let dir = temp_vault("ambiguous");
        write_note(&dir, "Home.md", "# Home\n[[README]]\n");
        write_note(&dir, "a/README.md", "# A\n");
        write_note(&dir, "b/README.md", "# B\n");

        let store = indexed_store(&dir);
        let links = store
            .vault_links_from(file_id(&store, "Home.md"), 20)
            .unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].resolution, "ambiguous-basename");
        assert_eq!(links[0].confidence, "none");
        assert!(links[0].dst_id.is_none());
        assert!(store
            .vault_link_targets(file_id(&store, "Home.md"))
            .unwrap()
            .is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn map_respects_limit_even_when_one_note_has_many_links() {
        let _env = FakeEnv::set("map_limit");
        enable_vault_index();
        let dir = temp_vault("map_limit");
        let links: Vec<String> = (0..12).map(|i| format!("[[Note{i}]]")).collect();
        write_note(&dir, "Home.md", &format!("# Home\n{}\n", links.join(" ")));
        for i in 0..12 {
            write_note(&dir, &format!("Note{i}.md"), "# Note\n");
        }

        index_project(&dir).unwrap();
        let result = super::vault_context(&dir, "map", Some("Home.md"), 2, 3).unwrap();
        match result {
            super::VaultContext::Map { notes, .. } => assert_eq!(notes.len(), 3),
            other => panic!("expected map result, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&dir);
    }
}
