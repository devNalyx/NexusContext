use gtk4::prelude::*;
use gtk4::{
    Align, Box as GtkBox, Button, Entry, Label, ListBox, Orientation, ScrolledWindow,
    SelectionMode,
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

    let form = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(6)
        .build();
    let repo_entry = Entry::builder()
        .placeholder_text("project path")
        .hexpand(true)
        .build();
    let pattern_entry = Entry::builder()
        .placeholder_text("name pattern")
        .hexpand(true)
        .build();
    let search_button = Button::with_label("Search");
    form.append(&repo_entry);
    form.append(&pattern_entry);
    form.append(&search_button);

    let results = ListBox::builder()
        .selection_mode(SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let scroller = ScrolledWindow::builder()
        .child(&results)
        .vexpand(true)
        .build();

    search_button.connect_clicked(move |_| {
        let repo_path = repo_entry.text().to_string();
        let pattern = pattern_entry.text().to_string();
        if repo_path.trim().is_empty() || pattern.trim().is_empty() {
            return;
        }

        while let Some(child) = results.first_child() {
            results.remove(&child);
        }

        let response = crate::client::call(
            "search.adhoc",
            serde_json::json!({ "repo_path": repo_path, "pattern": pattern }),
        );
        match response {
            Ok(serde_json::Value::Array(matches)) if !matches.is_empty() => {
                for m in matches {
                    let kind = m.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                    let name = m.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let file = m.get("file").and_then(|v| v.as_str()).unwrap_or("?");
                    let start = m.get("start_line").and_then(|v| v.as_u64()).unwrap_or(0);
                    let end = m.get("end_line").and_then(|v| v.as_u64()).unwrap_or(0);

                    let row = Label::builder()
                        .label(format!("[{kind}] {name}  —  {file}:{start}-{end}"))
                        .halign(Align::Start)
                        .margin_top(4)
                        .margin_bottom(4)
                        .margin_start(8)
                        .margin_end(8)
                        .build();
                    results.append(&row);
                }
            }
            Ok(_) => results.append(&Label::new(Some("No matches."))),
            Err(err) => results.append(&Label::new(Some(&format!("Error: {err}")))),
        }
    });

    container.append(&form);
    container.append(&scroller);
    container
}
