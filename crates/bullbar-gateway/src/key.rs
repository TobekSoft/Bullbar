//! API keys, read from the file an account names (`key_file`). The key lives only in memory, in a
//! type whose debug output hides it; it is never printed, logged, or put into config or a reply.

use std::fmt;
use std::path::{Path, PathBuf};

/// An API key.
#[derive(Clone)]
pub struct ApiKey(String);

impl ApiKey {
    /// The key, for the one place that sends it: the provider client.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(hidden)")
    }
}

/// Why a key file can't be used. Names the file, never its content.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// Missing, or not readable by Bullbar.
    #[error("key file {} can't be read", .0.display())]
    Unreadable(PathBuf),
    /// Holds nothing but white space.
    #[error("key file {} is empty", .0.display())]
    Empty(PathBuf),
    /// Holds more than one line, or characters a key never has.
    #[error("key file {} should hold the key alone, on one line", .0.display())]
    NotAKey(PathBuf),
}

/// Reads a key file: one line of visible characters, surrounding white space ignored.
pub fn read(path: &Path) -> Result<ApiKey, KeyError> {
    let text = std::fs::read_to_string(path).map_err(|_| KeyError::Unreadable(path.to_owned()))?;
    let key = text.trim();
    if key.is_empty() {
        return Err(KeyError::Empty(path.to_owned()));
    }
    if !key.chars().all(|c| c.is_ascii_graphic()) {
        return Err(KeyError::NotAKey(path.to_owned()));
    }
    Ok(ApiKey(key.to_owned()))
}

/// Whether other users on this machine can read the file (Unix permissions; elsewhere, unknown).
pub fn readable_by_others(path: &Path) -> Option<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).ok()?.permissions().mode();
        Some(mode & 0o077 != 0)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, content: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("bullbar-key-test-{}-{name}", std::process::id()));
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn a_key_is_read_trimmed_and_hidden_in_debug() {
        let path = file("ok", "  fake-key-0000\n");
        let key = read(&path).unwrap();
        assert_eq!(key.expose(), "fake-key-0000");
        assert_eq!(format!("{key:?}"), "ApiKey(hidden)");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn bad_key_files_are_named_without_their_content() {
        let missing = std::env::temp_dir().join("bullbar-key-test-does-not-exist");
        assert_eq!(
            read(&missing).unwrap_err(),
            KeyError::Unreadable(missing.clone())
        );
        let empty = file("empty", " \n");
        assert_eq!(read(&empty).unwrap_err(), KeyError::Empty(empty.clone()));
        let two = file("two", "fake-one\nfake-two\n");
        let err = read(&two).unwrap_err();
        assert_eq!(err, KeyError::NotAKey(two.clone()));
        assert!(!err.to_string().contains("fake-one"));
        for path in [empty, two] {
            std::fs::remove_file(path).unwrap();
        }
    }
}
