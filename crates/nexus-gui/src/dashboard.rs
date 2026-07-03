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
            let text = format!(
                "version: {}\ndata_dir: {}\nlog_file: {}\nprojects indexed: {}",
                result.get("version").and_then(|v| v.as_str()).unwrap_or("?"),
                result.get("data_dir").and_then(|v| v.as_str()).unwrap_or("?"),
                result.get("log_file").and_then(|v| v.as_str()).unwrap_or("?"),
                result
                    .get("projects_indexed")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
            );
            status_label.set_label(&text);
        }
        Err(err) => status_label.set_label(&format!("Error: {err}")),
    }
}
