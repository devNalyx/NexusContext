use gtk4::prelude::*;
use gtk4::{Align, Box as GtkBox, Button, CheckButton, Entry, Label, Orientation};

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

    // Distinct from allow_remote below: this is a feature switch (is
    // semantic search on at all), not a network-safety gate (is this
    // particular endpoint allowed to be contacted). Filling in an endpoint
    // to try it out doesn't silently start sending code to it.
    let enabled_check = CheckButton::builder()
        .label(
            "Enable embeddings (semantic search / query_memory). Off by default even with \
             an endpoint configured below.",
        )
        .build();

    let endpoint_entry = Entry::builder().placeholder_text("http://localhost:11434/v1").build();
    let model_entry = Entry::builder().placeholder_text("nomic-embed-text").build();
    let allow_remote_check = CheckButton::builder()
        .label(
            "Allow remote endpoint (not loopback/private - e.g. a Tailscale/VPN node). \
             Required or the daemon refuses to use it.",
        )
        .build();

    let status_label = Label::builder().label("").halign(Align::Start).wrap(true).build();
    let button_row = GtkBox::builder().orientation(Orientation::Horizontal).spacing(6).build();
    let save_button = Button::with_label("Save");
    let test_button = Button::with_label("Test Connection");
    button_row.append(&save_button);
    button_row.append(&test_button);

    {
        let enabled_check = enabled_check.clone();
        let endpoint_entry = endpoint_entry.clone();
        let model_entry = model_entry.clone();
        let allow_remote_check = allow_remote_check.clone();
        let status_label = status_label.clone();
        save_button.connect_clicked(move |_| {
            let result = crate::client::call(
                "config.set",
                serde_json::json!({
                    "embeddings": {
                        "enabled": enabled_check.is_active(),
                        "endpoint": endpoint_entry.text().to_string(),
                        "model": model_entry.text().to_string(),
                        "allow_remote": allow_remote_check.is_active(),
                    }
                }),
            );
            match result {
                Ok(_) => status_label.set_label("Saved."),
                Err(err) => status_label.set_label(&format!("Error: {err}")),
            }
        });
    }

    {
        let status_label = status_label.clone();
        test_button.connect_clicked(move |_| {
            status_label.set_label("Testing...");
            match crate::client::call("embeddings.test", serde_json::json!({})) {
                Ok(result) => {
                    let model = result.get("model").and_then(|v| v.as_str()).unwrap_or("?");
                    let dim = result.get("dim").and_then(|v| v.as_u64()).unwrap_or(0);
                    let latency_ms = result.get("latency_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                    status_label.set_label(&format!("Connected: {model}, {dim}-dim, {latency_ms}ms"));
                }
                Err(err) => status_label.set_label(&format!("Error: {err}")),
            }
        });
    }

    container.append(&hint);
    container.append(&enabled_check);
    container.append(&Label::builder().label("Embeddings endpoint").halign(Align::Start).build());
    container.append(&endpoint_entry);
    container.append(&Label::builder().label("Model").halign(Align::Start).build());
    container.append(&model_entry);
    container.append(&allow_remote_check);
    container.append(&button_row);
    container.append(&status_label);

    load_current(&enabled_check, &endpoint_entry, &model_entry, &allow_remote_check, &status_label);
    container
}

fn load_current(
    enabled_check: &CheckButton,
    endpoint_entry: &Entry,
    model_entry: &Entry,
    allow_remote_check: &CheckButton,
    status_label: &Label,
) {
    match crate::client::call("config.get", serde_json::json!({})) {
        Ok(config) => {
            if let Some(enabled) = config.pointer("/embeddings/enabled").and_then(|v| v.as_bool()) {
                enabled_check.set_active(enabled);
            }
            if let Some(endpoint) = config
                .pointer("/embeddings/endpoint")
                .and_then(|v| v.as_str())
            {
                endpoint_entry.set_text(endpoint);
            }
            if let Some(model) = config.pointer("/embeddings/model").and_then(|v| v.as_str()) {
                model_entry.set_text(model);
            }
            if let Some(allow_remote) = config
                .pointer("/embeddings/allow_remote")
                .and_then(|v| v.as_bool())
            {
                allow_remote_check.set_active(allow_remote);
            }
        }
        Err(err) => status_label.set_label(&format!("Error loading config: {err}")),
    }
}
