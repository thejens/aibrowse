//! Files and folders only the user can read: tokens, traces. On macOS and Linux that is
//! the mode (0600, 0700), set as the file is created so it is never readable by others,
//! not even briefly. On Windows, files under the user's profile directory are private to
//! the user by its default ACLs, which new files inherit.

use std::fs::OpenOptions;
use std::path::Path;

/// Makes `OpenOptions` create the file readable by the user only.
pub(crate) trait Private {
    fn private(&mut self) -> &mut Self;
}

impl Private for OpenOptions {
    fn private(&mut self) -> &mut Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            self.mode(0o600);
        }
        self
    }
}

/// Creates `dir` (and parents) readable by the user only, and tightens it if it existed.
pub(crate) fn private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}
