//! Which tabs, panes and details a person opens and acts in, one JSON line
//! an event in `<state>/usage/YYYY-MM.jsonl`. It records no typed text and
//! no project name. Design: docs/dev/usage-log.md.
//!
//! A file, not a table: the front ends' watch reloads on any commit to the
//! store, so a usage write from one front end would reload every other.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::keys::Tab;
use crate::{api, dates, screen, store};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The kinds of event, as `ev` names them.
pub const KINDS: [&str; 6] = ["tab", "project", "pane", "open", "act", "find"];

/// Longest field kept; a longer one is cut, so a bad caller cannot grow
/// the file a line at a time.
const FIELD_MAX: usize = 64;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Event {
    pub at: i64,
    /// `tui` or `web`.
    pub fe: String,
    /// One of `KINDS`.
    pub ev: String,
    pub tab: Option<String>,
    /// A pane's key, as `pane_key` gives it.
    pub pane: Option<String>,
    /// How a tab or project was reached, a detail's kind, an act's name or
    /// a find tool.
    pub name: Option<String>,
    /// Whether an act succeeded.
    pub ok: Option<bool>,
}

impl Event {
    pub fn new(fe: &str, ev: &str) -> Event {
        Event {
            at: dates::now(),
            fe: fe.into(),
            ev: ev.into(),
            ..Event::default()
        }
    }

    pub fn tab(mut self, tab: Tab) -> Event {
        self.tab = Some(tab.title().into());
        self
    }

    pub fn pane(mut self, title: &str) -> Event {
        self.pane = Some(pane_key(title));
        self
    }

    pub fn name(mut self, name: &str) -> Event {
        self.name = Some(name.into());
        self
    }

    pub fn ok(mut self, ok: bool) -> Event {
        self.ok = Some(ok);
        self
    }

    fn line(&self) -> String {
        let cut = |s: &Option<String>| {
            s.as_ref()
                .map(|s| s.chars().take(FIELD_MAX).collect::<String>())
        };
        let mut v = json!({ "at": self.at, "fe": cut(&Some(self.fe.clone())), "ev": self.ev });
        for (k, f) in [
            ("tab", &self.tab),
            ("pane", &self.pane),
            ("name", &self.name),
        ] {
            if let Some(s) = cut(f) {
                v[k] = json!(s);
            }
        }
        if let Some(ok) = self.ok {
            v["ok"] = json!(ok);
        }
        v.to_string()
    }

    fn parse(line: &str) -> Option<Event> {
        let v: Value = serde_json::from_str(line).ok()?;
        let s = |k: &str| v[k].as_str().map(String::from);
        Some(Event {
            at: v["at"].as_i64()?,
            fe: s("fe")?,
            ev: s("ev")?,
            tab: s("tab"),
            pane: s("pane"),
            name: s("name"),
            ok: v["ok"].as_bool(),
        })
    }
}

/// A pane's title without what varies: the finished runs' counts, the
/// report's dimension.
pub fn pane_key(title: &str) -> String {
    if title.starts_with("Runs by ") {
        return "Runs by".into();
    }
    title.split(':').next().unwrap_or(title).trim().into()
}

/// Appends `ev` unless `usage_log` is off. A front end ignores the error:
/// losing an event must not fail a key.
pub fn record(store: &store::Store, ev: &Event) -> Result<()> {
    if !api::config(store)?.usage_log {
        return Ok(());
    }
    record_in(&dir()?, ev)
}

pub fn dir() -> Result<PathBuf> {
    Ok(store::state_home()?.join("usage"))
}

pub fn record_in(dir: &Path, ev: &Event) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let month = &dates::format(dates::day(ev.at))[..7];
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{month}.jsonl")))?;
    writeln!(f, "{}", ev.line())?;
    Ok(())
}

/// Every event at or after `since`, oldest first. A line that does not
/// parse is skipped.
pub fn read_in(dir: &Path, since: i64) -> Result<Vec<Event>> {
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f)?;
        out.extend(
            text.lines()
                .filter_map(Event::parse)
                .filter(|e| e.at >= since),
        );
    }
    out.sort_by_key(|e| e.at);
    Ok(out)
}

/// Each numbered tab's panes as the store renders them now, by key: the
/// panes no event names are the ones the summary lists as unused.
pub fn panes_now(store: &store::Store) -> Vec<(Tab, Vec<String>)> {
    Tab::ALL
        .into_iter()
        .filter(|t| *t != Tab::Project)
        .map(|t| {
            let by = crate::report_runs::By::ALL[0].1;
            let titles = api::query(store, t.query(api::Selection::default(), by, "", None))
                .map(|v| match screen::screen(v).body {
                    screen::Body::Panes(ps) => ps.iter().map(|p| pane_key(&p.title)).collect(),
                    screen::Body::Detail(_) => Vec::new(),
                })
                .unwrap_or_default();
            (t, titles)
        })
        .collect()
}

/// The events as text: tab opens by front end, focuses and acts per pane,
/// acts, details, project opens and finds, then what none named.
pub fn summary(events: &[Event], panes: &[(Tab, Vec<String>)]) -> String {
    let count = |ev: &str| -> BTreeMap<(String, String), (usize, usize)> {
        let mut m: BTreeMap<(String, String), (usize, usize)> = BTreeMap::new();
        for e in events.iter().filter(|e| e.ev == ev) {
            let key = match ev {
                "tab" => (e.tab.clone().unwrap_or_default(), e.fe.clone()),
                "act" => (e.name.clone().unwrap_or_default(), String::new()),
                _ => (e.name.clone().unwrap_or_default(), String::new()),
            };
            let c = m.entry(key).or_default();
            c.0 += 1;
            c.1 += usize::from(e.ok == Some(false));
        }
        m
    };
    let days: std::collections::BTreeSet<i64> = events.iter().map(|e| dates::day(e.at)).collect();
    let mut out = format!("{} events over {} days\n", events.len(), days.len());

    let section = |out: &mut String, title: &str, rows: Vec<Vec<String>>| {
        out.push_str(&format!("\n{title}\n"));
        match rows.is_empty() {
            true => out.push_str("  none\n"),
            false => out.push_str(&crate::report::table(&rows, "  ")),
        }
    };
    let simple = |m: BTreeMap<(String, String), (usize, usize)>| -> Vec<Vec<String>> {
        m.into_iter()
            .map(|((n, _), (c, _))| vec![n, c.to_string()])
            .collect()
    };

    section(
        &mut out,
        "Tab opens",
        count("tab")
            .into_iter()
            .map(|((t, fe), (c, _))| vec![t, fe, c.to_string()])
            .collect(),
    );

    // Per pane: focuses and acts, every pane the tabs render now included.
    let mut per_pane: BTreeMap<(String, String), (usize, usize)> = BTreeMap::new();
    for (tab, keys) in panes {
        for k in keys {
            per_pane.entry((tab.title().into(), k.clone())).or_default();
        }
    }
    for e in events.iter().filter(|e| e.ev == "pane" || e.ev == "act") {
        let (Some(tab), Some(pane)) = (&e.tab, &e.pane) else {
            continue;
        };
        let c = per_pane.entry((tab.clone(), pane.clone())).or_default();
        match e.ev.as_str() {
            "pane" => c.0 += 1,
            _ => c.1 += 1,
        }
    }
    let mut rows = vec![vec![
        "tab".into(),
        "pane".into(),
        "focus".into(),
        "acts".into(),
    ]];
    rows.extend(
        per_pane
            .iter()
            .map(|((t, p), (f, a))| vec![t.clone(), p.clone(), f.to_string(), a.to_string()]),
    );
    section(&mut out, "Panes", rows);

    section(
        &mut out,
        "Acts",
        count("act")
            .into_iter()
            .map(|((n, _), (c, failed))| match failed {
                0 => vec![n, c.to_string()],
                f => vec![n, c.to_string(), format!("{f} failed")],
            })
            .collect(),
    );
    section(&mut out, "Details opened", simple(count("open")));
    section(&mut out, "Project opens, by way", simple(count("project")));
    section(&mut out, "Finds", simple(count("find")));

    let opened: std::collections::BTreeSet<&str> = events
        .iter()
        .filter(|e| e.ev == "tab")
        .filter_map(|e| e.tab.as_deref())
        .collect();
    let mut unused: Vec<String> = panes
        .iter()
        .filter(|(t, _)| !opened.contains(t.title()))
        .map(|(t, _)| format!("tab {}", t.title()))
        .collect();
    unused.extend(
        per_pane
            .iter()
            .filter(|(_, (f, a))| *f == 0 && *a == 0)
            .map(|((t, p), _)| format!("{t}: {p}")),
    );
    section(
        &mut out,
        "Not opened, focused or acted in",
        unused.into_iter().map(|u| vec![u]).collect(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pma-usage-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// An event reads back as written; long fields are cut, and a broken
    /// line is skipped rather than failing the read.
    #[test]
    fn events_round_trip_by_month() {
        let dir = scratch("trip");
        let mut ev = Event::new("tui", "act")
            .tab(Tab::Inbox)
            .pane("Waiting on you");
        ev = ev.name("ApproveRun").ok(true);
        ev.at = 1_793_700_000; // 2026-11
        record_in(&dir, &ev).unwrap();
        let mut long = Event::new("web", "pane").pane(&"x".repeat(200));
        long.at = 1_790_000_000; // 2026-09
        record_in(&dir, &long).unwrap();
        std::fs::write(dir.join("2026-10.jsonl"), "not json\n").unwrap();

        let got = read_in(&dir, 0).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[1], ev);
        assert_eq!(got[0].pane.as_ref().unwrap().len(), FIELD_MAX);
        assert!(dir.join("2026-11.jsonl").exists() && dir.join("2026-09.jsonl").exists());
        assert_eq!(read_in(&dir, 1_791_000_000).unwrap().len(), 1, "since");
        assert!(read_in(&dir.join("none"), 0).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pane_keys_drop_what_varies() {
        assert_eq!(
            pane_key("Finished runs: pushed, newest 100 of 212"),
            "Finished runs"
        );
        assert_eq!(pane_key("Runs by agent"), "Runs by");
        assert_eq!(pane_key("Waiting on you"), "Waiting on you");
    }

    /// The summary counts each kind and names the tabs and panes no event
    /// reached.
    #[test]
    fn the_summary_names_what_was_not_used() {
        let at = |mut e: Event| {
            e.at = 86_400 * 3;
            e
        };
        let events = vec![
            at(Event::new("tui", "tab").tab(Tab::Inbox).name("key")),
            at(Event::new("web", "tab").tab(Tab::Inbox).name("click")),
            at(Event::new("tui", "pane")
                .tab(Tab::Inbox)
                .pane("Working now")),
            at(Event::new("tui", "act")
                .tab(Tab::Inbox)
                .pane("Waiting on you")
                .name("Push")
                .ok(false)),
            at(Event::new("tui", "project").name("O")),
        ];
        let panes = vec![
            (
                Tab::Inbox,
                vec!["Waiting on you".to_string(), "Working now".to_string()],
            ),
            (Tab::Setup, vec!["Agents".to_string()]),
        ];
        let s = summary(&events, &panes);
        assert!(s.starts_with("5 events over 1 days"), "{s}");
        assert!(
            s.contains("inbox  tui  1") && s.contains("inbox  web  1"),
            "{s}"
        );
        assert!(s.contains("Push  1  1 failed"), "{s}");
        assert!(s.contains("O  1"), "{s}");
        let unused = s.split("Not opened").nth(1).unwrap();
        assert!(
            unused.contains("tab setup") && unused.contains("setup: Agents"),
            "{s}"
        );
        assert!(!unused.contains("Working now") && !unused.contains("Waiting on you"));
    }
}
