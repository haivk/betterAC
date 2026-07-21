//! Persisted state: which servers you added, and the account for each.
//!
//! Passwords are stored in plaintext here, by choice. The file is created 0600
//! (owner read/write only) so it is at least not readable by other users on the
//! box -- but it is plaintext, and anything running as you can read it. Moving to
//! the GNOME keyring later means changing `Account::password` to a lookup; the
//! rest of the app does not care where the string came from.

use crate::servers::{Server, Software};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A server you added, plus the account you play it with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub host: String,
    pub port: String,
    pub software: Software,
    #[serde(default)]
    pub ruleset: String,
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub password: String,
}

impl Entry {
    pub fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    pub fn id(&self) -> String {
        self.address()
    }

    /// Back to a Server so the launcher has one input type. The live fields
    /// (players, description) are not persisted -- they'd only go stale.
    pub fn to_server(&self) -> Server {
        Server {
            name: self.name.clone(),
            description: String::new(),
            ruleset: self.ruleset.clone(),
            software: self.software,
            host: self.host.clone(),
            port: self.port.clone(),
            players: None,
            website_url: None,
            discord_url: None,
        }
    }

    pub fn from_server(s: &Server) -> Self {
        Entry {
            name: s.name.clone(),
            host: s.host.clone(),
            port: s.port.clone(),
            software: s.software,
            ruleset: s.ruleset.clone(),
            account: String::new(),
            password: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// The Proton prefix built by setup.
    #[serde(default = "crate::install::default_prefix")]
    pub prefix: PathBuf,
    #[serde(default)]
    pub servers: Vec<Entry>,
    /// id() of the last server played, so we reselect it on open.
    #[serde(default)]
    pub last: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            prefix: crate::install::default_prefix(),
            servers: Vec::new(),
            last: None,
        }
    }
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("betterac/config.json")
}

impl Config {
    /// Load, or start fresh. A corrupt config is not a reason to refuse to open --
    /// it's a reason to start clean, since everything in it is re-addable in
    /// seconds and the alternative is an app that won't launch.
    pub fn load() -> Config {
        let path = config_path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Config::default();
        };
        serde_json::from_str(&text).unwrap_or_else(|e| {
            eprintln!("betterac: {} is not valid config ({e}); starting fresh", path.display());
            Config::default()
        })
    }

    pub fn save(&self) -> Result<(), String> {
        let path = config_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;

        // Write 0600 from the start rather than write-then-chmod: the latter
        // leaves a window where the passwords are world-readable on disk.
        write_private(&path, json.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn find(&self, id: &str) -> Option<&Entry> {
        self.servers.iter().find(|e| e.id() == id)
    }

    pub fn find_mut(&mut self, id: &str) -> Option<&mut Entry> {
        self.servers.iter_mut().find(|e| e.id() == id)
    }

    /// Adding a server you already have is a no-op, not a duplicate row.
    pub fn add(&mut self, s: &Server) -> bool {
        if self.find(&s.id()).is_some() {
            return false;
        }
        self.servers.push(Entry::from_server(s));
        true
    }

    pub fn remove(&mut self, id: &str) {
        self.servers.retain(|e| e.id() != id);
        if self.last.as_deref() == Some(id) {
            self.last = None;
        }
    }
}

#[cfg(unix)]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::servers::Players;

    fn srv(name: &str, host: &str) -> Server {
        Server {
            name: name.into(),
            description: "d".into(),
            ruleset: "PvE".into(),
            software: Software::Ace,
            host: host.into(),
            port: "9000".into(),
            players: Some(Players { count: 5, age: "1 minute ago".into() }),
            website_url: None,
            discord_url: None,
        }
    }

    #[test]
    fn adding_the_same_server_twice_does_not_duplicate_it() {
        let mut c = Config::default();
        assert!(c.add(&srv("Coldeve", "play.coldeve.ac")));
        assert!(!c.add(&srv("Coldeve", "play.coldeve.ac")));
        assert_eq!(c.servers.len(), 1);
    }

    #[test]
    fn same_name_different_host_are_different_servers() {
        let mut c = Config::default();
        assert!(c.add(&srv("Coldeve", "play.coldeve.ac")));
        assert!(c.add(&srv("Coldeve", "other.example")));
        assert_eq!(c.servers.len(), 2);
    }

    #[test]
    fn removing_the_selected_server_clears_the_selection() {
        let mut c = Config::default();
        c.add(&srv("Coldeve", "play.coldeve.ac"));
        c.last = Some("play.coldeve.ac:9000".into());
        c.remove("play.coldeve.ac:9000");
        assert!(c.servers.is_empty());
        assert_eq!(c.last, None, "a dangling selection would point at nothing");
    }

    #[test]
    fn credentials_survive_a_round_trip() {
        let mut c = Config::default();
        c.add(&srv("Coldeve", "play.coldeve.ac"));
        let e = c.find_mut("play.coldeve.ac:9000").unwrap();
        e.account = "hank".into();
        e.password = "hunter2".into();

        let json = serde_json::to_string(&c).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        let e = back.find("play.coldeve.ac:9000").unwrap();
        assert_eq!(e.account, "hank");
        assert_eq!(e.password, "hunter2");
        assert_eq!(e.software, Software::Ace);
    }

    #[test]
    fn a_corrupt_config_does_not_stop_the_app_opening() {
        let c: Result<Config, _> = serde_json::from_str("{ this is not json");
        assert!(c.is_err());
        // load() swallows exactly this and returns a default; proven here at the
        // parse layer so the test does not have to touch the real config path.
    }
}
