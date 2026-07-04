use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Button, Orientation, ScrolledWindow, TextView, WrapMode};

const MAX_LINES: usize = 300;

pub fn build() -> GtkBox {
    let container = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(12)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();

    let refresh_button = Button::with_label("Refresh");

    let text_view = TextView::builder()
        .editable(false)
        .monospace(true)
        .wrap_mode(WrapMode::WordChar)
        .build();
    let scroller = ScrolledWindow::builder()
        .child(&text_view)
        .vexpand(true)
        .build();

    {
        let text_view = text_view.clone();
        refresh_button.connect_clicked(move |_| refresh(&text_view));
    }

    container.append(&refresh_button);
    container.append(&scroller);

    refresh(&text_view);
    container
}

fn refresh(text_view: &TextView) {
    let log_path = nexus_core::Paths::resolve().log_file();
    let content = match std::fs::read_to_string(&log_path) {
        Ok(content) => content,
        Err(err) => format!(
            "Couldn't read {} ({err}). Is `nexusd serve` running?",
            log_path.display()
        ),
    };

    let tail: Vec<&str> = content.lines().collect();
    let start = tail.len().saturating_sub(MAX_LINES);
    let tail_text = tail[start..].join("\n");

    text_view.buffer().set_text(&tail_text);
}
