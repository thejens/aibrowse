use std::path::PathBuf;

use aibrowse_input::Persona;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::egress::Egress;
use crate::error::{BrowserError, Result};
use crate::identity::Display;

/// A persistent browser identity: Chrome's user-data-dir, the behavioral persona, and the
/// display the browser presents. All must stay stable for as long as the profile's
/// accounts exist: sites remember devices, and a device that changes shape is suspicious.
///
/// Layout: `~/.aibrowse/profiles/<name>/{chrome/, persona.json, display.json}`.
#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub root: PathBuf,
    pub persona: Persona,
    pub display: Display,
    /// The exit all its traffic leaves through, when pinned (`policy.json`'s
    /// `egress`, or `AIBROWSE_EGRESS`).
    pub egress: Option<Egress>,
}

impl Profile {
    pub fn open(name: &str) -> Result<Self> {
        let root = Self::root_of(name)?;
        std::fs::create_dir_all(root.join("chrome"))?;

        let persona = load_or_create(&root.join("persona.json"), || {
            Persona::from_seed(rand::random())
        })?;
        let display = load_or_create(&root.join("display.json"), Display::detect)?;
        let policy = std::fs::read(root.join("policy.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        let egress = Egress::for_policy(policy.as_ref())?;
        Ok(Self {
            name: name.to_owned(),
            root,
            persona,
            display,
            egress,
        })
    }

    /// Where the profile `name` lives (`~/.aibrowse/profiles/<name>`), whether or not
    /// it exists yet.
    pub fn root_of(name: &str) -> Result<PathBuf> {
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(BrowserError::Profile(format!(
                "invalid profile name {name:?}: use letters, digits, '-' and '_'"
            )));
        }
        let home = dirs::home_dir()
            .ok_or_else(|| BrowserError::Profile("cannot determine home directory".into()))?;
        Ok(home.join(".aibrowse/profiles").join(name))
    }

    pub fn user_data_dir(&self) -> PathBuf {
        self.root.join("chrome")
    }

    /// Where this profile's downloads land: `~/.aibrowse/downloads/<name>/`, apart from
    /// the user's own Downloads folder, and outside the profile so that clearing
    /// downloads never touches logins.
    pub fn downloads_dir(&self) -> PathBuf {
        let aibrowse = self
            .root
            .parent()
            .and_then(|profiles| profiles.parent())
            .unwrap_or(&self.root);
        aibrowse.join("downloads").join(&self.name)
    }

    /// Fails fast when another Chrome already runs this profile. Chrome would otherwise
    /// hand our launch over to the running instance and exit, leaving us waiting on a
    /// browser that never answers.
    pub fn ensure_not_in_use(&self) -> Result<()> {
        // Chrome's lock is a symlink whose target is "<hostname>-<pid>".
        let Ok(target) = std::fs::read_link(self.user_data_dir().join("SingletonLock")) else {
            return Ok(());
        };
        let pid = target
            .to_string_lossy()
            .rsplit('-')
            .next()
            .and_then(|pid| pid.parse::<u32>().ok());
        let alive = pid.is_some_and(|pid| {
            std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        });
        if let Some(pid) = pid.filter(|_| alive)
            && self.is_orphaned_chrome(pid)
        {
            // Left behind by a server that died without closing it (killed, crashed).
            // Nothing drives it any more, so reclaim the profile.
            tracing::warn!(pid, profile = %self.name, "terminating orphaned Chrome holding this profile");
            terminate(pid);
            return Ok(());
        }
        if alive {
            return Err(BrowserError::Profile(format!(
                "profile {:?} is already open in another browser session (Chrome pid {}); close it or use a different profile",
                self.name,
                pid.unwrap_or_default()
            )));
        }
        Ok(())
    }
}

impl Profile {
    /// Whether `pid` is a Chrome running this profile that its server abandoned: it was
    /// re-parented to launchd/init (parent pid 1) when its parent process died.
    fn is_orphaned_chrome(&self, pid: u32) -> bool {
        let Ok(output) = std::process::Command::new("ps")
            .args(["-o", "ppid=,command=", "-p", &pid.to_string()])
            .output()
        else {
            return false;
        };
        let line = String::from_utf8_lossy(&output.stdout);
        let line = line.trim();
        let parent_is_init = line.split_whitespace().next() == Some("1");
        let user_data_dir = format!("--user-data-dir={}", self.user_data_dir().display());
        parent_is_init && line.contains(&user_data_dir)
    }
}

/// SIGTERM, then SIGKILL if the process is still there after a few seconds.
fn terminate(pid: u32) {
    let signal = |sig: &str| {
        std::process::Command::new("kill")
            .args([sig, &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    signal("-TERM");
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if !signal("-0") {
            return;
        }
    }
    signal("-KILL");
}

/// Reads a JSON file, or creates it from `create` on first use. Values are generated once
/// and then fixed for the profile's lifetime.
fn load_or_create<T, F>(path: &std::path::Path, create: F) -> Result<T>
where
    T: Serialize + DeserializeOwned,
    F: FnOnce() -> T,
{
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| BrowserError::Profile(format!("corrupt {}: {e}", path.display()))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let value = create();
            let json = serde_json::to_vec_pretty(&value)
                .map_err(|e| BrowserError::Profile(e.to_string()))?;
            std::fs::write(path, json)?;
            Ok(value)
        }
        Err(err) => Err(err.into()),
    }
}
