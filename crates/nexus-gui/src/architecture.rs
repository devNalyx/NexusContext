use gtk4::prelude::*;
use gtk4::{Align, Box as GtkBox, Button, Entry, Label, Orientation, ScrolledWindow};

pub fn build() -> GtkBox {
    let container = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(12)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();

    let form = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(6)
        .build();
    let project_entry = Entry::builder()
        .placeholder_text("project path")
        .hexpand(true)
        .build();
    let load_button = Button::with_label("Load");
    form.append(&project_entry);
    form.append(&load_button);

    let summary_label = Label::builder()
        .label("Enter a project path and click Load.")
        .halign(Align::Start)
        .wrap(true)
        .build();

    let details = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(16)
        .build();
    let scroller = ScrolledWindow::builder().child(&details).vexpand(true).build();

    {
        let details = details.clone();
        let summary_label = summary_label.clone();
        let project_entry = project_entry.clone();
        load_button.connect_clicked(move |_| {
            let repo_path = project_entry.text().to_string();
            if repo_path.trim().is_empty() {
                return;
            }
            load(&repo_path, &summary_label, &details);
        });
    }

    container.append(&form);
    container.append(&summary_label);
    container.append(&scroller);
    container
}

fn load(repo_path: &str, summary_label: &Label, details: &GtkBox) {
    while let Some(child) = details.first_child() {
        details.remove(&child);
    }

    let response = crate::client::call(
        "projects.architecture",
        serde_json::json!({ "repo_path": repo_path }),
    );

    let data = match response {
        Ok(data) => data,
        Err(err) => {
            summary_label.set_label(&format!("Error: {err}"));
            return;
        }
    };

    let total_nodes = data.get("total_nodes").and_then(|v| v.as_i64()).unwrap_or(0);
    let total_edges = data.get("total_edges").and_then(|v| v.as_i64()).unwrap_or(0);
    let last_indexed_unix = data.get("last_indexed_unix").and_then(|v| v.as_u64()).unwrap_or(0);

    summary_label.set_label(&format!(
        "{total_nodes} nodes, {total_edges} edges — indexed {}",
        format_age(last_indexed_unix)
    ));

    details.append(&section_heading("Busiest files"));
    if let Some(busiest) = data.get("busiest_files").and_then(|v| v.as_array()) {
        if busiest.is_empty() {
            details.append(&Label::builder().label("(none)").halign(Align::Start).build());
        }
        for entry in busiest {
            let file = entry.get("file").and_then(|v| v.as_str()).unwrap_or("?");
            let count = entry.get("definitions").and_then(|v| v.as_i64()).unwrap_or(0);
            details.append(&row_label(&format!("{count:>4}  {file}")));
        }
    }

    details.append(&section_heading("Language breakdown"));
    if let Some(langs) = data.get("language_breakdown").and_then(|v| v.as_array()) {
        if langs.is_empty() {
            details.append(&Label::builder().label("(none)").halign(Align::Start).build());
        }
        for entry in langs {
            let ext = entry.get("extension").and_then(|v| v.as_str()).unwrap_or("?");
            let count = entry.get("files").and_then(|v| v.as_i64()).unwrap_or(0);
            details.append(&row_label(&format!("{count:>4}  .{ext}")));
        }
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
