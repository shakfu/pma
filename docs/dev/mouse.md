# Mouse support in the TUI

Proposal, 2026-11-28. Not implemented. Scope for mouse input in `crates/pma-tui/src/tui.rs`.

## Reference

`tzfm/rustel` (`crates/studio/src/app/mouse.rs`, `terminal.rs`) was surveyed:

- `EnableMouseCapture` for the whole session; disabled in the terminal guard's `Drop`.
- Hit-testing against `Rect`s kept from the last frame: `within(rect, x, y)`.
- Click, drag (faders, sliders, scrollbars), and wheel over unfocused panes. Right-click in one panel. No double-click.
- Shift-click is left to the terminal for native selection; Alt and Ctrl act as in-app modifiers.

Most of this serves a mixer-style UI. pma has tables, a detail view and modal dialogs, so it needs a subset.

## Scope

| Gesture | Mode | Effect | Maps to |
|-|-|-|-|
| Click tab label | Normal | switch tab | `Command::Tab` |
| Click row | Normal | focus pane, select row | `pane[t]`, `selected[t][i]` |
| Click selected row | Normal | open it | `key("Enter")` |
| Wheel over pane | Normal | move that pane's selection | `j` / `k` |
| Wheel | Detail | scroll | `ArrowDown` / `ArrowUp` |
| Wheel | Choose, Check | move `at` | `ArrowDown` / `ArrowUp` |
| Click option | Choose | set `at` | -- |
| Click option | Check | set `at`, toggle `on[at]` | -- |
| Click outside dialog | Help, Confirm, Choose, Check, Detail | close | `key("Escape")` |

Out of scope: drag, right-click, double-click timing, hover, resizing panes, clicks in the footer or a typed line.

**Acts stay on keys.** No click approves, dispatches, rejects or confirms. Opening a view is the most a click does, so a stray click cannot change state. In Confirm, a click outside cancels; no click answers `y`.

## Design

- `App::mouse(&mut self, ev: MouseEvent) -> Option<Request>`. It converts a gesture into a key name and calls `App::key`, or edits selection state directly. `App::key` stays the only path to a `Request`, so the key tests keep covering behaviour.
- `render` records hit regions in `App`, as `rows` and `extent` already are: each pane's inner `Rect` per tab, each tab label's column range in the title bar, and the open dialog's `Rect` and option rows.
- A row is `y - inner.y - 1` (less the header) plus the `TableState` offset.
- `run()` enables capture after `try_init` and disables it before `try_restore`. The pager, editor and shell hand-offs must disable it before handing over the terminal and re-enable after, or the child sees escape sequences.
- The panic hook must disable capture too.
- `Event::Mouse` is read in the same loop as `Event::Key`; `MouseEventKind::Moved` and `Drag` are ignored.

## Failure modes

- **Native selection.** Capture stops plain drag-select from copying ids, paths and log lines. Shift-drag still selects in xterm, kitty, wezterm and gnome-terminal (unverified on others). Undecided: always on, a toggle key, or a config flag.
- **herdr.** Unknown whether herdr forwards mouse reports to its panes. If it keeps them, none of this works under herdr. Test before committing to the design.
- **Stale frame.** A reload between draw and click can move rows; the click selects whatever row now holds that position. Keys have the same race. Accepted.
- **Small terminals.** `every_mode_renders_in_a_small_terminal` sizes can leave zero-height panes; a hit-test must reject empty `Rect`s.

## Tests

- Synthetic `MouseEvent`s against a `TestBackend` render, per gesture in the table.
- No click yields `Request::Act`.
- Clicks outside every region do nothing in Normal.

## Alternative

Wheel and tab clicks only: about 60 lines, no row hit-testing. Selection stays keyboard-driven. Most of the benefit is wheel scrolling; row clicks duplicate `j`/`k`/`Enter`.

## Open questions

1. Capture always on, toggled, or configured?
2. Does herdr forward mouse events?
3. Full scope or the wheel-only alternative first?
