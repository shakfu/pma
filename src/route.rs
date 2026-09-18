//! Routing policy: which worker and model take a task, and how much autonomy
//! the result gets. The policy is an artifact, not a judgment per task. One
//! decision per revision, applied deterministically to every dispatch.
//!
//! Everything a match reads is computed without a model: class from the
//! signal type or the item's tags, complexity from the versioned rule, tier
//! from the project. The document is JSON because that is what this crate
//! already parses; the design called it `routing.toml`, and adding a TOML
//! parser plus `serde` derive for one file is a larger change than the
//! syntax is worth.
//!
//! Nothing here approves, ships or merges. A route names an approval mode;
//! phases 4b and 4c are what act on one.

use serde_json::Value;

use crate::class::Class;

/// How much autonomy a route's runs get. `pma` records the mode; acting on
/// it belongs to the approval work of phases 4b and 4c.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    /// A patch and a summary; nothing is published.
    Propose,
    /// The user approves each run.
    Each,
    /// The user approves a filtered list.
    Batch,
    /// The tool publishes clean runs and reports afterwards.
    Unattended,
}

impl Approval {
    const ALL: [(&'static str, Approval); 4] = [
        ("propose", Approval::Propose),
        ("each", Approval::Each),
        ("batch", Approval::Batch),
        ("unattended", Approval::Unattended),
    ];

    pub fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(_, a)| *a == self)
            .map_or("", |(n, _)| n)
    }

    pub fn parse(s: &str) -> Option<Approval> {
        Self::ALL.iter().find(|(n, _)| *n == s).map(|(_, a)| *a)
    }
}

/// A second attempt at a stronger model after the check refused the first.
#[derive(Debug, Clone, PartialEq)]
pub struct Escalate {
    pub model: String,
    /// Attempts beyond the first. Only 1 is honoured today.
    pub attempts: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    /// For reading a replay. Defaults to the route's position.
    pub name: String,
    pub classes: Option<Vec<Class>>,
    pub complexity: Option<(i64, i64)>,
    pub tier: Option<(i64, i64)>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub escalate: Option<Escalate>,
    /// Overrides the class's own globs. Empty means no bound.
    pub scope: Option<Vec<String>>,
    pub approval: Approval,
}

/// What a dispatch knows before it chooses. Every field is recorded on the
/// run, so a replay reads these back rather than re-deriving them from a
/// task that may since have been edited or rescanned.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Subject {
    pub class: Class,
    pub complexity: i64,
    pub tier: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Policy {
    pub routes: Vec<Route>,
}

impl Route {
    fn matches(&self, s: &Subject) -> bool {
        if let Some(classes) = &self.classes
            && !classes.contains(&s.class)
        {
            return false;
        }
        if let Some((lo, hi)) = self.complexity
            && !(lo..=hi).contains(&s.complexity)
        {
            return false;
        }
        if let Some((lo, hi)) = self.tier {
            // An untiered project matches only a route that does not ask.
            match s.tier {
                Some(t) => {
                    if !(lo..=hi).contains(&i64::from(t)) {
                        return false;
                    }
                }
                None => return false,
            }
        }
        true
    }
}

impl Policy {
    /// The first route whose every stated condition holds. Order is the
    /// policy: a document is read top to bottom, like the file it came from.
    pub fn route(&self, s: &Subject) -> Option<&Route> {
        self.routes.iter().find(|r| r.matches(s))
    }

    /// Reads a policy document, refusing anything it cannot apply exactly.
    /// A route that silently does nothing is worse than one that is refused.
    pub fn parse(text: &str) -> Result<Policy, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
        let list = v
            .get("route")
            .and_then(Value::as_array)
            .ok_or("expected a `route` array")?;
        if list.is_empty() {
            return Err("a policy with no routes would refuse every dispatch".into());
        }
        let routes = list
            .iter()
            .enumerate()
            .map(|(i, r)| parse_route(i, r))
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Policy { routes })
    }

    /// The document as stored, so an activation and a replay read the same
    /// text the author wrote.
    pub fn to_json(&self) -> String {
        let routes: Vec<Value> = self
            .routes
            .iter()
            .map(|r| {
                let mut m = serde_json::Map::new();
                m.insert("name".into(), r.name.clone().into());
                let mut cond = serde_json::Map::new();
                if let Some(c) = &r.classes {
                    cond.insert(
                        "class".into(),
                        c.iter().map(|c| c.name()).collect::<Vec<_>>().into(),
                    );
                }
                if let Some((lo, hi)) = r.complexity {
                    cond.insert("complexity".into(), format!("{lo}-{hi}").into());
                }
                if let Some((lo, hi)) = r.tier {
                    cond.insert("tier".into(), format!("{lo}-{hi}").into());
                }
                m.insert("match".into(), Value::Object(cond));
                if let Some(a) = &r.agent {
                    m.insert("agent".into(), a.clone().into());
                }
                if let Some(x) = &r.model {
                    m.insert("model".into(), x.clone().into());
                }
                if let Some(e) = &r.escalate {
                    m.insert(
                        "escalate".into(),
                        serde_json::json!({"model": e.model, "attempts": e.attempts}),
                    );
                }
                if let Some(s) = &r.scope {
                    m.insert("scope".into(), s.clone().into());
                }
                m.insert("approval".into(), r.approval.name().into());
                Value::Object(m)
            })
            .collect();
        serde_json::to_string_pretty(&serde_json::json!({ "route": routes })).unwrap_or_default()
    }
}

fn parse_route(i: usize, v: &Value) -> Result<Route, String> {
    let at = |e: String| format!("route {}: {e}", i + 1);
    let name = v["name"]
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| format!("#{}", i + 1));
    let cond = &v["match"];
    let classes = match &cond["class"] {
        Value::Null => None,
        Value::String(s) => Some(vec![
            Class::parse(s).ok_or_else(|| at(format!("unknown class `{s}`")))?,
        ]),
        Value::Array(a) => Some(
            a.iter()
                .map(|c| {
                    c.as_str()
                        .and_then(Class::parse)
                        .ok_or_else(|| at(format!("unknown class `{c}`")))
                })
                .collect::<Result<Vec<_>, String>>()?,
        ),
        other => return Err(at(format!("class must be a name or a list, not {other}"))),
    };
    let range = |key: &str| -> Result<Option<(i64, i64)>, String> {
        match &cond[key] {
            Value::Null => Ok(None),
            Value::Number(n) => {
                let n = n
                    .as_i64()
                    .ok_or_else(|| at(format!("{key} must be whole")))?;
                Ok(Some((n, n)))
            }
            Value::String(s) => {
                let (lo, hi) = s.split_once('-').unwrap_or((s, s));
                let lo: i64 = lo
                    .trim()
                    .parse()
                    .map_err(|_| at(format!("{key} `{s}` is not a range")))?;
                let hi: i64 = hi
                    .trim()
                    .parse()
                    .map_err(|_| at(format!("{key} `{s}` is not a range")))?;
                if lo > hi {
                    return Err(at(format!("{key} `{s}` is empty")));
                }
                Ok(Some((lo, hi)))
            }
            other => Err(at(format!(
                "{key} must be a number or a range, not {other}"
            ))),
        }
    };
    let escalate = match &v["escalate"] {
        Value::Null => None,
        e => {
            let model = e["model"]
                .as_str()
                .ok_or_else(|| at("escalate needs a model".into()))?;
            Some(Escalate {
                model: model.into(),
                attempts: e["attempts"].as_i64().unwrap_or(1).clamp(0, 1),
            })
        }
    };
    let scope = match &v["scope"] {
        Value::Null => None,
        Value::Array(a) => Some(
            a.iter()
                .map(|g| {
                    g.as_str()
                        .map(String::from)
                        .ok_or_else(|| at("scope globs must be strings".into()))
                })
                .collect::<Result<Vec<_>, String>>()?,
        ),
        _ => return Err(at("scope must be a list of globs".into())),
    };
    let approval = match v["approval"].as_str() {
        None => return Err(at("no approval mode; state one explicitly".into())),
        Some(s) => Approval::parse(s).ok_or_else(|| at(format!("unknown approval `{s}`")))?,
    };
    let route = Route {
        name,
        classes,
        complexity: range("complexity")?,
        tier: range("tier")?,
        agent: v["agent"].as_str().map(String::from),
        model: v["model"].as_str().map(String::from),
        escalate,
        scope,
        approval,
    };
    // A limit no policy may raise. Checked when the document is read, so an
    // unacceptable route cannot be stored, let alone activated.
    if route.approval == Approval::Unattended
        && route.classes.as_ref().is_none_or(|c| {
            c.iter()
                .any(|c| matches!(c, Class::Privileged | Class::Judgment | Class::Never))
        })
    {
        return Err(at(
            "unattended needs an explicit class list, and never covers A-, C or D".into(),
        ));
    }
    Ok(route)
}

/// What a candidate policy would have done to the runs already recorded.
/// The comparison reads each run's own snapshot, so a task edited, retiered,
/// rescanned or reworked since does not change the answer.
pub struct Difference {
    pub run: i64,
    pub was: String,
    pub would_be: String,
}

/// `runs` are compared by the route each would match now. A run dispatched
/// before any policy existed has no recorded route; it is reported as `-`,
/// which is a difference worth seeing rather than a match.
pub fn replay(policy: &Policy, runs: &[crate::store::Run]) -> (Vec<Difference>, usize) {
    let mut differences = Vec::new();
    let mut compared = 0;
    for run in runs {
        let (Some(class), Some(complexity)) = (run.class, run.complexity) else {
            continue;
        };
        compared += 1;
        let subject = Subject {
            class,
            complexity,
            tier: run.tier,
        };
        let was = describe(
            run.route.as_deref(),
            run.agent.as_str(),
            run.model.as_deref(),
        );
        let would_be = match policy.route(&subject) {
            None => "refused: no route matches".to_string(),
            Some(r) => describe(
                Some(&r.name),
                r.agent.as_deref().unwrap_or(&run.agent),
                r.model.as_deref().or(run.model.as_deref()),
            ),
        };
        if was != would_be {
            differences.push(Difference {
                run: run.id,
                was,
                would_be,
            });
        }
    }
    (differences, compared)
}

fn describe(route: Option<&str>, agent: &str, model: Option<&str>) -> String {
    format!(
        "{} {agent}{}",
        route.unwrap_or("-"),
        model.map_or(String::new(), |m| format!("/{m}"))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"{
      "route": [
        {"name": "chores", "match": {"class": "A", "complexity": "1-2"},
         "agent": "claude", "model": "haiku",
         "escalate": {"model": "sonnet", "attempts": 1},
         "approval": "batch"},
        {"name": "specified", "match": {"class": ["B"], "complexity": "3-4", "tier": "1-3"},
         "model": "sonnet", "approval": "each"},
        {"name": "rest", "match": {}, "approval": "propose"}
      ]
    }"#;

    fn subject(class: Class, complexity: i64, tier: Option<u8>) -> Subject {
        Subject {
            class,
            complexity,
            tier,
        }
    }

    #[test]
    fn the_first_matching_route_wins() {
        let p = Policy::parse(DOC).unwrap();
        let named = |s| p.route(&s).unwrap().name.clone();
        assert_eq!(named(subject(Class::Mechanical, 1, Some(4))), "chores");
        assert_eq!(named(subject(Class::Specified, 3, Some(1))), "specified");
        // Right class, complexity outside the range.
        assert_eq!(named(subject(Class::Specified, 5, Some(1))), "rest");
        // Right class and complexity, tier outside it.
        assert_eq!(named(subject(Class::Specified, 3, Some(5))), "rest");
        // A condition that is not stated does not narrow anything.
        assert_eq!(named(subject(Class::Judgment, 5, None)), "rest");
    }

    /// A route that asks about tier cannot match a project without one.
    #[test]
    fn an_untiered_project_matches_only_routes_that_do_not_ask() {
        let p = Policy::parse(DOC).unwrap();
        assert_eq!(
            p.route(&subject(Class::Specified, 3, None)).unwrap().name,
            "rest"
        );
    }

    #[test]
    fn a_policy_that_matches_nothing_is_refused_rather_than_empty() {
        let p = Policy {
            routes: vec![Route {
                name: "narrow".into(),
                classes: Some(vec![Class::Mechanical]),
                complexity: None,
                tier: None,
                agent: None,
                model: None,
                escalate: None,
                scope: None,
                approval: Approval::Each,
            }],
        };
        assert!(p.route(&subject(Class::Specified, 3, Some(1))).is_none());
        assert!(Policy::parse(r#"{"route": []}"#).is_err());
        assert!(Policy::parse(r#"{"routes": []}"#).is_err());
    }

    #[test]
    fn a_document_round_trips_through_its_stored_form() {
        let p = Policy::parse(DOC).unwrap();
        assert_eq!(Policy::parse(&p.to_json()), Ok(p));
    }

    #[test]
    fn unreadable_routes_are_refused_with_their_position() {
        for (doc, why) in [
            (
                r#"{"route":[{"match":{},"approval":"maybe"}]}"#,
                "unknown approval",
            ),
            (r#"{"route":[{"match":{}}]}"#, "no approval mode"),
            (
                r#"{"route":[{"match":{"class":"Z"},"approval":"each"}]}"#,
                "unknown class",
            ),
            (
                r#"{"route":[{"match":{"complexity":"4-2"},"approval":"each"}]}"#,
                "is empty",
            ),
            (
                r#"{"route":[{"match":{"complexity":"low"},"approval":"each"}]}"#,
                "not a range",
            ),
            (
                r#"{"route":[{"match":{},"escalate":{"attempts":1},"approval":"each"}]}"#,
                "escalate needs a model",
            ),
        ] {
            let e = Policy::parse(doc).unwrap_err();
            assert!(e.contains(why) && e.contains("route 1"), "{doc}: {e}");
        }
    }

    /// A limit no document may raise, refused where it is read rather than
    /// where it would be acted on.
    #[test]
    fn unattended_never_covers_a_privileged_or_judgment_class() {
        for class in ["A-", "C", "D"] {
            let doc = format!(
                r#"{{"route":[{{"match":{{"class":"{class}"}},"approval":"unattended"}}]}}"#
            );
            assert!(Policy::parse(&doc).is_err(), "{class}");
        }
        // An unstated class list would cover them all.
        assert!(Policy::parse(r#"{"route":[{"match":{},"approval":"unattended"}]}"#).is_err());
        assert!(
            Policy::parse(r#"{"route":[{"match":{"class":["A","B"]},"approval":"unattended"}]}"#)
                .is_ok()
        );
    }
}
