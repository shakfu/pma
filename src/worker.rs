//! A coding agent as a record rather than a code path: how its prompt and
//! directory are passed, how a model is named, whether a budget cap exists,
//! how success and cost are read back, and whether it has a sandbox or a
//! command allowlist.
//!
//! What the record cannot carry, the pipeline carries instead. The worktree,
//! the stripped credentials, `pma` running `verify` itself and the path scope
//! check do not depend on the worker, so a worker with no allowlist and no
//! sandbox is admitted under the same gates as one with both.

use std::process::Command;

use crate::agent::{Report, tail};

/// How a worker's output is read back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parser {
    /// A `{"type":"result", ...}` line, as `claude -p --output-format json`
    /// writes: success, error flag and `total_cost_usd`.
    ClaudeJson,
    /// No structured output. The exit status decides, the last lines are the
    /// summary, and the cost is unknown rather than zero.
    TextTail,
}

impl Parser {
    const ALL: [(&'static str, Parser); 2] = [
        ("claude-json", Parser::ClaudeJson),
        ("text-tail", Parser::TextTail),
    ];

    pub fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(_, p)| *p == self)
            .map_or("", |(n, _)| n)
    }

    pub fn parse(s: &str) -> Option<Parser> {
        Self::ALL.iter().find(|(n, _)| *n == s).map(|(_, p)| *p)
    }

    pub fn names() -> String {
        Self::ALL
            .iter()
            .map(|(n, _)| *n)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// `exit_ok` is false when the process failed or was killed. A worker
    /// with structured output states its own result, so the status is only
    /// consulted where there is nothing else to read.
    pub fn report(self, output: &str, exit_ok: bool) -> Report {
        match self {
            Parser::ClaudeJson => crate::agent::parse_claude(output),
            Parser::TextTail => Report {
                ok: exit_ok,
                summary: tail(output, 20),
                cost_usd: None,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Worker {
    pub name: String,
    pub command: String,
    /// Arguments, with `{prompt}`, `{dir}`, `{model}` and `{budget}` filled
    /// in at dispatch.
    pub args: Vec<String>,
    /// A flag and a rule template separated by a space, such as
    /// `--allowedTools Bash({cmd})`. The flag is emitted once, the rule once
    /// per subcommand of the verify command. `None` means the worker has no
    /// allowlist, which the worktree and the scope check cover instead.
    pub allow: Option<String>,
    pub parse: Parser,
    /// Whether its output states what the run cost. A worker that does not
    /// leaves the cost unknown, never zero.
    pub reports_cost: bool,
    /// Whether the budget argument is a bound the worker holds itself.
    /// `claude` checks its cap between turns and can exceed it, so this is
    /// false for `claude` too: a timeout is not a spend bound.
    pub enforces_budget: bool,
    pub sandbox: bool,
    /// Whether a rework can continue the earlier session rather than
    /// re-prompting. Unused until a worker that does arrives.
    pub resumes: bool,
}

/// The fields `pma agent set` accepts, with what each one takes.
pub const FIELDS: [(&str, &str); 8] = [
    ("command", "the program to run"),
    ("args", "a JSON array of arguments"),
    ("allow", "an allowlist flag and rule, or empty for none"),
    ("parse", "claude-json or text-tail"),
    ("reports-cost", "true or false"),
    ("enforces-budget", "true or false"),
    ("sandbox", "true or false"),
    ("resumes", "true or false"),
];

impl Worker {
    /// The command to run in `dir`. An argument whose placeholder has no
    /// value is dropped, and with it the argument before it when that one is
    /// a flag, so `--model {model}` disappears whole when no model is chosen.
    pub fn build(
        &self,
        prompt: &str,
        dir: &std::path::Path,
        model: Option<&str>,
        budget: f64,
    ) -> Command {
        let dir_text = dir.to_string_lossy().into_owned();
        let budget = budget.to_string();
        let mut args: Vec<String> = Vec::new();
        for arg in &self.args {
            let filled = arg
                .replace("{prompt}", prompt)
                .replace("{dir}", &dir_text)
                .replace("{budget}", &budget);
            let filled = match model {
                Some(m) => filled.replace("{model}", m),
                None if filled.contains("{model}") => {
                    if args.last().is_some_and(|a| a.starts_with('-')) {
                        args.pop();
                    }
                    continue;
                }
                None => filled,
            };
            args.push(filled);
        }
        let mut cmd = Command::new(&self.command);
        cmd.args(args);
        cmd
    }

    /// Appends the allowlist for `verify`, so the worker may run the check
    /// `pma` runs afterwards. A worker without an allowlist gets nothing:
    /// its reach is bounded by the worktree, not by this.
    pub fn allow_verify(&self, cmd: &mut Command, verify: Option<&str>) {
        let (Some(allow), Some(verify)) = (&self.allow, verify) else {
            return;
        };
        let Some((flag, rule)) = allow.split_once(char::is_whitespace) else {
            return;
        };
        let rules: Vec<String> = crate::agent::bash_parts(verify)
            .iter()
            .map(|part| rule.trim().replace("{cmd}", part))
            .collect();
        if !rules.is_empty() {
            cmd.arg(flag.trim()).args(rules);
        }
    }

    /// The worker `pma` shipped with. Seeded into the store on upgrade, and
    /// editable from there like any other.
    pub fn claude() -> Worker {
        Worker {
            name: "claude".into(),
            command: "claude".into(),
            args: [
                "-p",
                "{prompt}",
                "--output-format",
                "json",
                "--permission-mode",
                "acceptEdits",
                "--model",
                "{model}",
                "--max-budget-usd",
                "{budget}",
            ]
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
            allow: Some("--allowedTools Bash({cmd})".into()),
            parse: Parser::ClaudeJson,
            reports_cost: true,
            // Checked between turns, so a run can exceed it: a one-word reply
            // under a $0.05 cap cost $0.09 on 2026-09-15.
            enforces_budget: false,
            sandbox: false,
            resumes: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn placeholders_are_filled_from_the_run() {
        let w = Worker::claude();
        let cmd = w.build("do it", Path::new("/w/a"), Some("haiku"), 0.5);
        assert_eq!(
            args_of(&cmd),
            [
                "-p",
                "do it",
                "--output-format",
                "json",
                "--permission-mode",
                "acceptEdits",
                "--model",
                "haiku",
                "--max-budget-usd",
                "0.5"
            ]
        );
    }

    /// Choosing no model must not pass the literal placeholder, nor leave a
    /// `--model` with nothing after it.
    #[test]
    fn an_unset_model_takes_its_flag_with_it() {
        let cmd = Worker::claude().build("do it", Path::new("/w/a"), None, 0.5);
        let args = args_of(&cmd);
        assert!(!args.iter().any(|a| a.contains("{model}")), "{args:?}");
        assert!(!args.contains(&"--model".to_string()), "{args:?}");
        assert!(
            args.ends_with(&["--max-budget-usd".into(), "0.5".into()]),
            "{args:?}"
        );
    }

    #[test]
    fn the_directory_is_passed_where_a_worker_wants_it() {
        let w = Worker {
            args: ["exec", "-C", "{dir}", "{prompt}"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            ..Worker::claude()
        };
        assert_eq!(
            args_of(&w.build("do it", Path::new("/w/a"), None, 1.0)),
            ["exec", "-C", "/w/a", "do it"]
        );
    }

    #[test]
    fn a_worker_without_an_allowlist_gets_no_rules() {
        let w = Worker {
            allow: None,
            ..Worker::claude()
        };
        let mut cmd = w.build("do it", Path::new("/w/a"), None, 1.0);
        let before = args_of(&cmd).len();
        w.allow_verify(&mut cmd, Some("make test"));
        assert_eq!(args_of(&cmd).len(), before);
    }

    #[test]
    fn the_allowlist_names_one_rule_per_subcommand() {
        let w = Worker::claude();
        let mut cmd = w.build("do it", Path::new("/w/a"), None, 1.0);
        w.allow_verify(&mut cmd, Some("make check && cargo test"));
        let args = args_of(&cmd);
        assert!(
            args.ends_with(&[
                "--allowedTools".into(),
                "Bash(make check)".into(),
                "Bash(cargo test)".into()
            ]),
            "{args:?}"
        );
    }

    /// A worker with no structured output reports an unknown cost, never a
    /// free run.
    #[test]
    fn text_tail_reads_the_exit_status_and_leaves_the_cost_unknown() {
        let ok = Parser::TextTail.report("line one\nline two\n", true);
        assert_eq!(
            ok,
            Report {
                ok: true,
                summary: "line one\nline two".into(),
                cost_usd: None
            }
        );
        assert!(!Parser::TextTail.report("boom", false).ok);
    }

    #[test]
    fn parser_names_round_trip() {
        for (name, parser) in Parser::ALL {
            assert_eq!(Parser::parse(name), Some(parser));
            assert_eq!(parser.name(), name);
        }
        assert_eq!(Parser::parse("codex-json"), None);
    }
}
