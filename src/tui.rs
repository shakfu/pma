//! `pma tui`: `pma next` as two lists, for agents and for you, or the matrix
//! as a 2x2 layout; `m` switches. The selected row's details sit below.
//!
//! Focus and selection are shown by border weight, a `> ` marker and reverse
//! video, not by colour, so the view reads the same without colour.

use std::io;

use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, List, ListItem, ListState, Paragraph, Wrap};

use crate::next::{Next, Row};
use crate::rank::{Placed, Quadrant};
use crate::report;

pub const KEYS: &str = "q quit  m next/matrix  tab/h/l switch  j/k move  g/G first/last";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Next,
    Matrix,
}

pub struct App {
    header: String,
    today: i64,
    view: View,
    /// `pma next`'s rows: for agents, then for you (runs, then tasks).
    lists: [Vec<Row>; 2],
    list_states: [ListState; 2],
    pane: usize,
    /// Tasks per quadrant, in matrix order.
    quadrants: [Vec<Placed>; 4],
    states: [ListState; 4],
    focus: usize,
}

impl App {
    pub fn new(header: String, placed: Vec<Placed>, today: i64, next: Next) -> App {
        let yours: Vec<Row> = next.runs.into_iter().chain(next.tasks).collect();
        let lists = [next.agents, yours];
        let list_states = std::array::from_fn(|i| {
            ListState::default().with_selected((!lists[i].is_empty()).then_some(0))
        });
        let pane = lists.iter().position(|l| !l.is_empty()).unwrap_or(0);
        let mut quadrants: [Vec<Placed>; 4] = Default::default();
        for p in placed {
            quadrants[p.quadrant as usize].push(p);
        }
        let states = std::array::from_fn(|i| {
            ListState::default().with_selected((!quadrants[i].is_empty()).then_some(0))
        });
        let focus = quadrants.iter().position(|q| !q.is_empty()).unwrap_or(0);
        App {
            header,
            today,
            view: View::Next,
            lists,
            list_states,
            pane,
            quadrants,
            states,
            focus,
        }
    }

    /// The row selected in the `next` view.
    fn selected_row(&self) -> Option<&Row> {
        self.lists[self.pane].get(self.list_states[self.pane].selected()?)
    }

    /// Applies one key; returns true when the app should quit.
    pub fn key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Char('m') => {
                self.view = match self.view {
                    View::Next => View::Matrix,
                    View::Matrix => View::Next,
                };
                return false;
            }
            _ => {}
        }
        match self.view {
            View::Matrix => self.matrix_key(code),
            View::Next => self.next_key(code),
        }
        false
    }

    fn next_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Tab | KeyCode::BackTab => self.pane = 1 - self.pane,
            KeyCode::Char('h') | KeyCode::Left | KeyCode::Char('1') => self.pane = 0,
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Char('2') => self.pane = 1,
            other => move_in(
                &mut self.list_states[self.pane],
                self.lists[self.pane].len(),
                other,
            ),
        }
    }

    pub fn selected(&self) -> Option<&Placed> {
        self.quadrants[self.focus].get(self.states[self.focus].selected()?)
    }

    fn matrix_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Tab => self.focus = (self.focus + 1) % 4,
            KeyCode::BackTab => self.focus = (self.focus + 3) % 4,
            // Columns: Q1 and Q3 are urgent, Q2 and Q4 are not.
            KeyCode::Char('l') | KeyCode::Right => self.focus |= 1,
            KeyCode::Char('h') | KeyCode::Left => self.focus &= !1,
            KeyCode::Char(c @ '1'..='4') => self.focus = c as usize - '1' as usize,
            other => move_in(
                &mut self.states[self.focus],
                self.quadrants[self.focus].len(),
                other,
            ),
        }
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let [top, grid, detail, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(6),
            Constraint::Length(5),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        frame.render_widget(Line::from(self.header.as_str()), top);
        match self.view {
            View::Next => self.render_next(frame, grid, detail),
            View::Matrix => self.render_matrix(frame, grid, detail),
        }
        frame.render_widget(Line::from(KEYS), footer);
    }

    fn render_next(&mut self, frame: &mut Frame, grid: Rect, detail: Rect) {
        let areas: [Rect; 2] =
            Layout::vertical([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)]).areas(grid);
        for (i, area) in areas.into_iter().enumerate() {
            let focused = i == self.pane;
            let rows = &self.lists[i];
            let items: Vec<ListItem> = rows
                .iter()
                .map(|r| ListItem::new(r.cells.join("  ")))
                .collect();
            let title = match i {
                0 => format!(" for agents ({}) ", rows.len()),
                _ => format!(" for you ({}) ", rows.len()),
            };
            frame.render_stateful_widget(
                pane(items, title, focused),
                area,
                &mut self.list_states[i],
            );
        }
        let text = match self.selected_row() {
            Some(r) => vec![
                Line::from(r.cells.join("  ")),
                Line::from(r.command.clone().unwrap_or_else(|| "for you to do".into())),
            ],
            None => vec![Line::from("nothing selected")],
        };
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .block(Block::bordered().title(" next ")),
            detail,
        );
    }

    fn render_matrix(&mut self, frame: &mut Frame, grid: Rect, detail: Rect) {
        let [upper, lower] =
            Layout::vertical([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)]).areas(grid);
        let halves = |r| {
            Layout::horizontal([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)]).areas::<2>(r)
        };
        let [q1, q2] = halves(upper);
        let [q3, q4] = halves(lower);

        for (i, area) in [q1, q2, q3, q4].into_iter().enumerate() {
            let q = Quadrant::ALL[i];
            let focused = i == self.focus;
            let tasks = &self.quadrants[i];
            let items: Vec<ListItem> = tasks
                .iter()
                .map(|p| {
                    let place = match p.task.line {
                        Some(line) => format!("{}:{line}", p.task.project),
                        None => p.task.project.clone(),
                    };
                    ListItem::new(format!(
                        "{place}  {}  {}",
                        report::when(p, self.today),
                        p.task.text
                    ))
                })
                .collect();
            let title = format!(" {q:?} {} ({}) ", q.title(), tasks.len());
            frame.render_stateful_widget(pane(items, title, focused), area, &mut self.states[i]);
        }

        let text = match self.selected() {
            Some(p) => {
                let t = &p.task;
                let target = match (&t.key, t.line) {
                    (Some(_), Some(line)) => format!("pma dispatch {}:{line}", t.project),
                    (Some(key), None) => format!("pma dispatch {}:{key}", t.project),
                    (None, _) => "not dispatchable".into(),
                };
                vec![
                    Line::from(format!(
                        "{}  T{}  {}  importance {:.2}  {}{}",
                        t.project,
                        t.tier,
                        t.priority.name(),
                        p.importance,
                        report::when(p, self.today),
                        t.group
                            .as_deref()
                            .map_or(String::new(), |g| format!("  group: {g}"))
                    )),
                    Line::from(t.text.as_str()),
                    Line::from(target),
                ]
            }
            None => vec![Line::from("no task selected")],
        };
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .block(Block::bordered().title(" task ")),
            detail,
        );
    }
}

/// A list with a title, its border thick and its selection reversed when it
/// has the focus, so neither depends on colour.
fn pane(items: Vec<ListItem<'_>>, title: String, focused: bool) -> List<'_> {
    let block = Block::bordered()
        .border_type(if focused {
            BorderType::Thick
        } else {
            BorderType::Plain
        })
        .title(title);
    List::new(items)
        .block(block)
        .highlight_symbol("> ")
        .highlight_style(if focused {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new()
        })
}

/// Moves a list's selection for `j`, `k`, `g` and `G` and their arrow keys.
fn move_in(state: &mut ListState, len: usize, code: KeyCode) {
    if len == 0 {
        return;
    }
    match code {
        KeyCode::Char('j') | KeyCode::Down => {
            state.select(Some(state.selected().map_or(0, |i| (i + 1).min(len - 1))));
        }
        KeyCode::Char('k') | KeyCode::Up => {
            state.select(Some(state.selected().map_or(0, |i| i.saturating_sub(1))));
        }
        KeyCode::Char('g') | KeyCode::Home => state.select(Some(0)),
        KeyCode::Char('G') | KeyCode::End => state.select(Some(len - 1)),
        _ => {}
    }
}

pub fn run(mut app: App) -> io::Result<()> {
    let mut terminal = ratatui::try_init()?;
    let result = (|| {
        loop {
            terminal.draw(|f| app.render(f))?;
            if let Event::Key(k) = event::read()? {
                let ctrl_c =
                    k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c');
                if k.kind == KeyEventKind::Press && (ctrl_c || app.key(k.code)) {
                    return Ok(());
                }
            }
        }
    })();
    ratatui::try_restore()?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rank::Task;
    use crate::todo::Priority;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn placed(project: &str, line: Option<i64>, key: Option<&str>, q: Quadrant) -> Placed {
        Placed {
            task: Task {
                project: project.into(),
                tier: 1,
                priority: Priority::High,
                text: format!("text of {project}"),
                line,
                key: key.map(String::from),
                group: None,
                due: None,
                tagged_urgent: false,
                signal_urgent: false,
                eligible: false,
                age_days: 3,
            },
            importance: 0.5,
            urgency: None,
            quadrant: q,
        }
    }

    fn row(cells: &[&str], command: Option<&str>) -> Row {
        Row {
            cells: cells.iter().map(|c| c.to_string()).collect(),
            command: command.map(String::from),
        }
    }

    fn lists() -> Next {
        Next {
            agents: vec![row(
                &["a:5", "T1", "high", "open 3d", "text of a"],
                Some("pma dispatch a:5"),
            )],
            runs: vec![row(
                &["#4", "ready", "to review", "b", "a run"],
                Some("pma review 4"),
            )],
            tasks: vec![row(
                &[
                    "c:2",
                    "T1",
                    "critical",
                    "open 9d",
                    "not #agent",
                    "urgent thing",
                ],
                None,
            )],
        }
    }

    /// The app on its matrix view, which the first tests drive.
    fn app() -> App {
        let mut app = App::new(
            "last scan just now".into(),
            vec![
                placed("a", Some(5), Some("a"), Quadrant::Q2),
                placed("b", Some(9), Some("b"), Quadrant::Q2),
                placed("c", None, Some("ci"), Quadrant::Q3),
            ],
            100,
            lists(),
        );
        app.key(KeyCode::Char('m'));
        app
    }

    #[test]
    fn focus_starts_on_the_first_non_empty_quadrant_and_moves() {
        let mut app = app();
        assert_eq!(app.selected().unwrap().task.project, "a");
        app.key(KeyCode::Char('j'));
        app.key(KeyCode::Char('j'));
        assert_eq!(
            app.selected().unwrap().task.project,
            "b",
            "stops at the last"
        );
        app.key(KeyCode::Char('g'));
        assert_eq!(app.selected().unwrap().task.project, "a");
        app.key(KeyCode::Char('h'));
        assert_eq!(app.focus, 0);
        assert!(app.selected().is_none(), "Q1 is empty");
        app.key(KeyCode::Char('j'));
        app.key(KeyCode::BackTab);
        assert_eq!(app.focus, 3);
        app.key(KeyCode::Char('3'));
        assert_eq!(app.selected().unwrap().task.project, "c");
        app.key(KeyCode::Char('l'));
        assert_eq!(app.focus, 3);
        assert!(!app.key(KeyCode::Char('x')));
        assert!(app.key(KeyCode::Char('q')));
    }

    fn screen(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
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

    /// The app opens on `pma next`: agents' tasks on top, yours below, runs
    /// before tasks, and the selected row's command in the details.
    #[test]
    fn opens_on_next_and_switches_to_the_matrix() {
        let mut app = App::new("last scan just now".into(), Vec::new(), 100, lists());
        let out = screen(&mut app);
        assert!(
            out.contains(" for agents (1) ") && out.contains(" for you (2) "),
            "{out}"
        );
        assert!(out.contains("> a:5  T1  high  open 3d  text of a"), "{out}");
        assert!(out.contains("pma dispatch a:5"), "{out}");
        app.key(KeyCode::Tab);
        let out = screen(&mut app);
        assert!(out.contains("pma review 4"), "{out}");
        app.key(KeyCode::Char('j'));
        let out = screen(&mut app);
        assert!(
            out.contains("for you to do"),
            "a task with no command: {out}"
        );
        app.key(KeyCode::Char('m'));
        assert!(screen(&mut app).contains(" Q1 Do right away (0) "));
        assert!(app.key(KeyCode::Char('q')));
    }

    #[test]
    fn renders_quadrants_selection_and_details() {
        let mut app = app();
        let out = screen(&mut app);
        assert!(out.starts_with("last scan just now\n"), "{out}");
        assert!(out.contains(" Q1 Do right away (0) "), "{out}");
        assert!(out.contains(" Q2 Schedule for later (2) "), "{out}");
        assert!(out.contains("> a:5  open 3d  text of a"), "{out}");
        assert!(out.contains("pma dispatch a:5"), "{out}");
        assert!(out.ends_with(KEYS), "{out}");

        app.key(KeyCode::Char('3'));
        let out = screen(&mut app);
        assert!(out.contains("pma dispatch c:ci"), "{out}");
        assert!(out.contains("┏"), "the focused quadrant has a thick border");
    }
}
