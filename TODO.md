# TODO

## Critical

## High

- [ ] `pma sync` checks an item's text before writing `gh:N` into its line #agent
  `link` in `src/sync.rs` checks only that the planned line still holds an unlinked item. If `TODO.md` gains a line above it while `gh issue create` runs, `gh:N` lands on the wrong item, and the next sync retitles that issue and opens a second one for the real item.
  Compare the item's `todo::normal_text` with the planned title, and refuse the write on a mismatch, as a changed file is refused now. Add a unit test that edits the file between plan and link.

- [ ] `pma lint` reports an unclosed code fence #agent
  `src/todo.rs` toggles fence state on any line starting with three backticks or tildes. A four-backtick fence closes on three, a tilde block closes on backticks, and a fence never closed hides every later item from the parser while lint exits 0.
  Close a fence only on a line of the same character at least as long as the opening one, as CommonMark does, and report a fence still open at end of file as an error at its opening line. Unit tests for each case.

- [ ] **`lint` and `scan` see one level only**: `discover` keeps directories holding `.git` directly under a root, and `lint` maps a directory to `<dir>/TODO.md`. A nested TODO.md inside a repository is never read, so `pma lint <root>/*` exited 0 while `hax/rxa`, `pktpy/docs` and `py/source/tests/matrix` all had errors. Decide between recursion and a one-level rule stated in the help text.

## Medium

- [ ] `pma scan` bounds each git and gh call with a timeout #agent
  `scan_project` in `src/scan.rs` runs `git` and `gh` through `Command::output()` with no deadline, so one hung `gh run list` hangs the whole scan. `src/deps.rs` already bounds its tools with `agent::run_limited`.
  Give scan's calls a deadline, record a timed-out call in the project's detail rather than failing the scan, and test it with a fake `gh` that sleeps.

- [x] A `##Critical` heading without a space is a lint error #agent

- [ ] A project whose name contains a dot can take `projects.<name>.verify` and `projects.<name>.publish` #agent

- [ ] `pma review 3 4 --approve --minutes 5` drops the minutes #agent

- [ ] `pma config <retired key>` says the key is unknown instead of explaining the retirement #agent

- [ ] `pma project import` refuses a CSV saved with a byte-order mark #agent

- [ ] A pull request closed without merging does not count as an attempt on its task #agent

- [ ] `pma dispatch --retry` resets the attempt count before it knows the run started #agent

- [ ] **Render `gh:N` as a link**: `report`, `rank`, `matrix` and `tui` print a bare issue number, and the project row now stores `owner/name` to resolve it against.

- [ ] **Absent projects still rank**: `status` and `matrix` score a project no scan can find, so its tasks compete for attention when nothing can be dispatched against them. Either drop them from ranking or mark them in the listing.

- [ ] **`pma clone --tag <tag>`**: create the checkout for a project whose record exists but whose working tree does not, from its stored `owner/name`. Needs a target root when several are registered, and a rule for a directory name already taken under another one.

## Low

- [ ] **Identity by slug rather than by directory name**: `discover` skips a repository whose basename another root already supplied, so two roots cannot both hold a `py`. Worth doing when a second root exists, not before.
