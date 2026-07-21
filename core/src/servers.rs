//! The server directory, from treestats.net.
//!
//! Live feed at https://treestats.net/servers.json -- 44 servers at time of
//! writing, each with the host/port and, crucially, the `software` field, which
//! is what decides the client's command-line shape (see `launcher`).
//!
//! Nothing here is hardcoded, but a snapshot is compiled in as a fallback so the
//! app still opens with a usable list when treestats is down or you're offline.

use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const DIRECTORY_URL: &str = "https://treestats.net/servers.json";

/// Shipped as the offline fallback. Refresh with `make snapshot`.
const SNAPSHOT: &str = include_str!("../data/servers-snapshot.json");

/// Which emulator the server runs. This is not cosmetic -- ACE and GDLE take
/// their account details in genuinely different argument shapes, and getting it
/// wrong just bounces you at the login screen with no useful error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Software {
    /// The default: ~85% of the directory, and the safer guess for anything new.
    #[default]
    Ace,
    Gdle,
}

impl Software {
    /// The feed is not consistent: it uses both "GDL" and "GDLE" for the same
    /// thing. Anything unrecognised is treated as ACE, which is what ~85% of the
    /// list runs and is the safer default.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_uppercase().as_str() {
            "GDL" | "GDLE" => Software::Gdle,
            _ => Software::Ace,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Software::Ace => "ACE",
            Software::Gdle => "GDLE",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Players {
    pub count: u32,
    /// Human string from the feed, e.g. "7 minutes ago" or "a day ago".
    #[serde(default)]
    pub age: String,
}

impl Players {
    /// A count from a day ago is not a player count, it's a historical note. The
    /// feed hands us the age as prose rather than a timestamp we can subtract, so
    /// we go by what it says: anything measured in minutes or hours is current.
    pub fn is_fresh(&self) -> bool {
        let a = self.age.to_ascii_lowercase();
        a.contains("second") || a.contains("minute") || a.contains("hour")
    }
}

/// One entry in the directory. Mirrors the treestats JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Server {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// "PvE" or "PvP".
    #[serde(default, rename = "type")]
    pub ruleset: String,
    #[serde(default, deserialize_with = "de_software")]
    pub software: Software,
    pub host: String,
    /// The feed quotes this ("9000"), so it is a string, not a number.
    pub port: String,
    #[serde(default)]
    pub players: Option<Players>,
    #[serde(default)]
    pub website_url: Option<String>,
    #[serde(default)]
    pub discord_url: Option<String>,
}

fn de_software<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Software, D::Error> {
    let s = String::deserialize(d)?;
    Ok(Software::parse(&s))
}

impl Server {
    pub fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Live population, or None if the server doesn't report it (15 of 44) or the
    /// number is too stale to be worth showing as current.
    pub fn online(&self) -> Option<u32> {
        self.players.as_ref().filter(|p| p.is_fresh()).map(|p| p.count)
    }

    /// Stable identity. Two servers can share a name across a rename, but the
    /// address is what you actually connect to.
    pub fn id(&self) -> String {
        self.address()
    }
}

/// Parse a directory payload. Entries that are missing a host or port are
/// dropped rather than allowed to become an unlaunchable row in the UI.
pub fn parse(json: &str) -> Result<Vec<Server>, serde_json::Error> {
    let mut list: Vec<Server> = serde_json::from_str(json)?;
    list.retain(|s| !s.host.trim().is_empty() && !s.port.trim().is_empty());
    sort(&mut list);
    Ok(list)
}

/// Busiest first -- population is the single most useful thing when you're
/// picking a server to play on. Servers with no live count fall to the bottom,
/// then alphabetical so the order is stable between refreshes.
pub fn sort(list: &mut [Server]) {
    list.sort_by(|a, b| {
        b.online()
            .cmp(&a.online())
            .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
    });
}

/// The compiled-in fallback list.
pub fn bundled() -> Vec<Server> {
    parse(SNAPSHOT).expect("bundled snapshot must be valid -- it is checked at build time")
}

/// Fetch the live directory. Blocking: call it off the main thread.
pub fn fetch() -> Result<Vec<Server>, String> {
    let body = ureq::get(DIRECTORY_URL)
        .timeout(Duration::from_secs(15))
        .call()
        .map_err(|e| format!("could not reach treestats.net: {e}"))?
        .into_string()
        .map_err(|e| format!("could not read the server list: {e}"))?;
    parse(&body).map_err(|e| format!("the server list was not valid JSON: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gdl_and_gdle_are_the_same_thing() {
        assert_eq!(Software::parse("GDL"), Software::Gdle);
        assert_eq!(Software::parse("GDLE"), Software::Gdle);
        assert_eq!(Software::parse("gdle"), Software::Gdle);
        assert_eq!(Software::parse("ACE"), Software::Ace);
        // unknown -> the majority case, not a panic
        assert_eq!(Software::parse("something-new"), Software::Ace);
    }

    #[test]
    fn stale_counts_are_not_reported_as_live() {
        let fresh = Players { count: 765, age: "7 minutes ago".into() };
        let hours = Players { count: 29, age: "11 hours ago".into() };
        let day = Players { count: 11, age: "a day ago".into() };
        assert!(fresh.is_fresh());
        assert!(hours.is_fresh());
        assert!(!day.is_fresh(), "a day-old count must not show as current");
    }

    #[test]
    fn the_bundled_snapshot_parses() {
        let list = bundled();
        assert!(list.len() > 20, "snapshot looks truncated: {} servers", list.len());
        assert!(list.iter().any(|s| s.name == "Coldeve"));
        // every entry must be launchable
        assert!(list.iter().all(|s| !s.host.is_empty() && !s.port.is_empty()));
    }

    #[test]
    fn busiest_server_sorts_first_and_unreported_sink() {
        let list = bundled();
        let first = &list[0];
        assert!(first.online().is_some(), "a populated server should lead the list");
        let last = list.last().unwrap();
        assert!(last.online().is_none(), "servers with no live count belong at the bottom");
        // descending by population
        let counts: Vec<u32> = list.iter().filter_map(|s| s.online()).collect();
        let mut sorted = counts.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(counts, sorted);
    }

    #[test]
    fn entries_without_a_host_are_dropped() {
        let json = r#"[
            {"name":"Good","host":"a.example","port":"9000","software":"ACE","type":"PvE","description":""},
            {"name":"Hostless","host":"","port":"9000","software":"ACE","type":"PvE","description":""},
            {"name":"Portless","host":"b.example","port":"","software":"GDL","type":"PvP","description":""}
        ]"#;
        let list = parse(json).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "Good");
    }
}
