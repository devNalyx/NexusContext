use gtk4::prelude::*;
use gtk4::{Align, Box as GtkBox, Button, DropDown, Label, Orientation, ScrolledWindow};
use serde_json::Value;

/// One review queue for both memories. Facts carry *staleness* (an anchored
/// file changed or `stale_after` elapsed); skills carry *atrophy* (a promoted
/// skill has not been used within its window). A pending skill candidate has
/// neither measurement and says so explicitly — the two labels are never
/// conflated.
///
/// This tab is deliberately read-only. Promotion and approval are agent +
/// human judgment calls made through `nexus memory …` or the MCP tools; a
/// "approve" button in a glib main loop would be an un-reviewed write.
pub fn build() -> GtkBox {
    let container = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(12)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();

    let title = Label::builder()
        .label("Review Queue")
        .css_classes(["title-2"])
        .halign(Align::Start)
        .build();

    let subtitle = Label::builder()
        .label("Read-only feed — approve or promote with `nexus memory` or the MCP tools.")
        .halign(Align::Start)
        .wrap(true)
        .build();

    let picker = crate::project_picker::build();
    let refresh_button = Button::with_label("Refresh");

    let details = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(16)
        .build();
    let scroller = ScrolledWindow::builder()
        .child(&details)
        .vexpand(true)
        .build();

    {
        let subtitle = subtitle.clone();
        let details = details.clone();
        let picker = picker.clone();
        refresh_button.connect_clicked(move |_| refresh(&subtitle, &details, &picker));
    }

    container.append(&title);
    container.append(&subtitle);
    container.append(&picker);
    container.append(&refresh_button);
    container.append(&scroller);

    refresh(&subtitle, &details, &picker);
    container
}

fn refresh(subtitle: &Label, details: &GtkBox, picker: &DropDown) {
    while let Some(child) = details.first_child() {
        details.remove(&child);
    }

    let params = match crate::project_picker::selected_path(picker) {
        Some(repo) => serde_json::json!({ "repo_path": repo, "limit": 50 }),
        None => serde_json::json!({ "limit": 50 }),
    };
    let data = match crate::client::call("review.list", params) {
        Ok(data) => data,
        Err(err) => {
            subtitle.set_label(&format!("Error: {err}"));
            return;
        }
    };

    if let Some(note) = data.get("note").and_then(Value::as_str) {
        subtitle.set_label(note);
    }

    details.append(&section_heading(
        "Facts — staleness (anchored file changed / stale_after)",
    ));
    let stale = data
        .get("fact_stale_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    details.append(&row_label(&format!("{stale} stale fact(s) in this repo")));
    append_rows(
        details,
        data.get("facts"),
        |f| {
            let id = f["id"].as_str().unwrap_or("?");
            let trust = f["trust"].as_str().unwrap_or("?");
            let status = f["status"].as_str().unwrap_or("?");
            let stale = if f["stale"].as_bool().unwrap_or(false) {
                "STALE"
            } else {
                "fresh"
            };
            let title = f["title"].as_str().unwrap_or("");
            let anchors = f["anchors"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|a| a["path"].as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            format!("{id}  [{trust}/{status}/{stale}]  {title}  — {anchors}")
        },
        "(no curated facts yet)",
    );

    details.append(&section_heading(
        "Skill candidates — atrophy: n/a (not promoted yet)",
    ));
    append_rows(
        details,
        data.get("skill_candidates"),
        |s| {
            let label = s["freshness_label"]
                .as_str()
                .unwrap_or("skill candidate — no atrophy data (not promoted)");
            let reason = s["heuristic_reason"].as_str().unwrap_or("");
            let excerpt = s["excerpt"].as_str().unwrap_or("");
            format!("{label}  |  {reason}  |  {excerpt}")
        },
        "(no pending skill candidates)",
    );

    details.append(&section_heading(
        "Promoted skills — atrophy (last used too long ago)",
    ));
    append_rows(
        details,
        data.get("promoted_skills"),
        |s| {
            let label = s["freshness_label"].as_str().unwrap_or("skill");
            let name = s["name"].as_str().unwrap_or("?");
            format!("{label}  |  {name}")
        },
        "(no promoted skills yet)",
    );

    if let Some(labels) = data.get("labels") {
        details.append(&section_heading("How the labels differ"));
        for key in ["fact", "skill"] {
            if let Some(value) = labels[key].as_str() {
                details.append(&row_label(&format!("{key}: {value}")));
            }
        }
    }
}

fn append_rows(
    details: &GtkBox,
    rows: Option<&Value>,
    render: impl Fn(&Value) -> String,
    empty: &str,
) {
    let Some(items) = rows.and_then(Value::as_array) else {
        details.append(&row_label(empty));
        return;
    };
    if items.is_empty() {
        details.append(&row_label(empty));
        return;
    }
    for item in items {
        details.append(&row_label(&render(item)));
    }
}

fn section_heading(text: &str) -> Label {
    Label::builder()
        .label(text)
        .halign(Align::Start)
        .css_classes(["heading"])
        .margin_top(8)
        .build()
}

fn row_label(text: &str) -> Label {
    Label::builder()
        .label(text)
        .halign(Align::Start)
        .wrap(true)
        .css_classes(["monospace"])
        .build()
}
