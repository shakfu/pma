#!/usr/bin/env python3
"""Commit and push modified TODO.md files across projects in one run.

Dry run by default: prints what would happen. Pass --apply to commit and push.
Only TODO.md is committed, so other changes in a working tree are left alone.
With no project names, every repo under --root whose TODO.md is modified is
taken.

A project is skipped, with the reason printed, when:
- TODO.md is not modified;
- the checked-out branch is not origin's default branch;
- the branch has no upstream, or already has unpushed commits, which a push
  would publish along with TODO.md;
- `pma lint` reports anything, so only conforming files are committed.
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Callable

from add_todo import git, git_out

MESSAGE = "conform TODO.md to pma format"


def find_pma() -> str | None:
    """`$PMA`, then `pma` on PATH, then this checkout's release build."""
    if os.environ.get("PMA"):
        return os.environ["PMA"]
    if found := shutil.which("pma"):
        return found
    local = Path(__file__).resolve().parents[1] / "target" / "release" / "pma"
    return str(local) if local.exists() else None


def pma_lint(repo: Path) -> str | None:
    """None when TODO.md lints clean, else the first diagnostic."""
    pma = find_pma()
    if pma is None:
        return "pma not found (set PMA or put pma on PATH)"
    r = subprocess.run([pma, "lint", str(repo)], capture_output=True, text=True)
    if r.returncode == 0 and not r.stdout.strip():
        return None
    first = (r.stdout.strip() or r.stderr.strip()).splitlines()
    return first[0] if first else f"exit {r.returncode}"


def skip_reason(repo: Path, lint: Callable[[Path], str | None]) -> str | None:
    if not git_out(repo, "status", "--porcelain", "--", "TODO.md"):
        return "TODO.md unchanged"
    branch = git_out(repo, "symbolic-ref", "--short", "-q", "HEAD")
    if branch is None:
        return "detached HEAD"
    default = git_out(repo, "symbolic-ref", "--short", "-q", "refs/remotes/origin/HEAD")
    if default is None:
        return "origin's default branch is unknown (git remote set-head origin -a)"
    if default.removeprefix("origin/") != branch:
        return f"on {branch}, default is {default.removeprefix('origin/')}"
    if git_out(repo, "rev-parse", "--abbrev-ref", "@{u}") is None:
        return f"{branch} has no upstream"
    ahead = git_out(repo, "rev-list", "--count", "@{u}..HEAD")
    if ahead and ahead != "0":
        return f"{ahead} unpushed commit(s) would be pushed too"
    if problem := lint(repo):
        return f"lint: {problem}"
    return None


def commit_and_push(repo: Path, message: str, push: bool) -> str:
    # Adding first lets a new, untracked TODO.md be committed. The pathspec
    # after -- then commits only TODO.md, even if other changes are staged.
    added = git(repo, "add", "--", "TODO.md")
    if added.returncode != 0:
        err = added.stderr.strip().splitlines()
        return "add FAILED: " + (err[-1] if err else "")
    commit = git(repo, "commit", "-m", message, "--", "TODO.md")
    if commit.returncode != 0:
        err = (commit.stderr or commit.stdout).strip().splitlines()
        return "commit FAILED: " + (err[-1] if err else "")
    if not push:
        return "committed, not pushed"
    pushed = git(repo, "push")
    if pushed.returncode != 0:
        err = pushed.stderr.strip().splitlines()
        return "committed, push FAILED: " + (err[-1] if err else "")
    return "committed and pushed"


def main(argv: list[str] | None = None, lint: Callable[[Path], str | None] = pma_lint) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("projects", nargs="*", help="project directory names (default: all with a modified TODO.md)")
    ap.add_argument("--root", type=Path, default=Path.home() / "projects" / "personal")
    ap.add_argument("-m", "--message", default=MESSAGE)
    ap.add_argument("--apply", action="store_true", help="commit and push (default: dry run)")
    ap.add_argument("--no-push", action="store_true", help="commit but do not push")
    args = ap.parse_args(argv)

    if args.projects:
        repos = [args.root / p for p in args.projects]
        for r in repos:
            if not (r / ".git").exists():
                ap.error(f"{r} is not a git repository")
    else:
        repos = sorted(
            p for p in args.root.iterdir()
            if (p / ".git").exists() and git_out(p, "status", "--porcelain", "--", "TODO.md")
        )

    failed = False
    for repo in repos:
        reason = skip_reason(repo, lint)
        if reason:
            outcome = f"skip: {reason}"
            failed |= bool(args.projects)  # a named project that cannot be committed is a failure
        elif not args.apply:
            outcome = "would commit" + ("" if args.no_push else " and push")
        else:
            outcome = commit_and_push(repo, args.message, push=not args.no_push)
            failed |= "FAILED" in outcome
        print(f"{repo.name:<16} {outcome}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
