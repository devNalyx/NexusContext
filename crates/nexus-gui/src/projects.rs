use gtk4::pango::EllipsizeMode;
use gtk4::prelude::*;
use gtk4::{
    Align, Box as GtkBox, Button, Entry, Label, ListBox, Orientation, ScrolledWindow, SelectionMode,
};

pub fn build() -> GtkBox {
    let container = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(12)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();

    let add_row = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(6)
        .build();
    let path_entry = Entry::builder()
        .placeholder_text("/path/to/project")
        .hexpand(true)
        .build();
    let reindex_button = Button::with_label("Index / Reindex");
    let import_button = Button::with_label("Import");
    import_button.set_tooltip_text(Some(
        "Load a snapshot from <path>/.nexuscontext/index.db.zst",
    ));
    add_row.append(&path_entry);
    add_row.append(&reindex_button);
    add_row.append(&import_button);

    let list = ListBox::builder()
        .selection_mode(SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let scroller = ScrolledWindow::builder().child(&list).vexpand(true).build();

    let refresh_button = Button::with_label("Refresh list");

    {
        let list = list.clone();
        refresh_button.connect_clicked(move |_| refresh_list(&list));
    }
    {
        let list = list.clone();
        let path_entry = path_entry.clone();
        reindex_button.connect_clicked(move |_| {
            let path = path_entry.text().to_string();
            if path.trim().is_empty() {
                return;
            }
            let result =
                crate::client::call("projects.reindex", serde_json::json!({ "repo_path": path }));
            if let Err(err) = result {
                show_error(&list, &err.to_string());
            } else {
                refresh_list(&list);
            }
        });
    }

    {
        let list = list.clone();
        let path_entry = path_entry.clone();
        import_button.connect_clicked(move |_| {
            let path = path_entry.text().to_string();
            if path.trim().is_empty() {
                return;
            }
            let result =
                crate::client::call("projects.import", serde_json::json!({ "repo_path": path }));
            if let Err(err) = result {
                show_error(&list, &err.to_string());
            } else {
                refresh_list(&list);
            }
        });
    }

    container.append(&add_row);
    container.append(&refresh_button);
    container.append(&scroller);

    refresh_list(&list);
    container
}

fn refresh_list(list: &ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }

    match crate::client::call("projects.list", serde_json::json!({})) {
        Ok(serde_json::Value::Array(projects)) if !projects.is_empty() => {
            for project in projects {
                let root_path = project
                    .get("root_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string();
                let nodes = project.get("nodes").and_then(|v| v.as_i64()).unwrap_or(0);
                let edges = project.get("edges").and_then(|v| v.as_i64()).unwrap_or(0);
                let last_queried = project
                    .get("last_queried_unix")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let disk_bytes = project
                    .get("disk_bytes")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);

                let labels = GtkBox::builder()
                    .orientation(Orientation::Vertical)
                    .spacing(4)
                    .hexpand(true)
                    .build();
                // A long path used to force the row wider than the viewport,
                // pushing the Delete button off-screen behind a horizontal
                // scrollbar instead of staying visible on the right. Middle
                // ellipsis keeps both the project name (end) and enough of
                // the parent path (start) visible; the tooltip has the rest.
                labels.append(
                    &Label::builder()
                        .label(&root_path)
                        .halign(Align::Start)
                        .hexpand(true)
                        .ellipsize(EllipsizeMode::Middle)
                        .max_width_chars(30)
                        .tooltip_text(&root_path)
                        .build(),
                );
                labels.append(
                    &Label::builder()
                        .label(format!("{nodes} nodes, {edges} edges"))
                        .halign(Align::Start)
                        .css_classes(["dim-label", "caption"])
                        .build(),
                );
                labels.append(
                    &Label::builder()
                        .label(format!(
                            "{} on disk - last used {}",
                            format_size(disk_bytes),
                            format_last_used(last_queried)
                        ))
                        .halign(Align::Start)
                        .css_classes(["dim-label", "caption"])
                        .build(),
                );

                let export_button = Button::with_label("Export");
                export_button.set_tooltip_text(Some(
                    "Write a snapshot to <project>/.nexuscontext/index.db.zst for teammates to import",
                ));
                {
                    let list = list.clone();
                    let root_path = root_path.clone();
                    export_button.connect_clicked(move |_| {
                        let result = crate::client::call(
                            "projects.export",
                            serde_json::json!({ "repo_path": root_path }),
                        );
                        match result {
                            Ok(_) => refresh_list(&list),
                            Err(err) => show_error(&list, &err.to_string()),
                        }
                    });
                }

                let delete_button = Button::with_label("Delete");
                delete_button.add_css_class("destructive-action");
                {
                    let list = list.clone();
                    let root_path = root_path.clone();
                    delete_button.connect_clicked(move |_| {
                        let result = crate::client::call(
                            "projects.delete",
                            serde_json::json!({ "repo_path": root_path }),
                        );
                        match result {
                            Ok(_) => refresh_list(&list),
                            Err(err) => show_error(&list, &err.to_string()),
                        }
                    });
                }

                let row = GtkBox::builder()
                    .orientation(Orientation::Horizontal)
                    .spacing(12)
                    .margin_top(8)
                    .margin_bottom(8)
                    .margin_start(8)
                    .margin_end(8)
                    .build();
                row.append(&labels);
                row.append(&export_button);
                row.append(&delete_button);
                list.append(&row);
            }
        }
        Ok(_) => list.append(&Label::new(Some("No projects indexed yet."))),
        Err(err) => show_error(list, &err.to_string()),
    }
}

fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// `0` means "never queried" (either a pre-upgrade registry entry, or a
/// project that's only ever been indexed, never actually searched/traced
/// against) - worth spelling out rather than printing a misleading
/// "56 years ago" from a 1970 epoch timestamp.
fn format_last_used(last_queried_unix: u64) -> String {
    if last_queried_unix == 0 {
        return "never".to_string();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(last_queried_unix);
    let elapsed = now.saturating_sub(last_queried_unix);
    match elapsed {
        s if s < 60 => "just now".to_string(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86400),
    }
}

fn show_error(list: &ListBox, message: &str) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    list.append(&Label::new(Some(&format!("Error: {message}"))));
}
