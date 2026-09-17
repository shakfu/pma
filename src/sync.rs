//! Hybrid sync between `Critical` items in TODO.md and GitHub Issues.
//!
//! TODO.md wins on text and priority; GitHub wins on closed state. Rules are
//! in `docs/dev/design.md`. Write-backs are left uncommitted in the clone.

use std::path::Path;
use std::process::Command;

use crate::todo::{self, Parsed, Priority, normal_text};

pub const LABEL: &str = "pma:critical";

#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub open: bool,
    pub author: String,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Open an issue for a `Critical` item and write `gh:N` into its line.
    Create {
        line: usize,
        title: String,
        body: String,
    },
    /// Write `gh:N` for an open, labelled, unlinked issue with the item's
    /// title: one created by a sync that stopped before writing the line.
    Link {
        line: usize,
        number: u64,
    },
    /// The issue is closed: mark the item done.
    Close {
        line: usize,
        number: u64,
    },
    Retitle {
        number: u64,
        title: String,
    },
    Label {
        number: u64,
    },
    Unlabel {
        number: u64,
    },
    /// `gh:N` names no issue in the repo.
    Missing {
        line: usize,
        number: u64,
    },
    /// An open issue by someone else, linked to no item.
    Untriaged {
        number: u64,
        title: String,
        author: String,
    },
}

impl Action {
    /// Whether applying it changes GitHub or TODO.md.
    pub fn is_change(&self) -> bool {
        !matches!(self, Action::Missing { .. } | Action::Untriaged { .. })
    }

    /// The TODO.md line the action concerns, if any.
    pub fn line(&self) -> Option<usize> {
        match self {
            Action::Create { line, .. }
            | Action::Link { line, .. }
            | Action::Close { line, .. }
            | Action::Missing { line, .. } => Some(*line),
            _ => None,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Action::Create { title, .. } => format!("open an issue for `{title}`"),
            Action::Link { number, .. } => format!("link #{number}, already open"),
            Action::Close { number, .. } => format!("mark done, #{number} is closed"),
            Action::Retitle { number, title } => format!("retitle #{number} to `{title}`"),
            Action::Label { number } => format!("label #{number} {LABEL}"),
            Action::Unlabel { number } => format!("unlabel #{number}, no longer Critical"),
            Action::Missing { number, .. } => {
                format!("warning: #{number} is not an issue in this repo")
            }
            Action::Untriaged {
                number,
                title,
                author,
            } => format!("untriaged #{number} by {author}: {title}"),
        }
    }
}

/// Compares a parsed TODO.md with the repo's issues. `me` is the GitHub login
/// whose issues are not untriaged.
pub fn plan(parsed: &Parsed, issues: &[Issue], me: &str) -> Vec<Action> {
    let linked: Vec<u64> = parsed.items.iter().filter_map(|i| i.gh).collect();
    let mut claimed: Vec<u64> = Vec::new();
    let mut actions = Vec::new();

    for item in parsed.items.iter().filter(|i| !i.done) {
        let critical = item.priority == Priority::Critical;
        let Some(number) = item.gh else {
            if !critical {
                continue;
            }
            let orphan = issues.iter().find(|i| {
                i.open
                    && i.labels.iter().any(|l| l == LABEL)
                    && !linked.contains(&i.number)
                    && !claimed.contains(&i.number)
                    && normal_text(&i.title) == normal_text(&item.text)
            });
            actions.push(match orphan {
                Some(issue) => {
                    claimed.push(issue.number);
                    Action::Link {
                        line: item.line,
                        number: issue.number,
                    }
                }
                None => Action::Create {
                    line: item.line,
                    title: item.text.clone(),
                    body: body(&item.description),
                },
            });
            continue;
        };
        let Some(issue) = issues.iter().find(|i| i.number == number) else {
            actions.push(Action::Missing {
                line: item.line,
                number,
            });
            continue;
        };
        if !issue.open {
            actions.push(Action::Close {
                line: item.line,
                number,
            });
            continue;
        }
        if issue.title != item.text {
            actions.push(Action::Retitle {
                number,
                title: item.text.clone(),
            });
        }
        let labelled = issue.labels.iter().any(|l| l == LABEL);
        if critical && !labelled {
            actions.push(Action::Label { number });
        } else if !critical && labelled {
            actions.push(Action::Unlabel { number });
        }
    }

    for issue in issues {
        if issue.open
            && issue.author != me
            && !linked.contains(&issue.number)
            && !claimed.contains(&issue.number)
        {
            actions.push(Action::Untriaged {
                number: issue.number,
                title: issue.title.clone(),
                author: issue.author.clone(),
            });
        }
    }
    actions
}

fn body(description: &[String]) -> String {
    let text: Vec<&str> = description.iter().map(|l| l.trim()).collect();
    let text = text.join("\n").trim().to_string();
    let note = "Opened by pma from a `## Critical` item in TODO.md.";
    if text.is_empty() {
        note.into()
    } else {
        format!("{text}\n\n{note}")
    }
}

/// Runs `gh`, returning stdout or an error with its stderr.
pub fn gh(args: &[&str]) -> Result<String, String> {
    let out = Command::new("gh")
        .args(args)
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                "`gh` is not on PATH; install and authenticate it".into()
            }
            _ => format!("gh: {e}"),
        })?;
    if !out.status.success() {
        return Err(format!(
            "gh {}: {}",
            args.iter().take(2).copied().collect::<Vec<_>>().join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn login() -> Result<String, String> {
    Ok(gh(&["api", "user", "--jq", ".login"])?.trim().to_string())
}

pub fn issues(repo: &str) -> Result<Vec<Issue>, String> {
    let out = gh(&[
        "issue",
        "list",
        "-R",
        repo,
        "--state",
        "all",
        "--limit",
        "5000",
        "--json",
        "number,title,state,author,labels",
    ])?;
    parse_issues(&out)
}

fn parse_issues(json: &str) -> Result<Vec<Issue>, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("gh issue list: {e}"))?;
    let rows = value.as_array().ok_or("gh issue list: expected an array")?;
    rows.iter()
        .map(|r| {
            Ok(Issue {
                number: r["number"]
                    .as_u64()
                    .ok_or("gh issue list: issue without a number")?,
                title: r["title"].as_str().unwrap_or_default().to_string(),
                open: r["state"].as_str() == Some("OPEN"),
                author: r["author"]["login"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                labels: r["labels"]
                    .as_array()
                    .map(|ls| {
                        ls.iter()
                            .filter_map(|l| l["name"].as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect()
}

/// Applies the changes in `actions` to GitHub and to `todo_path`, in plan
/// order, stopping at the first failure. Each write-back is saved before the
/// next call, so a stopped sync is resumed by `Link` rather than duplicated.
/// Returns the number of changes applied.
pub fn apply(repo: &str, todo_path: &Path, actions: &[Action]) -> Result<usize, String> {
    let io = |e: std::io::Error| format!("{}: {e}", todo_path.display());
    let edit = |f: &dyn Fn(&str) -> Option<String>| -> Result<(), String> {
        let text = std::fs::read_to_string(todo_path).map_err(io)?;
        let edited = f(&text)
            .ok_or_else(|| format!("{} changed during sync; run it again", todo_path.display()))?;
        std::fs::write(todo_path, edited).map_err(io)
    };
    let link = |line: usize, number: u64| {
        edit(&|t| {
            let unlinked = todo::parse(t)
                .items
                .iter()
                .any(|i| i.line == line && i.gh.is_none());
            unlinked
                .then(|| todo::add_token(t, line, &format!("gh:{number}")))
                .flatten()
        })
    };
    let mut label_ready = false;
    let ensure_label = |label_ready: &mut bool| -> Result<(), String> {
        if !*label_ready {
            gh(&[
                "label",
                "create",
                LABEL,
                "-R",
                repo,
                "--force",
                "--color",
                "B60205",
                "--description",
                "Critical item in TODO.md, synced by pma",
            ])?;
            *label_ready = true;
        }
        Ok(())
    };

    let mut applied = 0;
    for action in actions.iter().filter(|a| a.is_change()) {
        match action {
            Action::Create { line, title, body } => {
                ensure_label(&mut label_ready)?;
                let url = gh(&[
                    "issue", "create", "-R", repo, "--title", title, "--body", body, "--label",
                    LABEL,
                ])?;
                let number = url
                    .trim()
                    .rsplit('/')
                    .next()
                    .and_then(|n| n.parse::<u64>().ok())
                    .ok_or_else(|| {
                        format!("gh issue create: unexpected output `{}`", url.trim())
                    })?;
                link(*line, number)?;
            }
            Action::Link { line, number } => link(*line, *number)?,
            Action::Close { number, .. } => {
                edit(&|t| todo::mark_done(t, &format!("gh:{number}"), ""))?;
            }
            Action::Retitle { number, title } => {
                gh(&[
                    "issue",
                    "edit",
                    &number.to_string(),
                    "-R",
                    repo,
                    "--title",
                    title,
                ])?;
            }
            Action::Label { number } => {
                ensure_label(&mut label_ready)?;
                gh(&[
                    "issue",
                    "edit",
                    &number.to_string(),
                    "-R",
                    repo,
                    "--add-label",
                    LABEL,
                ])?;
            }
            Action::Unlabel { number } => {
                gh(&[
                    "issue",
                    "edit",
                    &number.to_string(),
                    "-R",
                    repo,
                    "--remove-label",
                    LABEL,
                ])?;
            }
            Action::Missing { .. } | Action::Untriaged { .. } => {}
        }
        applied += 1;
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(number: u64, title: &str, open: bool, author: &str, labels: &[&str]) -> Issue {
        Issue {
            number,
            title: title.into(),
            open,
            author: author.into(),
            labels: labels.iter().map(|l| l.to_string()).collect(),
        }
    }

    #[test]
    fn plan_covers_every_rule() {
        let text = "# TODO

## Critical

- [ ] new crash
  on empty input
- [ ] half synced
- [ ] linked gh:1
- [ ] renamed text gh:2
- [ ] closed upstream gh:3
- [ ] gone gh:9

## High

- [ ] demoted gh:4
- [ ] not critical
- [x] old gh:5
";
        let parsed = todo::parse(text);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        let issues = [
            issue(1, "linked", true, "me", &[LABEL]),
            issue(2, "old text", true, "me", &[]),
            issue(3, "closed upstream", false, "me", &[LABEL]),
            issue(4, "demoted", true, "me", &[LABEL]),
            issue(5, "old", true, "alice", &[]),
            issue(6, "Half  Synced", true, "me", &[LABEL]),
            issue(7, "feature request", true, "alice", &[]),
            issue(8, "mine, unlinked", true, "me", &[]),
            issue(10, "stale report", false, "bob", &[]),
        ];
        let actions = plan(&parsed, &issues, "me");
        assert_eq!(
            actions,
            [
                Action::Create {
                    line: 5,
                    title: "new crash".into(),
                    body: "on empty input\n\nOpened by pma from a `## Critical` item in TODO.md."
                        .into()
                },
                Action::Link { line: 7, number: 6 },
                Action::Retitle {
                    number: 2,
                    title: "renamed text".into()
                },
                Action::Label { number: 2 },
                Action::Close {
                    line: 10,
                    number: 3
                },
                Action::Missing {
                    line: 11,
                    number: 9
                },
                Action::Unlabel { number: 4 },
                Action::Untriaged {
                    number: 7,
                    title: "feature request".into(),
                    author: "alice".into()
                },
            ]
        );
        assert_eq!(actions.iter().filter(|a| a.is_change()).count(), 6);
    }

    #[test]
    fn gh_issue_json_is_read() {
        let json = r#"[{"number":12,"title":"crash","state":"OPEN","author":{"login":"me"},"labels":[{"name":"pma:critical"},{"name":"bug"}]},
                       {"number":3,"title":"old","state":"CLOSED","author":{"login":"bob"},"labels":[]}]"#;
        assert_eq!(
            parse_issues(json).unwrap(),
            [
                issue(12, "crash", true, "me", &[LABEL, "bug"]),
                issue(3, "old", false, "bob", &[]),
            ]
        );
        assert!(parse_issues("{}").is_err());
    }
}
