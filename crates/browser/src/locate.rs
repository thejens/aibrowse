use std::path::{Path, PathBuf};

use crate::error::{BrowserError, Result};

/// Finds the Chrome binary to drive, in order of preference:
///
/// 1. `$AIBROWSE_CHROME`, an explicit override.
/// 2. The newest Chrome for Testing under `~/.aibrowse/chrome` (`make chrome` installs it).
///    It is version-pinned, so CDP behavior doesn't shift under us on auto-update.
/// 3. A system Google Chrome, which is the most realistic fingerprint but auto-updates;
///    on Linux, else the distribution's Chromium (Chrome for Testing has no linux-arm64
///    build, so that is what arm64 servers run).
pub fn find_chrome() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("AIBROWSE_CHROME") {
        let path = PathBuf::from(path);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(BrowserError::ChromeNotFound(format!(
                "AIBROWSE_CHROME points to {}, which is not a file",
                path.display()
            )))
        };
    }
    if let Some(path) = newest_chrome_for_testing() {
        return Ok(path);
    }
    SYSTEM_CHROME
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .ok_or_else(|| {
            BrowserError::ChromeNotFound(
                "no Chrome for Testing in ~/.aibrowse/chrome (run `make chrome`) and no system Chrome"
                    .into(),
            )
        })
}

#[cfg(target_os = "macos")]
const SYSTEM_CHROME: &[&str] = &["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"];
/// Real binaries come before launcher scripts: Debian's `/usr/bin/chromium` wrapper adds
/// switches of its own (`--load-extension`, `--enable-remote-extensions`, ...) that would
/// make the browser differ from the one we configure.
#[cfg(not(target_os = "macos"))]
const SYSTEM_CHROME: &[&str] = &[
    "/opt/google/chrome/chrome",
    "/usr/bin/google-chrome-stable",
    "/usr/bin/google-chrome",
    "/usr/lib/chromium/chromium",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
];

/// `@puppeteer/browsers` installs into `<root>/chrome/<platform>-<version>/...`.
fn newest_chrome_for_testing() -> Option<PathBuf> {
    let root = dirs::home_dir()?.join(".aibrowse/chrome/chrome");
    let mut installs: Vec<(Vec<u32>, PathBuf)> = std::fs::read_dir(&root)
        .ok()?
        .filter_map(|entry| {
            let dir = entry.ok()?.path();
            let version = dir.file_name()?.to_str()?.rsplit('-').next()?.to_owned();
            let version: Vec<u32> = version
                .split('.')
                .map(|p| p.parse().ok())
                .collect::<Option<_>>()?;
            Some((version, binary_in(&dir)?))
        })
        .collect();
    installs.sort();
    installs.pop().map(|(_, path)| path)
}

fn binary_in(install_dir: &Path) -> Option<PathBuf> {
    [
        "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
        "chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
        "chrome-linux64/chrome",
    ]
    .iter()
    .map(|rel| install_dir.join(rel))
    .find(|p| p.is_file())
}
