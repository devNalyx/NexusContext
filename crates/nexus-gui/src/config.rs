use gtk4::prelude::*;
use gtk4::{Align, Box as GtkBox, Button, Entry, Label, Orientation};

pub fn build() -> GtkBox {
    let container = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(12)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();

    let hint = Label::builder()
        .label(
            "Embeddings are optional - structural tools (search, trace, architecture) \
             work with none of this configured.",
        )
        .halign(Align::Start)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .build();

    let endpoint_entry = Entry::builder().placeholder_text("http://localhost:11434/v1").build();
    let model_entry = Entry::builder().placeholder_text("nomic-embed-text").build();

    let status_label = Label::builder().label("").halign(Align::Start).build();
    let save_button = Button::with_label("Save");

    {
        let endpoint_entry = endpoint_entry.clone();
        let model_entry = model_entry.clone();
        let status_label = status_label.clone();
        save_button.connect_clicked(move |_| {
            let result = crate::client::call(
                "config.set",
                serde_json::json!({
                    "embeddings": {
                        "endpoint": endpoint_entry.text().to_string(),
                        "model": model_entry.text().to_string(),
                    }
                }),
            );
            match result {
                Ok(_) => status_label.set_label("Saved."),
                Err(err) => status_label.set_label(&format!("Error: {err}")),
            }
        });
    }

    container.append(&hint);
    container.append(&Label::builder().label("Embeddings endpoint").halign(Align::Start).build());
    container.append(&endpoint_entry);
    container.append(&Label::builder().label("Model").halign(Align::Start).build());
    container.append(&model_entry);
    container.append(&save_button);
    container.append(&status_label);

    load_current(&endpoint_entry, &model_entry, &status_label);
    container
}

fn load_current(endpoint_entry: &Entry, model_entry: &Entry, status_label: &Label) {
    match crate::client::call("config.get", serde_json::json!({})) {
        Ok(config) => {
            if let Some(endpoint) = config
                .pointer("/embeddings/endpoint")
                .and_then(|v| v.as_str())
            {
                endpoint_entry.set_text(endpoint);
            }
            if let Some(model) = config.pointer("/embeddings/model").and_then(|v| v.as_str()) {
                model_entry.set_text(model);
            }
        }
        Err(err) => status_label.set_label(&format!("Error loading config: {err}")),
    }
}
