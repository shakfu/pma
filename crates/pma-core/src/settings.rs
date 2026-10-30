//! `settings.toml`: the configuration a user edits. It holds the `config`
//! keys, roots, workers, presets and each project's tier and tags. The
//! store's tables of the same names mirror it, so queries keep their joins;
//! the file wins. Workflow and route revisions stay in the store, since
//! activating one is a gate a hand edit would skip.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value};

use crate::config::Config;
use crate::store::Preset;
use crate::worker::{Parser, Worker};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub const FILE: &str = "settings.toml";

const HEADER: &str = "\
# pma settings. Edit by hand, or with `pma config`, `pma root`, `pma project`,
# `pma agent` and `pma preset`, which keep comments. An invalid file stops
# every command until it is fixed.
";

/// What the file holds, read and checked.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Settings {
    /// Flat keys, as `pma config` names them, including `projects.<n>.*`.
    pub config: Vec<(String, String)>,
    pub roots: Vec<PathBuf>,
    pub tiers: BTreeMap<String, u8>,
    pub tags: Vec<(String, String)>,
    pub agents: Vec<Worker>,
    pub presets: Vec<Preset>,
}

/// The file's text and what it says. An unreadable or invalid file is an
/// error naming the path and the key.
pub fn read(path: &Path) -> Result<(String, Settings)> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let settings = parse(&doc).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((text, settings))
}

fn parse(doc: &DocumentMut) -> Result<Settings> {
    let mut s = Settings::default();
    for (key, item) in doc.iter() {
        match key {
            "roots" => {
                s.roots = strings(item, "roots")?
                    .into_iter()
                    .map(PathBuf::from)
                    .collect()
            }
            "config" => flatten(table(item, "config")?, "", &mut s.config)?,
            "projects" => {
                for (name, item) in table(item, "projects")?.iter() {
                    let at = format!("projects.{name}");
                    for (field, v) in table(item, &at)?.iter() {
                        let at = format!("{at}.{field}");
                        match field {
                            "tier" => {
                                let t = v
                                    .as_integer()
                                    .and_then(|t| u8::try_from(t).ok())
                                    .filter(|t| (1..=5).contains(t))
                                    .ok_or(format!("{at}: expected a tier from 1 to 5"))?;
                                s.tiers.insert(name.to_string(), t);
                            }
                            "tags" => {
                                for tag in strings(v, &at)? {
                                    s.tags.push((name.to_string(), tag));
                                }
                            }
                            "verify" | "sandbox" => s.config.push((
                                format!("projects.{name}.{field}"),
                                scalar(v).ok_or(format!("{at}: expected a value"))?,
                            )),
                            _ => return Err(format!("{at}: unknown field").into()),
                        }
                    }
                }
            }
            "agents" => {
                for (name, item) in table(item, "agents")?.iter() {
                    s.agents
                        .push(worker(name, table(item, &format!("agents.{name}"))?)?);
                }
            }
            "presets" => {
                for (name, item) in table(item, "presets")?.iter() {
                    s.presets
                        .push(preset(name, table(item, &format!("presets.{name}"))?)?);
                }
            }
            _ => return Err(format!("{key}: unknown section").into()),
        }
    }
    // Checked here, so a bad value names the file rather than failing later.
    let mut cfg = Config::default();
    for (key, value) in &s.config {
        if crate::config::retired(key).is_none() {
            cfg.set(key, value)
                .map_err(|e| format!("{}: {e}", toml_path(key)))?;
        }
    }
    for p in &s.presets {
        if !s.agents.iter().any(|w| w.name == p.agent) {
            return Err(format!("presets.{}: no worker `{}`", p.name, p.agent).into());
        }
    }
    Ok(s)
}

/// Where a flat key sits in the file, for messages.
fn toml_path(key: &str) -> String {
    match key.strip_prefix("projects.") {
        Some(rest) => format!("projects.{rest}"),
        None => format!("config.{key}"),
    }
}

fn table<'a>(item: &'a Item, at: &str) -> Result<&'a Table> {
    item.as_table()
        .ok_or_else(|| format!("{at}: expected a table").into())
}

fn strings(item: &Item, at: &str) -> Result<Vec<String>> {
    item.as_array()
        .and_then(|a| {
            a.iter()
                .map(|v| v.as_str().map(String::from))
                .collect::<Option<Vec<_>>>()
        })
        .ok_or_else(|| format!("{at}: expected a list of strings").into())
}

/// A value as `Config::set` reads it.
fn scalar(item: &Item) -> Option<String> {
    let v = item.as_value()?;
    Some(match v {
        Value::String(s) => s.value().clone(),
        Value::Integer(i) => i.value().to_string(),
        Value::Float(f) => f.value().to_string(),
        Value::Boolean(b) => if *b.value() { "on" } else { "off" }.into(),
        Value::Array(a) => a
            .iter()
            .map(|v| v.as_str().map(String::from))
            .collect::<Option<Vec<_>>>()?
            .join(","),
        _ => return None,
    })
}

fn flatten(t: &Table, prefix: &str, out: &mut Vec<(String, String)>) -> Result<()> {
    for (key, item) in t.iter() {
        let key = format!("{prefix}{key}");
        match item.as_table_like() {
            Some(_) if !item.is_inline_table() => {
                flatten(table(item, &key)?, &format!("{key}."), out)?
            }
            _ => out.push((
                key.clone(),
                scalar(item).ok_or(format!("config.{key}: expected a value"))?,
            )),
        }
    }
    Ok(())
}

fn worker(name: &str, t: &Table) -> Result<Worker> {
    let at = |f: &str| format!("agents.{name}.{f}");
    let text = |f: &str| -> Result<String> {
        t.get(f)
            .and_then(Item::as_str)
            .map(String::from)
            .ok_or_else(|| format!("{}: expected text", at(f)).into())
    };
    let flag = |f: &str| -> Result<bool> {
        match t.get(f) {
            None => Ok(false),
            Some(v) => v
                .as_bool()
                .ok_or_else(|| format!("{}: expected true or false", at(f)).into()),
        }
    };
    for (field, _) in t.iter() {
        if !matches!(
            field,
            "command"
                | "args"
                | "allow"
                | "parse"
                | "reports_cost"
                | "enforces_budget"
                | "sandbox"
                | "resumes"
                | "env"
        ) {
            return Err(format!("{}: unknown field", at(field)).into());
        }
    }
    let parse = text("parse")?;
    let env = match t.get("env") {
        None => BTreeMap::new(),
        Some(item) => item
            .as_table_like()
            .ok_or(format!("{}: expected a table", at("env")))?
            .iter()
            .map(|(k, v)| {
                v.as_str()
                    .map(|v| (k.to_string(), v.to_string()))
                    .ok_or(format!("{}.{k}: expected text", at("env")))
            })
            .collect::<std::result::Result<_, _>>()?,
    };
    Ok(Worker {
        name: name.to_string(),
        command: text("command")?,
        args: match t.get("args") {
            None => Vec::new(),
            Some(v) => strings(v, &at("args"))?,
        },
        allow: t
            .get("allow")
            .and_then(Item::as_str)
            .filter(|a| !a.is_empty())
            .map(String::from),
        parse: Parser::parse(&parse).ok_or(format!("{}: unknown parser `{parse}`", at("parse")))?,
        reports_cost: flag("reports_cost")?,
        enforces_budget: flag("enforces_budget")?,
        sandbox: flag("sandbox")?,
        resumes: flag("resumes")?,
        env,
    })
}

fn preset(name: &str, t: &Table) -> Result<Preset> {
    let at = |f: &str| format!("presets.{name}.{f}");
    for (field, _) in t.iter() {
        if !matches!(field, "agent" | "model" | "args") {
            return Err(format!("{}: unknown field", at(field)).into());
        }
    }
    Ok(Preset {
        name: name.to_string(),
        agent: t
            .get("agent")
            .and_then(Item::as_str)
            .map(String::from)
            .ok_or(format!("{}: expected text", at("agent")))?,
        model: t
            .get("model")
            .and_then(Item::as_str)
            .filter(|m| !m.is_empty())
            .map(String::from),
        args: match t.get("args") {
            None => Vec::new(),
            Some(v) => strings(v, &at("args"))?,
        },
    })
}

/// Writes a new file holding `s`, for the first run after the tables held
/// the configuration.
pub fn create(path: &Path, s: &Settings) -> Result<()> {
    let mut doc = DocumentMut::new();
    if !s.roots.is_empty() {
        doc["roots"] = Item::Value(Value::Array(array(
            s.roots.iter().map(|p| p.to_string_lossy().into_owned()),
        )));
    }
    for (key, value) in &s.config {
        set_config(&mut doc, key, value);
    }
    for (name, tier) in &s.tiers {
        set_tier(&mut doc, name, Some(*tier));
    }
    for (project, tag) in &s.tags {
        tag_project(&mut doc, project, tag, true);
    }
    for w in &s.agents {
        set_agent(&mut doc, w);
    }
    for p in &s.presets {
        set_preset(&mut doc, p);
    }
    // Leading text, so it stays above the first entry when parsed again; a
    // document's own comments are kept at its end.
    write(path, &format!("{HEADER}\n{doc}"))
}

/// Applies `f` to the file under an exclusive lock, so two commands editing
/// it at once do not lose a change. The result is checked before it is
/// written; a missing file starts empty.
pub fn edit<T>(path: &Path, f: impl FnOnce(&mut DocumentMut) -> T) -> Result<T> {
    let lock_path = path.with_extension("lock");
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| format!("{}: {e}", lock_path.display()))?;
    lock.lock()
        .map_err(|e| format!("{}: {e}", lock_path.display()))?;
    let (text, header) = match std::fs::read_to_string(path) {
        Ok(t) => (t, ""),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (String::new(), HEADER),
        Err(e) => return Err(format!("{}: {e}", path.display()).into()),
    };
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let out = f(&mut doc);
    let text = format!("{header}{}{doc}", if header.is_empty() { "" } else { "\n" });
    parse(&text.parse::<DocumentMut>()?).map_err(|e| format!("{}: {e}", path.display()))?;
    write(path, &text)?;
    Ok(out)
}

/// Through a temporary file and a rename, so a reader never sees half of it.
fn write(path: &Path, text: &str) -> Result<()> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(())
}

fn array(items: impl IntoIterator<Item = String>) -> Array {
    items.into_iter().collect()
}

/// The `[section.name]` table, made if absent.
fn entry<'a>(doc: &'a mut DocumentMut, section: &str, name: &str) -> &'a mut Table {
    let outer = doc
        .entry(section)
        .or_insert_with(|| {
            let mut t = Table::new();
            t.set_implicit(true);
            Item::Table(t)
        })
        .as_table_mut()
        .expect("checked by parse");
    outer
        .entry(name)
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .expect("checked by parse")
}

/// Drops `[section.name]` once it holds nothing, and `[section]` with it.
fn prune(doc: &mut DocumentMut, section: &str, name: &str) {
    let Some(outer) = doc.get_mut(section).and_then(Item::as_table_mut) else {
        return;
    };
    if outer
        .get(name)
        .and_then(Item::as_table)
        .is_some_and(Table::is_empty)
    {
        outer.remove(name);
    }
    if outer.is_empty() {
        doc.remove(section);
    }
}

/// A value of the type its key takes, so the file reads as TOML rather than
/// as quoted text.
fn typed(key: &str, value: &str) -> Value {
    match Config::kind(key) {
        // A whole number reads as one: `agent_budget = 3`, not `3.0`.
        Some("number") => match (value.parse::<i64>(), value.parse::<f64>()) {
            (Ok(i), _) => Value::from(i),
            (_, Ok(f)) => Value::from(f),
            _ => value.into(),
        },
        Some("count") => value
            .parse::<i64>()
            .map(Value::from)
            .unwrap_or_else(|_| value.into()),
        Some("tier") => value
            .parse::<i64>()
            .map(Value::from)
            .unwrap_or_else(|_| value.into()),
        Some("list") => Value::Array(array(
            value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from),
        )),
        Some("switch") => Value::from(value == "on"),
        _ => value.into(),
    }
}

pub fn set_config(doc: &mut DocumentMut, key: &str, value: &str) {
    if let Some((name, field)) = key
        .strip_prefix("projects.")
        .and_then(|r| r.rsplit_once('.'))
    {
        entry(doc, "projects", name)[field] = Item::Value(typed(key, value));
        return;
    }
    let mut t = doc
        .entry("config")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .expect("checked by parse");
    let parts: Vec<&str> = key.split('.').collect();
    for part in &parts[..parts.len() - 1] {
        t = t
            .entry(part)
            .or_insert_with(|| {
                let mut sub = Table::new();
                sub.set_dotted(true);
                Item::Table(sub)
            })
            .as_table_mut()
            .expect("checked by parse");
    }
    t[parts[parts.len() - 1]] = Item::Value(typed(key, value));
}

/// Returns whether the key was set.
pub fn reset_config(doc: &mut DocumentMut, key: &str) -> bool {
    if let Some((name, field)) = key
        .strip_prefix("projects.")
        .and_then(|r| r.rsplit_once('.'))
    {
        let removed = doc
            .get_mut("projects")
            .and_then(Item::as_table_mut)
            .and_then(|t| t.get_mut(name))
            .and_then(Item::as_table_mut)
            .and_then(|t| t.remove(field))
            .is_some();
        prune(doc, "projects", name);
        return removed;
    }
    let Some(mut t) = doc.get_mut("config").and_then(Item::as_table_mut) else {
        return false;
    };
    let parts: Vec<&str> = key.split('.').collect();
    for part in &parts[..parts.len() - 1] {
        match t.get_mut(part).and_then(Item::as_table_mut) {
            Some(sub) => t = sub,
            None => return false,
        }
    }
    t.remove(parts[parts.len() - 1]).is_some()
}

/// Returns false when the root was already listed.
pub fn add_root(doc: &mut DocumentMut, path: &str) -> bool {
    let roots = doc
        .entry("roots")
        .or_insert_with(|| Item::Value(Value::Array(Array::new())))
        .as_array_mut()
        .expect("checked by parse");
    if roots.iter().any(|v| v.as_str() == Some(path)) {
        return false;
    }
    roots.push(path);
    true
}

/// Returns false when the root was not listed.
pub fn remove_root(doc: &mut DocumentMut, path: &str) -> bool {
    let Some(roots) = doc.get_mut("roots").and_then(Item::as_array_mut) else {
        return false;
    };
    let before = roots.len();
    roots.retain(|v| v.as_str() != Some(path));
    before != roots.len()
}

pub fn set_tier(doc: &mut DocumentMut, name: &str, tier: Option<u8>) {
    match tier {
        Some(t) => entry(doc, "projects", name)["tier"] = Item::Value(i64::from(t).into()),
        None => {
            if let Some(t) = doc
                .get_mut("projects")
                .and_then(Item::as_table_mut)
                .and_then(|t| t.get_mut(name))
                .and_then(Item::as_table_mut)
            {
                t.remove("tier");
            }
            prune(doc, "projects", name);
        }
    }
}

/// Adds or removes one tag. Returns whether the file changed.
pub fn tag_project(doc: &mut DocumentMut, project: &str, tag: &str, on: bool) -> bool {
    let t = entry(doc, "projects", project);
    let tags = t
        .entry("tags")
        .or_insert_with(|| Item::Value(Value::Array(Array::new())))
        .as_array_mut()
        .expect("checked by parse");
    let had = tags.iter().any(|v| v.as_str() == Some(tag));
    match (on, had) {
        (true, false) => tags.push(tag),
        (false, true) => tags.retain(|v| v.as_str() != Some(tag)),
        _ => {}
    }
    if tags.is_empty() {
        t.remove("tags");
    }
    prune(doc, "projects", project);
    on != had
}

/// Drops `[projects.<name>]` whole, as `pma project forget` drops its
/// record. Settings are keyed by name, so a kept `verify` or `sandbox = false`
/// would apply to an unrelated repository that later takes the name.
pub fn forget_project(doc: &mut DocumentMut, name: &str) {
    if let Some(t) = doc.get_mut("projects").and_then(Item::as_table_mut) {
        t.remove(name);
    }
    prune(doc, "projects", name);
}

pub fn set_agent(doc: &mut DocumentMut, w: &Worker) {
    let mut t = Table::new();
    t["command"] = Item::Value(w.command.as_str().into());
    t["args"] = Item::Value(Value::Array(array(w.args.iter().cloned())));
    if let Some(allow) = &w.allow {
        t["allow"] = Item::Value(allow.as_str().into());
    }
    t["parse"] = Item::Value(w.parse.name().into());
    t["reports_cost"] = Item::Value(w.reports_cost.into());
    t["enforces_budget"] = Item::Value(w.enforces_budget.into());
    t["sandbox"] = Item::Value(w.sandbox.into());
    t["resumes"] = Item::Value(w.resumes.into());
    if !w.env.is_empty() {
        let mut env = InlineTable::new();
        for (k, v) in &w.env {
            env.insert(k, v.as_str().into());
        }
        t["env"] = Item::Value(Value::InlineTable(env));
    }
    *entry(doc, "agents", &w.name) = t;
}

/// Returns false when no worker has the name.
pub fn remove_agent(doc: &mut DocumentMut, name: &str) -> bool {
    let removed = doc
        .get_mut("agents")
        .and_then(Item::as_table_mut)
        .and_then(|t| t.remove(name))
        .is_some();
    prune(doc, "agents", name);
    removed
}

pub fn set_preset(doc: &mut DocumentMut, p: &Preset) {
    let mut t = Table::new();
    t["agent"] = Item::Value(p.agent.as_str().into());
    if let Some(m) = &p.model {
        t["model"] = Item::Value(m.as_str().into());
    }
    if !p.args.is_empty() {
        t["args"] = Item::Value(Value::Array(array(p.args.iter().cloned())));
    }
    *entry(doc, "presets", &p.name) = t;
}

/// Returns false when no preset has the name.
pub fn remove_preset(doc: &mut DocumentMut, name: &str) -> bool {
    let removed = doc
        .get_mut("presets")
        .and_then(Item::as_table_mut)
        .and_then(|t| t.remove(name))
        .is_some();
    prune(doc, "presets", name);
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pma-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(FILE)
    }

    /// What `create` writes, `read` reads back, typed as TOML.
    #[test]
    fn settings_round_trip_through_the_file() {
        let path = scratch("round");
        let s = Settings {
            config: vec![
                ("activity.ignore".into(), "docs/**,*.lock".into()),
                ("agent_budget".into(), "3".into()),
                ("default_tier".into(), "never".into()),
                ("notify".into(), "notify-send pma".into()),
                ("projects.argdec.sandbox".into(), "off".into()),
                ("projects.argdec.verify".into(), "make test".into()),
                ("tiers.2".into(), "0.7".into()),
            ],
            roots: vec![PathBuf::from("/home/u/projects")],
            tiers: BTreeMap::from([("argdec".to_string(), 2)]),
            tags: vec![("argdec".into(), "py".into())],
            agents: vec![Worker::claude()],
            presets: vec![Preset {
                name: "cheap".into(),
                agent: "claude".into(),
                model: Some("haiku".into()),
                args: vec![],
            }],
        };
        create(&path, &s).unwrap();
        let (text, back) = read(&path).unwrap();
        assert!(text.contains("agent_budget = 3"), "{text}");
        assert!(text.contains("tiers.2 = 0.7"), "{text}");
        assert!(
            text.contains("ignore = [\"docs/**\", \"*.lock\"]"),
            "{text}"
        );
        assert!(text.contains("[projects.argdec]\n"), "{text}");
        assert!(text.contains("sandbox = false"), "{text}");
        let mut config = back.config.clone();
        config.sort();
        assert_eq!(config, s.config);
        assert_eq!(
            (back.roots, back.tiers, back.tags),
            (s.roots, s.tiers, s.tags)
        );
        assert_eq!((back.agents, back.presets), (s.agents, s.presets));
    }

    /// An edit keeps what the user wrote around it, comments included.
    #[test]
    fn edits_keep_comments() {
        let path = scratch("comments");
        std::fs::write(
            &path,
            "# mine\nroots = [\"/a\"]\n\n[config]\n# spend\nagent_budget = 3 # per run\n",
        )
        .unwrap();
        edit(&path, |doc| {
            set_config(doc, "max_parallel", "1");
            add_root(doc, "/b");
        })
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        for kept in [
            "# mine",
            "# spend",
            "agent_budget = 3 # per run",
            "max_parallel = 1",
        ] {
            assert!(text.contains(kept), "{kept}:\n{text}");
        }
        assert!(text.contains("\"/a\", \"/b\""), "{text}");
        assert!(edit(&path, |doc| reset_config(doc, "agent_budget")).unwrap());
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("agent_budget")
        );
    }

    /// A bad value names its place in the file; an edit that would write one
    /// writes nothing.
    #[test]
    fn invalid_files_name_the_key() {
        let path = scratch("invalid");
        for (text, says) in [
            ("[config]\ntiers.2 = \"high\"\n", "config.tiers.2"),
            ("[config]\nnonsense = 1\n", "unknown setting"),
            ("[projects.a]\ntier = 9\n", "projects.a.tier"),
            (
                "[agents.x]\ncommand = \"x\"\nparse = \"nope\"\n",
                "agents.x.parse",
            ),
            ("[presets.p]\nagent = \"ghost\"\n", "no worker `ghost`"),
            ("[colours]\n", "unknown section"),
            ("roots = [\n", "settings.toml"),
        ] {
            std::fs::write(&path, text).unwrap();
            let e = read(&path).unwrap_err().to_string();
            assert!(e.contains(says), "{text}: {e}");
        }
        std::fs::write(&path, "[config]\nagent_budget = 3\n").unwrap();
        assert!(edit(&path, |doc| set_config(doc, "agent_budget", "x")).is_err());
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("agent_budget = 3")
        );
    }

    #[test]
    fn a_dotted_project_name_round_trips() {
        let path = scratch("dotted");
        edit(&path, |doc| {
            set_config(doc, "projects.socket.io.verify", "npm test")
        })
        .unwrap();
        let (text, s) = read(&path).unwrap();
        assert_eq!(
            s.config,
            [(
                "projects.socket.io.verify".to_string(),
                "npm test".to_string()
            )],
            "{text}"
        );
        edit(&path, |doc| {
            assert!(reset_config(doc, "projects.socket.io.verify"))
        })
        .unwrap();
        assert!(read(&path).unwrap().1.config.is_empty());
    }

    #[test]
    fn tags_tiers_and_forgetting_tidy_up() {
        let path = scratch("tags");
        edit(&path, |doc| {
            assert!(tag_project(doc, "a", "x", true));
            assert!(!tag_project(doc, "a", "x", true));
            set_tier(doc, "a", Some(2));
            set_config(doc, "projects.a.verify", "make");
        })
        .unwrap();
        let (_, s) = read(&path).unwrap();
        assert_eq!(s.tags, [("a".to_string(), "x".to_string())]);
        assert_eq!(s.tiers.get("a"), Some(&2));
        edit(&path, |doc| forget_project(doc, "a")).unwrap();
        let (text, s) = read(&path).unwrap();
        assert!(
            s.tags.is_empty() && s.tiers.is_empty() && s.config.is_empty(),
            "{text}"
        );
        assert!(!text.contains("projects"), "{text}");
    }
}
