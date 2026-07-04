mod architecture;
mod client;
mod config;
mod dashboard;
mod logs;
mod project_picker;
mod projects;
mod search;

use adw::prelude::*;
use gtk4::glib;
use libadwaita as adw;

const APP_ID: &str = "org.nexuscontext.Manager";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

fn build_ui(app: &adw::Application) {
    // `activate` fires again every time GNOME Shell "launches" an
    // already-running single-instance app (app grid, search, gtk-launch,
    // etc.) - without this check each re-activation stacked a brand new
    // window in the same process instead of refocusing the existing one.
    if let Some(window) = app.windows().first() {
        window.present();
        return;
    }

    let view_stack = adw::ViewStack::new();
    view_stack.add_titled_with_icon(
        &dashboard::build(),
        Some("dashboard"),
        "Dashboard",
        "speedometer-symbolic",
    );
    view_stack.add_titled_with_icon(
        &projects::build(),
        Some("projects"),
        "Projects",
        "folder-symbolic",
    );
    view_stack.add_titled_with_icon(
        &search::build(),
        Some("search"),
        "Search",
        "system-search-symbolic",
    );
    view_stack.add_titled_with_icon(
        &architecture::build(),
        Some("architecture"),
        "Architecture",
        "view-grid-symbolic",
    );
    view_stack.add_titled_with_icon(
        &config::build(),
        Some("config"),
        "Config",
        "preferences-system-symbolic",
    );
    view_stack.add_titled_with_icon(
        &logs::build(),
        Some("logs"),
        "Logs",
        "text-x-generic-symbolic",
    );

    // ViewSwitcherTitle is deprecated since libadwaita 1.4 in favor of
    // composing AdwHeaderBar + AdwViewSwitcher directly - a real migration,
    // not something to do blind without visually verifying the header bar
    // still renders correctly (not available in this environment). Pre-
    // existing, not introduced by any of this session's changes.
    #[allow(deprecated)]
    let view_switcher = adw::ViewSwitcherTitle::builder().stack(&view_stack).build();

    let header_bar = adw::HeaderBar::builder()
        .title_widget(&view_switcher)
        .build();

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header_bar);
    toolbar_view.set_content(Some(&view_stack));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("NexusContext Manager")
        .default_width(900)
        .default_height(640)
        .content(&toolbar_view)
        .build();

    window.present();
}
