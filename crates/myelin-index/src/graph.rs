use crate::store::SkillNeighborhood;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Emits Graphviz DOT for a single skill's bounded neighborhood - never
/// the whole store's graph at once, always scoped to one skill and what
/// backs it. Edge names mirror the original design ontology
/// (EVIDENCE_FOR, HARDENED_INTO, CORRECTS/REINFORCES) even though that
/// ontology was never built out as real graph tables - this is a view
/// over the actual relational schema, not a separate graph store.
pub fn to_dot(n: &SkillNeighborhood) -> String {
    let mut dot = String::from("digraph skill_neighborhood {\n  rankdir=LR;\n  node [shape=box, fontname=\"sans-serif\"];\n\n");

    dot.push_str(&format!(
        "  candidate [label=\"Candidate\\n{}\\nreps: {}\", style=filled, fillcolor=lightyellow];\n",
        escape(&n.candidate_title),
        n.rep_count
    ));
    dot.push_str(&format!(
        "  skill [label=\"Skill\\n{}\\nreason: {}\", style=filled, fillcolor=lightblue, shape=box3d];\n",
        escape(&n.skill_name),
        escape(&n.promoted_reason)
    ));
    dot.push_str("  candidate -> skill [label=\"HARDENED_INTO\"];\n\n");

    for (i, obs) in n.observations.iter().enumerate() {
        let proj = obs
            .project
            .as_deref()
            .map(|p| format!("\\n({})", escape(p)))
            .unwrap_or_default();
        dot.push_str(&format!(
            "  obs_{i} [label=\"Observation\\n{}{proj}\", shape=note];\n",
            escape(&truncate(&obs.summary, 60))
        ));
        dot.push_str(&format!(
            "  obs_{i} -> candidate [label=\"EVIDENCE_FOR\"];\n"
        ));
    }
    dot.push('\n');

    for (i, corr) in n.corrections.iter().enumerate() {
        let color = if corr.kind == "correction" {
            "orange"
        } else {
            "darkgreen"
        };
        let edge_label = if corr.kind == "correction" {
            "CORRECTS"
        } else {
            "REINFORCES"
        };
        dot.push_str(&format!(
            "  corr_{i} [label=\"{}\\n{}\", shape=ellipse, color={color}];\n",
            corr.kind,
            escape(&truncate(&corr.note, 60))
        ));
        dot.push_str(&format!("  corr_{i} -> skill [label=\"{edge_label}\"];\n"));
    }

    dot.push_str("}\n");
    dot
}

/// Shells out to Graphviz's `dot` to render a PNG - optional, best-effort.
/// Returns a clear error if `dot` isn't on PATH rather than panicking;
/// the `.dot` source is still useful on its own (any Graphviz-aware
/// viewer can open it), same Recommends-not-Depends posture as any
/// optional external tool.
pub fn render_png(dot: &str, output_path: &Path) -> anyhow::Result<()> {
    let mut child = Command::new("dot")
        .arg("-Tpng")
        .arg("-o")
        .arg(output_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| {
            anyhow::anyhow!("`dot` not found on PATH - install graphviz to render PNGs")
        })?;

    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(dot.as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        anyhow::bail!("dot failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{CorrectionRef, ObservationRef};

    fn sample_neighborhood() -> SkillNeighborhood {
        SkillNeighborhood {
            skill_id: 1,
            skill_name: "rotate leaked api key".to_string(),
            promoted_reason: "context_signal".to_string(),
            candidate_id: 2,
            candidate_title: "rotate leaked api key".to_string(),
            rep_count: 1,
            observations: vec![ObservationRef {
                summary: "revoke and reissue the key".to_string(),
                project: Some("repoA".to_string()),
            }],
            corrections: vec![CorrectionRef {
                kind: "correction".to_string(),
                note: "also invalidate cached tokens".to_string(),
            }],
        }
    }

    #[test]
    fn dot_includes_all_nodes_and_the_ontology_edge_labels() {
        let dot = to_dot(&sample_neighborhood());
        assert!(dot.starts_with("digraph skill_neighborhood {"));
        assert!(dot.contains("HARDENED_INTO"));
        assert!(dot.contains("EVIDENCE_FOR"));
        assert!(dot.contains("CORRECTS"));
        assert!(dot.contains("rotate leaked api key"));
        assert!(dot.contains("revoke and reissue the key"));
        assert!(dot.contains("also invalidate cached tokens"));
    }

    #[test]
    fn dot_escapes_quotes_and_backslashes_in_labels() {
        let mut n = sample_neighborhood();
        n.candidate_title = "has \"quotes\" and a \\backslash".to_string();
        let dot = to_dot(&n);
        assert!(dot.contains("has \\\"quotes\\\" and a \\\\backslash"));
    }

    #[test]
    fn long_text_is_truncated() {
        let mut n = sample_neighborhood();
        n.observations[0].summary = "x".repeat(200);
        let dot = to_dot(&n);
        assert!(dot.contains("..."));
        assert!(!dot.contains(&"x".repeat(200)));
    }

    #[test]
    fn render_png_gives_a_clear_error_when_dot_is_missing_from_path() {
        // Empty PATH guarantees `dot` can't be found, regardless of
        // whether graphviz happens to be installed on the test machine.
        // Mutates the process-wide PATH briefly - safe today since no
        // other test in this crate spawns a subprocess that needs it,
        // but worth knowing if that ever changes.
        let original_path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", "");
        let result = render_png("digraph{}", Path::new("/tmp/should-not-be-created.png"));
        std::env::set_var("PATH", original_path);

        let err = result.unwrap_err();
        assert!(err.to_string().contains("not found on PATH"));
    }
}
