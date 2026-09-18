# TODO

## Critical

## High

- [ ] **`lint` and `scan` see one level only**: `discover` keeps directories holding `.git` directly under a root, and `lint` maps a directory to `<dir>/TODO.md`. A nested TODO.md inside a repository is never read, so `pma lint <root>/*` exited 0 while `hax/rxa`, `pktpy/docs` and `py/source/tests/matrix` all had errors. Decide between recursion and a one-level rule stated in the help text.

## Medium

- [ ] **Render `gh:N` as a link**: `report`, `rank`, `matrix` and `tui` print a bare issue number, and the project row now stores `owner/name` to resolve it against.

- [ ] **Absent projects still rank**: `status` and `matrix` score a project no scan can find, so its tasks compete for attention when nothing can be dispatched against them. Either drop them from ranking or mark them in the listing.

- [ ] **`pma clone --tag <tag>`**: create the checkout for a project whose record exists but whose working tree does not, from its stored `owner/name`. Needs a target root when several are registered, and a rule for a directory name already taken under another one.

## Low

- [ ] **Identity by slug rather than by directory name**: `discover` skips a repository whose basename another root already supplied, so two roots cannot both hold a `py`. Worth doing when a second root exists, not before.
