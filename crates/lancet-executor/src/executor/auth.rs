use std::collections::BTreeSet;

pub(super) fn render_enabled_tools(authorized: &BTreeSet<String>) -> String {
    if authorized.is_empty() {
        "<none>".to_string()
    } else {
        authorized.iter().cloned().collect::<Vec<_>>().join(", ")
    }
}
