//! Chrome preferences aibrowse sets in a profile before each launch. Chrome reads its
//! preferences once at startup, so these are written to disk rather than sent over CDP.

use std::path::Path;

use serde_json::{Value, json};

use crate::error::Result;

/// `session.restore_on_startup` value for "open the New Tab page".
const OPEN_NEW_TAB_PAGE: i64 = 5;

/// Prepares the profile in `user_data_dir` for a launch, keeping every other preference:
///
/// - Downloads go to `downloads_dir` without a "where to save" prompt. (Setting this over
///   CDP with `Browser.setDownloadBehavior` crashes Chrome 154 once the profile has
///   download history.)
/// - No tab restore. aibrowse restores the agent's page itself; tabs Chrome brings back
///   from earlier sessions pile up in the background and can wedge input (a new tab
///   opened by a click then never finished dispatching). The saved session files are
///   removed too, since a crashed or killed exit restores regardless of the setting.
/// - The last exit counts as clean, so Chrome doesn't offer to restore after a crash or a
///   kill at shutdown (headless Chrome often outlives its teardown watchdog).
pub(crate) fn prepare(user_data_dir: &Path, downloads_dir: &Path) -> Result<()> {
    let default = user_data_dir.join("Default");
    let path = default.join("Preferences");
    let mut prefs: Value = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|err| {
            // Chrome discards a corrupt file anyway; starting from ours loses nothing.
            tracing::warn!(%err, path = %path.display(), "unreadable Chrome preferences");
            json!({})
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(err) => return Err(err.into()),
    };
    if !prefs.is_object() {
        prefs = json!({});
    }
    set(
        &mut prefs,
        "download",
        "default_directory",
        json!(downloads_dir),
    );
    set(&mut prefs, "download", "prompt_for_download", json!(false));
    // Marks the directory as already migrated, so Chrome keeps it as set.
    set(&mut prefs, "download", "directory_upgrade", json!(true));
    set(
        &mut prefs,
        "session",
        "restore_on_startup",
        json!(OPEN_NEW_TAB_PAGE),
    );
    set(&mut prefs, "profile", "exit_type", json!("Normal"));
    std::fs::create_dir_all(&default)?;
    let bytes = serde_json::to_vec(&prefs).expect("JSON values always serialize");
    std::fs::write(&path, bytes)?;
    match std::fs::remove_dir_all(default.join("Sessions")) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err.into()),
        _ => Ok(()),
    }
}

/// Sets `prefs[section][key]`, creating the section if needed.
fn set(prefs: &mut Value, section: &str, key: &str, value: Value) {
    let entry = &mut prefs[section];
    if !entry.is_object() {
        *entry = json!({});
    }
    entry[key] = value;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepares_the_profile_and_keeps_the_rest() {
        let root = std::env::temp_dir().join(format!("aibrowse-prefs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let default = root.join("chrome/Default");
        std::fs::create_dir_all(default.join("Sessions")).unwrap();
        std::fs::write(default.join("Sessions/Tabs_1"), "x").unwrap();
        std::fs::write(
            default.join("Preferences"),
            r#"{"intl":{"accept_languages":"sv"},"download":{"x":1},"profile":{"exit_type":"Crashed","name":"p"}}"#,
        )
        .unwrap();
        prepare(&root.join("chrome"), &root.join("downloads")).unwrap();
        let written: Value =
            serde_json::from_slice(&std::fs::read(default.join("Preferences")).unwrap()).unwrap();
        assert_eq!(written["intl"]["accept_languages"], "sv");
        assert_eq!(written["download"]["x"], 1);
        assert_eq!(written["download"]["prompt_for_download"], false);
        assert_eq!(
            written["download"]["default_directory"],
            json!(root.join("downloads"))
        );
        assert_eq!(written["session"]["restore_on_startup"], OPEN_NEW_TAB_PAGE);
        assert_eq!(written["profile"]["exit_type"], "Normal");
        assert_eq!(written["profile"]["name"], "p");
        assert!(!default.join("Sessions").exists());
        // A fresh profile: no preferences or sessions yet.
        let fresh = root.join("fresh");
        prepare(&fresh, &root.join("downloads")).unwrap();
        assert!(fresh.join("Default/Preferences").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
