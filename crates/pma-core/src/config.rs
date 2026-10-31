//! Configuration: built-in defaults plus overrides stored in the database,
//! addressed by flat keys such as `tiers.2`, `signals.ci` or
//! `projects.cyllama.verify`.

use std::collections::BTreeMap;

use crate::todo::Priority;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub important_threshold: f64,
    /// Days before `due:` at which a task becomes urgent.
    pub urgent_within: i64,
    pub quadrant_limit: i64,
    /// Multiplier per tier, index 0 is tier 1.
    pub tiers: [f64; 5],
    /// Weight per priority, in `Priority::ALL` order.
    pub priorities: [f64; 4],
    /// Priority of each signal task.
    pub signals: SignalPriorities,
    /// Globs for paths whose commits do not count as activity.
    pub activity_ignore: Vec<String>,
    /// Days a lock may go unchanged, and a `--deps` measurement stay current,
    /// before a project is in stale deps.
    pub deps_days: i64,
    /// Days without activity at which the activity signal reaches 1, per tier.
    pub activity_horizon: [i64; 5],
    /// The worker `pma dispatch` runs, by name in the `agents` table.
    pub agent: String,
    /// The preset a dispatch takes when nothing names one; `None` falls back to
    /// `agent` and that worker's own model.
    pub preset: Option<String>,
    /// Tier for a project that has none, so untiered projects are ranked
    /// rather than invisible. `None`, set as `never`, leaves them out.
    pub default_tier: Option<i64>,
    pub attribution: Attribution,
    /// Agents running at once.
    pub max_parallel: i64,
    /// USD a dispatch batch may spend.
    pub batch_budget: f64,
    /// USD one agent run may spend.
    pub agent_budget: f64,
    /// The most a workflow revision's worst case may cost, per unit of input,
    /// before `pma workflow activate` refuses it.
    pub workflow_budget: f64,
    /// USD one workflow instance may spend over every pass, after which an
    /// unattended pass approves no further plan. `None`, set as `off`, is no
    /// cap. Design: docs/dev/scheduling-work.md.
    pub instance_budget: Option<f64>,
    /// Minutes an agent run, or a verify run, may take.
    pub timeout: i64,
    /// A shell command run when a detached job ends, with `PMA_JOB_ID`,
    /// `PMA_JOB_STATE` and `PMA_JOB_SUMMARY` set.
    pub notify: Option<String>,
    /// Where an agent edits: the clone itself, or a worktree of it.
    pub workspace: Workspace,
    /// What agents run in. One setting because integrations exclude each other.
    pub integration: Integration,
    pub projects: BTreeMap<String, ProjectSettings>,
}

/// What agents run in: `pma integrate`. See docs/dev/herdr-integration.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Integration {
    /// A child process of `pma`.
    #[default]
    None,
    /// A pane of a herdr server.
    Herdr,
}

impl Integration {
    pub fn name(self) -> &'static str {
        match self {
            Integration::None => "none",
            Integration::Herdr => "herdr",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Integration::None),
            "herdr" => Some(Integration::Herdr),
            _ => None,
        }
    }
}

/// Where a run's agent edits. See docs/dev/clone-runs.md. The derived
/// default is the worktree, for a `Run` built without one; the setting
/// defaults to the clone in `Config::default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Workspace {
    /// The user's clone, from its working tree as it is. One run per
    /// project at a time; the edit waits there for review.
    Clone,
    /// A worktree of the remote default branch under pma's state directory.
    #[default]
    Worktree,
}

impl Workspace {
    pub fn name(self) -> &'static str {
        match self {
            Workspace::Clone => "clone",
            Workspace::Worktree => "worktree",
        }
    }

    pub fn parse(s: &str) -> Option<Workspace> {
        match s {
            "clone" => Some(Workspace::Clone),
            "worktree" => Some(Workspace::Worktree),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribution {
    User,
    CoAuthor,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProjectSettings {
    /// Shell command that checks an agent's work; `None` detects one.
    pub verify: Option<String>,
    /// Whether verify's writes are confined to the worktree. Off unless set:
    /// a test suite that writes outside `$TMPDIR` fails under it for no fault
    /// of the agent's.
    pub sandbox: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SignalPriorities {
    pub ci: Priority,
    pub deps: Priority,
    pub activity: Priority,
    pub hygiene: Priority,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            important_threshold: 0.4,
            urgent_within: 7,
            quadrant_limit: 10,
            tiers: [1.0, 0.8, 0.6, 0.4, 0.2],
            priorities: [1.0, 0.5, 0.2, 0.05],
            signals: SignalPriorities {
                ci: Priority::High,
                deps: Priority::Medium,
                activity: Priority::Low,
                hygiene: Priority::Medium,
            },
            // TODO.md is here so that editing the list, or the commit that
            // added an empty one to many repos at once, is not maintenance.
            activity_ignore: vec![".github/**".into(), "*.lock".into(), "TODO.md".into()],
            activity_horizon: [30, 60, 120, 240, 365],
            deps_days: 30,
            agent: "claude".into(),
            preset: None,
            default_tier: Some(5),
            attribution: Attribution::User,
            max_parallel: 2,
            batch_budget: 5.0,
            agent_budget: 1.0,
            workflow_budget: 25.0,
            instance_budget: None,
            timeout: 30,
            notify: None,
            workspace: Workspace::Clone,
            integration: Integration::None,
            projects: BTreeMap::new(),
        }
    }
}

/// A mutable reference to one setting, with its validation.
enum Slot<'a> {
    /// A finite number of at least the given minimum.
    Number(&'a mut f64, f64),
    /// A number as `Number`, or `off` for none.
    OptNumber(&'a mut Option<f64>, f64),
    /// A whole number from the minimum to the maximum.
    Count(&'a mut i64, i64, i64),
    /// 1 to 5, or `never` for none.
    Tier(&'a mut Option<i64>),
    Priority(&'a mut Priority),
    List(&'a mut Vec<String>),
    Attribution(&'a mut Attribution),
    Workspace(&'a mut Workspace),
    Integration(&'a mut Integration),
    /// Empty text is `None`.
    OptText(&'a mut Option<String>),
    /// Text that may not be empty.
    Text(&'a mut String),
    /// `on` or `off`.
    Switch(&'a mut bool),
}

/// Settings that no longer exist, with what replaced them. A retired name
/// stays here so `pma config <old>` explains it. `with_overrides` skips a
/// stored row for one, because an unknown key is a hard error on every
/// command and an upgrade must not brick a store that set it.
pub const RETIRED: [(&str, &str); 14] = [
    (
        "model",
        "a model belongs to a preset: `pma preset set <name> <agent> <model>`",
    ),
    (
        "dispatch_quadrants",
        "dispatch draws from eligible tasks: the ci and deps signals, and items tagged #agent",
    ),
    (
        "overflow_quadrants",
        "dispatch draws from eligible tasks; there is no overflow list",
    ),
    (
        "stale_after.1",
        "age no longer makes a task urgent; see `pma stale`",
    ),
    (
        "stale_after.2",
        "age no longer makes a task urgent; see `pma stale`",
    ),
    (
        "stale_after.3",
        "age no longer makes a task urgent; see `pma stale`",
    ),
    (
        "stale_after.4",
        "age no longer makes a task urgent; see `pma stale`",
    ),
    (
        "stale_after.5",
        "age no longer makes a task urgent; see `pma stale`",
    ),
    ("weights.tasks", WEIGHTS),
    ("weights.activity", WEIGHTS),
    ("weights.ci", WEIGHTS),
    ("weights.deps", WEIGHTS),
    ("weights.hygiene", WEIGHTS),
    (
        "publish",
        "where a run goes is the command now: `pma pr <id>` opens a pull request, `pma push <id>` pushes to the default branch",
    ),
];

const WEIGHTS: &str =
    "the health score is gone; `pma status` sorts by the worst state a project is in";

/// Why `key` names no setting: retired, with what replaced it, or unknown.
pub fn not_a_setting(key: &str) -> String {
    match retired(key) {
        Some(why) => format!("`{key}` was retired: {why}"),
        None => format!("unknown setting `{key}`"),
    }
}

pub fn retired(key: &str) -> Option<&'static str> {
    // Per project as well as global: every `projects.<name>.publish` went too.
    if key.starts_with("projects.") && key.ends_with(".publish") {
        return retired("publish");
    }
    RETIRED
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, why)| *why)
}

impl Config {
    /// A project's tier, else `default_tier`: what ranks it, and so what
    /// routes it.
    pub fn tier_or_default(&self, tier: Option<u8>) -> Option<u8> {
        tier.or_else(|| self.default_tier.and_then(|t| u8::try_from(t).ok()))
    }

    pub fn keys() -> Vec<String> {
        let mut keys: Vec<String> = ["important_threshold", "urgent_within", "quadrant_limit"]
            .map(String::from)
            .into();
        let per_tier = |prefix: &'static str| (1..=5).map(move |t| format!("{prefix}.{t}"));
        keys.extend(per_tier("tiers"));
        keys.extend(Priority::ALL.map(|p| format!("priorities.{}", p.name())));
        keys.extend(["ci", "deps", "activity", "hygiene"].map(|s| format!("signals.{s}")));
        keys.push("activity.ignore".into());
        keys.extend(per_tier("activity.horizon"));
        keys.push("deps.days".into());
        keys.extend(
            [
                "agent",
                "preset",
                "default_tier",
                "attribution",
                "max_parallel",
                "batch_budget",
                "agent_budget",
                "workflow_budget",
                "instance_budget",
                "timeout",
                "notify",
                "workspace",
                "integration",
            ]
            .map(String::from),
        );
        keys
    }

    /// Applies stored overrides to the defaults.
    pub fn with_overrides<'a>(
        rows: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Result<Config, String> {
        let mut cfg = Config::default();
        for (key, value) in rows {
            // A row the migration has not dropped yet, or one written by
            // another binary. Retirement must not make every command fail.
            if retired(key).is_some() {
                continue;
            }
            cfg.set(key, value).map_err(|e| {
                format!("stored setting {key} = {value:?} is invalid ({e}); run `pma config {key} --reset`")
            })?;
        }
        Ok(cfg)
    }

    /// The kind of value `key` takes, so `settings.toml` stores it typed.
    pub fn kind(key: &str) -> Option<&'static str> {
        Some(match Config::default().slot(key)? {
            Slot::Number(..) | Slot::OptNumber(..) => "number",
            Slot::Count(..) => "count",
            Slot::Tier(_) => "tier",
            Slot::List(_) => "list",
            Slot::Switch(_) => "switch",
            Slot::Priority(_)
            | Slot::Attribution(_)
            | Slot::Workspace(_)
            | Slot::Integration(_)
            | Slot::OptText(_)
            | Slot::Text(_) => "text",
        })
    }

    pub fn get(&self, key: &str) -> Option<String> {
        let mut copy = self.clone();
        Some(match copy.slot(key)? {
            Slot::Number(v, _) => v.to_string(),
            Slot::OptNumber(v, _) => v.map_or("off".into(), |n| n.to_string()),
            Slot::Count(v, ..) => v.to_string(),
            Slot::Tier(v) => v.map_or("never".into(), |n| n.to_string()),
            Slot::Priority(p) => p.name().into(),
            Slot::List(v) => v.join(","),
            Slot::Attribution(a) => match a {
                Attribution::User => "user".into(),
                Attribution::CoAuthor => "co-author".into(),
            },
            Slot::Workspace(w) => w.name().into(),
            Slot::Integration(i) => i.name().into(),
            Slot::OptText(v) => v.clone().unwrap_or_default(),
            Slot::Text(v) => v.clone(),
            Slot::Switch(v) => if *v { "on" } else { "off" }.into(),
        })
    }

    /// Sets one value; on error, nothing changes.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let mut next = self.clone();
        next.set_in_place(key, value)?;
        *self = next;
        Ok(())
    }

    fn set_in_place(&mut self, key: &str, value: &str) -> Result<(), String> {
        let slot = self.slot(key).ok_or_else(|| not_a_setting(key))?;
        let value = value.trim();
        match slot {
            Slot::Number(v, min) => {
                *v = value
                    .parse::<f64>()
                    .ok()
                    .filter(|n| n.is_finite() && *n >= min)
                    .ok_or_else(|| format!("expected a number >= {min}"))?;
            }
            Slot::OptNumber(v, min) => {
                *v = match value {
                    "off" => None,
                    _ => Some(
                        value
                            .parse::<f64>()
                            .ok()
                            .filter(|n| n.is_finite() && *n >= min)
                            .ok_or_else(|| format!("expected a number >= {min}, or `off`"))?,
                    ),
                };
            }
            Slot::Count(v, min, max) => {
                *v = value
                    .parse::<i64>()
                    .ok()
                    .filter(|n| (min..=max).contains(n))
                    .ok_or_else(|| match max {
                        i64::MAX => format!("expected a whole number >= {min}"),
                        _ => format!("expected a whole number from {min} to {max}"),
                    })?;
            }
            Slot::Tier(v) => {
                *v = match value {
                    "never" => None,
                    _ => Some(
                        value
                            .parse::<i64>()
                            .ok()
                            .filter(|n| (1..=5).contains(n))
                            .ok_or("expected a tier from 1 to 5, or `never`")?,
                    ),
                };
            }
            Slot::Priority(p) => {
                *p = Priority::parse(value).ok_or("expected critical, high, medium or low")?;
            }
            Slot::List(v) => {
                *v = value
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect();
            }
            Slot::Attribution(a) => {
                *a = match value {
                    "user" => Attribution::User,
                    "co-author" => Attribution::CoAuthor,
                    _ => return Err("expected user or co-author".into()),
                }
            }
            Slot::Workspace(w) => {
                *w = Workspace::parse(value).ok_or("expected clone or worktree")?;
            }
            Slot::Integration(i) => {
                *i = Integration::parse(value).ok_or("expected none or herdr")?;
            }
            Slot::OptText(v) => *v = (!value.is_empty()).then(|| value.to_string()),
            Slot::Text(v) => {
                if value.is_empty() {
                    return Err("expected a name".into());
                }
                *v = value.to_string();
            }
            Slot::Switch(v) => {
                *v = match value {
                    "on" => true,
                    "off" => false,
                    _ => return Err("expected on or off".into()),
                }
            }
        }
        Ok(())
    }

    fn slot(&mut self, key: &str) -> Option<Slot<'_>> {
        let tier = |s: &str| {
            s.parse::<usize>()
                .ok()
                .filter(|t| (1..=5).contains(t))
                .map(|t| t - 1)
        };
        Some(match key.split_once('.') {
            None => match key {
                "important_threshold" => Slot::Number(&mut self.important_threshold, 0.0),
                "urgent_within" => Slot::Count(&mut self.urgent_within, 0, i64::MAX),
                "quadrant_limit" => Slot::Count(&mut self.quadrant_limit, 1, i64::MAX),
                "agent" => Slot::Text(&mut self.agent),
                "preset" => Slot::OptText(&mut self.preset),
                "default_tier" => Slot::Tier(&mut self.default_tier),
                "attribution" => Slot::Attribution(&mut self.attribution),
                "max_parallel" => Slot::Count(&mut self.max_parallel, 1, i64::MAX),
                "batch_budget" => Slot::Number(&mut self.batch_budget, 0.0),
                "agent_budget" => Slot::Number(&mut self.agent_budget, 0.0),
                "workflow_budget" => Slot::Number(&mut self.workflow_budget, 0.0),
                "instance_budget" => Slot::OptNumber(&mut self.instance_budget, 0.0),
                // Four hours. Minutes become seconds, which must not overflow, and a
                // value meant as seconds is refused.
                "timeout" => Slot::Count(&mut self.timeout, 1, 240),
                "notify" => Slot::OptText(&mut self.notify),
                "workspace" => Slot::Workspace(&mut self.workspace),
                "integration" => Slot::Integration(&mut self.integration),
                _ => return None,
            },
            Some(("tiers", t)) => Slot::Number(&mut self.tiers[tier(t)?], 0.0),
            Some(("priorities", p)) => {
                let i = Priority::ALL.iter().position(|x| x.name() == p)?;
                Slot::Number(&mut self.priorities[i], 0.0)
            }
            Some(("signals", s)) => Slot::Priority(match s {
                "ci" => &mut self.signals.ci,
                "deps" => &mut self.signals.deps,
                "activity" => &mut self.signals.activity,
                "hygiene" => &mut self.signals.hygiene,
                _ => return None,
            }),
            Some(("activity", "ignore")) => Slot::List(&mut self.activity_ignore),
            Some(("deps", "days")) => Slot::Count(&mut self.deps_days, 0, i64::MAX),
            Some(("activity", rest)) => {
                let t = rest.strip_prefix("horizon.")?;
                Slot::Count(&mut self.activity_horizon[tier(t)?], 1, i64::MAX)
            }
            Some(("projects", rest)) => {
                // The field is the last component, so a name may contain dots.
                let (name, field) = rest.rsplit_once('.')?;
                if name.is_empty() || !matches!(field, "verify" | "sandbox") {
                    return None;
                }
                let settings = self.projects.entry(name.to_string()).or_default();
                match field {
                    "verify" => Slot::OptText(&mut settings.verify),
                    _ => Slot::Switch(&mut settings.sandbox),
                }
            }
            _ => return None,
        })
    }

    pub fn tier(&self, tier: u8) -> f64 {
        self.tiers[usize::from(tier) - 1]
    }

    pub fn priority(&self, p: Priority) -> f64 {
        self.priorities[p as usize]
    }

    pub fn project(&self, name: &str) -> ProjectSettings {
        self.projects.get(name).cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_reads_and_round_trips() {
        let keys = Config::keys();
        assert_eq!(keys.len(), 36);
        let defaults = Config::default();
        let mut copy = Config::default();
        for key in &keys {
            let value = defaults
                .get(key)
                .unwrap_or_else(|| panic!("{key} has no value"));
            copy.set(key, &value)
                .unwrap_or_else(|e| panic!("{key} = {value}: {e}"));
        }
        assert_eq!(copy, defaults);
    }

    #[test]
    fn set_changes_the_named_field() {
        let mut cfg = Config::default();
        cfg.set("tiers.3", "0.5").unwrap();
        cfg.set("priorities.high", "0.7").unwrap();
        cfg.set("default_tier", "3").unwrap();
        cfg.set("signals.ci", "critical").unwrap();
        cfg.set("activity.ignore", " docs/** , ,*.md").unwrap();
        cfg.set("activity.horizon.2", "14").unwrap();
        cfg.set("deps.days", "14").unwrap();
        assert_eq!(cfg.tiers[2], 0.5);
        assert_eq!(cfg.priority(Priority::High), 0.7);
        assert_eq!(cfg.default_tier, Some(3));
        assert_eq!(cfg.signals.ci, Priority::Critical);
        assert_eq!(cfg.activity_ignore, ["docs/**", "*.md"]);
        assert_eq!(cfg.activity_horizon[1], 14);
        assert_eq!(cfg.deps_days, 14);

        cfg.set("agent", "codex").unwrap();
        // `model` is retired: a model belongs to the worker record.
        assert!(cfg.set("model", "haiku").is_err());
        cfg.set("attribution", "co-author").unwrap();
        cfg.set("projects.cyllama.verify", " make check ").unwrap();
        assert_eq!(cfg.agent, "codex");
        assert_eq!(cfg.attribution, Attribution::CoAuthor);
        assert_eq!(cfg.project("cyllama").verify.as_deref(), Some("make check"));
        assert!(
            !cfg.project("cyllama").sandbox,
            "verify runs unconfined unless asked"
        );
        cfg.set("projects.cyllama.sandbox", "on").unwrap();
        assert!(cfg.project("cyllama").sandbox);
        assert_eq!(cfg.get("projects.cyllama.sandbox").as_deref(), Some("on"));
        assert_eq!(cfg.workspace, Workspace::Clone);
        cfg.set("workspace", "worktree").unwrap();
        assert_eq!(cfg.workspace, Workspace::Worktree);
        assert_eq!(cfg.get("workspace").as_deref(), Some("worktree"));
        assert!(cfg.set("workspace", "branch").is_err());
        cfg.set("projects.socket.io.verify", "npm test").unwrap();
        assert_eq!(cfg.project("socket.io").verify.as_deref(), Some("npm test"));
        assert_eq!(cfg.project("socket").verify, None);
        // Where a run goes is the command now, so both settings are retired.
        for key in ["publish", "projects.cyllama.publish"] {
            let e = cfg.set(key, "pr").unwrap_err();
            assert!(e.contains("was retired") && e.contains("pma pr"), "{e}");
        }
    }

    /// A store that set a retired key must still open, and asking about the
    /// name must say what replaced it.
    #[test]
    fn retired_keys_explain_themselves_instead_of_failing() {
        let cfg = Config::with_overrides([
            ("stale_after.1", "30"),
            ("dispatch_quadrants", "q1"),
            ("urgent_within", "3"),
        ])
        .expect("a retired row does not break the config");
        assert_eq!(cfg.urgent_within, 3);

        let e = Config::default()
            .set("stale_after.1", "30")
            .expect_err("setting one is refused");
        assert!(e.contains("was retired") && e.contains("pma stale"), "{e}");
        assert!(
            Config::default()
                .set("nonsense", "1")
                .unwrap_err()
                .contains("unknown setting")
        );
    }

    #[test]
    fn invalid_values_and_keys_are_refused() {
        let mut cfg = Config::default();
        for (key, value) in [
            ("tiers.0", "1"),
            ("tiers.6", "1"),
            ("tiers.1", "-0.1"),
            ("tiers.1", "NaN"),
            ("tiers.1", "inf"),
            ("urgent_within", "1.5"),
            ("quadrant_limit", "0"),
            ("default_tier", "-1"),
            ("default_tier", "0"),
            ("default_tier", "6"),
            ("signals.ci", "urgent"),
            ("weights.ci", "1"),
            ("activity.horizon.1", "0"),
            ("agent", ""),
            ("attribution", "agent"),
            ("max_parallel", "0"),
            ("timeout", "241"),
            ("timeout", "9223372036854775807"),
            ("projects..verify", "x"),
            ("projects.a.colour", "x"),
            ("projects.a.sandbox", "no"),
            ("nope", "1"),
        ] {
            assert!(cfg.set(key, value).is_err(), "{key} = {value} was accepted");
        }
        assert_eq!(cfg, Config::default(), "a refused set changes nothing");
    }

    #[test]
    fn stored_overrides_apply_or_name_the_bad_row() {
        let cfg = Config::with_overrides([("tiers.1", "0.9")]).unwrap();
        assert_eq!(cfg.tier(1), 0.9);
        let err = Config::with_overrides([("tiers.1", "x")]).unwrap_err();
        assert!(err.contains("pma config tiers.1 --reset"), "{err}");
    }
}
