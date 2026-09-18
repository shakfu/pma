//! What a ready run's recorded gates amount to. The reasons a run needs a
//! human are computed from the run, never stored, so changing a rule re-reads
//! the evidence rather than trusting a verdict recorded under rules nobody
//! can name any more.
//!
//! Nothing here approves or ships. `pma review` shows the reasons; the
//! approval modes of phase 4 are what consume an empty list.

use crate::class::Class;
use crate::store::{Run, RunState};

/// Files that implement a verify command, by the command that runs them. A
/// worker that edits its own check can turn any tree green, so a change to
/// one of these is read by a human whatever else passed. Test files are not
/// listed: adding a test is the point of a class B task.
fn drivers(command: &str) -> &'static [&'static str] {
    let word = command.split_whitespace().next().unwrap_or_default();
    let word = word.rsplit('/').next().unwrap_or(word);
    match word {
        "make" | "gmake" => &["Makefile", "GNUmakefile", "*.mk"],
        "just" => &["justfile", "Justfile", ".justfile"],
        "npm" | "yarn" | "pnpm" => &["package.json"],
        "nox" => &["noxfile.py"],
        "tox" => &["tox.ini"],
        // `uv run pytest`, `python -m pytest`, `pytest -q`.
        _ if command.contains("pytest") => &["conftest.py", "pytest.ini", "tox.ini", "setup.cfg"],
        _ => &[],
    }
}

/// Why this run needs a human. Empty means every recorded gate is clean; it
/// does not mean the change is correct, only that nothing the tool checks
/// objects to it.
pub fn review_reasons(run: &Run) -> Vec<String> {
    if run.state != RunState::Ready {
        return vec![format!("run is {}", run.state.name())];
    }
    let mut reasons = Vec::new();
    let Some(class) = run.class else {
        return vec!["dispatched before classes existed".into()];
    };
    if !matches!(class, Class::Mechanical | Class::Specified) {
        reasons.push(format!(
            "class {} is never accepted without review",
            class.name()
        ));
    }
    if run.commits.unwrap_or(0) > 0 {
        reasons.push("the agent committed; it was told not to".into());
    }

    match &run.changed_paths {
        None => reasons.push(format!(
            "changed paths unknown: {}",
            run.scope_error.as_deref().unwrap_or("not enumerated")
        )),
        Some(paths) if paths.is_empty() => reasons.push("the run changed nothing".into()),
        Some(paths) => {
            let outside = class.violations(&run.scope, paths);
            if !outside.is_empty() {
                reasons.push(format!(
                    "outside class {}: {}",
                    class.name(),
                    outside.join(" ")
                ));
            }
            if let Some(command) = &run.verify {
                let edited: Vec<&str> = paths
                    .iter()
                    .filter(|p| {
                        drivers(command)
                            .iter()
                            .any(|g| crate::scan::glob_match(g, p.as_str()))
                    })
                    .map(String::as_str)
                    .collect();
                if !edited.is_empty() {
                    reasons.push(format!(
                        "`{command}` is implemented by {}",
                        edited.join(" ")
                    ));
                }
            }
        }
    }
    reasons.extend(verify_reason(run, class));
    reasons
}

/// The base and head results read together. Base alone says whether the
/// repository was already broken; head alone says whether it is green now.
/// What each class needs of the pair differs.
fn verify_reason(run: &Run, class: Class) -> Option<String> {
    if run.verify.is_none() {
        return Some("no verify command; set projects.<name>.verify".into());
    }
    let (base, head) = (run.verify_base_ok, run.verify_ok);
    match (base, head) {
        // A base that could not be measured is unknown, not passing.
        (None, _) => Some("the base was not verified".into()),
        (_, None) => Some("verify did not run at the head".into()),
        (_, Some(false)) => Some("verify failed at the head".into()),
        // Mechanical work must leave a green tree green. A base that was
        // failing means the run changed more than its manifests were worth.
        (Some(false), Some(true)) if class == Class::Mechanical => {
            Some("the base was already failing, so a green head proves nothing here".into())
        }
        // Specified work is accepted by a check that fails at the base and
        // passes at the head. Green to green demonstrates nothing.
        (Some(true), Some(true)) if class == Class::Specified => {
            Some("the base already passed, so no check discriminates this change".into())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(class: Class, base: Option<bool>, head: Option<bool>) -> Run {
        Run {
            state: RunState::Ready,
            class: Some(class),
            scope: class.scope(),
            verify: Some("make test".into()),
            verify_base_ok: base,
            verify_ok: head,
            commits: Some(0),
            changed_paths: Some(vec!["Cargo.lock".into()]),
            ..Run::blank()
        }
    }

    #[test]
    fn mechanical_work_must_leave_a_green_tree_green() {
        assert!(review_reasons(&ready(Class::Mechanical, Some(true), Some(true))).is_empty());
        for (base, head, why) in [
            (Some(false), Some(true), "already failing"),
            (Some(true), Some(false), "failed at the head"),
            (None, Some(true), "not verified"),
            (Some(true), None, "did not run"),
        ] {
            let r = review_reasons(&ready(Class::Mechanical, base, head));
            assert!(
                r.iter().any(|s| s.contains(why)),
                "{base:?} {head:?}: {r:?}"
            );
        }
    }

    /// Passing the suite at both ends shows no regression. It does not show
    /// that a bug was fixed.
    #[test]
    fn specified_work_needs_a_check_that_discriminates() {
        let mut run = ready(Class::Specified, Some(false), Some(true));
        run.changed_paths = Some(vec!["src/parser.rs".into()]);
        assert!(review_reasons(&run).is_empty());

        run.verify_base_ok = Some(true);
        let r = review_reasons(&run);
        assert_eq!(r.len(), 1, "{r:?}");
        assert!(r[0].contains("no check discriminates"), "{r:?}");
    }

    #[test]
    fn a_run_that_edits_its_own_check_is_read_by_a_human() {
        let mut run = ready(Class::Specified, Some(false), Some(true));
        run.changed_paths = Some(vec!["src/parser.rs".into(), "Makefile".into()]);
        let r = review_reasons(&run);
        assert!(
            r.iter()
                .any(|s| s.contains("`make test` is implemented by Makefile")),
            "{r:?}"
        );

        // A test added under the suite is the point of the task, not a flag.
        run.changed_paths = Some(vec!["tests/parser.rs".into()]);
        assert!(review_reasons(&run).is_empty());
    }

    #[test]
    fn drivers_follow_the_command() {
        assert_eq!(drivers("make test"), ["Makefile", "GNUmakefile", "*.mk"]);
        assert_eq!(
            drivers("/usr/bin/make -C sub test"),
            ["Makefile", "GNUmakefile", "*.mk"]
        );
        assert_eq!(drivers("uv run pytest -q")[0], "conftest.py");
        assert!(drivers("cargo test").is_empty(), "no file implements it");
    }

    #[test]
    fn scope_commits_and_missing_evidence_each_block() {
        let mut run = ready(Class::Mechanical, Some(true), Some(true));
        run.changed_paths = Some(vec!["Cargo.lock".into(), ".github/workflows/ci.yml".into()]);
        assert!(review_reasons(&run)[0].contains("outside class A"));

        let mut run = ready(Class::Mechanical, Some(true), Some(true));
        run.commits = Some(1);
        assert!(review_reasons(&run)[0].contains("agent committed"));

        let mut run = ready(Class::Mechanical, Some(true), Some(true));
        run.changed_paths = None;
        run.scope_error = Some("git diff failed".into());
        assert!(review_reasons(&run)[0].contains("git diff failed"));

        let mut run = ready(Class::Mechanical, Some(true), Some(true));
        run.changed_paths = Some(Vec::new());
        assert!(review_reasons(&run)[0].contains("changed nothing"));
    }

    #[test]
    fn a_run_that_is_not_ready_says_so() {
        let mut run = ready(Class::Mechanical, Some(true), Some(true));
        run.state = RunState::Failed;
        assert_eq!(review_reasons(&run), ["run is failed"]);
    }

    #[test]
    fn judgment_and_privileged_classes_are_never_clean() {
        for class in [Class::Privileged, Class::Judgment, Class::Never] {
            let run = ready(class, Some(true), Some(true));
            let r = review_reasons(&run);
            assert!(
                r[0].contains("never accepted without review"),
                "{class:?}: {r:?}"
            );
        }
    }
}
