//! Parser and linter for the TODO.md format, v1.
//!
//! The format is specified in `docs/dev/design.md`. Parsing is line-based so
//! that later edits can rewrite single lines without re-rendering content the
//! parser does not own.

use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Priority {
    Critical,
    High,
    Medium,
    Low,
}

impl Priority {
    pub const ALL: [Priority; 4] = [
        Priority::Critical,
        Priority::High,
        Priority::Medium,
        Priority::Low,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Priority::Critical => "critical",
            Priority::High => "high",
            Priority::Medium => "medium",
            Priority::Low => "low",
        }
    }

    pub fn parse(s: &str) -> Option<Priority> {
        Self::ALL.into_iter().find(|p| p.name() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Section {
    Priority(Priority),
    Done,
}

impl Section {
    const ALL: [(&'static str, Section); 5] = [
        ("Critical", Section::Priority(Priority::Critical)),
        ("High", Section::Priority(Priority::High)),
        ("Medium", Section::Priority(Priority::Medium)),
        ("Low", Section::Priority(Priority::Low)),
        ("Done", Section::Done),
    ];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// 1-based line number of the item line.
    pub line: usize,
    /// The enclosing priority section; `None` for items under `## Done`.
    pub priority: Option<Priority>,
    pub done: bool,
    pub text: String,
    pub tags: Vec<String>,
    /// `YYYY-MM-DD`, already validated.
    pub due: Option<String>,
    pub gh: Option<u64>,
    /// Text of the nearest `###` heading above the item in its section.
    pub group: Option<String>,
    /// Indented lines under the item, verbatim.
    pub description: Vec<String>,
}

impl Item {
    /// Identity across scans and edits: `gh:N`, or the text in comparable form.
    pub fn key(&self) -> String {
        match self.gh {
            Some(n) => format!("gh:{n}"),
            None => normal_text(&self.text),
        }
    }

    /// Whether this is the task known by `key` or by `text`. Sync adds `gh:N`
    /// to an item, which changes its key but not its text.
    pub fn is_task(&self, key: &str, text: &str) -> bool {
        self.key() == key
            || (!text.trim().is_empty() && normal_text(&self.text) == normal_text(text))
    }
}

/// Item text in comparable form: lowercase, single-spaced.
pub fn normal_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Warning,
    Error,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Severity::Warning => "warning",
            Severity::Error => "error",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub line: usize,
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct Parsed {
    pub items: Vec<Item>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Parsed {
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    fn report(&mut self, line: usize, severity: Severity, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic {
            line,
            severity,
            message: message.into(),
        });
    }
}

/// Where the parser is: before any `##`, in a known section, or in another one.
#[derive(Clone, Copy)]
enum Place {
    Preamble,
    Known(Section),
    Other,
}

pub fn parse(text: &str) -> Parsed {
    let mut out = Parsed::default();
    let mut place = Place::Preamble;
    let mut seen_sections: HashMap<Section, usize> = HashMap::new();
    let mut title_line: Option<usize> = None;
    let mut first_content = true;
    let mut in_fence = false;
    // Blank lines seen since the open item's last line. Kept so a description
    // that spans a blank line is carried verbatim.
    let mut blanks = 0;
    let mut open: Option<Item> = None;
    // Plain bullets outside the known sections, reported when nothing else is
    // an item: that file's tasks are invisible to pma.
    let mut ignored_bullets = 0;
    let mut group: Option<String> = None;

    for (index, raw) in text.lines().enumerate() {
        let n = index + 1;
        let line = raw.trim_end();

        if line.is_empty() {
            blanks += 1;
            continue;
        }

        let indented = line.starts_with(' ') || line.starts_with('\t');
        if indented {
            match open.as_mut() {
                Some(item) => {
                    item.description
                        .extend(std::iter::repeat_n(String::new(), blanks));
                    item.description.push(raw.to_string());
                }
                None if matches!(place, Place::Known(_)) && !in_fence => {
                    out.report(n, Severity::Warning, "indented line is not under an item");
                }
                None => {}
            }
            blanks = 0;
            continue;
        }
        blanks = 0;
        finish(&mut open, &mut out);

        if first_content {
            first_content = false;
            if line != "# TODO" {
                out.report(n, Severity::Error, "the file must start with `# TODO`");
            }
        }

        if line.starts_with("```") || line.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }

        if line.starts_with("# ") {
            // A wrong first title is already reported above.
            match title_line {
                Some(first) => out.report(
                    n,
                    Severity::Error,
                    format!("a second `#` heading; the title is on line {first}"),
                ),
                None => title_line = Some(n),
            }
            continue;
        }

        if let Some(name) = line.strip_prefix("## ") {
            place = section_heading(name.trim(), n, &mut seen_sections, &mut out);
            group = None;
            continue;
        }
        // `###` alone clears the group; trimming removed any trailing space.
        if let Some(name) = line
            .strip_prefix("###")
            .filter(|rest| rest.is_empty() || rest.starts_with(' '))
        {
            group = Some(name.trim().to_string()).filter(|g| !g.is_empty());
            continue;
        }
        if line.starts_with('#') {
            // `####` and deeper change neither the section nor the group.
            continue;
        }

        match place {
            Place::Known(section) => {
                if let Some(mut item) = item_line(line, n, section, &mut out) {
                    item.group = group.clone();
                    open = Some(item);
                }
            }
            Place::Preamble | Place::Other => {
                if is_checkbox(line) {
                    let msg = match place {
                        Place::Preamble => "item outside a section is ignored",
                        _ => {
                            "item in this section is ignored; use Critical, High, Medium, Low or Done"
                        }
                    };
                    out.report(n, Severity::Warning, msg);
                } else if is_bullet(line) {
                    ignored_bullets += 1;
                }
            }
        }
    }
    finish(&mut open, &mut out);

    if first_content {
        out.report(1, Severity::Error, "the file must start with `# TODO`");
    }
    // A file with priority sections has adopted the format, so bullets outside
    // them are deliberate: notes, declined work, ideas.
    if out.items.is_empty() && ignored_bullets > 0 && seen_sections.is_empty() {
        out.report(
            1,
            Severity::Warning,
            format!(
                "no items; {ignored_bullets} list entries outside Critical, High, Medium, Low and Done are ignored"
            ),
        );
    }
    check_duplicates(&mut out);
    out
}

fn finish(open: &mut Option<Item>, out: &mut Parsed) {
    if let Some(item) = open.take() {
        out.items.push(item);
    }
}

fn section_heading(
    name: &str,
    n: usize,
    seen: &mut HashMap<Section, usize>,
    out: &mut Parsed,
) -> Place {
    let Some((canonical, section)) = Section::ALL
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(name))
    else {
        return Place::Other;
    };
    if name != *canonical {
        out.report(
            n,
            Severity::Error,
            format!("write the heading as `## {canonical}`"),
        );
    }
    if let Some(first) = seen.insert(*section, n) {
        out.report(
            n,
            Severity::Error,
            format!("`## {canonical}` appears again; the first is on line {first}"),
        );
    }
    Place::Known(*section)
}

fn is_bullet(line: &str) -> bool {
    line.starts_with("- ") || line.starts_with("* ") || line.starts_with("+ ")
}

/// Recognises any checkbox list entry, canonical or not.
fn is_checkbox(line: &str) -> bool {
    checkbox_mark(line).is_some_and(|mark| matches!(mark, ' ' | 'x' | 'X'))
}

fn checkbox_mark(line: &str) -> Option<char> {
    let rest = line
        .strip_prefix(['-', '*', '+'])
        .or_else(|| {
            let digits = line.trim_start_matches(|c: char| c.is_ascii_digit());
            (digits.len() < line.len())
                .then(|| digits.strip_prefix(['.', ')']))
                .flatten()
        })?
        .trim_start();
    let mark = rest.strip_prefix('[')?.chars().next()?;
    rest[1 + mark.len_utf8()..].starts_with(']').then_some(mark)
}

fn item_line(line: &str, n: usize, section: Section, out: &mut Parsed) -> Option<Item> {
    let canonical = match line.get(..5) {
        Some("- [ ]") => Some(false),
        Some("- [x]") => Some(true),
        _ => None,
    };
    let rest = &line[if canonical.is_some() { 5 } else { 0 }..];
    let Some(done) = canonical.filter(|_| rest.is_empty() || rest.starts_with(' ')) else {
        if is_checkbox(line) {
            out.report(
                n,
                Severity::Error,
                "write items as `- [ ] text` or `- [x] text`",
            );
        } else if is_bullet(line) {
            out.report(
                n,
                Severity::Error,
                "a list entry in this section must be a `- [ ]` item",
            );
        } else {
            out.report(n, Severity::Warning, "text in this section is not an item");
        }
        return None;
    };

    let words: Vec<&str> = rest.split_whitespace().collect();
    let mut split = words.len();
    while split > 0 && is_token(words[split - 1]) {
        split -= 1;
    }

    let mut item = Item {
        line: n,
        priority: match section {
            Section::Priority(p) => Some(p),
            Section::Done => None,
        },
        done,
        text: words[..split].join(" "),
        tags: Vec::new(),
        due: None,
        gh: None,
        group: None,
        description: Vec::new(),
    };

    for word in &words[split..] {
        if let Some(date) = word.strip_prefix("due:") {
            if crate::dates::parse(date).is_none() {
                out.report(
                    n,
                    Severity::Error,
                    format!("`{word}` is not a date; write `due:YYYY-MM-DD`"),
                );
            } else if item.due.replace(date.to_string()).is_some() {
                out.report(n, Severity::Error, "more than one `due:`");
            }
        } else if let Some(number) = word.strip_prefix("gh:") {
            match number.parse::<u64>() {
                Ok(v) if v > 0 && !number.starts_with('0') => {
                    if item.gh.replace(v).is_some() {
                        out.report(n, Severity::Error, "more than one `gh:`");
                    }
                }
                _ => out.report(
                    n,
                    Severity::Error,
                    format!("`{word}` is not an issue number"),
                ),
            }
        } else if !item.tags.iter().any(|t| t == &word[1..]) {
            item.tags.push(word[1..].to_string());
        }
    }

    if let Some(last) = words[..split].last()
        && last.len() > 1
        && last.starts_with('#')
        && last[1..].bytes().all(|b| b.is_ascii_digit())
    {
        out.report(
            n,
            Severity::Warning,
            format!(
                "`{last}` is text; write `gh:{}` to link the issue",
                &last[1..]
            ),
        );
    }

    if item.text.is_empty() {
        out.report(n, Severity::Error, "the item has no text");
    }
    match (section, done) {
        (Section::Done, false) => out.report(n, Severity::Error, "open item under `## Done`"),
        (Section::Priority(_), true) => {
            out.report(n, Severity::Warning, "finished item; move it to `## Done`")
        }
        _ => {}
    }
    Some(item)
}

/// A trailing token: `#tag`, `due:...` or `gh:...`. Malformed `due:` and `gh:`
/// values still count, so they are reported rather than read as text.
pub fn is_token(word: &str) -> bool {
    if word.starts_with("due:") || word.starts_with("gh:") {
        return true;
    }
    let Some(tag) = word.strip_prefix('#') else {
        return false;
    };
    tag.starts_with(|c: char| c.is_ascii_alphabetic())
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Marks the open item that `is_task(key, task_text)` finished and moves it,
/// with its description, to the top of `## Done`, which is appended when
/// missing. Other lines are left byte for byte. `None` when no open item
/// matches.
pub fn mark_done(text: &str, key: &str, task_text: &str) -> Option<String> {
    let parsed = parse(text);
    let item = parsed
        .items
        .iter()
        .find(|i| !i.done && i.priority.is_some() && i.is_task(key, task_text))?;
    let mut lines: Vec<&str> = text.split_inclusive('\n').collect();
    let start = item.line - 1;
    let moved: Vec<String> = lines
        .drain(start..start + 1 + item.description.len())
        .enumerate()
        .map(|(i, l)| {
            if i == 0 {
                format!("- [x]{}", &l[5..])
            } else {
                l.to_string()
            }
        })
        .collect();
    let ending = if text.contains("\r\n") { "\r\n" } else { "\n" };

    let mut in_fence = false;
    let done = lines.iter().position(|l| {
        if l.starts_with("```") || l.starts_with("~~~") {
            in_fence = !in_fence;
        }
        !in_fence && l.trim_end() == "## Done"
    });
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    if let Some(last) = out.last_mut()
        && !last.ends_with('\n')
    {
        last.push_str(ending);
    }
    let mut moved = moved;
    if let Some(last) = moved.last_mut()
        && !last.ends_with('\n')
    {
        last.push_str(ending);
    }
    match done {
        Some(h) => {
            let mut at = h + 1;
            while at < out.len() && out[at].trim().is_empty() {
                at += 1;
            }
            if at == h + 1 {
                out.insert(at, ending.to_string());
                at += 1;
            }
            out.splice(at..at, moved);
        }
        None => {
            if out.last().is_some_and(|l| !l.trim().is_empty()) {
                out.push(ending.to_string());
            }
            out.push(format!("## Done{ending}"));
            out.push(ending.to_string());
            out.extend(moved);
        }
    }
    Some(out.concat())
}

/// Appends ` token` to the item on 1-based `line`, keeping the line ending.
/// `None` when that line is not an item.
pub fn add_token(text: &str, line: usize, token: &str) -> Option<String> {
    if !parse(text).items.iter().any(|i| i.line == line) {
        return None;
    }
    let mut lines: Vec<String> = text.split_inclusive('\n').map(String::from).collect();
    let target = lines.get_mut(line - 1)?;
    let body = target.trim_end_matches(['\r', '\n']).trim_end().to_string();
    let ending = target[target.trim_end_matches(['\r', '\n']).len()..].to_string();
    *target = format!("{body} {token}{ending}");
    Some(lines.concat())
}

/// Unsynced items are identified by their normalised text, and synced ones by
/// `gh:N`, so either repeating breaks identity.
fn check_duplicates(out: &mut Parsed) {
    let mut texts: HashMap<String, usize> = HashMap::new();
    let mut issues: HashMap<u64, usize> = HashMap::new();
    let mut found = Vec::new();

    for item in &out.items {
        if !item.done
            && !item.text.is_empty()
            && let Some(first) = texts.insert(item.text.to_lowercase(), item.line)
        {
            found.push((
                item.line,
                format!("same text as the open item on line {first}"),
            ));
        }
        if let Some(gh) = item.gh
            && let Some(first) = issues.insert(gh, item.line)
        {
            found.push((item.line, format!("`gh:{gh}` is also on line {first}")));
        }
    }
    for (line, message) in found {
        out.report(line, Severity::Error, message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages(text: &str) -> Vec<(usize, Severity, String)> {
        parse(text)
            .diagnostics
            .into_iter()
            .map(|d| (d.line, d.severity, d.message))
            .collect()
    }

    fn assert_clean(text: &str) -> Parsed {
        let parsed = parse(text);
        assert!(
            parsed.diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            parsed.diagnostics
        );
        parsed
    }

    fn assert_reports(text: &str, line: usize, severity: Severity, fragment: &str) {
        let found = messages(text);
        assert!(
            found
                .iter()
                .any(|(l, s, m)| *l == line && *s == severity && m.contains(fragment)),
            "no {severity} containing {fragment:?} on line {line}; got {found:?}"
        );
    }

    const SPEC_EXAMPLE: &str = "# TODO

## Critical

- [ ] segfault on empty input #bug gh:42

## High

- [ ] support ggml 0.9 due:2026-10-01

## Medium

- [ ] flaky test on linux #urgent

## Low

## Done

- [x] drop python 3.9
";

    #[test]
    fn spec_example_parses_clean() {
        let parsed = assert_clean(SPEC_EXAMPLE);
        let summary: Vec<_> = parsed
            .items
            .iter()
            .map(|i| (i.line, i.priority, i.done, i.text.as_str()))
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    5,
                    Some(Priority::Critical),
                    false,
                    "segfault on empty input"
                ),
                (9, Some(Priority::High), false, "support ggml 0.9"),
                (13, Some(Priority::Medium), false, "flaky test on linux"),
                (19, None, true, "drop python 3.9"),
            ]
        );
        assert_eq!(parsed.items[0].tags, ["bug"]);
        assert_eq!(parsed.items[0].gh, Some(42));
        assert_eq!(parsed.items[1].due.as_deref(), Some("2026-10-01"));
        assert_eq!(parsed.items[2].tags, ["urgent"]);
    }

    #[test]
    fn only_trailing_words_are_tokens() {
        let parsed = assert_clean(
            "# TODO\n\n## High\n\n- [ ] port C# #bindings to due:parser module #api\n",
        );
        let item = &parsed.items[0];
        assert_eq!(item.text, "port C# #bindings to due:parser module");
        assert_eq!(item.tags, ["api"]);
        assert_eq!(item.due, None);
    }

    #[test]
    fn description_is_carried_verbatim_across_blank_lines() {
        let parsed =
            assert_clean("# TODO\n\n## Low\n\n- [ ] item\n  first\n\n    second\n\n- [ ] next\n");
        assert_eq!(parsed.items[0].description, ["  first", "", "    second"]);
        assert!(parsed.items[1].description.is_empty());
    }

    #[test]
    fn crlf_line_endings_are_accepted() {
        let parsed = assert_clean("# TODO\r\n\r\n## High\r\n\r\n- [ ] item #tag\r\n");
        assert_eq!(parsed.items[0].tags, ["tag"]);
    }

    #[test]
    fn other_sections_and_deeper_headings() {
        let text =
            "# TODO\n\n## Notes\n\nprose\n- [ ] ignored\n\n## High\n\n### parser\n\n- [ ] kept\n";
        let parsed = parse(text);
        assert_eq!(parsed.items.len(), 1);
        assert_eq!(parsed.items[0].priority, Some(Priority::High));
        assert_reports(text, 6, Severity::Warning, "ignored");
        assert_eq!(parsed.diagnostics.len(), 1);
    }

    #[test]
    fn items_take_the_nearest_level_three_heading_in_their_section() {
        let text = "# TODO\n\n## High\n\n- [ ] ungrouped\n\n### Security Hardening \n\n- [ ] a\n\n#### detail\n\n- [ ] b\n\n### \n\n- [ ] c\n\n## Low\n\n- [ ] d\n\n### Later\n\n- [ ] e\n";
        let groups: Vec<_> = parse(text)
            .items
            .into_iter()
            .map(|i| (i.text, i.group))
            .collect();
        let g = |s: &str| Some(s.to_string());
        assert_eq!(
            groups,
            [
                ("ungrouped".into(), None),
                ("a".into(), g("Security Hardening")),
                ("b".into(), g("Security Hardening")),
                ("c".into(), None),
                ("d".into(), None),
                ("e".into(), g("Later")),
            ]
        );
    }

    #[test]
    fn a_file_whose_tasks_are_all_plain_bullets_is_flagged() {
        let text = "# TODO\n\n## Ideas\n\n- one\n- two\n\n```\n- fenced\n```\n";
        assert_reports(text, 1, Severity::Warning, "no items; 2 list entries");
        assert_clean("# TODO\n\n## Ideas\n\n- one\n\n## Low\n\n- [ ] real\n");
        assert_clean("# TODO\n\n## High\n");
        assert_clean("# TODO\n\n## Low\n\n## Declined\n\n- not doing this\n");
    }

    #[test]
    fn fenced_code_is_not_structure() {
        let parsed = assert_clean(
            "# TODO\n\n## High\n\n```\n## Done\n- [ ] not an item\n```\n\n- [ ] real\n",
        );
        assert_eq!(parsed.items.len(), 1);
        assert_eq!(parsed.items[0].priority, Some(Priority::High));
    }

    #[test]
    fn title_is_required() {
        assert_reports("## High\n", 1, Severity::Error, "must start with `# TODO`");
        assert_reports(
            "\n\nintro\n# TODO\n",
            3,
            Severity::Error,
            "must start with `# TODO`",
        );
        assert_reports("", 1, Severity::Error, "must start with `# TODO`");
        assert_reports(
            "# TODO\n\n# Other\n",
            3,
            Severity::Error,
            "second `#` heading",
        );
        assert_reports(
            "```\n# TODO\n```\n",
            1,
            Severity::Error,
            "must start with `# TODO`",
        );
        assert_eq!(
            messages("# Tasks\n").len(),
            1,
            "a wrong title is reported once"
        );
    }

    #[test]
    fn section_names_are_exact() {
        assert_reports(
            "# TODO\n\n## critical\n",
            3,
            Severity::Error,
            "`## Critical`",
        );
        assert_reports(
            "# TODO\n\n## High\n\n## High\n",
            5,
            Severity::Error,
            "first is on line 3",
        );
    }

    #[test]
    fn noncanonical_items_are_errors() {
        for bad in [
            "* [ ] x", "- [X] x", "-[ ] x", "1. [ ] x", "+ [x] x", "- [ ]x",
        ] {
            let text = format!("# TODO\n\n## High\n\n{bad}\n");
            assert_reports(&text, 5, Severity::Error, "write items as");
            assert!(parse(&text).items.is_empty(), "{bad} was read as an item");
        }
        assert_reports(
            "# TODO\n\n## High\n\n- plain bullet\n",
            5,
            Severity::Error,
            "must be a `- [ ]` item",
        );
        assert_reports(
            "# TODO\n\n## High\n\nsome prose\n",
            5,
            Severity::Warning,
            "not an item",
        );
        assert_reports(
            "# TODO\n\n## High\n\n**bold** prose\n",
            5,
            Severity::Warning,
            "not an item",
        );
        assert_reports(
            "# TODO\n\n## High\n\n  stray\n",
            5,
            Severity::Warning,
            "not under an item",
        );
    }

    #[test]
    fn tokens_are_validated() {
        let item = |s: &str| format!("# TODO\n\n## High\n\n- [ ] thing {s}\n");
        assert_reports(&item("due:2026-02-30"), 5, Severity::Error, "not a date");
        assert_reports(&item("due:tomorrow"), 5, Severity::Error, "not a date");
        assert_reports(
            &item("due:2026-01-01 due:2026-01-02"),
            5,
            Severity::Error,
            "more than one `due:`",
        );
        assert_reports(&item("gh:0"), 5, Severity::Error, "not an issue number");
        assert_reports(&item("gh:x1"), 5, Severity::Error, "not an issue number");
        assert_reports(
            &item("gh:1 gh:2"),
            5,
            Severity::Error,
            "more than one `gh:`",
        );
        assert_reports(&item("#42"), 5, Severity::Warning, "write `gh:42`");
        assert_clean(&item("due:2028-02-29"));
    }

    #[test]
    fn item_state_must_match_section() {
        assert_reports(
            "# TODO\n\n## Done\n\n- [ ] open\n",
            5,
            Severity::Error,
            "open item under `## Done`",
        );
        assert_reports(
            "# TODO\n\n## Low\n\n- [x] closed\n",
            5,
            Severity::Warning,
            "move it to `## Done`",
        );
        assert_reports(
            "# TODO\n\n## Low\n\n- [ ] #only-tags\n",
            5,
            Severity::Error,
            "no text",
        );
        assert_reports("# TODO\n\n## Low\n\n- [ ]\n", 5, Severity::Error, "no text");
    }

    #[test]
    fn mark_done_moves_the_item_and_its_description() {
        let text = "# TODO\n\n## High\n\n- [ ] first #bug\n  why\n\n  more\n- [ ] Second  gh:4\n\n## Done\n\n- [x] old\n";
        let out = mark_done(text, "first", "").unwrap();
        assert_eq!(
            out,
            "# TODO\n\n## High\n\n- [ ] Second  gh:4\n\n## Done\n\n- [x] first #bug\n  why\n\n  more\n- [x] old\n"
        );
        assert_clean(&out);
        let out = mark_done(&out, "gh:4", "").unwrap();
        assert!(
            out.contains("## High\n\n\n## Done\n\n- [x] Second  gh:4\n- [x] first"),
            "{out}"
        );
        assert_eq!(
            mark_done(text, "old", "old"),
            None,
            "finished items are not open"
        );
        assert_eq!(mark_done(text, "missing", ""), None);
        assert!(
            mark_done(text, "second", "SECOND").is_some(),
            "a key that gained gh:N still matches by text"
        );
    }

    #[test]
    fn mark_done_adds_a_done_section_when_missing() {
        let out = mark_done("# TODO\n\n## Low\n\n- [ ] only", "only", "").unwrap();
        assert_eq!(out, "# TODO\n\n## Low\n\n## Done\n\n- [x] only\n");
        let out = mark_done("# TODO\r\n\r\n## Low\r\n- [ ] a\r\n## Done\r\n", "a", "").unwrap();
        assert_eq!(out, "# TODO\r\n\r\n## Low\r\n## Done\r\n\r\n- [x] a\r\n");
        let fenced = "# TODO\n\n## Low\n\n- [ ] a\n  ```\n## Notes\n\n```\n## Done\n```\n";
        assert!(
            mark_done(fenced, "a", "")
                .unwrap()
                .ends_with("```\n\n## Done\n\n- [x] a\n  ```\n")
        );
    }

    #[test]
    fn add_token_appends_to_item_lines_only() {
        let text = "# TODO\r\n\r\n## Critical\r\n\r\n- [ ] crash #bug  \r\n- [ ] last";
        let out = add_token(text, 5, "gh:12").unwrap();
        assert_eq!(
            out,
            "# TODO\r\n\r\n## Critical\r\n\r\n- [ ] crash #bug gh:12\r\n- [ ] last"
        );
        assert_eq!(parse(&out).items[0].gh, Some(12));
        assert!(
            add_token(text, 6, "gh:3")
                .unwrap()
                .ends_with("- [ ] last gh:3")
        );
        assert_eq!(add_token(text, 3, "gh:1"), None, "a heading is not an item");
        assert_eq!(add_token(text, 99, "gh:1"), None);
    }

    #[test]
    fn duplicates_break_identity() {
        let text = "# TODO\n\n## High\n\n- [ ] Fix  it gh:3\n\n## Low\n\n- [ ] fix it\n- [ ] other gh:3\n\n## Done\n\n- [x] fix it\n";
        assert_reports(text, 9, Severity::Error, "open item on line 5");
        assert_reports(text, 10, Severity::Error, "also on line 5");
        assert_eq!(
            parse(text).diagnostics.len(),
            2,
            "a finished repeat is not a duplicate"
        );
    }
}
