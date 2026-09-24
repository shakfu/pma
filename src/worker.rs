//! A coding agent as a record rather than a code path: how its prompt and
//! directory are passed, how a model is named, whether a budget cap exists,
//! how success and cost are read back, and whether it has a sandbox or a
//! command allowlist.
//!
//! What the record cannot carry, the pipeline carries instead. The worktree,
//! the stripped credentials, `pma` running `verify` itself and the path scope
//! check do not depend on the worker, so a worker with no allowlist and no
//! sandbox is admitted under the same gates as one with both.

use std::collections::BTreeMap;
use std::process::Command;

use crate::agent::{Report, tail};

/// How a worker's output is read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parser {
    /// A `{"type":"result", ...}` line, as `claude -p --output-format json`
    /// writes: success, error flag and `total_cost_usd`.
    ClaudeJson,
    /// No structured output. The exit status decides, the last lines are the
    /// summary, and the cost is unknown rather than zero.
    TextTail,
    /// The last JSON value in the output, read by dotted field paths:
    /// `json:<summary>:<cost>` or `json:<summary>:<cost>:<error-flag>`. A path
    /// that resolves to nothing leaves its value unknown, and the exit status
    /// decides unless an error flag says otherwise.
    ///
    /// A worker whose output shape nothing else reads is then a record rather
    /// than another variant here.
    Fields {
        summary: String,
        cost: String,
        error: Option<String>,
    },
}

impl Parser {
    const ALL: [(&'static str, Parser); 2] = [
        ("claude-json", Parser::ClaudeJson),
        ("text-tail", Parser::TextTail),
    ];

    pub fn name(&self) -> String {
        match self {
            Parser::Fields {
                summary,
                cost,
                error,
            } => match error {
                Some(e) => format!("json:{summary}:{cost}:{e}"),
                None => format!("json:{summary}:{cost}"),
            },
            other => Self::ALL
                .iter()
                .find(|(_, p)| p == other)
                .map_or(String::new(), |(n, _)| (*n).to_string()),
        }
    }

    pub fn parse(s: &str) -> Option<Parser> {
        if let Some(paths) = s.strip_prefix("json:") {
            let mut parts = paths.split(':').map(str::trim);
            let summary = parts.next()?.to_string();
            let cost = parts.next().unwrap_or_default().to_string();
            let error = parts.next().map(String::from).filter(|e| !e.is_empty());
            if summary.is_empty() {
                return None;
            }
            return Some(Parser::Fields {
                summary,
                cost,
                error,
            });
        }
        Self::ALL
            .iter()
            .find(|(n, _)| *n == s)
            .map(|(_, p)| p.clone())
    }

    pub fn names() -> String {
        format!(
            "{}, json:<summary>:<cost>[:<error>]",
            Self::ALL
                .iter()
                .map(|(n, _)| *n)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    /// `exit_ok` is false when the process failed or was killed. A worker
    /// with structured output states its own result, so the status is only
    /// consulted where there is nothing else to read.
    pub fn report(&self, output: &str, exit_ok: bool) -> Report {
        match self {
            Parser::ClaudeJson => crate::agent::parse_claude(output),
            Parser::TextTail => Report {
                ok: exit_ok,
                summary: tail(output, 20),
                cost_usd: None,
            },
            Parser::Fields {
                summary,
                cost,
                error,
            } => {
                let Some(value) = last_json(output) else {
                    return Report {
                        ok: false,
                        summary: tail(output, 20),
                        cost_usd: None,
                    };
                };
                let flagged = error
                    .as_deref()
                    .and_then(|path| at(&value, path))
                    .map(truthy);
                Report {
                    ok: exit_ok && flagged != Some(true),
                    summary: at(&value, summary)
                        .map(text)
                        .unwrap_or_else(|| tail(output, 20)),
                    cost_usd: at(&value, cost).and_then(|v| match v {
                        serde_json::Value::Number(n) => n.as_f64(),
                        serde_json::Value::String(s) => s.parse().ok(),
                        _ => None,
                    }),
                }
            }
        }
    }
}

/// The last JSON value in the output: the whole text where that parses, else
/// the last line that does. A worker that streams events therefore reports from
/// its final one.
fn last_json(output: &str) -> Option<serde_json::Value> {
    if let Ok(v) = serde_json::from_str(output.trim()) {
        return Some(v);
    }
    output
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str(l.trim()).ok())
}

/// A dotted path into a JSON value. `a.b.0` reads a field, then a field, then
/// an array index.
fn at<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    if path.is_empty() {
        return None;
    }
    let mut at = value;
    for step in path.split('.') {
        at = match step.parse::<usize>() {
            Ok(i) => at.get(i)?,
            Err(_) => at.get(step)?,
        };
    }
    Some(at)
}

fn text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn truthy(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::String(s) => !s.is_empty() && s != "false",
        serde_json::Value::Number(n) => n.as_f64().unwrap_or_default() != 0.0,
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Worker {
    pub name: String,
    pub command: String,
    /// Arguments, with `{prompt}`, `{dir}`, `{model}`, `{budget}` and
    /// `{timeout}` filled in at dispatch. `{extra}` stands for however many
    /// arguments a preset adds, and sits where a worker wants them: before a
    /// positional prompt for one that takes its message last.
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
    /// Environment the worker needs, such as a base URL for an
    /// OpenAI-compatible endpoint or a config path. Applied to the child after
    /// the credentials `agent::restrict` strips, so it cannot restore one.
    /// Provider API keys are inherited from the session and belong here only
    /// when a worker needs a different one from the shell's.
    pub env: BTreeMap<String, String>,
}

/// The fields `pma agent set` accepts, with what each one takes.
pub const FIELDS: [(&str, &str); 9] = [
    ("command", "the program to run"),
    ("args", "a JSON array of arguments"),
    ("allow", "an allowlist flag and rule, or empty for none"),
    (
        "parse",
        "claude-json, text-tail, or json:<summary>:<cost>[:<error>]",
    ),
    ("reports-cost", "true or false"),
    ("enforces-budget", "true or false"),
    ("sandbox", "true or false"),
    ("resumes", "true or false"),
    (
        "env",
        "a JSON object of environment variables, or {} for none",
    ),
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
        extra: &[String],
        timeout: std::time::Duration,
    ) -> Command {
        let dir_text = dir.to_string_lossy().into_owned();
        let budget = budget.to_string();
        let timeout = format!("{}s", timeout.as_secs());
        let mut args: Vec<String> = Vec::new();
        for arg in &self.args {
            // Zero or more arguments in one place, so a preset's effort flag
            // need not be a field this record knows about.
            if arg == "{extra}" {
                args.extend(extra.iter().cloned());
                continue;
            }
            let filled = arg
                .replace("{prompt}", prompt)
                .replace("{dir}", &dir_text)
                .replace("{budget}", &budget)
                .replace("{timeout}", &timeout);
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
        // Before `agent::restrict`, which the caller applies last, so a
        // record cannot put back a credential the pipeline took away.
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
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
                "{extra}",
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
            env: BTreeMap::new(),
        }
    }

    /// The two other workers `pma` ships a template for, both installed
    /// separately. Each reaches OpenAI, OpenRouter or an OpenAI-compatible
    /// endpoint through its own provider configuration: `{model}` is passed
    /// verbatim as `provider/model`, and the key comes from the environment.
    ///
    /// Both are seeded with `text-tail`, which takes the verdict from the exit
    /// status and leaves the cost unknown rather than guessing at a field. A
    /// `json:` parser reads a cost once its output shape is known, and that is
    /// a `pma agent set` away rather than a code change.
    pub fn opencode() -> Worker {
        Worker {
            name: "opencode".into(),
            command: "opencode".into(),
            args: [
                "run", "--dir", "{dir}", "--format", "json", "--auto", "--model", "{model}",
                "{extra}", "{prompt}",
            ]
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
            // `--auto` approves every permission that is not explicitly
            // denied, so there is no per-command allowlist to emit.
            allow: None,
            parse: Parser::TextTail,
            reports_cost: false,
            enforces_budget: false,
            sandbox: false,
            resumes: true,
            env: BTreeMap::new(),
        }
    }

    /// The same `claude`, in a disposable container. `sanduk` holds the API
    /// key on the host and bind-mounts the worktree, so the container has the
    /// files and never the credential.
    ///
    /// Four flags are not optional and are the reason this is a template
    /// rather than a line in the README. `--work-at-host-path` mounts the
    /// worktree at its own path, so a path in a diff or a stack trace resolves
    /// for whoever reads it here. `--stream-json` passes the agent's own
    /// output through, which is what `claude-json` reads a cost from -- without
    /// it `sanduk` reformats the result into prose and the cost is lost.
    /// `--no-report-instruction` keeps `REPORT.md` out of the worktree, where
    /// it would land in the diff and in the scope check. `--timeout` bounds
    /// the container from inside, so `sanduk` tears it down rather than
    /// leaving it for its own sweep.
    ///
    /// `key-safe` over `sealed`: sealed blocks the fetch that `cargo`, `go`
    /// and `pip` do mid-build, and a run that cannot fetch fails for a reason
    /// that has nothing to do with the task. `pma agent set sanduk args` is
    /// where a portfolio whose images carry their toolchains tightens it.
    ///
    /// No allowlist. The box is the bound, and a container that denies egress
    /// does not also need a `Bash()` rule: shipping both means believing in
    /// both.
    pub fn sanduk() -> Worker {
        Worker {
            name: "sanduk".into(),
            command: "sanduk".into(),
            args: [
                "run",
                "{prompt}",
                "-w",
                "{dir}",
                "--work-at-host-path",
                "--agent",
                "claude",
                // Named beside the agent: `sanduk`'s default provider is
                // openai, which `claude` cannot speak, so leaving it out
                // fails every dispatch before the container starts.
                "--provider",
                "anthropic",
                "--mode",
                "key-safe",
                "--model",
                "{model}",
                "--timeout",
                "{timeout}",
                "--stream-json",
                "--no-report-instruction",
                "{extra}",
            ]
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
            allow: None,
            parse: Parser::ClaudeJson,
            reports_cost: true,
            // `--budget` reaches OpenRouter alone, so nothing here bounds an
            // Anthropic run's spend. The timeout is the bound.
            enforces_budget: false,
            sandbox: true,
            resumes: false,
            env: BTreeMap::new(),
        }
    }

    pub fn omp() -> Worker {
        Worker {
            name: "omp".into(),
            command: "omp".into(),
            args: [
                "-p", "--mode", "json", "--cwd", "{dir}", "--model", "{model}", "{extra}",
                "{prompt}",
            ]
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
            allow: None,
            parse: Parser::TextTail,
            reports_cost: false,
            enforces_budget: false,
            sandbox: false,
            resumes: true,
            env: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::Duration;

    /// A worker's own bound, as dispatch passes it.
    const MINUTE: Duration = Duration::from_secs(60);

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn placeholders_are_filled_from_the_run() {
        let w = Worker::claude();
        let cmd = w.build("do it", Path::new("/w/a"), Some("haiku"), 0.5, &[], MINUTE);
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
        let cmd = Worker::claude().build("do it", Path::new("/w/a"), None, 0.5, &[], MINUTE);
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
            args_of(&w.build("do it", Path::new("/w/a"), None, 1.0, &[], MINUTE)),
            ["exec", "-C", "/w/a", "do it"]
        );
    }

    #[test]
    fn a_worker_without_an_allowlist_gets_no_rules() {
        let w = Worker {
            allow: None,
            ..Worker::claude()
        };
        let mut cmd = w.build("do it", Path::new("/w/a"), None, 1.0, &[], MINUTE);
        let before = args_of(&cmd).len();
        w.allow_verify(&mut cmd, Some("make test"));
        assert_eq!(args_of(&cmd).len(), before);
    }

    #[test]
    fn the_allowlist_names_one_rule_per_subcommand() {
        let w = Worker::claude();
        let mut cmd = w.build("do it", Path::new("/w/a"), None, 1.0, &[], MINUTE);
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
            assert_eq!(Parser::parse(name), Some(parser.clone()));
            assert_eq!(parser.name(), name);
        }
        assert_eq!(Parser::parse("codex-json"), None);
        for spec in ["json:result:usage.cost", "json:a.b.0:c:failed"] {
            assert_eq!(Parser::parse(spec).map(|p| p.name()).as_deref(), Some(spec));
        }
        // A summary path is the one thing a field parser cannot do without.
        assert_eq!(Parser::parse("json:"), None);
    }

    /// A worker whose output shape nothing else reads is a record: the paths
    /// say where the summary, the cost and the failure flag are.
    #[test]
    fn a_field_parser_reads_a_worker_it_was_never_told_about() {
        let p = Parser::parse("json:info.text:usage.cost_usd:info.failed").unwrap();
        let output = "starting\n{\"info\":{\"text\":\"did it\",\"failed\":false},\"usage\":{\"cost_usd\":0.42}}\n";
        assert_eq!(
            p.report(output, true),
            Report {
                ok: true,
                summary: "did it".into(),
                cost_usd: Some(0.42)
            }
        );
        // The flag outranks a zero exit status, and a cost as text is read.
        let failed =
            "{\"info\":{\"text\":\"gave up\",\"failed\":true},\"usage\":{\"cost_usd\":\"0.1\"}}";
        let r = p.report(failed, true);
        assert!(!r.ok);
        assert_eq!((r.summary.as_str(), r.cost_usd), ("gave up", Some(0.1)));
        // A path that resolves to nothing leaves the cost unknown, never zero,
        // and the output's tail stands in for the summary.
        let r = p.report("{\"other\":1}", true);
        assert_eq!(r.cost_usd, None);
        assert_eq!(r.summary, "{\"other\":1}");
        // No JSON at all is a failure, whatever the status said.
        assert!(!p.report("command not found\n", true).ok);
    }

    /// The environment a record carries is applied after the pipeline has
    /// stripped what an agent may not have, so a worker cannot restore a
    /// credential by naming it.
    #[test]
    fn a_workers_environment_cannot_put_back_a_stripped_credential() {
        let dir = std::env::temp_dir().join(format!("pma-worker-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut w = Worker::claude();
        w.command = "sh".into();
        w.args = vec![
            "-c".into(),
            "echo \"${OPENAI_BASE_URL-unset} ${GH_TOKEN-unset}\"".into(),
        ];
        w.env = BTreeMap::from([
            ("OPENAI_BASE_URL".into(), "http://localhost:11434/v1".into()),
            ("GH_TOKEN".into(), "sneaky".into()),
        ]);
        let mut cmd = w.build("", &dir, None, 1.0, &[], MINUTE);
        crate::agent::restrict(&mut cmd, &dir).unwrap();
        let out = cmd.output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "http://localhost:11434/v1 unset"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A worker that bounds itself is told when to stop, and is told sooner
    /// than the pipeline's own deadline so it stops itself first.
    #[test]
    fn a_timeout_reaches_a_worker_that_takes_one() {
        let cmd = Worker::sanduk().build(
            "do it",
            Path::new("/w/a"),
            None,
            1.0,
            &[],
            Duration::from_secs(870),
        );
        let args = args_of(&cmd);
        assert_eq!(
            args[args.iter().position(|a| a == "--timeout").unwrap() + 1],
            "870s"
        );
    }

    /// The container run is the same task in a box: the worktree at its own
    /// path, the agent's own stream passed through so the cost survives, and
    /// no REPORT.md written into the tree the diff is taken from.
    #[test]
    fn the_container_template_mounts_the_worktree_where_the_host_has_it() {
        let w = Worker::sanduk();
        let cmd = w.build(
            "fix the test",
            Path::new("/w/a"),
            Some("opus"),
            1.0,
            &[],
            MINUTE,
        );
        let args = args_of(&cmd);

        assert_eq!(w.command, "sanduk");
        assert_eq!(
            args[args.iter().position(|a| a == "-w").unwrap() + 1],
            "/w/a"
        );
        for flag in [
            "--work-at-host-path",
            "--stream-json",
            "--no-report-instruction",
        ] {
            assert!(args.contains(&flag.to_string()), "{flag} is not passed");
        }
        assert_eq!(
            args[args.iter().position(|a| a == "--model").unwrap() + 1],
            "opus"
        );
        assert_eq!(args[1], "fix the test", "the task is sanduk's positional");
    }

    /// The box is the bound. A container that denies egress does not also need
    /// a `Bash()` rule, and shipping both would mean believing in both.
    #[test]
    fn the_container_template_carries_no_allowlist() {
        let w = Worker::sanduk();
        let mut cmd = w.build("do it", Path::new("/w/a"), None, 1.0, &[], MINUTE);
        let before = args_of(&cmd);
        w.allow_verify(&mut cmd, Some("cargo test"));
        assert_eq!(args_of(&cmd), before);
    }

    /// `sanduk` reformats a run's result into a prose line of its own, which
    /// carries no cost. `--stream-json` passes the agent's records through
    /// instead, and `claude-json` reads the last of them.
    #[test]
    fn the_container_template_still_reads_a_cost_from_the_agents_own_stream() {
        let stream = concat!(
            "sanduk: sanduk-ab12 -> /w/a\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"working"}]}}"#,
            "\n",
            r#"{"type":"result","subtype":"success","is_error":false,"result":"fixed","total_cost_usd":0.42}"#,
            "\n",
            "sanduk: 41.2s wall\n",
        );
        let report = Worker::sanduk().parse.report(stream, true);
        assert!(report.ok);
        assert_eq!(report.cost_usd, Some(0.42));
        assert_eq!(report.summary, "fixed");
    }

    /// The templates are records too: nothing in the pipeline knows their
    /// names, and the model string is passed through for their own provider
    /// configuration to resolve.
    #[test]
    fn the_shipped_templates_pass_a_provider_qualified_model_through() {
        for (w, expect) in [
            (Worker::opencode(), "openai/gpt-5.2"),
            (Worker::omp(), "openrouter/anthropic/claude-sonnet-4.5"),
        ] {
            let cmd = w.build("do it", Path::new("/w/a"), Some(expect), 1.0, &[], MINUTE);
            let args = args_of(&cmd);
            assert!(args.contains(&expect.to_string()), "{args:?}");
            assert!(args.contains(&"/w/a".to_string()), "{args:?}");
            assert!(args.contains(&"do it".to_string()), "{args:?}");
            // Cost is unknown rather than zero until a `json:` path is set.
            assert!(!w.reports_cost);
            assert_eq!(w.parse, Parser::TextTail);
        }
    }
}
