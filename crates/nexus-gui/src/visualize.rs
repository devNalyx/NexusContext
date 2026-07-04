use gtk4::prelude::*;
use gtk4::{
    Align, Box as GtkBox, Button, CheckButton, Entry, Label, Orientation, Picture, ScrolledWindow,
    SpinButton,
};

/// Renders a function's call neighborhood as an image - shells out to
/// Graphviz's `dot` (the server side only returns DOT text; this is the
/// only client that needs a picture rather than data) to do the actual
/// layout, rather than hand-rolling a force-directed graph renderer in
/// Cairo. Deliberately scoped to one function's bounded neighborhood, not
/// the whole project graph - past a few hundred nodes a full graph render
/// turns into an unreadable hairball, so the same `trace_call_path` depth
/// limit that already bounds the CLI/MCP tool bounds this too.
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
    let project_picker = crate::project_picker::build();
    let refresh_projects_button = Button::from_icon_name("view-refresh-symbolic");
    refresh_projects_button.set_tooltip_text(Some("Refresh project list"));
    {
        let project_picker = project_picker.clone();
        refresh_projects_button.connect_clicked(move |_| {
            crate::project_picker::refresh(&project_picker);
        });
    }
    let function_entry = Entry::builder().placeholder_text("function name").build();
    let inbound_check = CheckButton::builder().label("Inbound (callers)").build();
    let depth_spin = SpinButton::with_range(1.0, 10.0, 1.0);
    depth_spin.set_value(3.0);
    depth_spin.set_tooltip_text(Some("How many call hops out to include"));
    let visualize_button = Button::with_label("Visualize");

    form.append(&project_picker);
    form.append(&refresh_projects_button);
    form.append(&function_entry);
    form.append(&inbound_check);
    form.append(&depth_spin);
    form.append(&visualize_button);

    let status_label = Label::builder()
        .label("")
        .halign(Align::Start)
        .wrap(true)
        .build();
    let picture = Picture::new();
    let scroller = ScrolledWindow::builder()
        .child(&picture)
        .vexpand(true)
        .build();

    {
        let project_picker = project_picker.clone();
        let function_entry = function_entry.clone();
        let inbound_check = inbound_check.clone();
        let depth_spin = depth_spin.clone();
        let status_label = status_label.clone();
        let picture = picture.clone();
        visualize_button.connect_clicked(move |_| {
            let Some(repo_path) = crate::project_picker::selected_path(&project_picker) else {
                status_label.set_label("Pick a project first.");
                return;
            };
            let function_name = function_entry.text().to_string();
            if function_name.trim().is_empty() {
                status_label.set_label("Enter a function name first.");
                return;
            }
            let direction = if inbound_check.is_active() {
                "inbound"
            } else {
                "outbound"
            };
            let depth = depth_spin.value() as u64;

            status_label.set_label("Rendering...");
            match render(&repo_path, &function_name, direction, depth) {
                Ok(png_path) => {
                    picture.set_filename(Some(&png_path));
                    status_label.set_label(&format!(
                        "Showing {function_name}'s {direction} call neighborhood."
                    ));
                }
                Err(err) => status_label.set_label(&format!("Error: {err}")),
            }
        });
    }

    container.append(&form);
    container.append(&status_label);
    container.append(&scroller);
    container
}

/// Fetches the DOT source from the daemon, shells out to `dot -Tpng`, and
/// returns the rendered PNG's path - or a clear, actionable error if
/// Graphviz isn't installed, rather than a raw "No such file or directory"
/// or a crash. This is an optional, best-effort feature: the rest of the
/// app works fully without `dot` present.
fn render(
    repo_path: &str,
    function_name: &str,
    direction: &str,
    depth: u64,
) -> anyhow::Result<std::path::PathBuf> {
    let result = crate::client::call(
        "viz.call_graph",
        serde_json::json!({
            "repo_path": repo_path,
            "function_name": function_name,
            "direction": direction,
            "depth": depth,
        }),
    )?;
    let dot = result
        .get("dot")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("daemon returned no DOT source"))?;

    let dir = std::env::temp_dir();
    let dot_path = dir.join("nexuscontext-viz.dot");
    let png_path = dir.join("nexuscontext-viz.png");
    std::fs::write(&dot_path, dot)?;

    let output = std::process::Command::new("dot")
        .arg("-Tpng")
        .arg("-o")
        .arg(&png_path)
        .arg(&dot_path)
        .output()
        .map_err(|err| {
            anyhow::anyhow!(
                "couldn't run `dot` ({err}) - install Graphviz: sudo apt install graphviz"
            )
        })?;
    if !output.status.success() {
        anyhow::bail!("dot failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    Ok(png_path)
}
