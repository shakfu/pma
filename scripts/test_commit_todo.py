from pathlib import Path

import pytest

import commit_todo
from test_add_todo import repo, run  # noqa: F401  (fixture and helper)


def clean(repo: Path) -> None:
    return None


def modify_todo(repo: Path, text: str = "# TODO\n\n## High\n\n- [ ] thing\n") -> None:
    """Leave a tracked, pushed TODO.md modified in the working tree."""
    (repo / "TODO.md").write_text("# TODO\n")
    run(repo, "git", "add", "TODO.md")
    run(repo, "git", "commit", "-qm", "todo")
    run(repo, "git", "push", "-q")
    (repo / "TODO.md").write_text(text)


def test_unchanged_todo_is_skipped(repo: Path):
    assert commit_todo.skip_reason(repo, clean) == "TODO.md unchanged"


def test_skip_rules(repo: Path):
    modify_todo(repo)
    assert commit_todo.skip_reason(repo, clean) is None
    assert commit_todo.skip_reason(repo, lambda r: "TODO.md:1: warning: x") == "lint: TODO.md:1: warning: x"

    run(repo, "git", "checkout", "-qb", "feature")
    assert commit_todo.skip_reason(repo, clean) == "on feature, default is main"
    run(repo, "git", "checkout", "-q", "main")

    (repo / "other").write_text("o\n")
    run(repo, "git", "add", "other")
    run(repo, "git", "commit", "-qm", "local", "--", "other")
    assert commit_todo.skip_reason(repo, clean) == "1 unpushed commit(s) would be pushed too"


def test_a_new_untracked_todo_is_committed(repo: Path):
    (repo / "TODO.md").write_text("# TODO\n")
    assert commit_todo.skip_reason(repo, clean) is None
    assert commit_todo.commit_and_push(repo, "msg", push=True) == "committed and pushed"
    assert run(repo, "git", "status", "--porcelain") == ""


def test_commits_only_todo_and_pushes(repo: Path):
    modify_todo(repo)
    (repo / "README").write_text("changed\n")
    (repo / "staged").write_text("s\n")
    run(repo, "git", "add", "staged")

    assert commit_todo.commit_and_push(repo, "msg", push=True) == "committed and pushed"

    assert run(repo, "git", "show", "--name-only", "--format=%s", "HEAD").split() == ["msg", "TODO.md"]
    assert run(repo, "git", "rev-list", "--count", "@{u}..HEAD") == "0"
    assert sorted(run(repo, "git", "status", "--porcelain").splitlines()) == [" M README", "A  staged"]


def test_main_dry_run_then_apply(repo: Path, capsys):
    modify_todo(repo)
    root = ["--root", str(repo.parent)]

    assert commit_todo.main(root, lint=clean) == 0
    assert capsys.readouterr().out.split() == ["proj", "would", "commit", "and", "push"]
    assert run(repo, "git", "status", "--porcelain") == " M TODO.md", "a dry run changes nothing"

    assert commit_todo.main(root + ["--apply"], lint=clean) == 0
    assert capsys.readouterr().out.split() == ["proj", "committed", "and", "pushed"]
    assert commit_todo.main(root, lint=clean) == 0
    assert capsys.readouterr().out == "", "nothing left to commit"


def test_named_project_that_cannot_be_committed_fails(repo: Path, capsys):
    assert commit_todo.main(["--root", str(repo.parent), "proj"], lint=clean) == 1
    assert "skip: TODO.md unchanged" in capsys.readouterr().out
