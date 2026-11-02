//! `pma tui`: every operating view and action, over SSH as well as at a
//! desk. One tab per view; keys come from `pma_core::keys`, rows, tones and
//! actions from `pma_core::screen`, so the web view shows and does the same.
//!
//! `App` is a state machine: a key returns a `Request` to load a view or take
//! an action, and the run loop sends it through `pma_core::api` and hands the
//! result back. Nothing here reads the store, so the tests need none.
//!
//! Colour carries meaning, from the terminal's own ANSI palette so it follows
//! the user's theme: green done, yellow waits on you, red failed or critical,
//! cyan under way, grey context. `NO_COLOR` turns it off; the selection is
//! still reverse video and a `>` marker, so nothing depends on colour alone.

use std::time::Duration;

use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell as TableCell, Clear, Padding, Paragraph, Row, Table,
    TableState, Wrap,
};

use pma_core::api::{self, Action, Outcome, Query, View};
use pma_core::keys::{self, Act, Ask, Command, Tab};
use pma_core::report_runs::By;
use pma_core::screen::{self, Body, Detail, Item, Kind, Pane, Pick, Screen, Section, Tone};
use pma_core::store::Store;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The scope chooser's first option: every project. No tag is spelled so.
const NO_SCOPE: &str = "*";

/// What the run loop does for a key.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Load(Query),
    Act(Action),
    /// A list to choose from: what `api::choices` gives for an act, the
    /// projects, to open one, or the tags, to scope the views to one.
    Choose {
        what: Choosing,
        pick: Pick,
        label: &'static str,
    },
    /// A prompt whose text starts from the store (`api::drafts`).
    Prompt {
        act: Act,
        pick: Pick,
        label: &'static str,
    },
    /// The rows `api::checklist` gives for an act, to check.
    Check {
        act: Act,
        pick: Pick,
        label: &'static str,
    },
    /// The run's diff in `$PAGER`, with the terminal handed over meanwhile.
    Pager {
        id: i64,
    },
    /// The project in `$VISUAL` or `$EDITOR`, with the terminal handed over.
    Editor {
        project: String,
    },
    Quit,
}

/// What a chooser's list is of.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Choosing {
    /// The options an act offers (`api::choices`).
    Act(Act),
    /// A project to open (`api::project_choices`).
    Project,
    /// A tag to scope every view to (`api::tag_choices`), or none.
    Scope,
}

#[derive(Debug, Clone, PartialEq)]
enum Mode {
    Normal,
    Help,
    /// A run's diff or a job's log, over the tab.
    Detail {
        screen: Screen,
        scroll: u16,
    },
    Confirm {
        action: Action,
        question: String,
    },
    Prompt {
        act: Act,
        label: &'static str,
        input: String,
        pick: Pick,
    },
    /// A list to choose from, with flags to toggle. `at` is a place among
    /// the options `filter` keeps; `/` starts the filter.
    Choose {
        what: Choosing,
        label: &'static str,
        pick: Pick,
        choices: api::Choices,
        at: usize,
        on: Vec<bool>,
        filter: Option<String>,
    },
    /// Typing the tab's filter; Enter keeps it, Escape clears it.
    Filter,
    /// Rows to check or uncheck for an act, such as a bundle's items before
    /// its approval.
    Check {
        act: Act,
        label: &'static str,
        pick: Pick,
        items: Vec<String>,
        on: Vec<bool>,
        at: usize,
    },
}

/// The colours each meaning takes. All plain under `NO_COLOR`.
#[derive(Debug, Clone, Copy)]
struct Palette {
    color: bool,
}

impl Palette {
    fn from_env() -> Palette {
        Palette {
            color: std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()),
        }
    }

    fn fg(self, c: Color) -> Style {
        match self.color {
            true => Style::new().fg(c),
            false => Style::new(),
        }
    }

    fn tone(self, t: Tone) -> Style {
        match t {
            Tone::Plain => Style::new(),
            Tone::Muted => self.fg(Color::DarkGray),
            Tone::Accent => self.fg(Color::Cyan),
            Tone::Good => self.fg(Color::Green),
            Tone::Warn => self.fg(Color::Yellow),
            Tone::Bad => self.fg(Color::Red).add_modifier(Modifier::BOLD),
            Tone::Strong => Style::new().add_modifier(Modifier::BOLD),
        }
    }

    fn accent(self) -> Style {
        self.fg(Color::Cyan)
    }

    fn muted(self) -> Style {
        self.fg(Color::DarkGray)
    }
}

pub struct App {
    tab: Tab,
    /// The project the project tab shows, and the tab it was opened from.
    project: String,
    previous: Tab,
    by: usize,
    /// The place in `api::FINISHED` the activity tab filters to.
    finished: usize,
    /// Per tab: what is shown, which is `full` with `filter` applied.
    screens: Vec<Option<std::result::Result<Screen, String>>>,
    full: Vec<Option<std::result::Result<Screen, String>>>,
    /// Per tab: the fuzzy filter its tables are narrowed by, if any.
    filter: Vec<Option<String>>,
    /// Per tab: the focused pane, and the selection in each pane.
    pane: Vec<usize>,
    selected: Vec<Vec<TableState>>,
    /// Per tab and pane: the rows the last draw had room for, so a page is
    /// what is seen.
    rows: Vec<Vec<u16>>,
    /// The open detail as last drawn: its lines once wrapped, and the rows
    /// it had, so a page and the end are what is seen.
    extent: (usize, u16),
    /// The tags every view is narrowed to, and every act over the portfolio
    /// acts within; empty is every project.
    scope: Vec<String>,
    mode: Mode,
    status: Option<(String, Tone)>,
    /// Runs and jobs the footer names, oldest first; Enter opens the first.
    notices: Vec<api::Transition>,
    /// Tasks marked on the current view, which `d` dispatches together.
    marks: Vec<Pick>,
    /// The run a decision was made on from its detail: the next to read
    /// follows it.
    advance: Option<i64>,
    busy: bool,
    palette: Palette,
}

impl Default for App {
    fn default() -> Self {
        App::new()
    }
}

fn index(tab: Tab) -> usize {
    Tab::ALL.iter().position(|t| *t == tab).expect("every tab")
}

impl App {
    pub fn new() -> App {
        let n = Tab::ALL.len();
        App {
            tab: Tab::Inbox,
            project: String::new(),
            previous: Tab::Inbox,
            by: 1,
            finished: 0,
            screens: vec![None; n],
            full: vec![None; n],
            filter: vec![None; n],
            pane: vec![0; n],
            selected: vec![Vec::new(); n],
            rows: vec![Vec::new(); n],
            extent: (0, 20),
            scope: Vec::new(),
            mode: Mode::Normal,
            status: None,
            notices: Vec::new(),
            marks: Vec::new(),
            advance: None,
            busy: false,
            palette: Palette::from_env(),
        }
    }

    /// The query that fills the current tab, within the scope.
    pub fn query(&self) -> Query {
        self.tab.query(
            self.selection(),
            By::ALL[self.by].1,
            &self.project,
            api::FINISHED[self.finished],
        )
    }

    /// The scope as a selection: the tags every view is narrowed to.
    fn selection(&self) -> api::Selection {
        api::Selection {
            projects: Vec::new(),
            tags: self.scope.clone(),
        }
    }

    /// Whether a refresh may replace what is shown: not while a prompt, a
    /// confirmation or a detail is open over it.
    pub fn idle(&self) -> bool {
        matches!(self.mode, Mode::Normal | Mode::Help)
    }

    /// Whether a job or an agent is under way, for the title bar.
    pub fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
    }

    fn say(&mut self, text: impl Into<String>, tone: Tone) {
        self.status = Some((text.into(), tone));
        self.notices.clear();
    }

    /// Runs and jobs that reached a state worth telling, from the watch.
    /// The footer names the first, with how many more wait behind it, and
    /// Enter opens it.
    pub fn noticed(&mut self, moved: &[api::Transition]) {
        self.notices.extend(moved.iter().cloned());
        self.show_notice();
    }

    fn show_notice(&mut self) {
        let Some(t) = self.notices.first() else {
            return;
        };
        let tone = match t.state.as_str() {
            "failed" | "stopped" => Tone::Bad,
            "pushed" | "merged" | "applied" | "done" => Tone::Good,
            _ => Tone::Warn,
        };
        let more = match self.notices.len() {
            1 => String::new(),
            n => format!("; {} more", n - 1),
        };
        self.status = Some((format!("{}  (Enter opens it{more})", t.line()), tone));
    }

    fn panes(&self) -> &[Pane] {
        match &self.screens[index(self.tab)] {
            Some(Ok(Screen {
                body: Body::Panes(p),
                ..
            })) => p,
            _ => &[],
        }
    }

    /// The selected row and the pane it is in.
    fn pick(&self) -> (Pick, Vec<Item>) {
        let t = index(self.tab);
        let Some(pane) = self.panes().get(self.pane[t]) else {
            return (Pick::Nothing, Vec::new());
        };
        let row = self.selected[t]
            .get(self.pane[t])
            .and_then(|s| s.selected())
            .and_then(|i| pane.items.get(i));
        (
            row.map_or(Pick::Nothing, |i| i.pick.clone()),
            pane.items.clone(),
        )
    }

    /// A view arrived. One for a tab replaces that tab's screen; a run or a
    /// job log opens over the tab.
    pub fn loaded(&mut self, view: std::result::Result<View, String>) {
        let s = match view {
            Ok(v) => screen::screen(v),
            Err(e) => {
                let t = index(self.tab);
                self.full[t] = Some(Err(e.clone()));
                self.screens[t] = Some(Err(e));
                return;
            }
        };
        let Some(tab) = s.tab else {
            self.mode = Mode::Detail {
                screen: s,
                scroll: 0,
            };
            return;
        };
        let t = index(tab);
        self.full[t] = Some(Ok(s));
        self.refilter(t);
    }

    /// Shows tab `t`'s screen through its filter: each table keeps the rows
    /// the filter matches, best first. The selection stays within what is
    /// shown, and a mark on a row no longer shown is dropped.
    fn refilter(&mut self, t: usize) {
        let Some(Ok(full)) = &self.full[t] else {
            self.screens[t] = self.full[t].clone();
            return;
        };
        let mut s = full.clone();
        if let (Some(f), Body::Panes(panes)) = (self.filter[t].as_deref(), &mut s.body)
            && !f.is_empty()
        {
            for pane in panes.iter_mut() {
                let kept = pma_core::fuzzy::rank(f, &pane.items, |i| {
                    i.texts().iter().map(|t| t.to_string()).collect()
                });
                pane.items = kept.into_iter().map(|i| pane.items[i].clone()).collect();
            }
        }
        if let Body::Panes(panes) = &s.body {
            // Keep each selection where it was, within the new length.
            let states = &mut self.selected[t];
            states.resize_with(panes.len(), TableState::default);
            for (state, pane) in states.iter_mut().zip(panes) {
                let len = pane.items.len();
                state.select(match (len, state.selected()) {
                    (0, _) => None,
                    (n, Some(i)) => Some(i.min(n - 1)),
                    (_, None) => Some(0),
                });
            }
            if self.pane[t] >= panes.len() || panes[self.pane[t]].items.is_empty() {
                self.pane[t] = panes
                    .iter()
                    .position(|p| !p.items.is_empty())
                    .unwrap_or(self.pane[t].min(panes.len().saturating_sub(1)));
            }
        }
        // A mark on a row no longer shown would be dispatched unseen. One
        // still shown takes the row's pick as it is now: its `retry` moves.
        if t == index(self.tab)
            && let Body::Panes(panes) = &s.body
        {
            let rows: Vec<&Pick> = panes
                .iter()
                .flat_map(|p| &p.items)
                .map(|i| &i.pick)
                .collect();
            self.marks = self
                .marks
                .iter()
                .filter_map(|m| rows.iter().find(|p| same_task(m, p)).map(|p| (*p).clone()))
                .collect();
        }
        self.screens[t] = Some(Ok(s));
    }

    /// The list for an act arrived, or failed to. A scope's list starts with
    /// the choice of no scope, so it is never empty.
    pub fn offered(
        &mut self,
        what: Choosing,
        pick: Pick,
        label: &'static str,
        choices: std::result::Result<api::Choices, String>,
    ) {
        let choices = choices.map(|mut c| {
            if what == Choosing::Scope {
                c.options.insert(
                    0,
                    pma_core::catalog::Choice {
                        name: NO_SCOPE.into(),
                        reads: "tag".into(),
                        about: "every project".into(),
                    },
                );
            }
            c
        });
        match choices {
            Ok(choices) if choices.options.is_empty() => self.say("Nothing to choose.", Tone::Warn),
            Ok(choices) => {
                let on = vec![false; choices.flags.len()];
                self.mode = Mode::Choose {
                    what,
                    label,
                    pick,
                    choices,
                    at: 0,
                    on,
                    filter: None,
                };
            }
            Err(e) => self.say(e, Tone::Bad),
        }
    }

    /// The checklist for an act arrived. An empty one asks nothing: the act
    /// is done at once, and its request returned.
    pub fn checking(
        &mut self,
        act: Act,
        pick: Pick,
        label: &'static str,
        items: std::result::Result<Vec<(String, bool)>, String>,
    ) -> Option<Request> {
        match items {
            Ok(items) if items.is_empty() => {
                let (_, pane) = self.pick();
                let confirms = pma_core::keys::BINDINGS.iter().any(|b| {
                    b.command == Command::Act(act) && matches!(b.ask, Ask::CheckOrConfirm(_))
                });
                match confirms {
                    true => self.begin(act, Ask::Confirm, pick, pane),
                    false => self.ask_done(act, &pick, &pane, ""),
                }
            }
            Ok(items) => {
                let (items, on) = items.into_iter().unzip();
                self.mode = Mode::Check {
                    act,
                    label,
                    pick,
                    items,
                    on,
                    at: 0,
                };
                None
            }
            Err(e) => {
                self.say(e, Tone::Bad);
                None
            }
        }
    }

    /// Opens the prompt `Request::Prompt` asked for, starting from `input`.
    pub fn prompted(
        &mut self,
        act: Act,
        pick: Pick,
        label: &'static str,
        input: std::result::Result<String, String>,
    ) {
        match input {
            Ok(input) => {
                self.mode = Mode::Prompt {
                    act,
                    label,
                    input,
                    pick,
                }
            }
            Err(e) => self.say(e, Tone::Bad),
        }
    }

    /// An action finished; what it said, or its error, shows in the footer.
    pub fn acted(&mut self, outcome: std::result::Result<Outcome, String>) {
        self.mode = Mode::Normal;
        match outcome {
            Ok(o) => {
                let warned = !o.warnings.is_empty();
                let lines: Vec<String> = o
                    .said
                    .into_iter()
                    .chain(o.warnings.into_iter().map(|w| format!("warning: {w}")))
                    .collect();
                let tone = if warned { Tone::Warn } else { Tone::Good };
                match lines.is_empty() {
                    true => self.say("Done.", tone),
                    false => self.say(lines.join("; "), tone),
                }
            }
            Err(e) => self.say(e, Tone::Bad),
        }
    }

    /// Applies one key, named as `KeyboardEvent.key` names it.
    pub fn key(&mut self, key: &str) -> Option<Request> {
        match &mut self.mode {
            Mode::Help => {
                self.mode = Mode::Normal;
                return None;
            }
            Mode::Detail { scroll, screen } => {
                // The last row the view can start at and still be full.
                let (total, rows) = self.extent;
                let end = total.saturating_sub(rows as usize).min(u16::MAX as usize) as u16;
                match key {
                    "j" | "ArrowDown" => *scroll = scroll.saturating_add(1).min(end),
                    "k" | "ArrowUp" => *scroll = scroll.saturating_sub(1),
                    "g" | "Home" => *scroll = 0,
                    "G" | "End" => *scroll = end,
                    "PageDown" | " " => *scroll = scroll.saturating_add(rows).min(end),
                    "PageUp" => *scroll = scroll.saturating_sub(rows),
                    "Escape" | "q" | "Enter" => self.mode = Mode::Normal,
                    _ => {
                        // An act that fits what the detail shows applies
                        // from inside it: approve after reading the diff.
                        let Body::Detail(d) = &screen.body else {
                            return None;
                        };
                        let pick = d.pick.clone();
                        let b = keys::lookup(key, self.tab, &pick)?;
                        if b.command == Command::ViewDiff
                            && let Pick::Run { id, .. } = pick
                        {
                            return Some(Request::Pager { id });
                        }
                        if b.command == Command::Edit
                            && let Some(project) = pick.project()
                        {
                            return Some(Request::Editor {
                                project: project.to_string(),
                            });
                        }
                        if let Command::Act(act) = b.command
                            && act.on_row()
                            && screen::fits(act, &pick)
                        {
                            self.advance = match pick {
                                Pick::Run { id, .. } if act.decides() => Some(id),
                                _ => None,
                            };
                            return self.begin(act, b.ask, pick, Vec::new());
                        }
                    }
                }
                return None;
            }
            Mode::Confirm { action, .. } => {
                let action = action.clone();
                self.mode = Mode::Normal;
                return match key {
                    "y" => {
                        if matches!(action, Action::Dispatch { .. } | Action::CloseRuns { .. }) {
                            self.marks.clear();
                        }
                        Some(Request::Act(action))
                    }
                    _ => {
                        self.say("Cancelled.", Tone::Muted);
                        None
                    }
                };
            }
            Mode::Prompt { input, .. } => {
                match key {
                    "Escape" => {
                        self.mode = Mode::Normal;
                        self.say("Cancelled.", Tone::Muted);
                    }
                    "Backspace" => {
                        input.pop();
                    }
                    "Enter" => {
                        let Mode::Prompt {
                            act, input, pick, ..
                        } = std::mem::replace(&mut self.mode, Mode::Normal)
                        else {
                            unreachable!("in a prompt");
                        };
                        let (_, pane) = self.pick();
                        return self.ask_done(act, &pick, &pane, &input);
                    }
                    k if k.chars().count() == 1 => input.push_str(k),
                    _ => {}
                }
                return None;
            }
            Mode::Choose {
                choices,
                at,
                on,
                filter,
                ..
            } => {
                let n = kept(choices, filter.as_deref()).len();
                match (key, filter.as_mut()) {
                    ("Escape", Some(_)) => {
                        *filter = None;
                        *at = 0;
                    }
                    ("Escape", None) => {
                        self.mode = Mode::Normal;
                        self.say("Cancelled.", Tone::Muted);
                    }
                    ("ArrowDown", _) | ("j", None) => *at = (*at + 1).min(n.saturating_sub(1)),
                    ("ArrowUp", _) | ("k", None) => *at = at.saturating_sub(1),
                    ("Home", _) | ("g", None) => *at = 0,
                    ("End", _) | ("G", None) => *at = n.saturating_sub(1),
                    ("/", None) => *filter = Some(String::new()),
                    ("Backspace", Some(f)) => {
                        f.pop();
                        *at = 0;
                    }
                    ("Enter", _) => {
                        let Mode::Choose {
                            what,
                            pick,
                            choices,
                            at,
                            on,
                            filter,
                            ..
                        } = std::mem::replace(&mut self.mode, Mode::Normal)
                        else {
                            unreachable!("in a list");
                        };
                        let Some(&i) = kept(&choices, filter.as_deref()).get(at) else {
                            self.say("Nothing matches.", Tone::Warn);
                            return None;
                        };
                        let name = choices.options[i].name.clone();
                        let act = match what {
                            Choosing::Act(act) => act,
                            Choosing::Project => return Some(self.open_project(name)),
                            Choosing::Scope => {
                                self.scope = match name.as_str() {
                                    NO_SCOPE => Vec::new(),
                                    tag => vec![tag.to_string()],
                                };
                                self.marks.clear();
                                return Some(Request::Load(self.query()));
                            }
                        };
                        let input = choices.line(&name, &on);
                        let (_, pane) = self.pick();
                        return self.ask_done(act, &pick, &pane, &input);
                    }
                    (k, Some(f)) if k.chars().count() == 1 => {
                        f.push_str(k);
                        *at = 0;
                    }
                    (k, None) => {
                        if let Some(i) = choices.flags.iter().position(|f| f.key == k) {
                            on[i] = !on[i];
                        }
                    }
                    _ => {}
                }
                return None;
            }
            Mode::Filter => {
                let t = index(self.tab);
                match key {
                    "Escape" => {
                        self.filter[t] = None;
                        self.refilter(t);
                        self.mode = Mode::Normal;
                        self.say("Filter cleared.", Tone::Muted);
                    }
                    "Enter" => {
                        if self.filter[t].as_deref().is_some_and(str::is_empty) {
                            self.filter[t] = None;
                        }
                        self.mode = Mode::Normal;
                    }
                    "Backspace" => {
                        if let Some(f) = &mut self.filter[t] {
                            f.pop();
                        }
                        self.refilter(t);
                    }
                    "ArrowDown" | "ArrowUp" => {
                        let len = self.panes().get(self.pane[t]).map_or(0, |p| p.items.len());
                        if let Some(state) = self.selected[t].get_mut(self.pane[t])
                            && len > 0
                        {
                            let at = state.selected().unwrap_or(0);
                            state.select(Some(match key {
                                "ArrowDown" => (at + 1).min(len - 1),
                                _ => at.saturating_sub(1),
                            }));
                        }
                    }
                    k if k.chars().count() == 1 => {
                        self.filter[t].get_or_insert_default().push_str(k);
                        self.refilter(t);
                    }
                    _ => {}
                }
                return None;
            }
            Mode::Check { items, on, at, .. } => {
                match key {
                    "j" | "ArrowDown" => *at = (*at + 1).min(items.len().saturating_sub(1)),
                    "k" | "ArrowUp" => *at = at.saturating_sub(1),
                    " " => on[*at] = !on[*at],
                    "Escape" => {
                        self.mode = Mode::Normal;
                        self.say("Cancelled.", Tone::Muted);
                    }
                    "Enter" => {
                        let Mode::Check { act, pick, on, .. } =
                            std::mem::replace(&mut self.mode, Mode::Normal)
                        else {
                            unreachable!("in a checklist");
                        };
                        let off: Vec<String> = (1..=on.len())
                            .filter(|n| !on[n - 1])
                            .map(|n| n.to_string())
                            .collect();
                        let (_, pane) = self.pick();
                        return self.ask_done(act, &pick, &pane, &off.join(","));
                    }
                    _ => {}
                }
                return None;
            }
            Mode::Normal => {}
        }
        if !self.notices.is_empty() {
            if key != "Enter" {
                self.notices.clear();
            } else {
                let pick = self.notices.remove(0).pick();
                self.status = None;
                // The next one waits in the footer for the next Enter.
                self.show_notice();
                return match pick {
                    Pick::Run { id, .. } => Some(Request::Load(Query::Run { id })),
                    Pick::Job { id, .. } => Some(Request::Load(Query::JobLog { id })),
                    _ => None,
                };
            }
        }
        let binding = keys::lookup(key, self.tab, &self.pick().0)?;
        let t = index(self.tab);
        match binding.command {
            Command::Tab(tab) => {
                self.tab = tab;
                self.status = None;
                self.marks.clear();
                return Some(Request::Load(self.query()));
            }
            Command::Down
            | Command::Up
            | Command::First
            | Command::Last
            | Command::PageDown
            | Command::PageUp => {
                let len = self.panes().get(self.pane[t]).map_or(0, |p| p.items.len());
                let p = self.pane[t];
                // A page is what the last draw had room for; 10 before one.
                let page = self.rows[t].get(p).copied().unwrap_or(10).max(1) as usize;
                if let Some(state) = self.selected[t].get_mut(p)
                    && len > 0
                {
                    let at = state.selected().unwrap_or(0);
                    state.select(Some(match binding.command {
                        Command::Down => (at + 1).min(len - 1),
                        Command::Up => at.saturating_sub(1),
                        Command::PageDown => (at + page).min(len - 1),
                        Command::PageUp => at.saturating_sub(page),
                        Command::First => 0,
                        _ => len - 1,
                    }));
                }
            }
            Command::Filter => {
                self.status = None;
                self.filter[t].get_or_insert_default();
                self.mode = Mode::Filter;
            }
            Command::Jump => {
                return Some(Request::Choose {
                    what: Choosing::Project,
                    pick: Pick::Nothing,
                    label: "go to project",
                });
            }
            Command::Scope => {
                return Some(Request::Choose {
                    what: Choosing::Scope,
                    pick: Pick::Nothing,
                    label: "scope to a tag",
                });
            }
            Command::ViewDiff => match self.pick().0 {
                Pick::Run { id, .. } => return Some(Request::Pager { id }),
                _ => self.say("Select a run first.", Tone::Warn),
            },
            Command::Edit => match self.edit_target() {
                Some(project) => return Some(Request::Editor { project }),
                None => self.say("Select a row that names a project first.", Tone::Warn),
            },
            Command::NextPane | Command::PrevPane => {
                let n = self.panes().len().max(1);
                self.pane[t] = match binding.command {
                    Command::NextPane => (self.pane[t] + 1) % n,
                    _ => (self.pane[t] + n - 1) % n,
                };
            }
            Command::Open => {
                return match self.pick().0 {
                    Pick::Run { id, .. } => Some(Request::Load(Query::Run { id })),
                    Pick::Job { id, .. } => Some(Request::Load(Query::JobLog { id })),
                    // A task opens in full wherever it is listed: its
                    // description is what the agent is given. `o` opens
                    // its project.
                    Pick::Task {
                        project, target, ..
                    } => Some(Request::Load(Query::task(&project, &target))),
                    Pick::Scan { id } => Some(Request::Load(Query::Scan { id })),
                    Pick::Route { revision, .. } => {
                        Some(Request::Load(Query::RouteDoc { revision }))
                    }
                    Pick::Campaign { name } => Some(Request::Load(Query::Campaign { name })),
                    other => other.project().map(|p| self.open_project(p.to_string())),
                };
            }
            Command::Back => {
                self.status = None;
                if !self.marks.is_empty() {
                    self.marks.clear();
                    self.say("Marks cleared.", Tone::Muted);
                } else if self.filter[t].is_some() {
                    self.filter[t] = None;
                    self.refilter(t);
                    self.say("Filter cleared.", Tone::Muted);
                } else if self.tab == Tab::Project {
                    self.tab = self.previous;
                    return Some(Request::Load(self.query()));
                }
            }
            Command::OpenProject => match self.pick().0.project() {
                Some(name) => return Some(self.open_project(name.to_string())),
                None => self.say("Select a row that names a project.", Tone::Warn),
            },
            Command::ReportBy => {
                self.by = (self.by + 1) % By::ALL.len();
                return Some(Request::Load(self.query()));
            }
            Command::Finished => {
                self.finished = (self.finished + 1) % api::FINISHED.len();
                return Some(Request::Load(self.query()));
            }
            Command::Help => self.mode = Mode::Help,
            Command::Quit => return Some(Request::Quit),
            Command::Mark => {
                let pick = self.pick().0;
                if !self.markable(&pick) {
                    self.say(
                        "Only a task, a project or an open run can be marked.",
                        Tone::Warn,
                    );
                    return None;
                }
                if let Err(e) = self.toggle_mark(&pick) {
                    self.say(e, Tone::Warn);
                    return None;
                }
                self.say_marks(&pick);
            }
            Command::MarkAll => {
                let picks: Vec<Pick> = self
                    .pick()
                    .1
                    .into_iter()
                    .map(|i| i.pick)
                    .filter(|p| self.markable(p))
                    .collect();
                if picks.is_empty() {
                    self.say("Nothing shown that can be marked.", Tone::Warn);
                    return None;
                }
                // Every shown row marked already: unmark them all.
                let all = picks
                    .iter()
                    .all(|p| self.marks.iter().any(|m| same_task(m, p)));
                for pick in &picks {
                    let marked = self.marks.iter().any(|m| same_task(m, pick));
                    if marked == all
                        && let Err(e) = self.toggle_mark(pick)
                    {
                        self.say(e, Tone::Warn);
                        return None;
                    }
                }
                self.say_marks(&picks[0]);
            }
            Command::Act(act) => {
                let (mut pick, pane) = self.pick();
                let runs_marked = self.marks.iter().any(|m| matches!(m, Pick::Run { .. }));
                let over_marks = match act {
                    Act::ForgetRun => runs_marked,
                    Act::Dispatch | Act::DispatchPush | Act::DispatchInteractive => !runs_marked,
                    Act::Run | Act::Configure => self.tab == Tab::Projects,
                    _ => false,
                };
                if over_marks && !self.marks.is_empty() {
                    pick = screen::marked(&self.marks);
                }
                self.advance = None;
                return self.begin(act, binding.ask, pick, pane);
            }
        }
        None
    }

    /// The project `E` opens: the selected row's, else the one in view.
    fn edit_target(&self) -> Option<String> {
        match self.pick().0.project() {
            Some(p) => Some(p.to_string()),
            None if self.tab == Tab::Project && !self.project.is_empty() => {
                Some(self.project.clone())
            }
            None => None,
        }
    }

    /// Whether a row can be marked here: a task, a project on the projects
    /// tab, or a run that can be closed.
    fn markable(&self, pick: &Pick) -> bool {
        match pick {
            Pick::Task { .. } => true,
            Pick::Manual { .. } | Pick::Project { .. } => self.tab == Tab::Projects,
            Pick::Run { .. } => screen::fits(Act::ForgetRun, pick),
            _ => false,
        }
    }

    /// Marks `pick`, or unmarks it. An act takes one kind of row, so a mark
    /// of another kind is refused rather than lost.
    fn toggle_mark(&mut self, pick: &Pick) -> std::result::Result<(), &'static str> {
        let is_run = |p: &Pick| matches!(p, Pick::Run { .. });
        if self.marks.iter().any(|m| is_run(m) != is_run(pick)) {
            return Err("Another kind of row is marked; Esc clears it first.");
        }
        match self.marks.iter().position(|m| same_task(m, pick)) {
            Some(i) => {
                self.marks.remove(i);
            }
            None => self.marks.push(pick.clone()),
        }
        Ok(())
    }

    /// Says how many rows are marked and what acts on them.
    fn say_marks(&mut self, kind: &Pick) {
        match self.marks.len() {
            0 => self.say("Nothing marked.", Tone::Muted),
            n if matches!(kind, Pick::Run { .. }) => self.say(
                format!("{n} marked; X closes them, leaving their files. Esc clears."),
                Tone::Accent,
            ),
            n if matches!(kind, Pick::Manual { .. } | Pick::Project { .. }) => self.say(
                format!(
                    "{n} marked; z runs one name over them, d or I dispatches each one's task. Esc clears."
                ),
                Tone::Accent,
            ),
            n => self.say(
                format!("{n} marked; d dispatches them, one run per project. Esc clears."),
                Tone::Accent,
            ),
        }
    }

    /// Opens `name`'s project view, remembering where it was opened from.
    fn open_project(&mut self, name: String) -> Request {
        self.project = name;
        self.marks.clear();
        self.filter[index(Tab::Project)] = None;
        if self.tab != Tab::Project {
            self.previous = self.tab;
        }
        self.tab = Tab::Project;
        self.status = None;
        Request::Load(self.query())
    }

    /// Starts an act on `pick`: asks what the binding asks, or does it.
    fn begin(&mut self, act: Act, ask: Ask, pick: Pick, pane: Vec<Item>) -> Option<Request> {
        match ask {
            Ask::Text(label) if api::drafts(act) => Some(Request::Prompt { act, pick, label }),
            Ask::Text(label) => {
                self.mode = Mode::Prompt {
                    act,
                    label,
                    input: screen::initial_input(act, &pick),
                    pick,
                };
                None
            }
            Ask::Choose(label) => Some(Request::Choose {
                what: Choosing::Act(act),
                pick,
                label,
            }),
            Ask::Check(label) | Ask::CheckOrConfirm(label) => {
                Some(Request::Check { act, pick, label })
            }
            Ask::Confirm => {
                match screen::action_scoped(act, &pick, &pane, "", &self.project, &self.scope) {
                    Ok(action) => {
                        self.mode = Mode::Confirm {
                            action,
                            question: screen::question(act, &pick),
                        };
                        None
                    }
                    Err(e) => {
                        self.say(e, Tone::Bad);
                        None
                    }
                }
            }
            Ask::Nothing => self.ask_done(act, &pick, &pane, ""),
        }
    }

    fn ask_done(&mut self, act: Act, pick: &Pick, pane: &[Item], input: &str) -> Option<Request> {
        match screen::action_scoped(act, pick, pane, input, &self.project, &self.scope) {
            Ok(a) => {
                if matches!(a, Action::Run { .. }) {
                    self.marks.clear();
                }
                self.say("Working...", Tone::Accent);
                Some(Request::Act(a))
            }
            Err(e) => {
                self.say(e, Tone::Bad);
                None
            }
        }
    }

    /// After a decision from a detail, the next ready run in the view, if any.
    fn next_review(&mut self) -> Option<Request> {
        let decided = self.advance.take()?;
        self.panes()
            .iter()
            .flat_map(|p| &p.items)
            .find_map(|i| match &i.pick {
                Pick::Run { id, state, .. } if state == "ready" && *id != decided => {
                    Some(Request::Load(Query::Run { id: *id }))
                }
                _ => None,
            })
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let foot = fold(self.footer(), frame.area().width);
        let [title, header, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(6),
            Constraint::Length(foot.len() as u16),
        ])
        .areas(frame.area());
        frame.render_widget(self.title_bar(title.width), title);
        let t = index(self.tab);
        let p = self.palette;
        match self.screens[t].clone() {
            None => frame.render_widget(Line::styled(" Loading...", p.muted()), header),
            Some(Err(e)) => {
                frame.render_widget(Line::styled(format!(" {e}"), p.tone(Tone::Bad)), header);
            }
            Some(Ok(s)) => {
                if let Some(h) = &s.header {
                    frame.render_widget(Line::styled(format!(" {h}"), p.muted()), header);
                }
                let body = match s.summary.is_empty() {
                    true => body,
                    false => {
                        let [facts, rest] =
                            Layout::vertical([Constraint::Length(3), Constraint::Min(4)])
                                .areas(body);
                        self.render_summary(frame, facts, &s.summary);
                        rest
                    }
                };
                if let Body::Panes(panes) = &s.body {
                    self.render_body(frame, body, panes);
                }
            }
        }
        frame.render_widget(Text::from(foot), footer);
        match self.mode.clone() {
            Mode::Help => self.render_help(frame, body),
            Mode::Detail { screen: s, scroll } => {
                if let Body::Detail(d) = &s.body {
                    self.render_detail(frame, body, d, s.header.as_deref(), scroll);
                }
            }
            Mode::Confirm { question, .. } => self.render_confirm(frame, &question),
            Mode::Prompt { label, input, .. } => self.render_prompt(frame, label, &input),
            Mode::Choose {
                label,
                choices,
                at,
                on,
                filter,
                ..
            } => self.render_choose(frame, body, label, &choices, at, &on, filter.as_deref()),
            Mode::Check {
                label,
                items,
                on,
                at,
                ..
            } => self.render_check(frame, body, label, &items, &on, at),
            // The filter shows in each pane's title as it is typed.
            Mode::Normal | Mode::Filter => {}
        }
    }

    fn title_bar(&self, width: u16) -> Line<'static> {
        let p = self.palette;
        let mut spans = vec![Span::styled(
            " pma ",
            p.accent().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        )];
        spans.push(Span::raw(" "));
        for b in keys::BINDINGS {
            if let Command::Tab(tab) = b.command {
                let current = tab == self.tab;
                spans.push(Span::styled(
                    format!(" {}", b.key),
                    match current {
                        true => p.accent().add_modifier(Modifier::BOLD),
                        false => p.muted(),
                    },
                ));
                spans.push(Span::styled(
                    format!(" {} ", tab.title()),
                    match current {
                        true => p
                            .accent()
                            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                        false => Style::new(),
                    },
                ));
            }
        }
        if self.tab == Tab::Project {
            spans.push(Span::styled(
                format!(" {} ", self.project),
                p.accent()
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            ));
        }
        for tag in &self.scope {
            spans.push(Span::styled(
                format!("  #{tag} "),
                p.tone(Tone::Warn).add_modifier(Modifier::BOLD),
            ));
        }
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        let busy = match self.busy {
            true => (
                " working ",
                p.tone(Tone::Accent).add_modifier(Modifier::BOLD),
            ),
            false => (" idle ", p.muted()),
        };
        let gap = (width as usize).saturating_sub(used + busy.0.len());
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(busy.0, busy.1));
        Line::from(spans)
    }

    fn footer(&self) -> Line<'static> {
        let p = self.palette;
        match &self.mode {
            Mode::Prompt { .. } => {
                return hints(p, &[("Enter", "submit"), ("Esc", "cancel")]);
            }
            Mode::Filter => {
                return hints(
                    p,
                    &[
                        ("type", "filter"),
                        ("Up/Down", "move"),
                        ("Enter", "keep"),
                        ("Esc", "clear"),
                    ],
                );
            }
            Mode::Confirm { .. } => return hints(p, &[("y", "yes"), ("any key", "no")]),
            Mode::Choose {
                filter: Some(_), ..
            } => {
                return hints(
                    p,
                    &[
                        ("type", "filter"),
                        ("Up/Down", "move"),
                        ("Enter", "start"),
                        ("Esc", "all"),
                    ],
                );
            }
            Mode::Choose { choices, .. } => {
                let mut pairs = vec![("j/k", "move"), ("/", "search")];
                pairs.extend(choices.flags.iter().map(|f| (f.key, f.label)));
                pairs.extend([("Enter", "start"), ("Esc", "cancel")]);
                return hints(p, &pairs);
            }
            Mode::Detail { screen, .. } => {
                let pick = match &screen.body {
                    Body::Detail(d) => d.pick.clone(),
                    Body::Panes(_) => Pick::Nothing,
                };
                let mut fit: Vec<&keys::Binding> = keys::on_tab(self.tab)
                    .iter()
                    .filter(|b| {
                        matches!(b.command, Command::Act(a) if a.on_row() && screen::fits(a, &pick))
                    })
                    .copied()
                    .collect();
                next_first(&mut fit, &pick);
                let mut pairs: Vec<(&str, &str)> = fit.iter().map(|b| (b.key, b.label)).collect();
                if matches!(pick, Pick::Run { .. }) {
                    pairs.push(("v", "diff in pager"));
                }
                pairs.extend([
                    ("j/k", "scroll"),
                    ("PgDn/PgUp", "page"),
                    ("G", "end"),
                    ("Esc", "close"),
                ]);
                return hints(p, &pairs);
            }
            Mode::Check { .. } => {
                return hints(
                    p,
                    &[
                        ("j/k", "move"),
                        ("Space", "toggle"),
                        ("Enter", "submit"),
                        ("Esc", "cancel"),
                    ],
                );
            }
            Mode::Help => return hints(p, &[("any key", "close")]),
            Mode::Normal => {}
        }
        if let Some((text, tone)) = &self.status {
            return Line::from(Span::styled(format!(" {text}"), p.tone(*tone)));
        }
        // The acts that fit the selected row. The ones that need no row are
        // the same on every row and live in `?`; they show only where there
        // is no row, so an empty tab still says what it can do.
        let pick = self.pick().0;
        let none = pick == Pick::Nothing;
        let mut fit: Vec<&keys::Binding> = keys::on_tab(self.tab)
            .iter()
            .filter(|b| match b.command {
                Command::Act(a) => a.on_row() && screen::fits(a, &pick),
                Command::Open => {
                    matches!(
                        pick,
                        Pick::Run { .. }
                            | Pick::Job { .. }
                            | Pick::Scan { .. }
                            | Pick::Route { .. }
                            | Pick::Campaign { .. }
                    ) || pick.project().is_some()
                }
                Command::OpenProject => pick.project().is_some(),
                Command::ViewDiff => matches!(pick, Pick::Run { .. }),
                Command::Edit => self.edit_target().is_some(),
                Command::Mark => self.markable(&pick),
                _ => false,
            })
            .chain(
                keys::on_tab(self.tab)
                    .iter()
                    .filter(|b| none && matches!(b.command, Command::Act(a) if !a.on_row())),
            )
            .copied()
            .collect();
        next_first(&mut fit, &pick);
        let mut pairs: Vec<(&str, &str)> = fit.iter().map(|b| (b.key, b.label)).collect();
        pairs.extend([("?", "keys"), ("q", "quit")]);
        hints(p, &pairs)
    }

    fn render_body(&mut self, frame: &mut Frame, area: Rect, panes: &[Pane]) {
        let [lists, preview] =
            Layout::vertical([Constraint::Min(4), Constraint::Length(3)]).areas(area);
        let t = index(self.tab);
        let areas: Vec<Rect> = match (self.tab, panes.len()) {
            // The matrix: urgent on the left, important on top.
            (Tab::Matrix, 4) => {
                let [upper, lower] =
                    Layout::vertical([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
                        .areas(lists);
                let halves = |r| {
                    Layout::horizontal([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
                        .areas::<2>(r)
                };
                let [q1, q2] = halves(upper);
                let [q3, q4] = halves(lower);
                vec![q1, q2, q3, q4]
            }
            _ => Layout::vertical(
                heights(panes, lists.height)
                    .into_iter()
                    .map(Constraint::Length),
            )
            .split(lists)
            .to_vec(),
        };
        self.rows[t].resize(panes.len(), 0);
        for (i, (pane, area)) in panes.iter().zip(areas).enumerate() {
            let focused = i == self.pane[t];
            let block = self.pane_block(pane, i, focused);
            let inner = block.inner(area);
            // Less the header row: what a page moves by.
            self.rows[t][i] = inner.height.saturating_sub(1);
            frame.render_widget(block, area);
            if pane.items.is_empty() {
                frame.render_widget(
                    Paragraph::new(Line::styled(
                        pane.empty,
                        self.palette.muted().add_modifier(Modifier::ITALIC),
                    ))
                    .alignment(Alignment::Center)
                    .wrap(Wrap { trim: true }),
                    inner,
                );
                continue;
            }
            let table = self.table(pane, focused);
            if let Some(state) = self.selected[t].get_mut(i) {
                frame.render_stateful_widget(table, inner, state);
            }
        }
        self.render_preview(frame, preview, panes);
    }

    /// A project's facts, label and value, toned and wrapped.
    fn render_summary(&self, frame: &mut Frame, area: Rect, facts: &[(String, screen::Cell)]) {
        let p = self.palette;
        let mut spans = Vec::new();
        for (label, value) in facts {
            spans.push(Span::styled(format!("{label} "), p.muted()));
            spans.push(Span::styled(
                value.text.clone(),
                p.tone(value.tone).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::raw("    "));
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans))
                .wrap(Wrap { trim: true })
                .block(Block::new().padding(Padding::new(1, 1, 0, 0))),
            area,
        );
    }

    /// A pane's frame: its title and count, and under a filter the filter
    /// and how many of its rows it keeps.
    fn pane_block(&self, pane: &Pane, i: usize, focused: bool) -> Block<'static> {
        let p = self.palette;
        let border = match focused {
            true => p.accent(),
            false => p.muted(),
        };
        let t = index(self.tab);
        let total = match &self.full[t] {
            Some(Ok(Screen {
                body: Body::Panes(full),
                ..
            })) => full.get(i).map_or(pane.items.len(), |f| f.items.len()),
            _ => pane.items.len(),
        };
        let count = match self.filter[t].as_deref() {
            Some(f) if !f.is_empty() => format!("{} of {total} /{f} ", pane.items.len()),
            _ => format!("{} ", pane.items.len()),
        };
        let title = Line::from(vec![
            Span::styled(
                format!(" {} ", pane.title),
                match focused {
                    true => p.accent().add_modifier(Modifier::BOLD),
                    false => Style::new().add_modifier(Modifier::BOLD),
                },
            ),
            Span::styled(count, p.muted()),
        ]);
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(border)
            .title(title)
    }

    fn table(&self, pane: &Pane, focused: bool) -> Table<'static> {
        let p = self.palette;
        // A column no row fills says nothing; it is left out.
        let shown: Vec<usize> = (0..pane.columns.len())
            .filter(|i| {
                pane.items
                    .iter()
                    .any(|it| it.cells.get(*i).is_some_and(|c| !c.text.is_empty()))
            })
            .collect();
        let widths: Vec<Constraint> = shown
            .iter()
            .map(|i| (*i, &pane.columns[*i]))
            .map(|(i, c)| match c.kind {
                Kind::Wide => Constraint::Fill(1),
                _ => {
                    let longest = pane
                        .items
                        .iter()
                        .map(|it| it.cells.get(i).map_or(0, |c| c.text.chars().count()))
                        .max()
                        .unwrap_or(0)
                        .max(c.title.chars().count());
                    Constraint::Length(longest.min(28) as u16)
                }
            })
            .collect();
        // A column for marks, only while this pane holds one.
        let marked = |pick: &Pick| self.marks.iter().any(|m| same_task(m, pick));
        let marking = pane.items.iter().any(|it| marked(&it.pick));
        let mut widths = widths;
        if marking {
            widths.insert(0, Constraint::Length(1));
        }
        let header = Row::new(marking.then(|| TableCell::from("")).into_iter().chain(
            shown.iter().map(|i| &pane.columns[*i]).map(|c| {
                let cell = TableCell::from(c.title.to_uppercase());
                match c.kind {
                    Kind::Number => {
                        cell.content(Text::from(c.title.to_uppercase()).right_aligned())
                    }
                    _ => cell,
                }
            }),
        ))
        .style(p.muted().add_modifier(Modifier::BOLD));
        let rows = pane.items.iter().map(|it| {
            let marked = marked(&it.pick);
            let mark = marking.then(|| {
                TableCell::from(Span::styled(
                    if marked { "+" } else { "" },
                    p.accent().add_modifier(Modifier::BOLD),
                ))
            });
            Row::new(
                mark.into_iter().chain(
                    shown
                        .iter()
                        .map(|i| (&it.cells[*i], &pane.columns[*i]))
                        .map(|(c, col)| {
                            let text = Text::from(Span::styled(c.text.clone(), p.tone(c.tone)));
                            TableCell::from(match col.kind {
                                Kind::Number => text.right_aligned(),
                                _ => text,
                            })
                        }),
                ),
            )
            .style(match marked {
                true => Style::new().add_modifier(Modifier::BOLD),
                false => Style::new(),
            })
        });
        Table::new(rows, widths)
            .header(header)
            .column_spacing(2)
            // Only the focused pane marks its selection: one cursor on screen.
            .highlight_symbol(match focused {
                true => Span::styled("> ", p.accent().add_modifier(Modifier::BOLD)),
                false => Span::raw("  "),
            })
            .highlight_spacing(ratatui::widgets::HighlightSpacing::Always)
            .row_highlight_style(match focused {
                true => Style::new().add_modifier(Modifier::REVERSED),
                false => Style::new(),
            })
    }

    /// The focused row in full, column by column: the table truncates what
    /// this shows.
    fn render_preview(&self, frame: &mut Frame, area: Rect, panes: &[Pane]) {
        let p = self.palette;
        let t = index(self.tab);
        let block = Block::new()
            .borders(Borders::TOP)
            .border_style(p.muted())
            .title(Span::styled(" selected ", p.muted()));
        let row = panes.get(self.pane[t]).and_then(|pane| {
            let i = self.selected[t].get(self.pane[t])?.selected()?;
            Some((pane, pane.items.get(i)?))
        });
        let line = match row {
            None => Line::styled("Nothing selected.", p.muted()),
            Some((pane, item)) => {
                let mut spans = Vec::new();
                // The table shows the rest; a wide column is what it may cut.
                let wide = item
                    .cells
                    .iter()
                    .zip(&pane.columns)
                    .any(|(c, col)| col.kind == Kind::Wide && !c.text.is_empty());
                for (c, col) in item.cells.iter().zip(&pane.columns) {
                    if c.text.is_empty() || (wide && col.kind != Kind::Wide) {
                        continue;
                    }
                    spans.push(Span::styled(format!("{} ", col.title), p.muted()));
                    spans.push(Span::styled(c.text.clone(), p.tone(c.tone)));
                    spans.push(Span::raw("   "));
                }
                Line::from(spans)
            }
        };
        frame.render_widget(
            Paragraph::new(line)
                .wrap(Wrap { trim: true })
                .block(block.padding(Padding::horizontal(1))),
            area,
        );
    }

    fn render_help(&self, frame: &mut Frame, area: Rect) {
        let p = self.palette;
        let on = keys::on_tab(self.tab);
        let group = |title: &str, f: &dyn Fn(&keys::Binding) -> bool| -> Vec<Line<'static>> {
            let mut lines = vec![Line::styled(
                title.to_string(),
                p.accent().add_modifier(Modifier::BOLD),
            )];
            // Keys that do the same thing share a line: `j, ArrowDown`.
            let mut seen: Vec<(Command, &str, Vec<&str>)> = Vec::new();
            for b in on.iter().filter(|b| f(b)) {
                match seen
                    .iter_mut()
                    .find(|(c, l, _)| *c == b.command && *l == b.label)
                {
                    Some((_, _, keys)) => keys.push(b.key),
                    None => seen.push((b.command, b.label, vec![b.key])),
                }
            }
            for (_, label, keys) in seen {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!(
                            "{:>16}  ",
                            keys.iter()
                                .map(|k| keys::shown(k))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        Style::new().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(label),
                ]));
            }
            lines.push(Line::raw(""));
            lines
        };
        let mut left = group("Views", &|b| matches!(b.command, Command::Tab(_)));
        left.extend(group("Move", &|b| {
            !matches!(b.command, Command::Tab(_) | Command::Act(_))
        }));
        let mut right = group(&format!("On {}", self.tab.title()), &|b| {
            matches!(b.command, Command::Act(_)) && !b.tabs.is_empty()
        });
        let mut third = group("Anywhere", &|b| {
            matches!(b.command, Command::Act(_)) && b.tabs.is_empty()
        });
        // A third column where one list would run off the bottom and there is
        // the width for it.
        let wide = right.len() + third.len() + 2 > area.height as usize && area.width >= 108;
        if !wide {
            right.append(&mut third);
        }
        let tallest = left.len().max(right.len()).max(third.len());
        let height = (tallest as u16 + 2).min(area.height);
        let area = centered(area, if wide { 108 } else { 84.min(area.width) }, height);
        frame.render_widget(Clear, area);
        let block = dialog(p, " Keys ", p.accent());
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let columns = if wide { 3 } else { 2 };
        let areas =
            Layout::horizontal(vec![Constraint::Ratio(1, columns); columns as usize]).split(inner);
        frame.render_widget(Paragraph::new(left), areas[0]);
        frame.render_widget(Paragraph::new(right), areas[1]);
        if wide {
            frame.render_widget(Paragraph::new(third), areas[2]);
        }
    }

    fn render_detail(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        d: &Detail,
        header: Option<&str>,
        scroll: u16,
    ) {
        let p = self.palette;
        let mut lines: Vec<Line> = Vec::new();
        if let Some(n) = &d.notice {
            lines.push(Line::styled(n.clone(), p.tone(Tone::Warn)));
            lines.push(Line::raw(""));
        }
        let width = d
            .fields
            .iter()
            .map(|(l, _)| l.chars().count())
            .max()
            .unwrap_or(0);
        for (label, value) in &d.fields {
            lines.push(Line::from(vec![
                Span::styled(format!("{label:>width$}  "), p.muted()),
                Span::raw(value.clone()),
            ]));
        }
        for (title, kind, text) in &d.sections {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                title.clone(),
                p.accent().add_modifier(Modifier::BOLD),
            ));
            for l in text.lines() {
                lines.push(match kind {
                    Section::Diff => diff_line(p, l),
                    Section::Log => Line::raw(l.to_string()),
                    Section::Prose => prose_line(p, l),
                });
            }
        }
        let mut title = vec![Span::styled(
            format!(" {} ", d.title),
            Style::new().add_modifier(Modifier::BOLD),
        )];
        if let Some(b) = &d.badge {
            title.push(Span::styled(format!("{} ", b.text), p.tone(b.tone)));
        }
        if let Some(h) = header {
            title.push(Span::styled(format!(" {h} "), p.muted()));
        }
        // The border and padding take 4 columns and 2 rows. A line wider
        // than the inside wraps onto more rows; the count is what a page and
        // the end move by.
        let inner = area.width.saturating_sub(4).max(1) as usize;
        let total: usize = lines.iter().map(|l| l.width().div_ceil(inner).max(1)).sum();
        self.extent = (total, area.height.saturating_sub(2).max(1));
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0))
                .block(
                    dialog(p, "", p.accent())
                        .title(Line::from(title))
                        .padding(Padding::horizontal(1)),
                ),
            area,
        );
    }

    fn render_confirm(&self, frame: &mut Frame, question: &str) {
        let p = self.palette;
        let width = (question.chars().count() as u16 + 8).clamp(40, 80);
        let area = centered(frame.area(), width.min(frame.area().width), 5);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    question.to_string(),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Line::raw(""),
                Line::from(vec![
                    Span::styled("y", p.tone(Tone::Warn).add_modifier(Modifier::BOLD)),
                    Span::raw(" yes    "),
                    Span::styled("any other key", Style::new().add_modifier(Modifier::BOLD)),
                    Span::raw(" no"),
                ]),
            ])
            .wrap(Wrap { trim: true })
            .block(dialog(p, " Confirm ", p.tone(Tone::Warn)).padding(Padding::horizontal(1))),
            area,
        );
    }

    /// The list for an act: one line per option, its name and what it does,
    /// and the flags with their state.
    #[allow(clippy::too_many_arguments)]
    fn render_choose(
        &self,
        frame: &mut Frame,
        area: Rect,
        label: &str,
        choices: &api::Choices,
        at: usize,
        on: &[bool],
        filter: Option<&str>,
    ) {
        let p = self.palette;
        let width = choices
            .options
            .iter()
            .map(|c| c.name.chars().count())
            .max()
            .unwrap_or(0);
        let mut lines: Vec<Line> = Vec::new();
        if let Some(f) = filter {
            lines.push(Line::from(vec![
                Span::styled("/ ", p.accent().add_modifier(Modifier::BOLD)),
                Span::raw(f.to_string()),
            ]));
        }
        let kept = kept(choices, filter);
        if kept.is_empty() {
            lines.push(Line::styled("  Nothing matches.", p.muted()));
        }
        lines.extend(kept.iter().enumerate().map(|(i, &o)| {
            let c = &choices.options[o];
            let chosen = i == at;
            let mark = match chosen {
                true => Span::styled("> ", p.accent().add_modifier(Modifier::BOLD)),
                false => Span::raw("  "),
            };
            let name = Span::styled(
                format!("{:<width$}  ", c.name),
                match chosen {
                    true => p.accent().add_modifier(Modifier::BOLD),
                    false => Style::new().add_modifier(Modifier::BOLD),
                },
            );
            Line::from(vec![mark, name, Span::styled(c.about.clone(), p.muted())])
        }));
        if !choices.flags.is_empty() {
            lines.push(Line::raw(""));
            let mut spans = Vec::new();
            for (f, on) in choices.flags.iter().zip(on) {
                spans.push(Span::styled(
                    match on {
                        true => "[x] ",
                        false => "[ ] ",
                    },
                    match on {
                        true => p.tone(Tone::Good),
                        false => p.muted(),
                    },
                ));
                spans.push(Span::styled(
                    f.key.to_string(),
                    p.accent().add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::raw(format!(" {}    ", f.label)));
            }
            lines.push(Line::from(spans));
        }
        let height = (lines.len() as u16 + 2).min(area.height);
        let longest = lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 4;
        let area = centered(area, longest.min(area.width), height);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(lines)
                .scroll((
                    at.saturating_sub(height.saturating_sub(3) as usize) as u16,
                    0,
                ))
                .block(
                    dialog(p, &format!(" {label} "), p.accent()).padding(Padding::horizontal(1)),
                ),
            area,
        );
    }

    fn render_check(
        &self,
        frame: &mut Frame,
        area: Rect,
        label: &str,
        items: &[String],
        on: &[bool],
        at: usize,
    ) {
        let p = self.palette;
        let lines: Vec<Line> = items
            .iter()
            .zip(on)
            .enumerate()
            .map(|(i, (text, on))| {
                let cursor = match i == at {
                    true => Span::styled("> ", p.accent().add_modifier(Modifier::BOLD)),
                    false => Span::raw("  "),
                };
                let (box_, tone) = match on {
                    true => ("[x] ", p.tone(Tone::Good)),
                    false => ("[ ] ", p.muted()),
                };
                Line::from(vec![
                    cursor,
                    Span::styled(box_, tone),
                    Span::raw(format!("{}. {text}", i + 1)),
                ])
            })
            .collect();
        let height = (lines.len() as u16 + 2).min(area.height);
        let longest = lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 4;
        let area = centered(area, longest.min(area.width), height);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(lines)
                .scroll((
                    at.saturating_sub(height.saturating_sub(3) as usize) as u16,
                    0,
                ))
                .block(
                    dialog(p, &format!(" {label} "), p.accent()).padding(Padding::horizontal(1)),
                ),
            area,
        );
    }

    fn render_prompt(&self, frame: &mut Frame, label: &str, input: &str) {
        let p = self.palette;
        let area = centered(frame.area(), 70.min(frame.area().width), 5);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::raw(input.to_string()),
                    Span::styled(" ", Style::new().add_modifier(Modifier::REVERSED)),
                ]),
                Line::raw(""),
                Line::styled("Enter submits, Esc cancels", p.muted()),
            ])
            .wrap(Wrap { trim: false })
            .block(dialog(p, &format!(" {label} "), p.accent()).padding(Padding::horizontal(1))),
            area,
        );
    }
}

/// Key hints: the key bold in the accent colour, its label plain.
/// Breaks a footer into lines of `width`, so no key is cut off: between hints
/// when it is a list of them, anywhere in a message.
fn fold(line: Line<'static>, width: u16) -> Vec<Line<'static>> {
    let width = (width as usize).max(1);
    let length = |spans: &[Span]| {
        spans
            .iter()
            .map(|s| s.content.chars().count())
            .sum::<usize>()
    };
    let mut lines = Vec::new();
    match line.spans.as_slice() {
        // A message: one styled span.
        [only] => {
            let chars: Vec<char> = only.content.chars().collect();
            for part in chars.chunks(width) {
                lines.push(Line::from(Span::styled(
                    part.iter().collect::<String>(),
                    only.style,
                )));
            }
        }
        [margin, hints @ ..] => {
            let mut row = vec![margin.clone()];
            for hint in hints.chunks(2) {
                if row.len() > 1 && length(&row) + length(hint) > width {
                    lines.push(Line::from(std::mem::replace(
                        &mut row,
                        vec![margin.clone()],
                    )));
                }
                row.extend_from_slice(hint);
            }
            lines.push(Line::from(row));
        }
        [] => {}
    }
    if lines.is_empty() {
        lines.push(Line::default());
    }
    lines
}

fn hints(p: Palette, pairs: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (key, label) in pairs {
        spans.push(Span::styled(
            keys::shown(key).to_string(),
            p.accent().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {label}   "), p.muted()));
    }
    Line::from(spans)
}

fn dialog(_p: Palette, title: &str, border: Style) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title(Span::styled(
            title.to_string(),
            Style::new().add_modifier(Modifier::BOLD),
        ))
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [v] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    let [h] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(v);
    h
}

/// A line of an agent's summary, which is markdown: `**bold**` is bold,
/// `code` is in the accent colour, and the markers go.
fn prose_line(p: Palette, l: &str) -> Line<'static> {
    let mut spans = Vec::new();
    let (mut bold, mut code) = (false, false);
    let mut word = String::new();
    let mut chars = l.chars().peekable();
    let flush = |word: &mut String, spans: &mut Vec<Span<'static>>, bold: bool, code: bool| {
        if word.is_empty() {
            return;
        }
        let mut style = match code {
            true => p.accent(),
            false => Style::new(),
        };
        if bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        spans.push(Span::styled(std::mem::take(word), style));
    };
    while let Some(c) = chars.next() {
        match c {
            '`' => {
                flush(&mut word, &mut spans, bold, code);
                code = !code;
            }
            '*' if !code && chars.peek() == Some(&'*') => {
                chars.next();
                flush(&mut word, &mut spans, bold, code);
                bold = !bold;
            }
            c => word.push(c),
        }
    }
    flush(&mut word, &mut spans, bold, code);
    Line::from(spans)
}

/// Rows for each stacked pane: up to five of its own rows first, so a short
/// pane is never squeezed out, then the rest shared by how many more each
/// holds. Three rows hold a border pair and an empty pane's message.
fn heights(panes: &[Pane], total: u16) -> Vec<u16> {
    let chrome = 3u16;
    let want: Vec<u16> = panes
        .iter()
        .map(|p| chrome + p.items.len().min(5) as u16)
        .collect();
    let base: u16 = want.iter().sum();
    let mut out = want.clone();
    let extra: Vec<u16> = panes
        .iter()
        .map(|p| p.items.len().saturating_sub(5) as u16)
        .collect();
    let spare = total.saturating_sub(base);
    let weight: u16 = extra.iter().sum();
    if weight == 0 {
        // To a pane with rows to show, not to an empty one above it.
        let i = panes.iter().position(|p| !p.items.is_empty()).unwrap_or(0);
        if let Some(h) = out.get_mut(i) {
            *h += spare;
        }
    } else {
        for (h, e) in out.iter_mut().zip(&extra) {
            *h += (spare as u32 * *e as u32 / weight as u32).min(*e as u32) as u16;
        }
        let used: u16 = out.iter().sum();
        if let Some(i) = extra.iter().position(|e| *e > 0) {
            out[i] += total.saturating_sub(used);
        }
    }
    out
}

/// One line of a unified diff, coloured by what it is.
fn diff_line(p: Palette, l: &str) -> Line<'static> {
    let style = if l.starts_with("+++") || l.starts_with("---") || l.starts_with("diff ") {
        Style::new().add_modifier(Modifier::BOLD)
    } else if l.starts_with('+') {
        p.tone(Tone::Good)
    } else if l.starts_with('-') {
        p.fg(Color::Red)
    } else if l.starts_with("@@") {
        p.accent()
    } else if l.starts_with("index ") {
        p.muted()
    } else {
        Style::new()
    };
    Line::styled(l.to_string(), style)
}

/// A terminal key as `KeyboardEvent.key` names it, the names the key table
/// uses.
pub fn key_name(code: KeyCode) -> Option<String> {
    Some(match code {
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "Enter".into(),
        KeyCode::Esc => "Escape".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::BackTab => "BackTab".into(),
        KeyCode::Backspace => "Backspace".into(),
        KeyCode::Up => "ArrowUp".into(),
        KeyCode::Down => "ArrowDown".into(),
        KeyCode::Left => "h".into(),
        KeyCode::Right => "l".into(),
        KeyCode::Home => "Home".into(),
        KeyCode::End => "End".into(),
        KeyCode::PageUp => "PageUp".into(),
        KeyCode::PageDown => "PageDown".into(),
        _ => return None,
    })
}

/// Whether two picks are the same task, whatever else about it changed.
fn same_task(a: &Pick, b: &Pick) -> bool {
    match (a, b) {
        (Pick::Task { target: x, .. }, Pick::Task { target: y, .. }) => x == y,
        (Pick::Manual { project: x }, Pick::Manual { project: y }) => x == y,
        (
            Pick::Project {
                project: x,
                target: s,
            },
            Pick::Project {
                project: y,
                target: t,
            },
        ) => x == y && s == t,
        (Pick::Run { id: x, .. }, Pick::Run { id: y, .. }) => x == y,
        _ => false,
    }
}

/// Puts the act that moves `pick` on (`screen::next_act`) first, so a
/// footer cut short at the terminal's width still shows it.
fn next_first(bindings: &mut [&keys::Binding], pick: &Pick) {
    if let Some(next) = screen::next_act(pick) {
        bindings.sort_by_key(|b| b.command != Command::Act(next));
    }
}

/// The options of `choices` that `filter` matches, by name and description,
/// best first: every one, in order, without a filter.
fn kept(choices: &api::Choices, filter: Option<&str>) -> Vec<usize> {
    pma_core::fuzzy::rank(filter.unwrap_or_default(), &choices.options, |c| {
        vec![c.name.clone(), c.about.clone()]
    })
}

/// Sends a request through the API and gives the app the result. An action is
/// followed by a reload of the tab, so the view shows what it changed.
fn execute(store: &Store, app: &mut App, request: Request) {
    match request {
        Request::Load(q) => app.loaded(api::query(store, q).map_err(|e| e.to_string())),
        Request::Act(a) => {
            let outcome = api::act(store, a).map_err(|e| e.to_string());
            if outcome.is_err() {
                app.advance = None;
            }
            app.acted(outcome);
            let q = app.query();
            app.loaded(api::query(store, q).map_err(|e| e.to_string()));
            if let Some(next) = app.next_review() {
                execute(store, app, next);
            }
        }
        Request::Choose { what, pick, label } => {
            let choices = match what {
                Choosing::Act(act) => api::choices(store, act),
                Choosing::Project => api::project_choices(store),
                Choosing::Scope => api::tag_choices(store),
            };
            app.offered(what, pick, label, choices.map_err(|e| e.to_string()));
        }
        Request::Prompt { act, pick, label } => {
            let input = api::initial_input(store, act, &pick).map_err(|e| e.to_string());
            app.prompted(act, pick, label, input);
        }
        Request::Check { act, pick, label } => {
            let items = api::checklist(store, act, &pick).map_err(|e| e.to_string());
            if let Some(next) = app.checking(act, pick, label, items) {
                execute(store, app, next);
            }
        }
        // The run loop hands the terminal over for these.
        Request::Pager { .. } | Request::Editor { .. } | Request::Quit => {}
    }
}

/// A run's diff as text for a pager: a heading, then the diff with ANSI
/// colour unless `NO_COLOR` is set.
fn pager_text(store: &Store, id: i64) -> Result<String> {
    let View::Run { run, diff, .. } = api::query(store, Query::Run { id })? else {
        return Err(format!("#{id} is not a run").into());
    };
    let colour = Palette::from_env().color;
    let paint = |code: &str, l: &str| match colour {
        true => format!("\x1b[{code}m{l}\x1b[0m"),
        false => l.to_string(),
    };
    let mut out = format!(
        "{}\n{}\n\n",
        paint("1", &format!("#{} {}: {}", run.id, run.project, run.text)),
        run.diffstat.as_deref().unwrap_or("")
    );
    for l in diff.lines() {
        let line = if l.starts_with("+++") || l.starts_with("---") || l.starts_with("diff ") {
            paint("1", l)
        } else if l.starts_with('+') {
            paint("32", l)
        } else if l.starts_with('-') {
            paint("31", l)
        } else if l.starts_with("@@") {
            paint("36", l)
        } else {
            l.to_string()
        };
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
}

/// Shows `text` in `$PAGER`, `less -R` by default, and waits for it to close.
/// The caller has restored the terminal first and takes it back after.
fn show_in_pager(id: i64, text: &str) -> Result<()> {
    let file = std::env::temp_dir().join(format!("pma-review-{id}-{}.diff", std::process::id()));
    std::fs::write(&file, text).map_err(|e| format!("{}: {e}", file.display()))?;
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(r#"${PAGER:-less -R} "$1""#)
        .arg("sh")
        .arg(&file)
        .status();
    let _ = std::fs::remove_file(&file);
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("the pager exited with {s}").into()),
        Err(e) => Err(format!("could not start the pager: {e}").into()),
    }
}

/// Opens `dir` in `$VISUAL`, else `$EDITOR`, else `vi`, from inside it, and
/// waits for it to close. The caller has restored the terminal first.
fn open_in_editor(dir: &std::path::Path) -> Result<()> {
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(r#"${VISUAL:-${EDITOR:-vi}} "$1""#)
        .arg("sh")
        .arg(dir)
        .current_dir(dir)
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("the editor exited with {s}").into()),
        Err(e) => Err(format!("could not start the editor: {e}").into()),
    }
}

/// Runs the TUI until `q`. It refreshes the tab every 2 seconds while a job
/// or an agent run is under way, and every 30 otherwise.
pub fn run(store: &Store) -> Result<()> {
    let mut terminal = ratatui::try_init()?;
    // `try_init` installs no panic hook, so a panic would leave the terminal
    // raw on the alternate screen, with the message drawn over it.
    let report = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        report(info);
    }));
    let mut app = App::new();
    let result = (|| -> Result<()> {
        let first = Request::Load(app.query());
        execute(store, &mut app, first);
        // The store's data version, read once a second, says when another
        // process committed; the cadence below is the fallback, and what
        // this process's own writes refresh under.
        let mut watch = api::Watch::new(store).ok();
        let mut loaded = std::time::Instant::now();
        loop {
            let busy = match &watch {
                Some(w) => w.busy,
                None => api::busy(store).unwrap_or(false),
            };
            app.set_busy(busy);
            terminal.draw(|f| app.render(f))?;
            let cadence = match busy {
                true => Duration::from_secs(2),
                false => Duration::from_secs(30),
            };
            if !event::poll(Duration::from_secs(1))? {
                let moved = watch.as_mut().and_then(|w| w.poll(store).ok().flatten());
                let changed = moved.is_some();
                if let Some(moved) = moved {
                    app.noticed(&moved);
                }
                if (changed || loaded.elapsed() >= cadence) && app.idle() {
                    let again = Request::Load(app.query());
                    execute(store, &mut app, again);
                    loaded = std::time::Instant::now();
                }
                continue;
            }
            let Event::Key(k) = event::read()? else {
                continue;
            };
            if k.kind != KeyEventKind::Press {
                continue;
            }
            if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
                return Ok(());
            }
            let Some(name) = key_name(k.code) else {
                continue;
            };
            match app.key(&name) {
                Some(Request::Quit) => return Ok(()),
                Some(Request::Pager { id }) => {
                    // The pager has the terminal until it exits; the view is
                    // drawn afresh after.
                    let shown = pager_text(store, id).and_then(|text| {
                        ratatui::try_restore()?;
                        let shown = show_in_pager(id, &text);
                        terminal = ratatui::try_init()?;
                        terminal.clear()?;
                        shown
                    });
                    match shown {
                        Ok(()) => app.status = None,
                        Err(e) => app.say(e.to_string(), Tone::Bad),
                    }
                }
                Some(Request::Editor { project }) => {
                    let edited = store
                        .project(&project)
                        .and_then(|row| {
                            row.ok_or_else(|| format!("unknown project `{project}`").into())
                        })
                        .and_then(|row| {
                            ratatui::try_restore()?;
                            let edited = open_in_editor(&row.path);
                            terminal = ratatui::try_init()?;
                            terminal.clear()?;
                            // Even after an editor error: a file may have
                            // been saved before it.
                            Store::open_default()
                                .and_then(|mut s| {
                                    pma_core::refresh::reread(&mut s, &[(project, row.path)])
                                })
                                .and(edited)
                        });
                    match edited {
                        Ok(()) => app.status = None,
                        Err(e) => app.say(e.to_string(), Tone::Bad),
                    }
                    let again = Request::Load(app.query());
                    execute(store, &mut app, again);
                }
                Some(request) => {
                    // Draw "working" before a slow action blocks the loop.
                    terminal.draw(|f| app.render(f))?;
                    execute(store, &mut app, request);
                }
                None => {}
            }
        }
    })();
    ratatui::try_restore()?;
    let _ = std::panic::take_hook();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use pma_core::next::{Next, Row as NextRow, Subject};
    use pma_core::store::{Job, Note};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn screen_text(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(110, 30)).unwrap();
        terminal.draw(|f| app.render(f)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The colour a piece of text was drawn in.
    fn colour_of(app: &mut App, text: &str) -> Option<Color> {
        let mut terminal = Terminal::new(TestBackend::new(110, 30)).unwrap();
        terminal.draw(|f| app.render(f)).unwrap();
        let buffer = terminal.backend().buffer();
        for y in 0..buffer.area.height {
            let line: String = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect();
            if let Some(at) = line.find(text) {
                let x = line[..at].chars().count() as u16;
                return Some(buffer[(x, y)].fg);
            }
        }
        None
    }

    fn colour(app: App) -> App {
        App {
            palette: Palette { color: true },
            ..app
        }
    }

    fn next_view() -> View {
        View::Next {
            header: "last scan just now".into(),
            limit: None,
            lists: next_view_lists(),
            repos: Default::default(),
        }
    }

    fn next_view_lists() -> Next {
        Next {
            agents: vec![NextRow {
                cells: vec![
                    "a:5".into(),
                    "T1".into(),
                    "critical".into(),
                    "open 3d".into(),
                    "text of a".into(),
                ],
                command: Some("pma dispatch a:5".into()),
                subject: Subject::Task {
                    project: "a".into(),
                    target: "a:5".into(),
                    retry: false,
                },
                issue: None,
            }],
            runs: vec![NextRow {
                cells: vec![
                    "#4".into(),
                    "ready".into(),
                    "to review".into(),
                    "b".into(),
                    "a run".into(),
                ],
                command: Some("pma review 4".into()),
                subject: Subject::Run {
                    fixing: false,
                    empty: false,
                    id: 4,
                    state: "ready".into(),
                    project: "b".into(),
                },
                issue: None,
            }],
            tasks: vec![],
        }
    }

    fn job(id: i64, state: &str) -> Job {
        Job {
            id,
            args: vec!["workflow".into(), "run".into(), "w".into()],
            cwd: "/".into(),
            state: state.into(),
            created_at: 0,
            started_at: None,
            finished_at: None,
            pid: None,
            exit_code: None,
            awaiting: None,
            project: None,
        }
    }

    /// The app opens on `next`; a digit loads another tab; keys act on the
    /// selected row as the table says.
    #[test]
    fn tabs_load_and_keys_act_on_the_selection() {
        let mut app = App::new();
        assert_eq!(
            app.query(),
            Tab::Inbox.query(api::Selection::default(), By::Class, "", None)
        );
        app.loaded(Ok(next_view()));
        let out = screen_text(&mut app);
        assert!(
            out.contains("Ready for agents 1") && out.contains("Runs waiting on you 1"),
            "{out}"
        );
        assert!(out.contains("TASK") && out.contains("PRIORITY"), "{out}");
        assert!(out.contains("> a:5"), "{out}");
        assert!(out.contains("Tasks for you 0"), "{out}");
        assert!(
            out.contains("No critical or urgent task needs you."),
            "{out}"
        );
        assert!(out.contains("last scan just now"), "{out}");

        // `d` asks first, then dispatches the selected task.
        assert_eq!(app.key("d"), None);
        assert!(screen_text(&mut app).contains("dispatch a:5?"));
        let Some(Request::Act(Action::Dispatch { targets, .. })) = app.key("y") else {
            panic!("a dispatch");
        };
        assert_eq!(targets, ["a:5"]);

        let t = index(app.tab);
        let start = app.pane[t];
        app.key("Tab");
        assert_ne!(app.pane[t], start);
        app.key("BackTab");
        assert_eq!(app.pane[t], start, "BackTab undoes Tab");

        app.key("Tab");
        assert_eq!(
            app.key("Enter"),
            Some(Request::Load(Query::Run { id: 4 })),
            "opens the waiting run"
        );
        assert_eq!(
            app.key("3"),
            Some(Request::Load(Query::Activity { finished: None }))
        );
        // `b` filters the finished runs to the next state, and back round.
        assert_eq!(
            app.key("b"),
            Some(Request::Load(Query::Activity {
                finished: Some(pma_core::store::RunState::Rejected)
            }))
        );
        for _ in 1..api::FINISHED.len() {
            app.key("b");
        }
        assert_eq!(app.query(), Query::Activity { finished: None });
        app.loaded(Ok(View::Jobs(vec![
            job(7, "awaiting-approval"),
            job(6, "done"),
        ])));
        assert!(screen_text(&mut app).contains("> #7"));
        assert_eq!(
            app.key("a"),
            Some(Request::Act(Action::ApproveJob { id: 7 }))
        );
        app.key("j");
        assert_eq!(app.key("a"), None);
        assert!(
            app.status.as_ref().unwrap().0.contains("not waiting"),
            "{:?}",
            app.status
        );
        assert_eq!(
            app.key("Enter"),
            Some(Request::Load(Query::JobLog { id: 6 }))
        );
        app.loaded(Ok(View::JobLog {
            id: 6,
            text: Some("plan abc".into()),
            awaiting: false,
        }));
        assert!(!app.idle(), "a detail holds off refreshes");
        assert!(screen_text(&mut app).contains("plan abc"));
        app.key("Escape");
        assert!(app.idle());
        assert_eq!(app.key("q"), Some(Request::Quit));
    }

    /// Space marks tasks; `d` then dispatches the marked ones together,
    /// whichever row is selected. A dispatch, Escape or another view clears
    /// the marks.
    #[test]
    fn marked_tasks_are_dispatched_together() {
        let mut app = App::new();
        // Two tasks for agents; the second's `retry` as given.
        let view = |retry: bool| {
            let mut lists = next_view_lists();
            let mut second = lists.agents[0].clone();
            second.cells[0] = "a:9".into();
            second.subject = Subject::Task {
                project: "a".into(),
                target: "a:9".into(),
                retry,
            };
            lists.agents.push(second);
            View::Next {
                header: String::new(),
                limit: None,
                lists,
                repos: Default::default(),
            }
        };
        app.loaded(Ok(view(false)));
        app.key(" ");
        app.key("j");
        app.key(" ");
        assert_eq!(app.marks.len(), 2);
        // A refresh that moves a marked task's `retry` keeps the mark.
        app.loaded(Ok(view(true)));
        assert_eq!(app.marks.len(), 2, "{:?}", app.marks);
        assert!(screen_text(&mut app).contains("2 marked"));
        app.key("Tab");
        app.key(" ");
        assert!(
            app.status.as_ref().unwrap().0.contains("Another kind"),
            "a run is not marked over tasks"
        );
        assert_eq!(app.marks.len(), 2, "the tasks stay marked");
        assert_eq!(app.key("d"), None);
        assert!(screen_text(&mut app).contains("dispatch 2 tasks (a:5, a:9)?"));
        let Some(Request::Act(Action::Dispatch { targets, .. })) = app.key("y") else {
            panic!("a dispatch");
        };
        assert_eq!(targets, ["a:5", "a:9"]);
        assert!(app.marks.is_empty());

        app.key("Tab");
        app.key(" ");
        app.key("Escape");
        assert!(app.marks.is_empty());
        app.key(" ");
        app.key("2");
        assert!(app.marks.is_empty(), "another view clears them");
    }

    /// A decision from a detail opens the next ready run, not the one just
    /// decided, which a queued act has not moved yet. From a list it opens
    /// nothing.
    #[test]
    fn a_decision_opens_the_next_ready_run() {
        let mut app = App::new();
        app.loaded(Ok(next_view()));
        let ready: Vec<i64> = app
            .panes()
            .iter()
            .flat_map(|p| &p.items)
            .filter_map(|i| match &i.pick {
                Pick::Run { id, state, .. } if state == "ready" => Some(*id),
                _ => None,
            })
            .collect();
        let first = ready[0];
        app.advance = Some(first + 1000);
        assert_eq!(
            app.next_review(),
            Some(Request::Load(Query::Run { id: first }))
        );
        assert_eq!(app.next_review(), None, "once");
        app.advance = Some(first);
        if ready.len() == 1 {
            assert_eq!(app.next_review(), None, "not the run just decided");
        }
    }

    /// `u` on a ready bundle asks what `a` asks, then pushes: the checklist
    /// is the one question.
    #[test]
    fn a_bundle_push_checks_its_items() {
        let mut app = App::new();
        app.loaded(Ok(next_view()));
        app.key("Tab");
        let Some(Request::Check { act, pick, label }) = app.key("u") else {
            panic!("a checklist request");
        };
        assert_eq!(act, Act::Push);
        let items = vec![("one".to_string(), true), ("two".to_string(), true)];
        assert_eq!(app.checking(act, pick, label, Ok(items)), None);
        app.key("j");
        app.key(" ");
        let Some(Request::Act(Action::Publish { drop, target, .. })) = app.key("Enter") else {
            panic!("a push");
        };
        assert_eq!(drop, Some(vec![2]));
        assert_eq!(target, pma_core::publish::Target::Push);
    }

    /// Approving a run asks for its checklist first. A bundle's items come
    /// back checked or not; what is left unchecked is dropped. A run with
    /// none is approved without a question.
    #[test]
    fn a_bundle_approval_checks_its_items() {
        let mut app = App::new();
        app.loaded(Ok(next_view()));
        app.key("Tab");
        let Some(Request::Check { act, pick, label }) = app.key("a") else {
            panic!("a checklist request");
        };
        assert_eq!(act, Act::ApproveRun);
        let items = vec![
            ("one".to_string(), true),
            ("two".to_string(), true),
            ("three".to_string(), false),
        ];
        assert_eq!(app.checking(act, pick.clone(), label, Ok(items)), None);
        let out = screen_text(&mut app);
        assert!(
            out.contains("[x] 1. one") && out.contains("[ ] 3. three"),
            "{out}"
        );
        app.key("j");
        app.key(" ");
        let Some(Request::Act(Action::ApproveRun { id, drop, .. })) = app.key("Enter") else {
            panic!("an approval");
        };
        assert_eq!((id, drop), (4, vec![2, 3]));

        let Some(Request::Act(Action::ApproveRun { drop, .. })) =
            app.checking(act, pick, label, Ok(vec![]))
        else {
            panic!("approved without a question");
        };
        assert!(drop.is_empty());
    }

    /// `o` opens the selected row's project; Escape goes back to where it was
    /// opened from.
    #[test]
    fn a_project_opens_from_its_row_and_closes_back() {
        let mut app = App::new();
        app.loaded(Ok(next_view()));
        assert_eq!(
            app.key("Enter"),
            Some(Request::Load(Query::task("a", "a:5"))),
            "Enter opens the task wherever it is listed"
        );
        assert_eq!(app.tab, Tab::Inbox);
        assert_eq!(
            app.key("o"),
            Some(Request::Load(Query::Project { name: "a".into() }))
        );
        assert_eq!(app.tab, Tab::Project);
        assert!(
            screen_text(&mut app).contains(" a "),
            "the title bar names it"
        );
        // `r` lists what a rescan can do; the first choice is a plain scan
        // of that project, not the portfolio.
        let Some(Request::Choose { what, .. }) = app.key("r") else {
            panic!("a rescan list");
        };
        assert_eq!(what, Choosing::Act(Act::ScanProject));
        app.offered(
            Choosing::Act(Act::ScanProject),
            Pick::Nothing,
            "rescan",
            Ok(api::Choices {
                options: vec![pma_core::catalog::Choice {
                    name: "scan".into(),
                    reads: "project".into(),
                    about: "again".into(),
                }],
                flags: vec![],
            }),
        );
        let Some(Request::Act(Action::Scan { of, .. })) = app.key("Enter") else {
            panic!("a rescan");
        };
        app.loaded(Ok(View::Project {
            name: "a".into(),
            path: "/a".into(),
            scanned_at: None,
            tags: vec![],
            verify: None,
            status: None,
            placed: vec![pma_core::rank::Placed {
                task: pma_core::rank::Task {
                    project: "a".into(),
                    tier: 1,
                    priority: pma_core::todo::Priority::High,
                    text: "text of a".into(),
                    line: Some(5),
                    key: Some("k".into()),
                    group: None,
                    due: None,
                    tagged_urgent: false,
                    signal_urgent: false,
                    age_days: 0,
                    eligible: true,
                    manual: false,
                    gh: None,
                },
                importance: 1.0,
                urgency: None,
                quadrant: pma_core::rank::Quadrant::Q1,
            }],
            today: 0,
            runs: vec![],
            jobs: vec![],
            state: std::env::temp_dir(),
            todo: None,
            repo: None,
        }));
        assert_eq!(
            app.key("Enter"),
            Some(Request::Load(Query::Task {
                project: "a".into(),
                line: Some(5),
                key: None,
            })),
            "in its project, Enter opens the task in full"
        );
        assert_eq!(of.projects, ["a"], "that project, not the portfolio");
        assert_eq!(
            app.key("Escape"),
            Some(Request::Load(Query::Inbox {
                of: api::Selection::default(),
            }))
        );
        assert_eq!(app.tab, Tab::Inbox);
    }

    /// Meaning is coloured: a critical priority red, a run waiting on you
    /// yellow; `NO_COLOR` draws both plain.
    #[test]
    fn colour_carries_meaning_unless_turned_off() {
        let mut app = colour(App::new());
        app.loaded(Ok(next_view()));
        assert_eq!(colour_of(&mut app, "critical"), Some(Color::Red));
        assert_eq!(colour_of(&mut app, "ready"), Some(Color::Yellow));
        let mut plain = App {
            palette: Palette { color: false },
            ..App::new()
        };
        plain.loaded(Ok(next_view()));
        assert_eq!(colour_of(&mut plain, "critical"), Some(Color::Reset));
    }

    /// A prompt reads text, starts from the note's own, and builds the action
    /// on Enter; Escape leaves it.
    #[test]
    fn prompts_read_text_for_the_action() {
        let mut app = App::new();
        app.key("7");
        app.loaded(Ok(View::Notes(vec![Note {
            id: 3,
            created_at: 0,
            updated_at: 0,
            text: "old".into(),
        }])));
        app.key("e");
        assert!(screen_text(&mut app).contains("old"));
        app.key("Backspace");
        app.key("!");
        assert_eq!(
            app.key("Enter"),
            Some(Request::Act(Action::EditNote {
                id: 3,
                text: "ol!".into()
            }))
        );
        app.key("n");
        app.key("x");
        assert_eq!(app.key("Escape"), None);
        assert_eq!(app.status.as_ref().unwrap().0, "Cancelled.");
        app.key("x");
        assert!(screen_text(&mut app).contains("remove note #3?"));
        assert_eq!(app.key("n"), None, "anything but y cancels");
    }

    #[test]
    fn outcomes_and_errors_show_in_the_footer() {
        let mut app = colour(App::new());
        app.loaded(Err("nothing scanned yet; run `pma scan`".into()));
        assert!(screen_text(&mut app).contains("nothing scanned yet"));
        app.acted(Ok(Outcome {
            said: vec!["job 3 queued".into()],
            warnings: vec![],
        }));
        assert_eq!(colour_of(&mut app, "job 3 queued"), Some(Color::Green));
        app.acted(Err("run #1 is now approved".into()));
        assert_eq!(colour_of(&mut app, "run #1 is now"), Some(Color::Red));
        app.key("?");
        let help = screen_text(&mut app);
        assert!(
            help.contains("add note") && help.contains("Views"),
            "{help}"
        );
        app.key("x");
        assert!(app.idle());
    }

    /// A run's diff is coloured by line: additions green, removals red.
    #[test]
    fn a_diff_is_coloured_by_line() {
        let mut app = colour(App::new());
        app.loaded(Ok(View::JobLog {
            id: 1,
            text: None,
            awaiting: false,
        }));
        let Mode::Detail { screen: s, .. } = app.mode.clone() else {
            panic!("a detail");
        };
        let Body::Detail(mut d) = s.body else {
            panic!("a detail body");
        };
        d.sections = vec![(
            "Diff".into(),
            Section::Diff,
            "@@ -1 +1 @@\n-old line\n+new line\n".into(),
        )];
        app.mode = Mode::Detail {
            screen: Screen {
                tab: None,
                header: None,
                summary: Vec::new(),
                body: Body::Detail(d),
            },
            scroll: 0,
        };
        assert_eq!(colour_of(&mut app, "+new line"), Some(Color::Green));
        assert_eq!(colour_of(&mut app, "-old line"), Some(Color::Red));
        assert_eq!(colour_of(&mut app, "@@ -1"), Some(Color::Cyan));
    }

    /// Every key the table binds on a tab does something there or says why
    /// not: none is silently dropped.
    #[test]
    fn every_bound_key_is_handled() {
        for tab in Tab::ALL {
            for b in keys::on_tab(tab) {
                let mut app = App::new();
                app.tab = tab;
                let before = (app.status.clone(), app.mode.clone(), app.tab, app.by);
                let out = app.key(b.key);
                let after = (app.status.clone(), app.mode.clone(), app.tab, app.by);
                let quiet = matches!(
                    b.command,
                    Command::Down
                        | Command::Up
                        | Command::First
                        | Command::Last
                        | Command::NextPane
                        | Command::PrevPane
                        | Command::PageDown
                        | Command::PageUp
                        | Command::Back
                        | Command::Open
                );
                assert!(
                    out.is_some() || before != after || quiet,
                    "`{}` on {tab:?} did nothing",
                    b.key
                );
            }
        }
    }

    /// A small terminal, or one made small mid-run, still draws every mode
    /// rather than panicking on a width or height that went to zero.
    #[test]
    fn every_mode_renders_in_a_small_terminal() {
        for (w, h) in [(40, 8), (12, 4), (3, 1)] {
            let mut app = App::new();
            app.loaded(Ok(next_view()));
            let modes = [
                Mode::Normal,
                Mode::Help,
                Mode::Confirm {
                    action: Action::Scan {
                        of: api::Selection::default(),
                        offline: true,
                        deps: false,
                    },
                    question: "scan every project?".into(),
                },
                Mode::Prompt {
                    act: Act::AddNote,
                    label: "note",
                    input: "x".into(),
                    pick: Pick::Nothing,
                },
                Mode::Choose {
                    what: Choosing::Act(Act::Run),
                    label: "run",
                    pick: Pick::Nothing,
                    choices: api::Choices {
                        options: vec![pma_core::catalog::Choice {
                            name: "review".into(),
                            reads: "project".into(),
                            about: "Review".into(),
                        }],
                        flags: vec![],
                    },
                    at: 0,
                    on: vec![],
                    filter: Some("rev".into()),
                },
                Mode::Check {
                    act: Act::ApproveRun,
                    label: "items done",
                    pick: Pick::Nothing,
                    items: vec!["a long item text".into(), "another".into()],
                    on: vec![true, false],
                    at: 1,
                },
            ];
            for mode in modes {
                app.mode = mode.clone();
                let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
                terminal
                    .draw(|f| app.render(f))
                    .unwrap_or_else(|e| panic!("{w}x{h} {mode:?}: {e}"));
            }
        }
    }

    /// `z` asks for the list of names; the choice and its toggled flags
    /// build the run, on the project in view.
    #[test]
    fn a_run_is_chosen_from_the_list() {
        let mut app = App::new();
        app.tab = Tab::Project;
        app.project = "spore".into();
        let Some(Request::Choose { what, label, .. }) = app.key("z") else {
            panic!("a list");
        };
        assert_eq!((what, label), (Choosing::Act(Act::Run), "run"));
        let choice = |name: &str, about: &str| pma_core::catalog::Choice {
            name: name.into(),
            reads: "project".into(),
            about: about.into(),
        };
        let choices = api::Choices {
            options: vec![
                choice("review", "Review a project"),
                choice("fix-ci", "Fix failing CI"),
            ],
            flags: vec![api::Flag {
                key: "p",
                arg: "--push",
                label: "push when clean",
            }],
        };
        app.offered(
            Choosing::Act(Act::Run),
            Pick::Nothing,
            "run",
            Ok(choices.clone()),
        );
        assert!(!app.idle());
        let out = screen_text(&mut app);
        assert!(
            out.contains("> review") && out.contains("[ ] p push when clean"),
            "{out}"
        );
        app.key("j");
        app.key("p");
        assert!(screen_text(&mut app).contains("[x] p"));
        let Some(Request::Act(Action::Run {
            name,
            projects,
            push,
            ..
        })) = app.key("Enter")
        else {
            panic!("a run");
        };
        assert_eq!(
            (name.as_str(), projects[0].as_str(), push),
            ("fix-ci", "spore", true)
        );

        // `/` filters by name; letters then type rather than move or toggle.
        app.offered(Choosing::Act(Act::Run), Pick::Nothing, "run", Ok(choices));
        for k in ["/", "F", "i", "x"] {
            app.key(k);
        }
        let out = screen_text(&mut app);
        assert!(out.contains("/ Fix") && !out.contains("review"), "{out}");
        app.key("q");
        assert!(screen_text(&mut app).contains("Nothing matches."));
        app.key("Backspace");
        let Some(Request::Act(Action::Run { name, push, .. })) = app.key("Enter") else {
            panic!("a run");
        };
        assert_eq!((name.as_str(), push), ("fix-ci", false));
        app.offered(
            Choosing::Act(Act::Run),
            Pick::Nothing,
            "run",
            Err("no store".into()),
        );
        assert!(app.status.as_ref().unwrap().0.contains("no store"));
        assert!(app.idle());
    }

    /// Inside a run's detail its acts apply: `u` on a ready run, after
    /// reading its diff, asks to approve and push it.
    #[test]
    fn acts_apply_inside_a_detail() {
        let mut app = App::new();
        app.loaded(Ok(next_view()));
        app.key("Tab");
        assert_eq!(app.key("Enter"), Some(Request::Load(Query::Run { id: 4 })));
        app.loaded(Ok(View::Run {
            run: Box::new(pma_core::store::Run {
                id: 4,
                project: "b".into(),
                text: "a run".into(),
                state: pma_core::store::RunState::Ready,
                ..Default::default()
            }),
            attempts: vec![],
            changelog: None,
            diff: "+hi".into(),
            moved: false,
            repo: None,
        }));
        let text = screen_text(&mut app);
        assert!(
            text.contains("u push") && text.contains("+hi"),
            "the footer offers the run's acts: {text}"
        );
        // Not a bundle: no rows, so a yes or no.
        let Some(Request::Check { act, pick, label }) = app.key("u") else {
            panic!("a checklist from the store");
        };
        assert_eq!(act, Act::Push);
        assert_eq!(app.checking(act, pick, label, Ok(vec![])), None);
        assert!(screen_text(&mut app).contains("approve and push #4?"));
        let Some(Request::Act(Action::Publish { ids, target, .. })) = app.key("y") else {
            panic!("a push");
        };
        assert_eq!((ids, target), (vec![4], pma_core::publish::Target::Push));

        // `m` asks for the commit's subject, which the store drafts.
        let Some(Request::Prompt { act, pick, label }) = app.key("m") else {
            panic!("a prompt from the store");
        };
        assert_eq!(act, Act::Commit);
        app.prompted(act, pick, label, Ok("a run".into()));
        assert!(screen_text(&mut app).contains("a run"));
        app.key("!");
        let Some(Request::Act(Action::Publish {
            target, subject, ..
        })) = app.key("Enter")
        else {
            panic!("a commit");
        };
        assert_eq!(target, pma_core::publish::Target::Commit);
        assert_eq!(subject.as_deref(), Some("a run!"));
    }

    /// A staged run's footer offers commit first, so a narrow terminal
    /// still shows the step that comes next.
    #[test]
    fn a_staged_run_offers_commit_first() {
        let mut app = App::new();
        app.loaded(Ok(next_view()));
        app.key("Tab");
        app.key("Enter");
        app.loaded(Ok(View::Run {
            run: Box::new(pma_core::store::Run {
                id: 4,
                project: "b".into(),
                text: "a run".into(),
                state: pma_core::store::RunState::Staged,
                ..Default::default()
            }),
            attempts: vec![],
            changelog: None,
            diff: "+hi".into(),
            moved: false,
            repo: None,
        }));
        let text = screen_text(&mut app);
        // The footer now folds, so it opens a line above the screen's last.
        assert!(
            text.lines().any(|l| l.trim_start().starts_with("m commit")),
            "{text}"
        );
    }

    /// A run reaching review is named in the footer and Enter opens it;
    /// any other key lets it go.
    #[test]
    fn a_noticed_run_opens_on_enter() {
        let mut app = App::new();
        app.loaded(Ok(next_view()));
        app.noticed(&[api::Transition {
            kind: "run",
            id: 9,
            project: "spore".into(),
            text: "fix CI".into(),
            state: "ready".into(),
        }]);
        assert!(
            screen_text(&mut app).contains("#9 spore: ready for review: fix CI  (Enter opens it)")
        );
        assert_eq!(app.key("Enter"), Some(Request::Load(Query::Run { id: 9 })));
        // A batch queues: each Enter opens the next one.
        let run = |id: i64| api::Transition {
            kind: "run",
            id,
            project: "p".into(),
            text: "t".into(),
            state: "ready".into(),
        };
        app.noticed(&[run(10), run(11)]);
        assert!(
            screen_text(&mut app).contains("#10 p: ready for review: t  (Enter opens it; 1 more)")
        );
        assert_eq!(app.key("Enter"), Some(Request::Load(Query::Run { id: 10 })));
        assert!(screen_text(&mut app).contains("#11 p: ready for review: t  (Enter opens it)"));
        assert_eq!(app.key("Enter"), Some(Request::Load(Query::Run { id: 11 })));
        app.noticed(&[api::Transition {
            kind: "job",
            id: 2,
            project: String::new(),
            text: "pma scan".into(),
            state: "done".into(),
        }]);
        app.key("j");
        assert_eq!(
            app.key("Enter"),
            Some(Request::Load(Query::task("a", "a:5"))),
            "the notice is gone; Enter opens the task"
        );
    }

    #[test]
    fn terminal_keys_take_the_table_names() {
        assert_eq!(key_name(KeyCode::Esc).as_deref(), Some("Escape"));
        assert_eq!(key_name(KeyCode::Down).as_deref(), Some("ArrowDown"));
        assert_eq!(key_name(KeyCode::Char('G')).as_deref(), Some("G"));
        assert_eq!(key_name(KeyCode::BackTab).as_deref(), Some("BackTab"));
        for b in keys::BINDINGS {
            assert!(
                b.key.chars().count() == 1
                    || [
                        "Enter",
                        "Escape",
                        "Tab",
                        "BackTab",
                        "ArrowDown",
                        "ArrowUp",
                        "Home",
                        "End",
                        "PageDown",
                        "PageUp"
                    ]
                    .contains(&b.key),
                "the TUI cannot send `{}`",
                b.key
            );
        }
    }

    /// A view of `n` tasks, `a:1` to `a:n`, each titled `task <n>`.
    fn tasks_view(n: usize) -> View {
        let mut lists = next_view_lists();
        let first = lists.agents[0].clone();
        lists.agents = (1..=n)
            .map(|i| {
                let mut row = first.clone();
                row.cells[0] = format!("a:{i}");
                row.cells[4] = format!("task {i}");
                row.subject = Subject::Task {
                    project: "a".into(),
                    target: format!("a:{i}"),
                    retry: false,
                };
                row
            })
            .collect();
        View::Next {
            header: String::new(),
            limit: None,
            lists,
            repos: Default::default(),
        }
    }

    /// `/` filters the tab's tables by a fuzzy match over each row, best
    /// first, and says how many of the rows it keeps; Enter keeps it, `*`
    /// marks what it shows, and Escape clears marks, then the filter.
    #[test]
    fn a_filter_narrows_the_rows_and_star_marks_them() {
        let mut app = App::new();
        app.loaded(Ok(tasks_view(12)));
        assert_eq!(app.key("/"), None);
        assert_eq!(app.mode, Mode::Filter);
        for c in ["t", "1", "2"] {
            app.key(c);
        }
        let shown: Vec<String> = app.panes()[0]
            .items
            .iter()
            .map(|i| i.cells[0].text.clone())
            .collect();
        // `task 12` holds `t12`; no other row has it within one cell, though
        // `a:2` has the letters in order across `T1` and `task 2`.
        assert_eq!(shown, ["a:12"]);
        assert!(screen_text(&mut app).contains("1 of 12 /t12"));
        app.key("Enter");
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(
            app.panes()[0].items.len(),
            shown.len(),
            "Enter keeps the filter"
        );
        app.key("*");
        assert_eq!(app.marks.len(), shown.len());
        app.key("*");
        assert!(app.marks.is_empty(), "again unmarks them all");
        app.key("*");
        app.key("Escape");
        assert!(app.marks.is_empty() && app.panes()[0].items.len() == shown.len());
        app.key("Escape");
        assert_eq!(app.panes()[0].items.len(), 12, "then the filter goes");
        // A refresh keeps the filter.
        app.key("/");
        for c in ["t", "1", "2"] {
            app.key(c);
        }
        app.key("Enter");
        app.loaded(Ok(tasks_view(12)));
        assert_eq!(app.panes()[0].items.len(), 1);
    }

    /// A page is what the last draw had room for.
    #[test]
    fn page_keys_move_by_what_was_drawn() {
        let mut app = App::new();
        app.loaded(Ok(tasks_view(40)));
        screen_text(&mut app);
        let page = app.rows[index(Tab::Inbox)][0] as usize;
        assert!(page > 1, "{page}");
        app.key("PageDown");
        assert_eq!(app.selected[index(Tab::Inbox)][0].selected(), Some(page));
        app.key("PageUp");
        assert_eq!(app.selected[index(Tab::Inbox)][0].selected(), Some(0));
    }

    /// A detail pages by the rows it was drawn with and `G` reaches its end;
    /// `v` on a run, in a list or in its detail, asks for the pager.
    #[test]
    fn a_detail_pages_by_its_height_and_v_asks_for_the_pager() {
        let mut app = App::new();
        app.loaded(Ok(next_view()));
        app.key("Tab");
        assert_eq!(app.key("v"), Some(Request::Pager { id: 4 }));
        assert_eq!(
            app.key("E"),
            Some(Request::Editor {
                project: "b".into()
            }),
            "E opens the run's project"
        );
        let pick = Pick::Run {
            id: 4,
            clean: false,
            fixing: false,
            empty: false,
            state: "ready".into(),
            project: "b".into(),
        };
        let diff = (0..200)
            .map(|i| format!("+line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        app.mode = Mode::Detail {
            screen: Screen {
                tab: None,
                header: None,
                summary: Vec::new(),
                body: Body::Detail(Box::new(Detail {
                    title: "#4".into(),
                    pick,
                    badge: None,
                    notice: None,
                    fields: Vec::new(),
                    sections: vec![("Diff".into(), Section::Diff, diff)],
                })),
            },
            scroll: 0,
        };
        screen_text(&mut app);
        let (total, rows) = app.extent;
        assert!(total > 200 && rows > 5, "{total} {rows}");
        let scroll = |app: &App| match &app.mode {
            Mode::Detail { scroll, .. } => *scroll,
            _ => panic!("a detail"),
        };
        app.key("PageDown");
        assert_eq!(scroll(&app), rows);
        app.key("G");
        assert_eq!(scroll(&app) as usize, total - rows as usize);
        app.key("j");
        assert_eq!(scroll(&app) as usize, total - rows as usize, "no further");
        app.key("g");
        assert_eq!(scroll(&app), 0);
        assert_eq!(
            app.key("v"),
            Some(Request::Pager { id: 4 }),
            "from the detail"
        );
        assert_eq!(
            app.key("E"),
            Some(Request::Editor {
                project: "b".into()
            }),
            "E from the detail"
        );
    }

    /// `#` lists the tags in use, and no scope first; the chosen tag narrows
    /// every view's query, shows in the title bar, and bounds a scan.
    #[test]
    fn a_scope_narrows_the_views_and_the_acts_over_them() {
        let mut app = App::new();
        app.loaded(Ok(next_view()));
        let Some(Request::Choose {
            what: Choosing::Scope,
            ..
        }) = app.key("#")
        else {
            panic!("a tag list");
        };
        let choice = |name: &str, about: &str| pma_core::catalog::Choice {
            name: name.into(),
            reads: "tag".into(),
            about: about.into(),
        };
        let tags = api::Choices {
            options: vec![choice("audio", "5 projects")],
            flags: vec![],
        };
        app.offered(Choosing::Scope, Pick::Nothing, "scope", Ok(tags.clone()));
        let Mode::Choose { choices, .. } = &app.mode else {
            panic!("a list");
        };
        assert_eq!(choices.options[0].name, NO_SCOPE, "no scope comes first");
        app.key("j");
        let Some(Request::Load(Query::Inbox { of })) = app.key("Enter") else {
            panic!("a reload within the scope");
        };
        assert_eq!(of.tags, ["audio"]);
        assert!(screen_text(&mut app).contains("#audio"));
        let Some(Request::Act(Action::Scan { of, .. })) = app.key("c") else {
            panic!("a scan");
        };
        assert_eq!(of.tags, ["audio"]);
        // Back to every project.
        app.key("#");
        app.offered(Choosing::Scope, Pick::Nothing, "scope", Ok(tags));
        let Some(Request::Load(Query::Inbox { of })) = app.key("Enter") else {
            panic!("a reload");
        };
        assert!(of.tags.is_empty() && app.scope.is_empty());
    }

    /// `O` lists every project; the chosen one opens.
    #[test]
    fn jump_opens_the_chosen_project() {
        let mut app = App::new();
        app.loaded(Ok(next_view()));
        let Some(Request::Choose {
            what: Choosing::Project,
            ..
        }) = app.key("O")
        else {
            panic!("a project list");
        };
        let choice = |name: &str, about: &str| pma_core::catalog::Choice {
            name: name.into(),
            reads: "project".into(),
            about: about.into(),
        };
        app.offered(
            Choosing::Project,
            Pick::Nothing,
            "go to project",
            Ok(api::Choices {
                options: vec![choice("alpha", "audio T1"), choice("margo", "")],
                flags: vec![],
            }),
        );
        app.key("/");
        for c in ["m", "g"] {
            app.key(c);
        }
        assert_eq!(
            app.key("Enter"),
            Some(Request::Load(Query::Project {
                name: "margo".into()
            }))
        );
        assert_eq!(app.tab, Tab::Project);
    }

    fn pane_of(rows: usize) -> Pane {
        Pane {
            title: "p".into(),
            columns: Vec::new(),
            items: (0..rows)
                .map(|_| Item {
                    cells: Vec::new(),
                    pick: Pick::Nothing,
                })
                .collect(),
            empty: "none",
        }
    }

    /// Space marks open runs too, and `X` closes them as one act.
    #[test]
    fn marked_runs_close_together() {
        let run = |id: i64, state: &str| Pick::Run {
            id,
            clean: false,
            fixing: false,
            empty: false,
            state: state.into(),
            project: "p".into(),
        };
        assert_eq!(
            screen::marked(&[run(5, "failed"), run(6, "ready")]),
            Pick::Runs { ids: vec![5, 6] }
        );
        assert!(screen::fits(Act::ForgetRun, &Pick::Runs { ids: vec![5] }));
        assert!(same_task(&run(5, "failed"), &run(5, "ready")));
        assert!(!same_task(&run(5, "failed"), &run(6, "failed")));
        assert_eq!(
            screen::action(Act::ForgetRun, &Pick::Runs { ids: vec![5, 6] }, &[], ""),
            Ok(Action::CloseRuns { ids: vec![5, 6] })
        );
        let job = Pick::Job {
            id: 17,
            awaiting: false,
        };
        let said = screen::action(Act::ForgetRun, &job, &[], "").unwrap_err();
        assert!(said.contains("needs no closing"), "{said}");
    }

    /// Spare rows go to what has rows to show, as when nothing waits on you
    /// and three tasks do.
    #[test]
    fn an_empty_pane_takes_no_spare_rows() {
        let panes = [pane_of(0), pane_of(0), pane_of(3)];
        assert_eq!(heights(&panes, 40), vec![3, 3, 34]);
        let none = [pane_of(0), pane_of(0)];
        assert_eq!(heights(&none, 40), vec![37, 3]);
    }

    /// No key is cut off, a key stays with its label, and the space bar has a
    /// name.
    #[test]
    fn the_footer_folds_between_hints() {
        let p = App::new().palette;
        let line = hints(p, &[(" ", "mark"), ("d", "dispatch"), ("c", "scan")]);
        let text = |l: &Line| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        assert!(text(&line).contains("Space mark"), "{}", text(&line));
        let lines = fold(line.clone(), 30);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(text(&lines[0]).contains("d dispatch"));
        assert!(text(&lines[1]).contains("c scan"));
        assert!(lines.iter().all(|l| text(l).chars().count() <= 30));
        assert_eq!(fold(line, 200).len(), 1);
        let message = Line::from(Span::raw("x".repeat(50)));
        assert_eq!(fold(message, 20).len(), 3);
    }
}
