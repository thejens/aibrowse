//! Chrome switches one operating system needs and the others must not get.
//!
//! Each one exists because the OS would otherwise interrupt or break the session, or
//! make headful and headless look different to a page.

/// The switches for the OS this binary runs on, in the given mode.
pub(crate) fn switches(headless: bool) -> Vec<String> {
    let mut switches = Vec::new();
    // Without this, Chrome for Testing on macOS asks for Keychain access to encrypt
    // cookies. The mock key is constant, so cookies still persist across runs; they are
    // simply not protected by the Keychain (acceptable for the local PoC only).
    if cfg!(target_os = "macos") {
        switches.push("--use-mock-keychain".to_owned());
    }
    if cfg!(target_os = "linux") {
        // The Linux counterpart: on a desktop, Chrome would ask to unlock the GNOME
        // keyring or KWallet; on a server there is none and it would probe D-Bus for one.
        // "basic" encrypts cookies with a fixed key, so they persist just the same.
        switches.push("--password-store=basic".to_owned());
        // On Linux a horizontal wheel past the page's left edge navigates back, the
        // touchpad swipe gesture; on macOS only real trackpad gestures do. Synthetic
        // wheel ticks count as such a swipe here, so aiming's sideways scroll on a page
        // that can't scroll sideways left it (history back). Browser behavior only:
        // pages can't observe the setting. (Chrome reads only the last
        // --disable-features switch; further features belong in this one.)
        switches.push("--disable-features=TouchpadOverscrollHistoryNavigation".to_owned());
        // Without a GPU (a server, a container: no /dev/dri), headful Chrome on Xvfb
        // has no working GL and pages get no WebGL at all, while headless renders WebGL
        // with SwiftShader by itself. Headful is pointed at SwiftShader too, so the modes
        // look the same. Headless must not get the switch: with it, text typed into
        // the live view during a handoff never submitted its form (cause not yet
        // known).
        if !headless && !std::path::Path::new("/dev/dri").exists() {
            switches.push("--use-angle=swiftshader".to_owned());
        }
        // Headless Chrome on Linux has no input devices, so pages see `(hover: none)`
        // and `(pointer: none)`, a touch-only device, where headful (desktop or Xvfb)
        // has a mouse. Media emulation ignores these features; Blink's settings declare
        // the mouse instead (pointer 4 = fine, hover 2 = hover). macOS headless has one.
        if headless {
            switches.push(
                "--blink-settings=primaryPointerType=4,availablePointerTypes=4,primaryHoverType=2,availableHoverTypes=2"
                    .to_owned(),
            );
        }
        // Chrome puts shared memory (rendering, IPC) in /dev/shm and crashes tabs when it
        // runs out. Docker gives containers 64 MiB there by default; when it is that
        // small, Chrome is told to use the temp directory instead.
        if shm_size("/proc/mounts").is_some_and(|size| size < MIN_SHM_BYTES) {
            switches.push("--disable-dev-shm-usage".to_owned());
        }
    }
    switches
}

/// Below this, /dev/shm is a container's default allotment rather than a host's (a
/// desktop's is half its RAM).
const MIN_SHM_BYTES: u64 = 1 << 30;

/// The size of the /dev/shm tmpfs, from a mount table in /proc/mounts format.
fn shm_size(mounts_path: &str) -> Option<u64> {
    let mounts = std::fs::read_to_string(mounts_path).ok()?;
    parse_shm_size(&mounts)
}

fn parse_shm_size(mounts: &str) -> Option<u64> {
    // "shm /dev/shm tmpfs rw,nosuid,nodev,noexec,relatime,size=65536k 0 0"
    let options = mounts.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let _source = fields.next()?;
        (fields.next()? == "/dev/shm").then(|| fields.nth(1))?
    })?;
    let size = options.split(',').find_map(|o| o.strip_prefix("size="))?;
    let (digits, unit) = size.split_at(size.find(|c: char| !c.is_ascii_digit())?);
    let scale = match unit {
        "k" => 1 << 10,
        "m" => 1 << 20,
        "g" => 1 << 30,
        _ => return None,
    };
    digits.parse::<u64>().ok().map(|n| n * scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_shm_size_from_the_mount_table() {
        let docker = "overlay / overlay rw 0 0\nshm /dev/shm tmpfs rw,nosuid,nodev,noexec,relatime,size=65536k 0 0\n";
        assert_eq!(parse_shm_size(docker), Some(64 << 20));
        let sized = "shm /dev/shm tmpfs rw,nosuid,nodev,noexec,relatime,size=2097152k 0 0\n";
        assert_eq!(parse_shm_size(sized), Some(2 << 30));
        assert_eq!(parse_shm_size("proc /proc proc rw 0 0\n"), None);
    }
}
