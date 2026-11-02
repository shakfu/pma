# Refreshing without `pma scan`

Decision, 2026-11-01. After the first scan the store follows the files; `pma scan` remains for scripts and for "check everything now".

| Data | When it is re-read | Cost on 112 projects |
|-|-|-|
| `TODO.md`, git state | a stamp of 5 files changed: every 5 s with a front end open, and before `next`, `matrix`, `status`, `task` and `dispatch` | 61 ms to stat 443 files, against 3.38 s of `git status` |
| New repositories | the same check | one directory read per root |
| CI, absent projects | a full scan every `scan.every` minutes (30; 0 turns it off) while a front end is open | one `pma scan` |
| Dependencies | `pma scan --deps`, unchanged | seconds per project |

Limits:

- An unstaged edit to a tracked file does not change the stamp, so `dirty` can lag until the next commit, stage or full scan. Checking every tracked file's time is what `git status` does, at the cost above.

- `ahead` after a push can lag the same way: a push writes no file in the stamp.

- A clone an agent is running in waits until the run ends.

- A linked worktree has `.git` as a file, so only its `TODO.md` is watched.

- Without a front end open, CI goes stale. `pma scan` on a timer covers that.

A read no person asked for adds a row to `pma history` only if it found a change. The header's "last scan" is the last full scan, since a refresh reads few projects and no CI.
