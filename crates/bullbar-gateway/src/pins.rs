//! Topic pins (ADR 0021): which cloud account each topic's Personal and Sensitive work goes to.
//! The first account a topic leaves for becomes its pin; moving it needs the user's yes. Kept in
//! `pins.json` in the state folder, readable only by the user, written whole and renamed into
//! place so a crash never leaves half a file.
//!
//! A topic is a request's category (ADR 0021: the classifier will refine this in chunk 4).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use bullbar_core::levels::Category;
use bullbar_core::provider::AccountId;

/// The file's name in the state folder.
pub const FILE: &str = "pins.json";

/// Why pins couldn't be read or kept.
#[derive(Debug, thiserror::Error)]
pub enum PinsError {
    /// The file couldn't be read or written.
    #[error("{}: the topic pins can't be read or written: {source}", .path.display())]
    Io {
        /// Where.
        path: PathBuf,
        /// What went wrong.
        source: std::io::Error,
    },
    /// The file isn't one Bullbar wrote.
    #[error("{}: not a pins file Bullbar wrote ({reason}); fix or delete it", .path.display())]
    Unreadable {
        /// Where.
        path: PathBuf,
        /// What's wrong with it.
        reason: String,
    },
    /// The topic was pinned elsewhere while this request was being decided.
    #[error("the {0} topic was pinned elsewhere meanwhile")]
    Moved(String),
}

/// The pins, in memory and on disk.
pub struct Pins {
    path: PathBuf,
    map: Mutex<BTreeMap<String, String>>,
}

impl Pins {
    /// Reads the state folder's pins; none yet is an empty set. A file that can't be read stops
    /// `serve` starting: without it, a topic could quietly land somewhere new.
    pub fn load(state: &Path) -> Result<Self, PinsError> {
        let path = state.join(FILE);
        let map = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<BTreeMap<String, String>>(&text).map_err(|e| {
                PinsError::Unreadable {
                    path: path.clone(),
                    reason: e.to_string(),
                }
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(source) => return Err(PinsError::Io { path, source }),
        };
        for account in map.values() {
            if account.parse::<AccountId>().is_err() {
                return Err(PinsError::Unreadable {
                    path,
                    reason: format!("`{account}` isn't an account name"),
                });
            }
        }
        Ok(Pins {
            path,
            map: Mutex::new(map),
        })
    }

    /// The account a topic is pinned to.
    pub fn get(&self, topic: &Category) -> Option<AccountId> {
        let map = self.map.lock().ok()?;
        map.get(&topic.to_string()).and_then(|a| a.parse().ok())
    }

    /// Every pin, topic and account.
    pub fn all(&self) -> Vec<(String, String)> {
        self.map
            .lock()
            .map(|m| m.iter().map(|(t, a)| (t.clone(), a.clone())).collect())
            .unwrap_or_default()
    }

    /// Pins `topic` to `account`, provided it is still pinned where the decision saw it
    /// (`expected`, `None` for not pinned). Written to disk before it counts.
    pub fn record(
        &self,
        topic: &Category,
        account: &AccountId,
        expected: Option<&AccountId>,
    ) -> Result<(), PinsError> {
        let mut map = self.map.lock().map_err(|_| PinsError::Unreadable {
            path: self.path.clone(),
            reason: "a writer failed part-way".to_owned(),
        })?;
        let key = topic.to_string();
        let current = map.get(&key).map(String::as_str);
        if current == Some(account.as_str()) {
            return Ok(());
        }
        if current != expected.map(AccountId::as_str) {
            return Err(PinsError::Moved(key));
        }
        let mut next = map.clone();
        next.insert(key, account.to_string());
        self.write(&next)?;
        *map = next;
        Ok(())
    }

    fn write(&self, map: &BTreeMap<String, String>) -> Result<(), PinsError> {
        let io = |source| PinsError::Io {
            path: self.path.clone(),
            source,
        };
        let text = serde_json::to_string_pretty(map).map_err(|e| PinsError::Unreadable {
            path: self.path.clone(),
            reason: e.to_string(),
        })?;
        let temporary = self.path.with_extension("json.new");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temporary).map_err(io)?;
        std::io::Write::write_all(&mut file, format!("{text}\n").as_bytes()).map_err(io)?;
        file.sync_all().map_err(io)?;
        std::fs::rename(&temporary, &self.path).map_err(io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("bullbar-pins-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn id(s: &str) -> AccountId {
        s.parse().unwrap()
    }

    #[test]
    fn a_pin_is_kept_across_restarts_and_moves_only_from_where_it_was() {
        let dir = state("kept");
        let pins = Pins::load(&dir).unwrap();
        assert_eq!(pins.get(&Category::Health), None);
        pins.record(&Category::Health, &id("provider-a"), None)
            .unwrap();
        // A decision that saw no pin can't take the topic elsewhere now.
        assert!(matches!(
            pins.record(&Category::Health, &id("provider-b"), None),
            Err(PinsError::Moved(_))
        ));
        let again = Pins::load(&dir).unwrap();
        assert_eq!(again.get(&Category::Health), Some(id("provider-a")));
        again
            .record(
                &Category::Health,
                &id("provider-b"),
                Some(&id("provider-a")),
            )
            .unwrap();
        assert_eq!(
            Pins::load(&dir).unwrap().get(&Category::Health),
            Some(id("provider-b"))
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_damaged_file_is_an_error_not_an_empty_set() {
        let dir = state("damaged");
        std::fs::write(dir.join(FILE), "{ not json").unwrap();
        assert!(matches!(
            Pins::load(&dir),
            Err(PinsError::Unreadable { .. })
        ));
        std::fs::write(dir.join(FILE), r#"{ "health": "not an id!" }"#).unwrap();
        assert!(matches!(
            Pins::load(&dir),
            Err(PinsError::Unreadable { .. })
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
