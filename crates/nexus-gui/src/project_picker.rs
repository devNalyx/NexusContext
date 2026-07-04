use gtk4::prelude::*;
use gtk4::{DropDown, StringList};

/// A dropdown of already-indexed project paths, populated from
/// `projects.list` - replaces free-text path entry in the Search and
/// Architecture tabs. Typing the exact full path from memory (rather than
/// picking from what's actually indexed) is exactly what caused "no index
/// found for X" errors when someone typed a project's name instead of its
/// full path.
pub fn build() -> DropDown {
    let dropdown = DropDown::from_strings(&[]);
    refresh(&dropdown);
    dropdown
}

pub fn refresh(dropdown: &DropDown) {
    let paths: Vec<String> = match crate::client::call("projects.list", serde_json::json!({})) {
        Ok(serde_json::Value::Array(projects)) => projects
            .iter()
            .filter_map(|p| p.get("root_path").and_then(|v| v.as_str()))
            .map(|s| s.to_string())
            .collect(),
        _ => Vec::new(),
    };
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    dropdown.set_model(Some(&StringList::new(&refs)));
}

pub fn selected_path(dropdown: &DropDown) -> Option<String> {
    dropdown
        .selected_item()
        .and_downcast::<gtk4::StringObject>()
        .map(|s| s.string().to_string())
}
