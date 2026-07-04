use gtk4::prelude::*;
use gtk4::{Align, Box as GtkBox, Button, Label, Orientation, ScrolledWindow};
use serde_json::Value;

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
        .label("Usage Stats")
        .css_classes(["title-2"])
        .halign(Align::Start)
        .build();

    let subtitle_label = Label::builder()
        .label("Not loaded yet.")
        .halign(Align::Start)
        .wrap(true)
        .build();

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
        let subtitle_label = subtitle_label.clone();
        let details = details.clone();
        refresh_button.connect_clicked(move |_| refresh(&subtitle_label, &details));
    }

    container.append(&title);
    container.append(&subtitle_label);
    container.append(&refresh_button);
    container.append(&scroller);

    refresh(&subtitle_label, &details);
    container
}

fn refresh(subtitle_label: &Label, details: &GtkBox) {
    while let Some(child) = details.first_child() {
        details.remove(&child);
    }

    let data = match crate::client::call("stats.get", serde_json::json!({})) {
        Ok(data) => data,
        Err(err) => {
            subtitle_label.set_label(&format!("Error: {err}"));
            return;
        }
    };

    let collecting_since = data
        .get("collecting_since_unix")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    subtitle_label.set_label(&format!(
        "Collecting since {}",
        format_age(collecting_since)
    ));

    details.append(&section_heading("MCP Tool Calls"));
    append_tool_rows(details, data.get("mcp_tools"));

    details.append(&section_heading("Control API / GUI Calls"));
    append_tool_rows(details, data.get("control_methods"));

    details.append(&section_heading("Background Auto-Reindex"));
    if let Some(reindex) = data.get("reindex") {
        let total = reindex
            .get("total_auto_reindex_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let failed = reindex
            .get("total_auto_reindex_fail_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let avg_ms = reindex
            .get("avg_auto_reindex_ms")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        details.append(&row_label(&format!(
            "{total} succeeded, {failed} failed, avg {avg_ms:.0} ms"
        )));

        if let Some(projects) = reindex.get("projects").and_then(|v| v.as_array()) {
            if projects.is_empty() {
                details.append(&row_label("(no auto-reindex activity recorded yet)"));
            }
            for entry in projects {
                let root = entry
                    .get("root_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let count = entry
                    .get("auto_reindex_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let fail_count = entry
                    .get("auto_reindex_fail_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let avg_ms = entry
                    .get("avg_auto_reindex_ms")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let last_unix = entry
                    .get("last_auto_reindex_unix")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                details.append(&row_label(&format!(
                    "{count:>4} ok, {fail_count:>3} failed, avg {avg_ms:>6.0} ms — last {} — {root}",
                    format_age(last_unix)
                )));
            }
        }
    }
}

fn append_tool_rows(details: &GtkBox, tools: Option<&Value>) {
    let Some(tools) = tools.and_then(|v| v.as_array()) else {
        details.append(&row_label("(none)"));
        return;
    };
    if tools.is_empty() {
        details.append(&row_label("(no calls recorded yet)"));
        return;
    }

    let mut rows: Vec<&Value> = tools.iter().collect();
    rows.sort_by_key(|t| {
        std::cmp::Reverse(t.get("call_count").and_then(|v| v.as_u64()).unwrap_or(0))
    });

    for entry in rows {
        let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let calls = entry
            .get("call_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let errors = entry
            .get("error_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let avg_latency = entry
            .get("avg_latency_ms")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let max_latency = entry
            .get("max_latency_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let output_bytes = entry
            .get("total_output_bytes")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let last_called = entry
            .get("last_called_unix")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        details.append(&row_label(&format!(
            "{calls:>5} calls  {errors:>3} err  avg {avg_latency:>6.0} ms  max {max_latency:>6} ms  {output_bytes:>8} bytes total  last {}  {name}",
            format_age(last_called)
        )));
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
        .css_classes(["monospace"])
        .build()
}

fn format_age(unix_secs: u64) -> String {
    if unix_secs == 0 {
        return "never".to_string();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let age = now.saturating_sub(unix_secs);

    if age < 60 {
        format!("{age}s ago")
    } else if age < 3600 {
        format!("{}m ago", age / 60)
    } else if age < 86400 {
        format!("{}h ago", age / 3600)
    } else {
        format!("{}d ago", age / 86400)
    }
}
