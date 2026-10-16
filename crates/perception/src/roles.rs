//! Role classification. Chrome's AX roles are mostly ARIA names, plus a few internal ones
//! ("StaticText", "RootWebArea", "DisclosureTriangle").

/// Roles an agent can act on; these get a ref.
pub fn is_interactive(role: &str) -> bool {
    matches!(
        role,
        "button"
            | "link"
            | "textbox"
            | "searchbox"
            | "combobox"
            | "listbox"
            | "option"
            | "checkbox"
            | "radio"
            | "switch"
            | "slider"
            | "spinbutton"
            | "tab"
            | "menuitem"
            | "menuitemcheckbox"
            | "menuitemradio"
            | "treeitem"
            | "DisclosureTriangle"
            | "MenuListOption"
            | "PopUpButton"
    )
}

/// Structure that helps the agent orient: kept as headers in the outline.
pub fn is_landmark(role: &str) -> bool {
    matches!(
        role,
        "banner"
            | "navigation"
            | "main"
            | "complementary"
            | "contentinfo"
            | "search"
            | "form"
            | "region"
            | "dialog"
            | "alertdialog"
            | "alert"
            | "menu"
            | "menubar"
            | "tablist"
            | "tabpanel"
            | "toolbar"
            | "grid"
            | "table"
            | "list"
            | "Iframe"
    )
}

/// Content worth showing at `Detail::Full`.
pub fn is_content(role: &str) -> bool {
    matches!(
        role,
        "heading"
            | "paragraph"
            | "image"
            | "img"
            | "listitem"
            | "cell"
            | "row"
            | "status"
            | "StaticText"
    )
}

/// Landmarks that are only meaningful when they have a name.
pub fn needs_name(role: &str) -> bool {
    matches!(role, "region" | "form" | "list" | "table" | "grid")
}
