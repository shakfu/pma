//! Configuration: built-in defaults plus overrides stored in the database,
//! addressed by flat keys such as `tiers.2`, `weights.ci` or
//! `projects.cyllama.verify`.

use std::collections::BTreeMap;

use crate::rank::Quadrant;
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
    pub weights: Weights,
    /// Days open without `due:` before a task is urgent; `None` is never.
    pub stale_after: [Option<i64>; 5],
    pub signals: SignalPriorities,
    /// Globs for paths whose commits do not count as activity.
    pub activity_ignore: Vec<String>,
    /// Days without activity at which the activity signal reaches 1, per tier.
    pub activity_horizon: [i64; 5],
    /// Quadrants `pma dispatch --auto` draws from.
    pub dispatch_quadrants: Vec<Quadrant>,
    /// Drawn from once `dispatch_quadrants` has no candidate left.
    pub overflow_quadrants: Vec<Quadrant>,
    pub publish: Publish,
    pub attribution: Attribution,
    /// Agents running at once.
    pub max_parallel: i64,
    /// USD a dispatch batch may spend.
    pub batch_budget: f64,
    /// USD one agent run may spend.
    pub agent_budget: f64,
    /// Minutes an agent run, or a verify run, may take.
    pub timeout: i64,
    pub projects: BTreeMap<String, ProjectSettings>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Publish {
    /// Rebase onto the default branch and push it.
    Push,
    /// Push the task branch and open a pull request.
    Pr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribution {
    User,
    CoAuthor,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectSettings {
    /// Shell command that checks an agent's work; `None` detects one.
    pub verify: Option<String>,
    /// Overrides the global `publish`.
    pub publish: Option<Publish>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Weights {
    pub tasks: f64,
    pub activity: f64,
    pub ci: f64,
    pub deps: f64,
    pub hygiene: f64,
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
            weights: Weights {
                tasks: 5.0,
                activity: 1.0,
                ci: 3.0,
                deps: 1.0,
                hygiene: 2.0,
            },
            stale_after: [Some(30), Some(60), Some(90), None, None],
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
            dispatch_quadrants: vec![Quadrant::Q1, Quadrant::Q2],
            overflow_quadrants: vec![],
            publish: Publish::Pr,
            attribution: Attribution::User,
            max_parallel: 2,
            batch_budget: 5.0,
            agent_budget: 1.0,
            timeout: 30,
            projects: BTreeMap::new(),
        }
    }
}

/// A mutable reference to one setting, with its validation.
enum Slot<'a> {
    /// A finite number of at least the given minimum.
    Number(&'a mut f64, f64),
    Count(&'a mut i64, i64),
    Never(&'a mut Option<i64>),
    Priority(&'a mut Priority),
    List(&'a mut Vec<String>),
    Quadrants(&'a mut Vec<Quadrant>),
    Publish(&'a mut Publish),
    OptPublish(&'a mut Option<Publish>),
    Attribution(&'a mut Attribution),
    /// Empty text is `None`.
    OptText(&'a mut Option<String>),
}

const PUBLISH: [(&str, Publish); 2] = [("push", Publish::Push), ("pr", Publish::Pr)];

fn publish_name(p: Publish) -> &'static str {
    PUBLISH.iter().find(|(_, v)| *v == p).map_or("", |(n, _)| n)
}

fn parse_publish(s: &str) -> Result<Publish, String> {
    PUBLISH
        .iter()
        .find(|(n, _)| *n == s)
        .map(|(_, v)| *v)
        .ok_or_else(|| "expected push or pr".into())
}

impl Config {
    pub fn keys() -> Vec<String> {
        let mut keys: Vec<String> = ["important_threshold", "urgent_within", "quadrant_limit"]
            .map(String::from)
            .into();
        let per_tier = |prefix: &'static str| (1..=5).map(move |t| format!("{prefix}.{t}"));
        keys.extend(per_tier("tiers"));
        keys.extend(Priority::ALL.map(|p| format!("priorities.{}", p.name())));
        keys.extend(["tasks", "activity", "ci", "deps", "hygiene"].map(|w| format!("weights.{w}")));
        keys.extend(per_tier("stale_after"));
        keys.extend(["ci", "deps", "activity", "hygiene"].map(|s| format!("signals.{s}")));
        keys.push("activity.ignore".into());
        keys.extend(per_tier("activity.horizon"));
        keys.extend(
            [
                "dispatch_quadrants",
                "overflow_quadrants",
                "publish",
                "attribution",
                "max_parallel",
                "batch_budget",
                "agent_budget",
                "timeout",
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
            cfg.set(key, value).map_err(|e| {
                format!("stored setting {key} = {value:?} is invalid ({e}); run `pma config {key} --reset`")
            })?;
        }
        Ok(cfg)
    }

    pub fn get(&self, key: &str) -> Option<String> {
        let mut copy = self.clone();
        Some(match copy.slot(key)? {
            Slot::Number(v, _) => v.to_string(),
            Slot::Count(v, _) => v.to_string(),
            Slot::Never(v) => v.map_or("never".into(), |n| n.to_string()),
            Slot::Priority(p) => p.name().into(),
            Slot::List(v) => v.join(","),
            Slot::Quadrants(v) => v
                .iter()
                .map(|q| format!("{q:?}").to_lowercase())
                .collect::<Vec<_>>()
                .join(","),
            Slot::Publish(p) => publish_name(*p).into(),
            Slot::OptPublish(p) => p.map_or("", publish_name).into(),
            Slot::Attribution(a) => match a {
                Attribution::User => "user".into(),
                Attribution::CoAuthor => "co-author".into(),
            },
            Slot::OptText(v) => v.clone().unwrap_or_default(),
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
        let slot = self
            .slot(key)
            .ok_or_else(|| format!("unknown setting `{key}`"))?;
        let value = value.trim();
        match slot {
            Slot::Number(v, min) => {
                *v = value
                    .parse::<f64>()
                    .ok()
                    .filter(|n| n.is_finite() && *n >= min)
                    .ok_or_else(|| format!("expected a number >= {min}"))?;
            }
            Slot::Count(v, min) => {
                *v = value
                    .parse::<i64>()
                    .ok()
                    .filter(|n| *n >= min)
                    .ok_or_else(|| format!("expected a whole number >= {min}"))?;
            }
            Slot::Never(v) => {
                *v = match value {
                    "never" => None,
                    _ => Some(
                        value
                            .parse::<i64>()
                            .ok()
                            .filter(|n| *n >= 0)
                            .ok_or("expected a number of days or `never`")?,
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
            Slot::Quadrants(v) => {
                *v = value
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| {
                        Quadrant::ALL
                            .into_iter()
                            .find(|q| format!("{q:?}").eq_ignore_ascii_case(s))
                            .ok_or_else(|| format!("`{s}` is not q1, q2, q3 or q4"))
                    })
                    .collect::<Result<_, _>>()?;
            }
            Slot::Publish(p) => *p = parse_publish(value)?,
            Slot::OptPublish(p) => {
                *p = (!value.is_empty())
                    .then(|| parse_publish(value))
                    .transpose()?
            }
            Slot::Attribution(a) => {
                *a = match value {
                    "user" => Attribution::User,
                    "co-author" => Attribution::CoAuthor,
                    _ => return Err("expected user or co-author".into()),
                }
            }
            Slot::OptText(v) => *v = (!value.is_empty()).then(|| value.to_string()),
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
                "urgent_within" => Slot::Count(&mut self.urgent_within, 0),
                "quadrant_limit" => Slot::Count(&mut self.quadrant_limit, 1),
                "dispatch_quadrants" => Slot::Quadrants(&mut self.dispatch_quadrants),
                "overflow_quadrants" => Slot::Quadrants(&mut self.overflow_quadrants),
                "publish" => Slot::Publish(&mut self.publish),
                "attribution" => Slot::Attribution(&mut self.attribution),
                "max_parallel" => Slot::Count(&mut self.max_parallel, 1),
                "batch_budget" => Slot::Number(&mut self.batch_budget, 0.0),
                "agent_budget" => Slot::Number(&mut self.agent_budget, 0.0),
                "timeout" => Slot::Count(&mut self.timeout, 1),
                _ => return None,
            },
            Some(("tiers", t)) => Slot::Number(&mut self.tiers[tier(t)?], 0.0),
            Some(("priorities", p)) => {
                let i = Priority::ALL.iter().position(|x| x.name() == p)?;
                Slot::Number(&mut self.priorities[i], 0.0)
            }
            Some(("weights", w)) => Slot::Number(
                match w {
                    "tasks" => &mut self.weights.tasks,
                    "activity" => &mut self.weights.activity,
                    "ci" => &mut self.weights.ci,
                    "deps" => &mut self.weights.deps,
                    "hygiene" => &mut self.weights.hygiene,
                    _ => return None,
                },
                0.0,
            ),
            Some(("stale_after", t)) => Slot::Never(&mut self.stale_after[tier(t)?]),
            Some(("signals", s)) => Slot::Priority(match s {
                "ci" => &mut self.signals.ci,
                "deps" => &mut self.signals.deps,
                "activity" => &mut self.signals.activity,
                "hygiene" => &mut self.signals.hygiene,
                _ => return None,
            }),
            Some(("activity", "ignore")) => Slot::List(&mut self.activity_ignore),
            Some(("activity", rest)) => {
                let t = rest.strip_prefix("horizon.")?;
                Slot::Count(&mut self.activity_horizon[tier(t)?], 1)
            }
            Some(("projects", rest)) => {
                let (name, field) = rest.rsplit_once('.')?;
                if name.is_empty() || name.contains('.') || !["verify", "publish"].contains(&field)
                {
                    return None;
                }
                let settings = self.projects.entry(name.to_string()).or_default();
                match field {
                    "verify" => Slot::OptText(&mut settings.verify),
                    "publish" => Slot::OptPublish(&mut settings.publish),
                    _ => return None,
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

    pub fn publish_for(&self, name: &str) -> Publish {
        self.project(name).publish.unwrap_or(self.publish)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_reads_and_round_trips() {
        let keys = Config::keys();
        assert_eq!(keys.len(), 40);
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
        cfg.set("weights.ci", "10").unwrap();
        cfg.set("stale_after.1", "never").unwrap();
        cfg.set("stale_after.5", "400").unwrap();
        cfg.set("signals.ci", "critical").unwrap();
        cfg.set("activity.ignore", " docs/** , ,*.md").unwrap();
        cfg.set("activity.horizon.2", "14").unwrap();
        assert_eq!(cfg.tiers[2], 0.5);
        assert_eq!(cfg.priority(Priority::High), 0.7);
        assert_eq!(cfg.weights.ci, 10.0);
        assert_eq!(cfg.stale_after[0], None);
        assert_eq!(cfg.stale_after[4], Some(400));
        assert_eq!(cfg.signals.ci, Priority::Critical);
        assert_eq!(cfg.activity_ignore, ["docs/**", "*.md"]);
        assert_eq!(cfg.activity_horizon[1], 14);

        cfg.set("dispatch_quadrants", "Q1, q3").unwrap();
        cfg.set("overflow_quadrants", "").unwrap();
        cfg.set("publish", "push").unwrap();
        cfg.set("attribution", "co-author").unwrap();
        cfg.set("projects.cyllama.verify", " make check ").unwrap();
        cfg.set("projects.cyllama.publish", "pr").unwrap();
        assert_eq!(cfg.dispatch_quadrants, [Quadrant::Q1, Quadrant::Q3]);
        assert!(cfg.overflow_quadrants.is_empty());
        assert_eq!(cfg.attribution, Attribution::CoAuthor);
        assert_eq!(cfg.project("cyllama").verify.as_deref(), Some("make check"));
        assert_eq!(cfg.publish_for("cyllama"), Publish::Pr);
        assert_eq!(cfg.publish_for("other"), Publish::Push);
        assert_eq!(cfg.get("projects.cyllama.publish").unwrap(), "pr");
        cfg.set("projects.cyllama.publish", "").unwrap();
        assert_eq!(cfg.publish_for("cyllama"), Publish::Push, "empty unsets");
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
            ("stale_after.2", "-1"),
            ("signals.ci", "urgent"),
            ("weights.nope", "1"),
            ("activity.horizon.1", "0"),
            ("dispatch_quadrants", "q1,q5"),
            ("publish", "merge"),
            ("attribution", "agent"),
            ("max_parallel", "0"),
            ("projects.a.b.verify", "x"),
            ("projects.a.colour", "x"),
            ("projects.a.publish", "merge"),
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
