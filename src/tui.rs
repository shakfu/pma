//! `pma tui`: the matrix as a 2x2 layout, with the selected task's details.
//!
//! Focus and selection are shown by border weight, a `> ` marker and reverse
//! video, not by colour, so the view reads the same without colour.

use std::io;

use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, List, ListItem, ListState, Paragraph, Wrap};

use crate::rank::{Placed, Quadrant};
use crate::report;

pub const KEYS: &str = "q quit  tab/h/l quadrant  j/k move  g/G first/last  1-4 jump";

pub struct App {
    header: String,
    today: i64,
    /// Tasks per quadrant, in matrix order.
    quadrants: [Vec<Placed>; 4],
    states: [ListState; 4],
    focus: usize,
}

impl App {
    pub fn new(header: String, placed: Vec<Placed>, today: i64) -> App {
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
            quadrants,
            states,
            focus,
        }
    }

    pub fn selected(&self) -> Option<&Placed> {
        self.quadrants[self.focus].get(self.states[self.focus].selected()?)
    }

    /// Applies one key; returns true when the app should quit.
    pub fn key(&mut self, code: KeyCode) -> bool {
        let len = self.quadrants[self.focus].len();
        let state = &mut self.states[self.focus];
        match code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Tab => self.focus = (self.focus + 1) % 4,
            KeyCode::BackTab => self.focus = (self.focus + 3) % 4,
            // Columns: Q1 and Q3 are urgent, Q2 and Q4 are not.
            KeyCode::Char('l') | KeyCode::Right => self.focus |= 1,
            KeyCode::Char('h') | KeyCode::Left => self.focus &= !1,
            KeyCode::Char(c @ '1'..='4') => self.focus = c as usize - '1' as usize,
            KeyCode::Char('j') | KeyCode::Down if len > 0 => {
                state.select(Some(state.selected().map_or(0, |i| (i + 1).min(len - 1))));
            }
            KeyCode::Char('k') | KeyCode::Up if len > 0 => {
                state.select(Some(state.selected().map_or(0, |i| i.saturating_sub(1))));
            }
            KeyCode::Char('g') | KeyCode::Home if len > 0 => state.select(Some(0)),
            KeyCode::Char('G') | KeyCode::End if len > 0 => state.select(Some(len - 1)),
            _ => {}
        }
        false
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
            let block = Block::bordered()
                .border_type(if focused {
                    BorderType::Thick
                } else {
                    BorderType::Plain
                })
                .title(format!(" {q:?} {} ({}) ", q.title(), tasks.len()));
            let list = List::new(items)
                .block(block)
                .highlight_symbol("> ")
                .highlight_style(if focused {
                    Style::new().add_modifier(Modifier::REVERSED)
                } else {
                    Style::new()
                });
            frame.render_stateful_widget(list, area, &mut self.states[i]);
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
        frame.render_widget(Line::from(KEYS), footer);
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
                age_days: 3,
            },
            importance: 0.5,
            urgency: None,
            quadrant: q,
        }
    }

    fn app() -> App {
        App::new(
            "last scan just now".into(),
            vec![
                placed("a", Some(5), Some("a"), Quadrant::Q2),
                placed("b", Some(9), Some("b"), Quadrant::Q2),
                placed("c", None, Some("ci"), Quadrant::Q3),
            ],
            100,
        )
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
