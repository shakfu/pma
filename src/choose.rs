//! The task list `pma dispatch <project>` opens when no task is named.
//!
//! Every open task of the project is listed, plus the `ci` and `deps` signals
//! when the last scan found them. A task that cannot be dispatched is shown
//! with its reason rather than hidden, so a short list is never a mystery.
//! Nothing starts checked: dispatch spends money, and Enter on an untouched
//! list must do nothing.

use std::io;

use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};

pub const KEYS: &str = "space toggle  a all  j/k move  enter dispatch  q cancel";

/// One line of the list: a task of the project, as the last scan left it.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// The item's key, or `ci` or `deps`.
    pub key: String,
    pub text: String,
    /// TODO.md line; `None` for a signal.
    pub line: Option<i64>,
    pub gh: Option<i64>,
    /// `critical`, `high`, `medium`, `low`, or `signal`.
    pub priority: String,
    /// Why it cannot be chosen, when it cannot.
    pub blocked: Option<String>,
}

impl Row {
    fn place(&self) -> String {
        match self.line {
            Some(line) => line.to_string(),
            None => self.key.clone(),
        }
    }
}

pub struct App {
    project: String,
    rows: Vec<Row>,
    checked: Vec<bool>,
    state: ListState,
    /// Set once the list is finished: the chosen rows, or none on cancel.
    outcome: Option<Vec<usize>>,
}

impl App {
    pub fn new(project: String, rows: Vec<Row>) -> App {
        let checked = vec![false; rows.len()];
        let first = rows.iter().position(|r| r.blocked.is_none());
        App {
            project,
            rows,
            checked,
            state: ListState::default().with_selected(first.or(Some(0))),
            outcome: None,
        }
    }

    fn selectable(&self, i: usize) -> bool {
        self.rows.get(i).is_some_and(|r| r.blocked.is_none())
    }

    fn move_to(&mut self, next: usize) {
        self.state.select(Some(next.min(self.rows.len() - 1)));
    }

    /// Applies one key; returns true when the list is finished. `outcome`
    /// says whether it was confirmed or cancelled.
    pub fn key(&mut self, code: KeyCode) -> bool {
        let len = self.rows.len();
        let at = self.state.selected().unwrap_or(0);
        match code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Enter => {
                self.outcome = Some(
                    (0..len)
                        .filter(|i| self.checked[*i] && self.selectable(*i))
                        .collect(),
                );
                return true;
            }
            KeyCode::Char(' ') if self.selectable(at) => self.checked[at] = !self.checked[at],
            // All, or none when everything selectable is already checked.
            KeyCode::Char('a') => {
                let on = (0..len).any(|i| self.selectable(i) && !self.checked[i]);
                for i in 0..len {
                    self.checked[i] = on && self.selectable(i);
                }
            }
            KeyCode::Char('j') | KeyCode::Down if len > 0 => self.move_to(at + 1),
            KeyCode::Char('k') | KeyCode::Up if len > 0 => self.move_to(at.saturating_sub(1)),
            KeyCode::Char('g') | KeyCode::Home if len > 0 => self.move_to(0),
            KeyCode::Char('G') | KeyCode::End if len > 0 => self.move_to(len - 1),
            _ => {}
        }
        false
    }

    /// The rows the user confirmed, or `None` if the list was cancelled.
    pub fn chosen(&self) -> Option<Vec<&Row>> {
        let picked = self.outcome.as_ref()?;
        Some(picked.iter().map(|i| &self.rows[*i]).collect())
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let [top, list, detail, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(4),
            Constraint::Length(5),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        let taken = self.checked.iter().filter(|c| **c).count();
        frame.render_widget(
            Line::from(format!(
                "{}: {} open, {taken} chosen",
                self.project,
                self.rows.len()
            )),
            top,
        );

        let width = self
            .rows
            .iter()
            .map(|r| r.place().len())
            .max()
            .unwrap_or(0)
            .max(4);
        let items: Vec<ListItem> = self
            .rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let box_ = match (&r.blocked, self.checked[i]) {
                    (Some(_), _) => "[-]",
                    (None, true) => "[x]",
                    (None, false) => "[ ]",
                };
                ListItem::new(format!(
                    "{box_} {:<width$}  {:<8}  {}",
                    r.place(),
                    r.priority,
                    r.text
                ))
            })
            .collect();
        frame.render_stateful_widget(
            List::new(items)
                .block(Block::bordered().title(" tasks "))
                .highlight_symbol("> ")
                .highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
            list,
            &mut self.state,
        );

        let text = match self.state.selected().and_then(|i| self.rows.get(i)) {
            Some(r) => vec![
                Line::from(r.text.as_str()),
                Line::from(match (&r.blocked, r.gh) {
                    (Some(why), _) => why.clone(),
                    (None, Some(n)) => format!("issue #{n}"),
                    (None, None) => String::new(),
                }),
            ],
            None => vec![Line::from("no open task")],
        };
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .block(Block::bordered().title(" task ")),
            detail,
        );
        frame.render_widget(Line::from(KEYS), footer);
    }
}

/// Shows the list until it is confirmed or cancelled. `None` is a cancel.
pub fn run(app: &mut App) -> io::Result<Option<Vec<Row>>> {
    let mut terminal = ratatui::try_init()?;
    let result = (|| {
        loop {
            terminal.draw(|f| app.render(f))?;
            if let Event::Key(k) = event::read()? {
                let ctrl_c =
                    k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c');
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                if ctrl_c {
                    return Ok(None);
                }
                if app.key(k.code) {
                    return Ok(app.chosen().map(|rs| rs.into_iter().cloned().collect()));
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
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn row(line: i64, text: &str, blocked: Option<&str>) -> Row {
        Row {
            key: text.to_lowercase(),
            text: text.into(),
            line: Some(line),
            gh: None,
            priority: "high".into(),
            blocked: blocked.map(String::from),
        }
    }

    fn app() -> App {
        App::new(
            "cynn".into(),
            vec![
                row(5, "marked manual", Some("tagged #manual")),
                row(9, "support ggml 0.9", None),
                row(13, "flaky test on linux", None),
                Row {
                    key: "ci".into(),
                    text: "fix CI: build".into(),
                    line: None,
                    gh: None,
                    priority: "signal".into(),
                    blocked: None,
                },
            ],
        )
    }

    #[test]
    fn the_cursor_starts_on_the_first_task_that_can_be_chosen() {
        let app = app();
        assert_eq!(app.state.selected(), Some(1));
    }

    #[test]
    fn a_blocked_task_cannot_be_checked_by_hand_or_by_all() {
        let mut app = app();
        app.key(KeyCode::Char('g'));
        app.key(KeyCode::Char(' '));
        assert!(!app.checked[0], "space on a blocked row does nothing");
        app.key(KeyCode::Char('a'));
        assert_eq!(app.checked, [false, true, true, true]);
        app.key(KeyCode::Enter);
        assert_eq!(app.chosen().unwrap().len(), 3);
    }

    #[test]
    fn all_toggles_off_once_everything_is_checked() {
        let mut app = app();
        app.key(KeyCode::Char('a'));
        app.key(KeyCode::Char('a'));
        assert_eq!(app.checked, [false; 4]);
    }

    /// Enter on an untouched list dispatches nothing, and is not a cancel.
    #[test]
    fn confirming_nothing_is_an_empty_choice_not_a_cancel() {
        let mut app = app();
        assert!(app.key(KeyCode::Enter));
        assert_eq!(app.chosen().map(|c| c.len()), Some(0));
    }

    #[test]
    fn quitting_leaves_no_choice_at_all() {
        let mut app = app();
        app.key(KeyCode::Char(' '));
        assert!(app.key(KeyCode::Char('q')));
        assert!(app.chosen().is_none());
    }

    #[test]
    fn the_cursor_stops_at_both_ends() {
        let mut app = app();
        for _ in 0..8 {
            app.key(KeyCode::Char('j'));
        }
        assert_eq!(app.state.selected(), Some(3));
        for _ in 0..8 {
            app.key(KeyCode::Char('k'));
        }
        assert_eq!(app.state.selected(), Some(0));
    }

    fn screen(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
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

    #[test]
    fn renders_boxes_places_and_the_reason_a_task_is_blocked() {
        let mut app = app();
        app.key(KeyCode::Char(' '));
        let out = screen(&mut app);
        assert!(out.starts_with("cynn: 4 open, 1 chosen\n"), "{out}");
        assert!(out.contains("[-] 5     high      marked manual"), "{out}");
        assert!(
            out.contains("> [x] 9     high      support ggml 0.9"),
            "{out}"
        );
        assert!(out.contains("[ ] ci    signal    fix CI: build"), "{out}");
        assert!(out.ends_with(KEYS), "{out}");

        app.key(KeyCode::Char('g'));
        assert!(screen(&mut app).contains("tagged #manual"));
    }
}
