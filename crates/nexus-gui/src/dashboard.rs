use gtk4::prelude::*;
use gtk4::{Align, Box as GtkBox, Button, Label, Orientation};

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
        .label("Daemon Status")
        .css_classes(["title-2"])
        .halign(Align::Start)
        .build();

    let status_label = Label::builder()
        .label("Not checked yet.")
        .halign(Align::Start)
        .wrap(true)
        .build();

    let refresh_button = Button::with_label("Refresh");
    {
        let status_label = status_label.clone();
        refresh_button.connect_clicked(move |_| refresh(&status_label));
    }

    container.append(&title);
    container.append(&status_label);
    container.append(&refresh_button);

    refresh(&status_label);
    container
}

fn refresh(status_label: &Label) {
    match crate::client::call("status.get", serde_json::json!({})) {
        Ok(result) => {
            let pressure_events = result
                .pointer("/watch_budget/pressure_events")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            // Only called out when non-zero - see nexusd::watcher's own
            // doc comment: this is the loud signal that
            // fs.inotify.max_user_watches is actually constraining real
            // usage, not routine status noise for the common case where
            // it never comes up at all.
            let pressure_note = if pressure_events > 0 {
                format!(
                    " ({pressure_events} project(s) skipped or evicted - inotify budget is tight; \
                     raise fs.inotify.max_user_watches if you want everything auto-watched)"
                )
            } else {
                String::new()
            };
            let text = format!(
                "version: {}\ndata_dir: {}\nlog_file: {}\nprojects indexed: {}\nauto-sync watching: {} project(s)\ninotify watches: ~{}/{} used{}",
                result.get("version").and_then(|v| v.as_str()).unwrap_or("?"),
                result.get("data_dir").and_then(|v| v.as_str()).unwrap_or("?"),
                result.get("log_file").and_then(|v| v.as_str()).unwrap_or("?"),
                result
                    .get("projects_indexed")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                result
                    .get("projects_watched")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                result
                    .pointer("/watch_budget/estimated_watches_used")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                result
                    .pointer("/watch_budget/estimated_watches_budget")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                pressure_note,
            );
            status_label.set_label(&text);
        }
        Err(err) => status_label.set_label(&format!("Error: {err}")),
    }
}
