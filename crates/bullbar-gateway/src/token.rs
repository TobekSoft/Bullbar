//! Bullbar's access tokens: one for the control channel, one for the chat endpoint (ADR 0016).
//! Each is generated on first start into a file of its own in the state folder, readable only by
//! the user, and never printed. They are different types, so code can't take one for the other,
//! and each listener accepts only its own.
//!
//! "Token" here is an access key, not the units a provider bills in.

use std::fmt;
use std::io::Write;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

/// Which token: its file and its name in messages.
pub trait Kind {
    /// The file name in the state folder.
    const FILE: &'static str;
    /// What messages call it.
    const NAME: &'static str;
}

/// The control channel's token: answers questions, works the shutter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForControl {}

impl Kind for ForControl {
    const FILE: &'static str = "control.token";
    const NAME: &'static str = "control token";
}

/// The chat endpoint's token: what chat clients send as their API key. It can send requests and
/// nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForChat {}

impl Kind for ForChat {
    const FILE: &'static str = "chat.token";
    const NAME: &'static str = "chat token";
}

/// Not an access token: the per-install secret that keys the scrubber's surrogates (ADR 0020,
/// point 3). Made and kept the same way: random, in its own file, readable only by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForScrubbing {}

impl Kind for ForScrubbing {
    const FILE: &'static str = "scrub.secret";
    const NAME: &'static str = "scrubbing secret";
}

/// An access token of one kind. Its debug output hides it.
pub struct Token<K> {
    value: String,
    kind: PhantomData<K>,
}

/// The control channel's token.
pub type ControlToken = Token<ForControl>;
/// The chat endpoint's token.
pub type ChatToken = Token<ForChat>;

impl<K> Clone for Token<K> {
    fn clone(&self) -> Self {
        Token {
            value: self.value.clone(),
            kind: PhantomData,
        }
    }
}

impl<K> PartialEq for Token<K> {
    fn eq(&self, other: &Self) -> bool {
        self.matches(&other.value)
    }
}

impl<K> Eq for Token<K> {}

impl<K: Kind> fmt::Debug for Token<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Token({}, hidden)", K::NAME)
    }
}

impl<K> Token<K> {
    fn new(value: String) -> Self {
        Token {
            value,
            kind: PhantomData,
        }
    }

    /// Compares in time that doesn't depend on where the first difference is.
    pub fn matches(&self, offered: &str) -> bool {
        let (a, b) = (self.value.as_bytes(), offered.as_bytes());
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }

    /// Whether a request's `Authorization: Bearer …` header carries this token.
    pub fn authorises(&self, headers: &axum::http::HeaderMap) -> bool {
        headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .is_some_and(|offered| self.matches(offered))
    }

    /// The value, for the command line's client to send. Never print or log it.
    pub(crate) fn expose(&self) -> &str {
        &self.value
    }
}

/// Why a token couldn't be had.
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    /// The state folder or token file couldn't be written or read.
    #[error(
        "{}: the {name} can't be read or written (is this the state folder `serve` uses?)",
        .path.display()
    )]
    Io {
        /// Which token.
        name: &'static str,
        /// Where.
        path: PathBuf,
        /// What went wrong.
        source: std::io::Error,
    },
    /// The system's random source failed.
    #[error("no random source for the {0}: {1}")]
    Random(&'static str, getrandom::Error),
    /// The file holds no token.
    #[error("{} holds no {name}; delete it and restart `bullbar serve`", .path.display())]
    Empty {
        /// Which token.
        name: &'static str,
        /// Where.
        path: PathBuf,
    },
}

/// The token file's path in the state folder.
pub fn path<K: Kind>(state: &Path) -> PathBuf {
    state.join(K::FILE)
}

/// Reads a token from the state folder, creating both on first start: the folder readable only by
/// the user, the file too.
pub fn load_or_create<K: Kind>(state: &Path) -> Result<Token<K>, TokenError> {
    let path = path::<K>(state);
    if path.exists() {
        return read(state);
    }
    let io = |source| TokenError::Io {
        name: K::NAME,
        path: path.clone(),
        source,
    };
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(state).map_err(io)?;
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| TokenError::Random(K::NAME, e))?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&path).map_err(io)?;
    file.write_all(format!("{token}\n").as_bytes())
        .map_err(io)?;
    Ok(Token::new(token))
}

/// Reads an existing token.
pub fn read<K: Kind>(state: &Path) -> Result<Token<K>, TokenError> {
    let path = path::<K>(state);
    let text = std::fs::read_to_string(&path).map_err(|source| TokenError::Io {
        name: K::NAME,
        path: path.clone(),
        source,
    })?;
    let token = text.trim();
    if token.is_empty() {
        return Err(TokenError::Empty {
            name: K::NAME,
            path,
        });
    }
    Ok(Token::new(token.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("bullbar-token-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn each_token_is_made_once_kept_and_private() {
        let dir = state("once");
        let control = load_or_create::<ForControl>(&dir).unwrap();
        let chat = load_or_create::<ForChat>(&dir).unwrap();
        assert_eq!(load_or_create::<ForControl>(&dir).unwrap(), control);
        assert_eq!(read::<ForChat>(&dir).unwrap(), chat);
        // Two tokens, not one under two names.
        assert!(!control.matches(chat.expose()));
        assert_eq!(format!("{chat:?}"), "Token(chat token, hidden)");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir), 0o700);
            assert_eq!(mode(&dir.join("control.token")), 0o600);
            assert_eq!(mode(&dir.join("chat.token")), 0o600);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tokens_match_only_exactly() {
        let token = ChatToken::new("abc123".into());
        assert!(token.matches("abc123"));
        for wrong in ["", "abc12", "abc1234", "abc124"] {
            assert!(!token.matches(wrong), "{wrong}");
        }
    }
}
