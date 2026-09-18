//! Deterministic complexity, 1 to 5, from features a scan and a dispatch
//! already know. No model is called.
//!
//! The value drives routing in phase 4, so both the features and the rule
//! that reads them are versioned and snapshotted on the run. Replaying a
//! recorded decision means re-running the rule named there, not the current
//! one.

use crate::class::Class;

/// Bumped whenever `estimate` changes what it returns for the same features.
/// A run records the version it was dispatched under.
pub const ESTIMATOR: &str = "v1";

/// What the rule reads. Every field is measured, not judged.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Features {
    /// The task text or its description names a path or a symbol, so the
    /// work has somewhere to start.
    pub names_a_place: bool,
    pub text_words: i64,
    pub description_lines: i64,
    /// Files tracked in the repository.
    pub repo_files: i64,
    /// How long `verify` took at the base, or `None` when there is none.
    pub verify_seconds: Option<i64>,
    /// Decided runs in this project, and how many of those were accepted.
    pub prior_decided: i64,
    pub prior_accepted: i64,
}

/// A repository large enough that a change is likely to reach further than
/// the task says. Measured in tracked files.
const LARGE_REPO: i64 = 500;

/// Below this, a task's text cannot carry an acceptance criterion.
const TERSE: i64 = 4;

impl Features {
    /// The stored shape, written out field by field rather than derived, so
    /// a replay years later reads the same names this file names.
    pub fn to_json(self) -> String {
        serde_json::json!({
            "names_a_place": self.names_a_place,
            "text_words": self.text_words,
            "description_lines": self.description_lines,
            "repo_files": self.repo_files,
            "verify_seconds": self.verify_seconds,
            "prior_decided": self.prior_decided,
            "prior_accepted": self.prior_accepted,
        })
        .to_string()
    }

    /// A missing or unreadable field reads as its default, so an older row
    /// replays rather than failing.
    pub fn from_json(text: &str) -> Option<Features> {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        let int = |k: &str| v[k].as_i64().unwrap_or(0);
        Some(Features {
            names_a_place: v["names_a_place"].as_bool().unwrap_or(false),
            text_words: int("text_words"),
            description_lines: int("description_lines"),
            repo_files: int("repo_files"),
            verify_seconds: v["verify_seconds"].as_i64(),
            prior_decided: int("prior_decided"),
            prior_accepted: int("prior_accepted"),
        })
    }

    /// Reads the text and description. The other fields come from the
    /// repository and the store, so the caller fills them in.
    pub fn of(text: &str, description: &str) -> Features {
        Features {
            names_a_place: names_a_place(text) || names_a_place(description),
            text_words: text.split_whitespace().count() as i64,
            description_lines: description.lines().filter(|l| !l.trim().is_empty()).count() as i64,
            ..Features::default()
        }
    }
}

/// A word that looks like a path or a symbol: `src/scan.rs`, `TODO.md`,
/// `glob_match()`, `Store::open`. Deliberately crude, and only ever one
/// point of five.
fn names_a_place(text: &str) -> bool {
    text.split_whitespace().any(|word| {
        let word = word.trim_matches(|c: char| !c.is_alphanumeric() && c != ')');
        word.ends_with("()")
            || word.contains("::")
            || (word.contains('/') && !word.contains("://"))
            || word.rsplit_once('.').is_some_and(|(stem, ext)| {
                !stem.is_empty() && ext.len() <= 4 && ext.chars().all(|c| c.is_ascii_alphabetic())
            })
    })
}

/// 1 is a mechanical change with a check that decides it; 5 is judgment work
/// no gate can accept. The rule is a sum of named adjustments so a recorded
/// estimate can be argued with.
pub fn estimate(class: Class, f: &Features) -> i64 {
    let mut score = match class {
        // A change bounded to manifests, accepted by the existing suite.
        Class::Mechanical => 2,
        Class::Privileged => 3,
        Class::Specified => 3,
        Class::Judgment | Class::Never => 5,
    };
    if class == Class::Judgment || class == Class::Never {
        return score;
    }
    if f.names_a_place {
        score -= 1;
    }
    if f.description_lines >= 3 {
        score -= 1;
    }
    if f.text_words < TERSE && f.description_lines == 0 {
        score += 1;
    }
    if f.repo_files > LARGE_REPO {
        score += 1;
    }
    if f.verify_seconds.is_none() {
        score += 1;
    }
    // Two decided runs is a thin basis, but it is the project's own record
    // rather than a guess about the task.
    if f.prior_decided >= 2 && f.prior_accepted * 2 < f.prior_decided {
        score += 1;
    }
    score.clamp(1, 5)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn specified() -> Features {
        Features {
            text_words: 8,
            repo_files: 50,
            verify_seconds: Some(10),
            ..Features::default()
        }
    }

    #[test]
    fn a_path_or_a_symbol_counts_as_a_starting_point() {
        for text in [
            "fix src/scan.rs",
            "glob_match() rejects **",
            "Store::open leaks",
            "update README.md",
        ] {
            assert!(names_a_place(text), "{text}");
        }
        for text in [
            "make the parser faster",
            "see https://example.com/x",
            "decide on the api",
        ] {
            assert!(!names_a_place(text), "{text}");
        }
    }

    #[test]
    fn a_bounded_class_with_a_check_is_simple() {
        assert_eq!(estimate(Class::Mechanical, &specified()), 2);
        let located = Features {
            names_a_place: true,
            ..specified()
        };
        assert_eq!(estimate(Class::Mechanical, &located), 1);
        assert_eq!(estimate(Class::Specified, &located), 2);
    }

    #[test]
    fn a_one_liner_in_a_large_repository_without_a_check_is_hard() {
        let f = Features {
            text_words: 2,
            repo_files: 900,
            verify_seconds: None,
            ..Features::default()
        };
        assert_eq!(estimate(Class::Specified, &f), 5);
    }

    /// Detail is the one input the user controls, and the largest gap in the
    /// portfolio: 2 of 160 items carry a description.
    #[test]
    fn a_description_lowers_the_estimate() {
        let mut f = specified();
        assert_eq!(estimate(Class::Specified, &f), 3);
        f.description_lines = 3;
        assert_eq!(estimate(Class::Specified, &f), 2);
    }

    /// The project's own record, not a guess about the task.
    #[test]
    fn a_project_that_rejects_this_work_raises_it() {
        let mut f = specified();
        f.prior_decided = 4;
        f.prior_accepted = 1;
        assert_eq!(estimate(Class::Specified, &f), 4);
        f.prior_accepted = 2;
        assert_eq!(estimate(Class::Specified, &f), 3, "half is not worse");
        f.prior_decided = 1;
        f.prior_accepted = 0;
        assert_eq!(estimate(Class::Specified, &f), 3, "one run decides nothing");
    }

    #[test]
    fn judgment_work_is_five_whatever_else_is_true() {
        let f = Features {
            names_a_place: true,
            description_lines: 10,
            ..specified()
        };
        assert_eq!(estimate(Class::Judgment, &f), 5);
        assert_eq!(estimate(Class::Never, &f), 5);
    }

    #[test]
    fn features_round_trip_through_their_stored_shape() {
        let f = Features {
            names_a_place: true,
            text_words: 9,
            description_lines: 2,
            repo_files: 120,
            verify_seconds: Some(31),
            prior_decided: 5,
            prior_accepted: 3,
        };
        assert_eq!(Features::from_json(&f.to_json()), Some(f));
        // A row written before a field existed replays as its default.
        assert_eq!(
            Features::from_json(r#"{"text_words":4}"#),
            Some(Features {
                text_words: 4,
                ..Features::default()
            })
        );
        assert_eq!(Features::from_json("not json"), None);
    }

    #[test]
    fn features_read_the_text_and_the_description() {
        let f = Features::of("fix the parser", "it drops src/a.rs\n\nand b\n");
        assert_eq!(
            (f.names_a_place, f.text_words, f.description_lines),
            (true, 3, 2)
        );
    }
}
