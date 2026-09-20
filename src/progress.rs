//! A one-line progress bar on stderr, for `pma scan`.
//!
//! Silent when stderr is not a terminal, so a pipe or a CI log holds the same
//! bytes it held before.

use std::io::{IsTerminal, Write};
use std::sync::Mutex;

/// Bar width in characters, inside `[]`.
const WIDTH: usize = 20;

/// The whole line, matching the 72-column limit the help is held to.
const LINE: usize = 72;

pub struct Bar {
    total: usize,
    /// The count of finished items, and the lock that serialises the writes;
    /// `None` when there is no terminal to draw on.
    done: Option<Mutex<usize>>,
}

impl Bar {
    pub fn new(total: usize) -> Bar {
        let bar = Bar {
            total,
            done: std::io::stderr().is_terminal().then(|| Mutex::new(0)),
        };
        if bar.done.is_some() {
            draw(&line(0, total, ""));
        }
        bar
    }

    /// Records one finished item and redraws. Called from the scan threads.
    pub fn done(&self, label: &str) {
        let Some(done) = &self.done else { return };
        let mut done = done.lock().unwrap();
        *done += 1;
        draw(&line(*done, self.total, label));
    }

    /// Clears the line, so what the command prints starts at column 0.
    pub fn finish(&self) {
        if self.done.is_some() {
            draw("");
        }
    }
}

/// Returns to column 0, writes, and erases whatever the last line left.
fn draw(text: &str) {
    let mut err = std::io::stderr().lock();
    let _ = write!(err, "\r{text}\x1b[K");
    let _ = err.flush();
}

/// `[####----------------] 12/95 alpha`.
fn line(done: usize, total: usize, label: &str) -> String {
    let filled = (done * WIDTH).checked_div(total).unwrap_or(WIDTH);
    let counter = format!("{done}/{total}");
    let bar = format!(
        "[{}{}] {counter}",
        "#".repeat(filled),
        "-".repeat(WIDTH - filled)
    );
    if label.is_empty() {
        return bar;
    }
    format!(
        "{bar} {}",
        crate::report::truncate(label, LINE.saturating_sub(bar.len() + 1))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_fills_with_the_count() {
        assert_eq!(line(0, 4, ""), "[--------------------] 0/4");
        assert_eq!(line(1, 4, "a"), "[#####---------------] 1/4 a");
        assert_eq!(line(4, 4, "d"), "[####################] 4/4 d");
    }

    /// Redrawing at column 0 means a line wider than the terminal wraps and
    /// leaves the earlier line behind, so the label is cut instead.
    #[test]
    fn a_long_label_is_cut_to_the_line() {
        let long = "x".repeat(200);
        assert_eq!(line(7, 95, &long).len(), LINE);
    }

    #[test]
    fn nothing_to_scan_is_a_full_bar() {
        assert_eq!(line(0, 0, ""), "[####################] 0/0");
    }
}
