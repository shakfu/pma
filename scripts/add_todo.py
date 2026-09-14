#!/usr/bin/env python3
"""Add an empty TODO.md to every project that lacks one, then commit and push.

Dry run by default: prints what would happen. Pass --apply to write, commit and
push. Only TODO.md is staged and committed, so other changes in a dirty working
tree are left alone.

A project is skipped, with the reason printed, when:
- it is a GitHub fork: a commit on the fork's default branch diverges it from
  upstream and leaks into later pull requests;
- a TODO.md already exists in a subdirectory (up to 3 levels);
- the checked-out branch is not origin's default branch;
- the branch has no upstream, or already has unpushed commits, which a push
  would publish along with TODO.md;
- TODO.md is gitignored.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

TEMPLATE = "# TODO\n\n## Critical\n\n## High\n\n## Medium\n\n## Low\n"
MESSAGE = "add TODO.md"
NESTED_DEPTH = 3
PRUNE = {".git", "node_modules", "build", "target", ".venv", "venv"}


def git(repo: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", "-C", str(repo), *args], capture_output=True, text=True
    )


def git_out(repo: Path, *args: str) -> str | None:
    r = git(repo, *args)
    return r.stdout.strip() if r.returncode == 0 else None


def github_slug(url: str) -> str | None:
    m = re.search(r"github\.com[:/]([^/]+/[^/]+?)(?:\.git)?/?$", url)
    return m.group(1) if m else None


def gh_is_fork(slug: str) -> bool | None:
    r = subprocess.run(
        ["gh", "api", f"repos/{slug}", "-q", ".fork"],
        capture_output=True,
        text=True,
    )
    return json.loads(r.stdout) if r.returncode == 0 else None


def has_root_todo(repo: Path) -> bool:
    # Case-insensitive, so a todo.md on a case-sensitive filesystem counts.
    return any(p.name.lower() == "todo.md" for p in repo.iterdir())


def nested_todo(repo: Path) -> Path | None:
    for dirpath, dirnames, filenames in os.walk(repo):
        depth = len(Path(dirpath).relative_to(repo).parts)
        dirnames[:] = [] if depth >= NESTED_DEPTH else [d for d in dirnames if d not in PRUNE]
        if depth and any(f.lower() == "todo.md" for f in filenames):
            return Path(dirpath).relative_to(repo) / "TODO.md"
    return None


def skip_reason(repo: Path, is_fork: Callable[[str], bool | None]) -> str | None:
    """Return why repo must be skipped, or None when it is safe to add TODO.md."""
    if nested := nested_todo(repo):
        return f"has {nested}"
    if git(repo, "check-ignore", "-q", "TODO.md").returncode == 0:
        return "TODO.md is gitignored"

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

    url = git_out(repo, "remote", "get-url", "origin") or ""
    slug = github_slug(url)
    if slug is None:
        return f"origin is not a GitHub repo: {url}"
    fork = is_fork(slug)
    if fork is None:
        return f"could not read fork status of {slug}"
    if fork:
        return f"{slug} is a fork"
    return None


def add_todo(repo: Path, push: bool) -> str:
    """Write, commit and push TODO.md. Returns a one-line outcome."""
    todo = repo / "TODO.md"
    todo.write_text(TEMPLATE)
    add = git(repo, "add", "TODO.md")
    # Pathspec after -- commits only TODO.md, even if other changes are staged.
    commit = add.returncode == 0 and git(repo, "commit", "-m", MESSAGE, "--", "TODO.md")
    if not commit or commit.returncode != 0:
        err = (commit.stderr if commit else add.stderr).strip().splitlines()
        git(repo, "reset", "-q", "--", "TODO.md")
        todo.unlink()
        return "FAILED to commit, TODO.md removed: " + (err[-1] if err else "")
    if not push:
        return "committed, not pushed"
    pushed = git(repo, "push")
    if pushed.returncode != 0:
        err = pushed.stderr.strip().splitlines()
        return "committed, push FAILED: " + (err[-1] if err else "")
    return "committed and pushed"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("projects", nargs="*", help="limit to these project directory names")
    ap.add_argument("--root", type=Path, default=Path.home() / "projects" / "personal")
    ap.add_argument("--apply", action="store_true", help="write, commit and push (default: dry run)")
    ap.add_argument("--no-push", action="store_true", help="commit but do not push")
    args = ap.parse_args(argv)

    repos = sorted(
        p for p in args.root.iterdir()
        if (p / ".git").exists() and (not args.projects or p.name in args.projects)
    )
    failed = False
    for repo in repos:
        if has_root_todo(repo):
            continue
        reason = skip_reason(repo, gh_is_fork)
        if reason:
            outcome = f"skip: {reason}"
        elif not args.apply:
            outcome = "would add, commit" + ("" if args.no_push else " and push")
        else:
            outcome = add_todo(repo, push=not args.no_push)
            failed |= "FAILED" in outcome
        print(f"{repo.name:<16} {outcome}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
