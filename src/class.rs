//! Maintenance class of a task: what an agent may take, and which paths a
//! correct change touches. The class is predicted at dispatch from the signal
//! type and the item's tags; the paths a run actually changed are checked
//! against `scope` afterwards, so a misprediction cannot widen the scope.

/// Classes from `docs/dev/design-review.md`, "Classes of maintenance work".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// A. Mechanical: accepted by the existing suite, confined to manifests.
    Mechanical,
    /// A-. Mechanical but privileged: a workflow runs with repository tokens,
    /// so this is never unattended whatever its complexity.
    Privileged,
    /// B. Specified: acceptance is a test, scope is what the task names.
    Specified,
    /// C. Judgment: no automatic acceptance. Reserved for the `propose`
    /// approval mode; nothing produces it yet.
    Judgment,
    /// D. Never dispatched: releases, credentials, licences, and work the
    /// user marked `#manual`.
    Never,
}

/// Paths no class but A- may touch, whatever class was predicted. A workflow
/// runs with repository tokens and CI validates the changed workflow rather
/// than checking it, so the green-CI gate is worth least exactly where the
/// change is most dangerous.
pub const PRIVILEGED: [&str; 4] = [".github/**", "LICENSE", "COPYING", "**/.netrc"];

/// Files `pma` writes itself. No class may change one: an agent is told not
/// to, and ship marks the item done after the rebase. Keeping the file out of
/// every scope is what lets that edit be admitted without granting an agent
/// access to it.
pub const OWNED: [&str; 1] = ["TODO.md"];

/// Manifests and lock files a dependency update may touch. One list covers
/// every ecosystem, so a project needs no per-class setting.
const MANIFESTS: [&str; 10] = [
    "Cargo.toml",
    "Cargo.lock",
    "go.mod",
    "go.sum",
    "pyproject.toml",
    "uv.lock",
    "requirements*.txt",
    "package.json",
    "package-lock.json",
    "*.lock",
];

impl Class {
    const ALL: [(&'static str, Class); 5] = [
        ("A", Class::Mechanical),
        ("A-", Class::Privileged),
        ("B", Class::Specified),
        ("C", Class::Judgment),
        ("D", Class::Never),
    ];

    pub fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(_, c)| *c == self)
            .map_or("", |(n, _)| n)
    }

    pub fn parse(s: &str) -> Option<Class> {
        Self::ALL.iter().find(|(n, _)| *n == s).map(|(_, c)| *c)
    }

    /// The class of a task, from its key and the tags it carries on origin.
    /// `#manual` wins over everything, including a signal, so marking a task
    /// always takes it out of dispatch. An unclassified item is B: the
    /// conservative choice, since B is never unattended without a test that
    /// discriminates the base from the candidate.
    pub fn of(key: &str, tags: &[String]) -> Class {
        if tags.iter().any(|t| t == "manual") {
            return Class::Never;
        }
        match key {
            "deps" => Class::Mechanical,
            _ => Class::Specified,
        }
    }

    /// Globs a correct change may touch, or an empty list when the class
    /// states no bound. B names files the task alone knows, so it has none;
    /// the privileged paths still apply.
    pub fn scope(self) -> Vec<String> {
        match self {
            Class::Mechanical => MANIFESTS.iter().map(|s| (*s).into()).collect(),
            Class::Privileged => vec![".github/**".into()],
            _ => Vec::new(),
        }
    }

    /// Paths the run changed that its class does not permit. Privileged paths
    /// are judged against the paths actually changed, not against the class
    /// predicted at dispatch, so a task misread as mechanical cannot edit a
    /// workflow.
    pub fn violations<'a>(self, scope: &[String], paths: &'a [String]) -> Vec<&'a str> {
        paths
            .iter()
            .filter(|p| {
                if OWNED.iter().any(|g| crate::scan::glob_match(g, p.as_str())) {
                    return true;
                }
                let privileged = PRIVILEGED
                    .iter()
                    .any(|g| crate::scan::glob_match(g, p.as_str()));
                if privileged {
                    return self != Class::Privileged;
                }
                !scope.is_empty() && !scope.iter().any(|g| crate::scan::glob_match(g, p.as_str()))
            })
            .map(String::as_str)
            .collect()
    }

    /// Whether an agent may be dispatched to this class at all.
    pub fn dispatchable(self) -> bool {
        self != Class::Never
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn signals_and_tags_decide_the_class() {
        assert_eq!(Class::of("deps", &[]), Class::Mechanical);
        assert_eq!(Class::of("ci", &[]), Class::Specified);
        assert_eq!(Class::of("fix the parser", &[]), Class::Specified);
        assert_eq!(
            Class::of("fix the parser", &tags(["bug"].as_ref())),
            Class::Specified
        );
        // `#agent` marks eligibility, not class.
        assert_eq!(
            Class::of("fix it", &tags(["agent"].as_ref())),
            Class::Specified
        );
    }

    #[test]
    fn manual_is_never_dispatched_even_as_a_signal() {
        assert_eq!(Class::of("deps", &tags(["manual"].as_ref())), Class::Never);
        assert_eq!(Class::of("ci", &tags(["manual"].as_ref())), Class::Never);
        assert!(!Class::Never.dispatchable());
        assert!(Class::Mechanical.dispatchable());
    }

    #[test]
    fn only_mechanical_classes_bound_their_paths() {
        assert!(
            Class::Mechanical
                .scope()
                .contains(&"Cargo.lock".to_string())
        );
        assert_eq!(Class::Privileged.scope(), [".github/**"]);
        assert!(
            Class::Specified.scope().is_empty(),
            "the task names its files"
        );
    }

    fn paths(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn a_bounded_class_refuses_paths_outside_its_globs() {
        let a = Class::Mechanical;
        let scope = a.scope();
        assert!(
            a.violations(&scope, &paths(&["Cargo.lock", "sub/Cargo.toml", "uv.lock"]))
                .is_empty()
        );
        assert_eq!(
            a.violations(&scope, &paths(&["Cargo.lock", "src/main.rs"])),
            ["src/main.rs"]
        );
    }

    #[test]
    fn an_unbounded_class_still_refuses_privileged_paths() {
        let b = Class::Specified;
        assert!(b.scope().is_empty());
        assert!(
            b.violations(&[], &paths(&["src/main.rs", "tests/cli.rs"]))
                .is_empty()
        );
        assert_eq!(
            b.violations(
                &[],
                &paths(&[".github/workflows/ci.yml", "LICENSE", "a/.netrc"])
            ),
            [".github/workflows/ci.yml", "LICENSE", "a/.netrc"]
        );
    }

    /// A deps task that edits a workflow is refused although `.github/**` is
    /// nowhere in its scope, and although its class says nothing about it.
    #[test]
    fn a_misclassified_workflow_edit_cannot_pass() {
        let a = Class::Mechanical;
        assert_eq!(
            a.violations(&a.scope(), &paths(&[".github/workflows/ci.yml"])),
            [".github/workflows/ci.yml"]
        );
        // Only A- may, and then nothing else.
        let p = Class::Privileged;
        assert!(
            p.violations(&p.scope(), &paths(&[".github/workflows/ci.yml"]))
                .is_empty()
        );
        assert_eq!(
            p.violations(&p.scope(), &paths(&["src/main.rs"])),
            ["src/main.rs"]
        );
    }

    /// `pma` marks the item done itself, after the rebase. An agent that
    /// edits the file is caught at review, whatever its class.
    #[test]
    fn no_class_may_change_a_file_pma_writes() {
        for class in [Class::Mechanical, Class::Privileged, Class::Specified] {
            assert_eq!(
                class.violations(&class.scope(), &paths(&["TODO.md"])),
                ["TODO.md"],
                "{class:?}"
            );
        }
    }

    #[test]
    fn names_round_trip() {
        for (name, class) in Class::ALL {
            assert_eq!(Class::parse(name), Some(class));
            assert_eq!(class.name(), name);
        }
        assert_eq!(Class::parse("A+"), None);
    }
}
