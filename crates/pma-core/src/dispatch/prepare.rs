use super::*;

/// How a run refused before it started says so, in `Run::error`. A run that
/// never reached the agent is not a run that failed: the caller leaves its
/// work where it was rather than recording a result for it.
pub const NOT_STARTED: &str = "not started: batch budget";

/// What `prepare` made of a pick.
#[derive(Debug)]
pub enum Prepared {
    /// With a warning for each bundle item left out.
    Queued(Box<Run>, Vec<String>),
    /// The task is not dispatchable as the last scan described it. Other
    /// tasks of the project may still be.
    Refused(String),
}

/// Checks `pick` against the remote default branch, then creates its worktree
/// and branch and records a queued run. An error concerns the whole project,
/// such as a failed fetch.
pub fn prepare(
    store: &Store,
    home: &Path,
    cfg: &Config,
    over: &Overrides,
    pick: &Pick,
) -> Result<Prepared> {
    // A bundle loses the items origin no longer holds open.
    let mut pick = pick.clone();
    let mut dropped = Vec::new();
    let project = pick.project.clone();
    let repo = &pick.repo.clone();
    // Before the fetch, which reads `url.*` and `core.sshCommand`.
    clone_unchanged(store, repo)?;
    let in_clone = pick.workspace == Workspace::Clone;
    if in_clone {
        crate::publish::settle_committed(store)?;
    }
    if in_clone && let Some(held) = clone_busy(&store.runs()?, repo) {
        let state = store.run(held)?.state;
        return Ok(Prepared::Refused(format!(
            "{project}: its clone holds run #{held} ({}); {}, or dispatch with --worktree",
            state.name(),
            clears_clone(state, held)
        )));
    }
    // A clone run's `base` is `HEAD`, which publishing requires unmoved; its
    // `start` is the working tree, which may differ.
    let start = in_clone.then(|| working_tree_commit(repo)).transpose()?;
    let (default_branch, base) = if in_clone {
        (
            git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?,
            git(repo, &["rev-parse", "HEAD"])?,
        )
    } else if pick.local {
        (
            git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?,
            working_tree_commit(repo)?,
        )
    } else {
        git(repo, &["fetch", "--quiet", "origin"])?;
        let default_branch = scan::default_branch(repo).ok_or(scan::DEFAULT_BRANCH_UNKNOWN)?;
        let base = git(
            repo,
            &[
                "rev-parse",
                &format!("refs/remotes/origin/{default_branch}"),
            ],
        )?;
        (default_branch, base)
    };
    let refuse = |why: String| Ok(Prepared::Refused(format!("{project}: {why}")));

    let key = pick.key.clone();
    let (details, tags) = match key.as_str() {
        "ci" => {
            let Some(scan::Ci::Failing(workflows)) = store.project(&pick.project)?.map(|p| p.ci)
            else {
                return refuse("CI was not failing at the last scan".into());
            };
            // CI failed on the default branch, whatever a local clone has
            // checked out.
            let branch = match pick.local || in_clone {
                true => scan::default_branch(repo).ok_or(scan::DEFAULT_BRANCH_UNKNOWN)?,
                false => default_branch.clone(),
            };
            match failed_ci_logs(repo, &branch, &workflows)? {
                Ok(logs) => (logs, Vec::new()),
                Err(why) => return refuse(why),
            }
        }
        "deps" => {
            let detail = store
                .project(&pick.project)?
                .map(|p| p.deps_detail)
                .unwrap_or_default();
            let mut prompt = format!(
                "Outdated dependencies at the last `pma scan --deps`:\n\n```\n{detail}\n```\n\n\
                 Update them within the project's version constraints first. Change a \
                 constraint only where the tests still pass, and list any dependency you \
                 left behind, with the reason.\n"
            );
            if let Some(log) = failed_refresh(store, home, &pick.project)? {
                prompt.push_str(&format!(
                    "\n`pma deps refresh` took every update the constraints allow, and the \
                     project's check then failed. Find the update that broke it. Fix the code, \
                     or hold that package back with a constraint and say why. The check's \
                     output:\n\n```\n{log}\n```\n"
                ));
            }
            (prompt, Vec::new())
        }
        BUNDLE => {
            let from = start.as_deref().unwrap_or(&base);
            let file = git(repo, &["show", &format!("{from}:TODO.md")]).ok();
            let mut kept = Vec::new();
            for item in &pick.items {
                let why = match on_origin(file.as_deref(), &item.key, &item.text) {
                    OnOrigin::Open(description, tags) => {
                        if Class::of(&item.key, &tags).dispatchable() {
                            kept.push((item.clone(), description));
                            continue;
                        }
                        "is tagged #manual and is not dispatched".to_string()
                    }
                    OnOrigin::Done => format!("is already done on origin/{default_branch}"),
                    OnOrigin::Absent => format!(
                        "is not in TODO.md on origin/{default_branch}; commit and push it first"
                    ),
                };
                dropped.push(format!("{project}: `{}` {why}; left out", item.text));
            }
            if kept.is_empty() {
                return refuse(format!(
                    "no item of the bundle can be dispatched: {}",
                    dropped.join("; ")
                ));
            }
            if let [(item, description)] = &kept[..] {
                pick.key = item.key.clone();
                pick.text = item.text.clone();
                pick.gh = item.gh;
                pick.items.clear();
                (description.clone(), Vec::new())
            } else {
                let mut details = String::new();
                for (n, (item, description)) in kept.iter().enumerate() {
                    details.push_str(&format!("{}. {}\n", n + 1, item.text));
                    for line in description.lines() {
                        details.push_str(&format!("   {line}\n"));
                    }
                    details.push('\n');
                }
                pick.items = kept.into_iter().map(|(item, _)| item).collect();
                pick.text = bundle_text(&pick.items);
                (details, Vec::new())
            }
        }
        key if without_item(key) && !is_signal(key) => {
            (pick.details.clone().unwrap_or_default(), Vec::new())
        }
        _ => {
            // The tree the agent starts from: the working tree for a clone run.
            let from = start.as_deref().unwrap_or(&base);
            let file = git(repo, &["show", &format!("{from}:TODO.md")]).ok();
            match on_origin(file.as_deref(), &pick.key, &pick.text) {
                OnOrigin::Open(description, tags) => (description, tags),
                OnOrigin::Done => {
                    return refuse(format!(
                        "`{}` is already done on origin/{default_branch}; pull the clone, then `pma scan`",
                        pick.text
                    ));
                }
                OnOrigin::Absent => {
                    return refuse(format!(
                        "`{}` is not in TODO.md on origin/{default_branch}; commit and push it first",
                        pick.text
                    ));
                }
            }
        }
    };

    // Before the worktree, so a task the user marked leaves nothing behind.
    // A campaign states its class: it has no item to read tags from.
    let class = pick.class.unwrap_or_else(|| Class::of(&pick.key, &tags));
    if !class.dispatchable() {
        return refuse(format!(
            "`{}` is class {} and is not dispatched; remove `#manual` to change that",
            pick.text,
            class.name()
        ));
    }

    if in_clone {
        // The run's `worktree` is the clone; `branch` is what it has checked
        // out, which is where a push goes.
        let prepared = queue(
            store,
            home,
            cfg,
            over,
            &pick,
            repo,
            &default_branch.clone(),
            class,
            default_branch,
            base,
            start,
            details,
        );
        return noting(prepared, dropped);
    }

    let parent = home.join("worktrees").join(&pick.project);
    let stem = slug(&pick.text);
    let (slug, worktree) = (1..)
        .map(|n| match n {
            1 => stem.clone(),
            n => format!("{stem}-{n}"),
        })
        .map(|s| (s.clone(), parent.join(&s)))
        .find(|(s, path)| {
            // A remote branch is left by an earlier pull request; pushing
            // over it would be rejected.
            !path.exists()
                && [
                    format!("refs/heads/pma/{s}"),
                    format!("refs/remotes/origin/pma/{s}"),
                ]
                .iter()
                .all(|r| git(repo, &["rev-parse", "--verify", "--quiet", r]).is_err())
        })
        .expect("an unused slug exists");
    let branch = format!("pma/{slug}");
    std::fs::create_dir_all(&parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    git(
        repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            &branch,
            path_arg(&worktree)?,
            &base,
        ],
    )?;
    // Anything short of a queued run releases the worktree and its branch
    // here: no run row exists to release them later.
    let prepared = queue(
        store,
        home,
        cfg,
        over,
        &pick,
        &worktree,
        &branch,
        class,
        default_branch,
        base,
        None,
        details,
    );
    if !matches!(prepared, Ok(Prepared::Queued(..))) {
        let _ = remove_worktree(repo, &worktree, &branch);
    }
    noting(prepared, dropped)
}

/// A queued run with the bundle items `prepare` left out.
fn noting(prepared: Result<Prepared>, dropped: Vec<String>) -> Result<Prepared> {
    prepared.map(|p| match p {
        Prepared::Queued(run, _) => Prepared::Queued(run, dropped),
        p => p,
    })
}

/// What ends the hold of a run in `state` on its clone.
fn clears_clone(state: RunState, id: i64) -> String {
    match state {
        RunState::Queued | RunState::Running => "wait for it to finish".into(),
        RunState::Approved => format!("push it with `pma push {id}`, or reject it"),
        RunState::Staged => format!("commit it with `pma commit {id}`, or reject it"),
        RunState::Committed => format!("push it with `pma push {id}`"),
        RunState::PrOpen => format!("merge it with `pma merge {id}`"),
        _ => "approve or reject it first".into(),
    }
}

/// The clone run that holds `repo`, if one does: one that is queued, under
/// way, or waiting in review or in its clone's index. A failed one too, since
/// its agent may have left edits that only a reject reverts.
pub fn clone_busy(runs: &[Run], repo: &Path) -> Option<i64> {
    runs.iter()
        .filter(|r| r.repo == repo && r.workspace == Workspace::Clone && !r.state.is_final())
        .map(|r| r.id)
        .next()
}

/// The part of `prepare` that runs once the worktree exists.
#[allow(clippy::too_many_arguments)]
fn queue(
    store: &Store,
    home: &Path,
    cfg: &Config,
    over: &Overrides,
    pick: &Pick,
    worktree: &Path,
    branch: &str,
    class: Class,
    default_branch: String,
    base: String,
    start: Option<String>,
    details: String,
) -> Result<Prepared> {
    let repo = &pick.repo;
    let refuse = |why: String| Ok(Prepared::Refused(format!("{}: {why}", pick.project)));
    if let Err(e) = exclude_pma(repo) {
        eprintln!("warning: {e}");
    }
    // Before base verify, which runs the project's code and could install a
    // hook of its own.
    let git_snapshot = git_snapshot(&Run {
        repo: repo.clone(),
        worktree: worktree.to_path_buf(),
        workspace: pick.workspace,
        ..Run::default()
    })?;
    let verify = verify_command(cfg, &pick.project, worktree);
    // Inserted before the base check, which can take the whole timeout, so the
    // run shows as queued meanwhile. Routing fills in the rest.
    let mut run = Run {
        id: 0,
        project: pick.project.clone(),
        task_key: pick.key.clone(),
        text: pick.text.clone(),
        gh: pick.gh,
        quadrant: pick.quadrant.clone(),
        // Placeholders until routing, below, which needs the base check.
        agent: cfg.agent.clone(),
        repo: repo.clone(),
        branch: branch.to_string(),
        worktree: worktree.to_path_buf(),
        default_branch,
        base,
        prompt: String::new(),
        state: RunState::Queued,
        feedback: None,
        seconds: None,
        cost_usd: None,
        summary: None,
        commits: None,
        diffstat: None,
        verify: verify.clone(),
        verify_ok: None,
        error: None,
        outcome: None,
        dispatched_at: None,
        ready_at: None,
        decided_at: None,
        published_at: None,
        review_seconds: None,
        class: Some(class),
        scope: class.scope(),
        tier: cfg.tier_or_default(pick.tier),
        description: (!details.trim().is_empty()).then(|| details.clone()),
        agent_budget: Some(cfg.agent_budget),
        timeout_minutes: Some(cfg.timeout),
        verify_base_ok: None,
        verify_base_seconds: None,
        changed_paths: None,
        scope_error: None,
        model: None,
        complexity: None,
        features: None,
        estimator: None,
        route_revision: None,
        route: None,
        approval: None,
        approved_tree: None,
        approved_head: None,
        approved_by: None,
        // Set by the workflow pass; a task dispatched on its own has none.
        workflow_instance: pick.workflow.as_ref().map(|u| u.instance),
        node: pick.workflow.as_ref().map(|u| u.node.clone()),
        unit: pick.workflow.as_ref().map(|u| u.unit.clone()),
        lap: pick.workflow.as_ref().map_or(0, |u| u.lap),
        preset: None,
        extra_args: Vec::new(),
        git_snapshot: Some(git_snapshot),
        reviewed_tree: None,
        infra: false,
        workspace: pick.workspace,
        start: start.clone(),
        denied: Vec::new(),
        review_measured: None,
        pane: None,
        room: None,
        interactive: pick.interactive,
        agent_state: None,
        items: pick.items.clone(),
    };
    store.insert_run(&mut run)?;
    let routed = (|| -> Result<Option<String>> {
        let (verify_base_ok, verify_base_seconds) = match &verify {
            Some(v) if !pick.interactive => {
                let at = run.start.as_deref().unwrap_or(&run.base).to_string();
                let at = at.as_str();
                let sandbox = cfg.project(&pick.project).sandbox;
                match (
                    cfg.verify_base_alongside,
                    store.verify_base(&pick.project, at, v, cfg.timeout, sandbox)?,
                ) {
                    // `attempt` measures it.
                    (true, None) => (None, None),
                    (false, None) => {
                        // The agent waits on this, and an uncached suite can take
                        // the whole timeout: say so, as nothing else shows it.
                        eprintln!(
                            "#{} {}: checking the base first: `{v}`, up to {} minutes; log {}. \
                             `pma config verify_base_alongside on` starts the agent beside it.",
                            run.id,
                            pick.project,
                            cfg.timeout,
                            base_log(home, &pick.project, at).display()
                        );
                        base_verify(store, home, cfg, &pick.project, at, v, worktree)?
                    }
                    _ => base_verify(store, home, cfg, &pick.project, at, v, worktree)?,
                }
            }
            _ => (None, None),
        };
        let mut features = Features::of(&pick.text, &details);
        features.repo_files = tracked_files(worktree);
        // Unmeasured yet when it runs beside the agent; the last time stands in.
        features.verify_seconds = match verify_base_seconds {
            Some(seconds) => Some(seconds),
            None => store.last_base_seconds(&pick.project)?,
        }
        .filter(|_| verify.is_some());
        (features.prior_decided, features.prior_accepted) = store.decided_runs(&pick.project)?;
        let complexity = complexity::estimate(class, &features);

        // A policy applies only once someone activates it; in shadow it is
        // computed and recorded but the settings still decide.
        // An untiered project is ranked at `default_tier`, so it is routed there.
        let tier = cfg.tier_or_default(pick.tier);
        let subject = Subject {
            class,
            complexity,
            tier,
            // A task dispatched on its own names no node, which is what a route
            // stating no node condition serves; a workflow unit names its node.
            node: pick.workflow.as_ref().map(|u| u.node.as_str()),
            lap: pick.workflow.as_ref().map_or(0, |u| u.lap),
        };
        let active = store.active_route()?;
        let mut routed = (None, None, None, None, class.scope());
        if let Some(rev) = &active
            && rev.shadow
        {
            // Recorded for `pma route replay`; it refuses, scopes and approves
            // nothing.
            let policy = rev.policy()?;
            routed.0 = Some(rev.revision);
            routed.1 = policy.route(&subject).map(|r| r.name.clone());
        } else if let Some(rev) = &active {
            let policy = rev.policy()?;
            let Some(route) = policy.route(&subject) else {
                return Ok(Some(format!(
                    "`{}` matches no route in policy revision {}; add one or widen the last",
                    pick.text, rev.revision
                )));
            };
            let scope = route.scope.clone().unwrap_or_else(|| class.scope());
            routed = (
                Some(rev.revision),
                Some(route.name.clone()),
                Some(route.approval),
                Some((route.agent.clone(), route.model.clone())),
                scope,
            );
        }
        let (route_revision, route_name, approval, applied, scope) = routed;
        let chosen = choose_worker(store, cfg, over, applied)?;
        run.agent = chosen.agent.clone();
        run.model = chosen.model.clone();
        run.preset = chosen.preset.clone();
        run.extra_args = chosen.args.clone();
        run.scope = scope;
        run.tier = tier;
        run.verify_base_ok = verify_base_ok;
        run.verify_base_seconds = verify_base_seconds;
        run.complexity = Some(complexity);
        run.features = Some(features);
        run.estimator = Some(complexity::ESTIMATOR.into());
        run.route_revision = route_revision;
        run.route = route_name;
        run.approval = approval;
        run.prompt = prompt(&run, &details, verify.as_deref());
        Ok(None)
    })();
    match routed {
        Ok(None) => {
            store.route_queued(&run)?;
            Ok(Prepared::Queued(Box::new(run), Vec::new()))
        }
        // Never started, so it leaves no record, as before it had a row.
        Ok(Some(why)) => {
            store.delete_queued(run.id)?;
            refuse(why)
        }
        Err(e) => {
            if let Err(d) = store.delete_queued(run.id) {
                eprintln!("warning: #{}: {d}", run.id);
            }
            Err(e)
        }
    }
}

/// A task's item in the remote `TODO.md`.
#[derive(Debug, PartialEq)]
enum OnOrigin {
    /// Open, with its description lines and its tags.
    Open(String, Vec<String>),
    Done,
    /// Not in the file, or no file.
    Absent,
}

fn on_origin(file: Option<&str>, key: &str, text: &str) -> OnOrigin {
    let Some(file) = file else {
        return OnOrigin::Absent;
    };
    let items = todo::parse(file).items;
    let mut matching = items.iter().filter(|i| i.is_task(key, text));
    match matching.clone().find(|i| !i.done) {
        Some(item) => OnOrigin::Open(
            item.description
                .iter()
                .map(|l| format!("{}\n", l.trim_start()))
                .collect(),
            item.tags.clone(),
        ),
        None if matching.next().is_some() => OnOrigin::Done,
        None => OnOrigin::Absent,
    }
}

/// The failed-job log of each workflow's latest decisive run, the run scan
/// judged failing. The inner error refuses the task: a workflow passes now.
fn failed_ci_logs(
    repo: &Path,
    branch: &str,
    workflows: &[String],
) -> Result<std::result::Result<String, String>> {
    use crate::sync::gh;
    let url = git(repo, &["remote", "get-url", "origin"])?;
    let slug = scan::github_slug(&url).ok_or("origin is not on GitHub")?;
    let listed = gh(&[
        "workflow",
        "list",
        "-R",
        &slug,
        "--limit",
        "200",
        "--json",
        "id,name,state",
    ])?;
    let active = scan::active_workflows(&listed)?;
    let mut logs = String::new();
    for workflow in workflows {
        // By id: two workflow files may share a name, which `gh` refuses as
        // ambiguous. The newest failure among them is the one that counts.
        let mut failed = None;
        for (id, _) in active.iter().filter(|(_, name)| name == workflow) {
            let runs = gh(&[
                "run",
                "list",
                "-R",
                &slug,
                "--branch",
                branch,
                "--workflow",
                &id.to_string(),
                "--limit",
                "20",
                "--json",
                "databaseId,status,conclusion",
            ])?;
            failed = failed.max(scan::latest_failed_run(&runs)?);
        }
        let Some(id) = failed else {
            return Ok(Err(format!(
                "`{workflow}` no longer fails on {branch}; run `pma scan`"
            )));
        };
        // A startup failure has no job log; the agent still gets the run id.
        let log = gh(&["run", "view", &id.to_string(), "-R", &slug, "--log-failed"])
            .map(|log| agent::tail(&log, CI_LOG_LINES))
            .unwrap_or_else(|e| format!("(no failed-job log: {e})"));
        logs.push_str(&format!(
            "Workflow `{workflow}`, run {id}, last {CI_LOG_LINES} lines of `gh run view --log-failed`:\n\n```\n{log}\n```\n\n"
        ));
    }
    Ok(Ok(logs))
}

/// Where an agent drafts changelog entries, relative to its worktree. pma
/// moves the file out before any diff, so it is never part of the change.
pub(super) const CHANGELOG_DRAFT: &str = ".pma/changelog.md";

/// Whether a run's agent is asked for changelog entries: its project keeps a
/// `CHANGELOG.md`.
pub fn wants_changelog(run: &Run) -> bool {
    run.repo.join("CHANGELOG.md").is_file()
}

/// Where a run's drafted changelog entries are kept, outside its tree.
pub fn changelog_draft(home: &Path, id: i64) -> PathBuf {
    home.join("runs").join(id.to_string()).join("changelog.md")
}

fn prompt(run: &Run, details: &str, verify: Option<&str>) -> String {
    let where_ = match run.workspace {
        Workspace::Clone => {
            "in the developer's own clone. Files you did not change are theirs; leave them as they are."
        }
        Workspace::Worktree => "in a git worktree of it.",
    };
    let head = format!(
        "You are working on the project `{}`, {where_}\n\n",
        run.project
    );
    // A prompt's title is its first line, which the details repeat.
    let mut p = if run.task_key == PROMPT {
        format!("{head}Task, from the developer:\n")
    } else if run.items.is_empty() {
        format!("{head}Task: {}\n", run.text)
    } else {
        format!("{head}Tasks, numbered:\n")
    };
    if !details.trim().is_empty() {
        p.push('\n');
        p.push_str(details);
    }
    p.push_str(
        "\nRules:\n\
         - Leave your changes uncommitted. Do not commit, push, or switch branches.\n\
         - Do not edit TODO.md. pma marks the task done when the change is published.\n",
    );
    if wants_changelog(run) {
        p.push_str(&format!(
            "- Do not edit CHANGELOG.md. Write one entry per separable change to \
             `{CHANGELOG_DRAFT}`: a bold one-line title, then what went wrong and what \
             changed. pma keeps it out of your change and shows it to the reviewer.\n"
        ));
    }
    if let Some(v) = verify {
        p.push_str(&format!(
            "- Run `{v}` to check your change. pma runs it again after you finish.\n"
        ));
    }
    p.push_str(match run.items.is_empty() {
        true => "\nEnd with a short summary of what you changed and what is left undone.\n",
        false => {
            "\nEnd with a short summary, by task number, of what you changed and what is \
             left undone.\n"
        }
    });
    p
}

/// What a dispatch settled on: a worker, a model, the arguments that configure
/// it, and the preset those came from.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Chosen {
    pub agent: String,
    pub model: Option<String>,
    pub args: Vec<String>,
    pub preset: Option<String>,
}

/// Which worker runs, at which model, with which configuration. Highest first:
/// the `-a` and `-m` flags, a preset named on the command line, an applied
/// route, then the default preset. A model named nowhere is a model this
/// dispatch does not state: the worker is run without one and uses its own,
/// which `runs.model` then records as null.
pub fn choose_worker(
    store: &Store,
    cfg: &Config,
    over: &Overrides,
    applied: Option<(Option<String>, Option<String>)>,
) -> Result<Chosen> {
    let (route_agent, route_model) = applied.unwrap_or((None, None));
    let default = match (&over.preset, &cfg.preset) {
        (Some(_), _) => None,
        (None, Some(name)) => Some(store.preset(name)?),
        (None, None) => None,
    };
    let agent = over
        .agent
        .clone()
        .or_else(|| over.preset.as_ref().map(|p| p.agent.clone()))
        .or(route_agent)
        .or_else(|| default.as_ref().map(|p| p.agent.clone()))
        .unwrap_or_else(|| cfg.agent.clone());
    // The default preset's model is for its own worker; a route that named
    // another worker does not inherit it.
    let model = over
        .model
        .clone()
        .or_else(|| over.preset.as_ref().and_then(|p| p.model.clone()))
        .or(route_model)
        .or_else(|| {
            default
                .as_ref()
                .filter(|p| p.agent == agent)
                .and_then(|p| p.model.clone())
        });
    let named = over.preset.clone().or(default);
    // Arguments come with the preset that named them or not at all: a route
    // names a worker and a model, never a flag.
    let (args, preset) = match &named {
        Some(p) if p.agent == agent => (p.args.clone(), Some(p.name.clone())),
        _ => (Vec::new(), None),
    };
    Ok(Chosen {
        agent,
        model,
        args,
        preset,
    })
}

/// The project's `verify` setting, else a command detected in the worktree.
/// `none` disables verification.
pub fn verify_command(cfg: &Config, project: &str, worktree: &Path) -> Option<String> {
    match cfg.project(project).verify {
        Some(v) if v == "none" => None,
        Some(v) => Some(v),
        None => agent::detect_verify(worktree),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::tests::bundle;

    #[test]
    fn prompts_carry_the_task_rules_and_verify() {
        let run = Run {
            id: 1,
            project: "cynn".into(),
            task_key: "k".into(),
            text: "Add evaluate()".into(),
            gh: None,
            quadrant: None,
            agent: "claude".into(),
            repo: PathBuf::new(),
            branch: String::new(),
            worktree: PathBuf::new(),
            default_branch: "main".into(),
            base: String::new(),
            prompt: String::new(),
            state: RunState::Queued,
            feedback: None,
            seconds: None,
            cost_usd: None,
            summary: None,
            commits: None,
            diffstat: None,
            verify: None,
            verify_ok: None,
            error: None,
            outcome: None,
            dispatched_at: None,
            ready_at: None,
            decided_at: None,
            published_at: None,
            review_seconds: None,
            class: Some(Class::Specified),
            scope: Vec::new(),
            tier: Some(1),
            description: None,
            agent_budget: None,
            timeout_minutes: None,
            verify_base_ok: None,
            verify_base_seconds: None,
            changed_paths: None,
            scope_error: None,
            model: None,
            complexity: None,
            features: None,
            estimator: None,
            route_revision: None,
            route: None,
            approval: None,
            approved_tree: None,
            approved_head: None,
            approved_by: None,
            workflow_instance: None,
            node: None,
            unit: None,
            lap: 0,
            preset: None,
            extra_args: Vec::new(),
            git_snapshot: None,
            reviewed_tree: None,
            infra: false,
            workspace: Workspace::Worktree,
            start: None,
            denied: Vec::new(),
            review_measured: None,
            pane: None,
            room: None,
            interactive: false,
            agent_state: None,
            items: Vec::new(),
        };
        let p = prompt(&run, "for all classes\n", Some("make test"));
        assert!(p.contains("`cynn`"));
        assert!(
            p.contains("Task: Add evaluate()\n\nfor all classes\n"),
            "{p}"
        );
        assert!(p.contains("Do not commit"));
        assert!(p.contains("Run `make test` to check your change."));
        assert!(!prompt(&run, "", None).contains("pma runs"));

        let repo = std::env::temp_dir().join(format!("pma-changelog-{}", std::process::id()));
        std::fs::create_dir_all(&repo).unwrap();
        let run = Run {
            repo: repo.clone(),
            ..run
        };
        assert!(
            !prompt(&run, "", None).contains("CHANGELOG.md"),
            "the project keeps none"
        );
        std::fs::write(repo.join("CHANGELOG.md"), "# Changelog\n").unwrap();
        let p = prompt(&run, "", None);
        assert!(p.contains("Do not edit CHANGELOG.md"), "{p}");
        assert!(p.contains("`.pma/changelog.md`"), "{p}");
        let _ = std::fs::remove_dir_all(repo);
    }

    /// A prompt's run is named by its first line, cut at 80 characters; the
    /// agent reads the whole prompt once.
    #[test]
    fn a_prompt_run_carries_the_whole_prompt() {
        assert_eq!(prompt_title("  fix it\nand more"), "fix it");
        let long = "word ".repeat(30);
        let title = prompt_title(&long);
        assert!(
            title.ends_with("...") && title.chars().count() <= 83,
            "{title}"
        );
        let run = Run {
            task_key: PROMPT.into(),
            text: "fix it".into(),
            ..Run::blank()
        };
        let p = prompt(&run, "fix it\nand more\n", None);
        assert!(
            p.contains("Task, from the developer:\n\nfix it\nand more\n"),
            "{p}"
        );
        assert_eq!(p.matches("fix it").count(), 1, "{p}");
    }

    /// A bundle's prompt numbers its tasks, and asks for a summary by number,
    /// which is how the approver says which are done.
    #[test]
    fn a_bundle_prompt_numbers_its_tasks() {
        let p = prompt(&bundle(&[]), "1. task a\n\n2. task b\n", None);
        assert!(
            p.contains("Tasks, numbered:\n\n1. task a\n\n2. task b\n"),
            "{p}"
        );
        assert!(!p.contains("Task: "), "{p}");
        assert!(p.contains("summary, by task number"), "{p}");
    }

    /// An item named like a signal, a campaign or a workflow unit is still an
    /// item: checked on origin before dispatch, and ticked when published.
    #[test]
    fn an_item_key_never_reads_as_another_kind_of_task() {
        let file = "# TODO\n\n## High\n\n- [ ] CI\n- [ ] deps\n- [ ] Workflow: migrate\n\
                    - [ ] campaign: x\n- [ ] item:ci\n- [ ] fix CI\n";
        let keys: Vec<String> = todo::parse(file).items.iter().map(|i| i.key()).collect();
        assert_eq!(
            keys,
            [
                "item:ci",
                "item:deps",
                "item:workflow: migrate",
                "item:campaign: x",
                "item:item:ci",
                "fix ci"
            ]
        );
        assert!(keys.iter().all(|k| !without_item(k)), "{keys:?}");
        assert_eq!(
            on_origin(Some(file), "item:ci", "CI"),
            OnOrigin::Open(String::new(), Vec::new())
        );
    }

    #[test]
    fn items_are_open_done_or_absent_on_origin() {
        let file = "# TODO\n\n## High\n\n- [ ] open one #manual gh:3\n  why\n- [x] finished\n";
        assert_eq!(
            on_origin(Some(file), "gh:3", "open one"),
            OnOrigin::Open("why\n".into(), vec!["manual".into()]),
            "the tags decide the class, so they travel with the item"
        );
        assert_eq!(
            on_origin(Some(file), "open one", "open one"),
            OnOrigin::Open("why\n".into(), vec!["manual".into()]),
            "a key that lost gh:N still matches by text"
        );
        assert_eq!(
            on_origin(Some(file), "finished", "finished"),
            OnOrigin::Done
        );
        assert_eq!(on_origin(Some(file), "other", "other"), OnOrigin::Absent);
        assert_eq!(on_origin(None, "open one", "open one"), OnOrigin::Absent);
    }
}
