import subprocess
from pathlib import Path

import pytest

import add_todo


def run(cwd: Path, *args: str) -> str:
    return subprocess.run(
        args, cwd=cwd, check=True, capture_output=True, text=True
    ).stdout.rstrip()


@pytest.fixture
def repo(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """A clone of a bare 'origin' with one pushed commit on main."""
    monkeypatch.setenv("GIT_CONFIG_GLOBAL", str(tmp_path / "gitconfig"))
    run(tmp_path, "git", "config", "--global", "user.name", "t")
    run(tmp_path, "git", "config", "--global", "user.email", "t@example.com")
    run(tmp_path, "git", "config", "--global", "init.defaultBranch", "main")
    run(tmp_path, "git", "init", "-q", "--bare", "origin.git")
    run(tmp_path, "git", "clone", "-q", "origin.git", "proj")
    proj = tmp_path / "proj"
    (proj / "README").write_text("x\n")
    run(proj, "git", "add", "README")
    run(proj, "git", "commit", "-qm", "init")
    run(proj, "git", "push", "-q", "-u", "origin", "main")
    run(proj, "git", "remote", "set-head", "origin", "-a")
    run(proj, "git", "remote", "set-url", "origin", str(tmp_path / "origin.git"))
    return proj


def not_fork(slug: str) -> bool:
    return False


def reason(proj: Path, is_fork=not_fork) -> str | None:
    # The local bare remote is not on GitHub; pretend it is for the checks after it.
    original = add_todo.github_slug
    add_todo.github_slug = lambda url: "owner/proj"
    try:
        return add_todo.skip_reason(proj, is_fork)
    finally:
        add_todo.github_slug = original


def test_template_passes_pma_lint(tmp_path: Path):
    (tmp_path / "TODO.md").write_text(add_todo.TEMPLATE)
    pma = Path(__file__).resolve().parents[1] / "target" / "debug" / "pma"
    if not pma.exists():
        pytest.skip("build pma first")
    out = subprocess.run([pma, "lint", tmp_path], capture_output=True, text=True)
    assert out.returncode == 0 and out.stdout == ""


def test_clean_repo_is_eligible(repo: Path):
    assert reason(repo) is None


@pytest.mark.parametrize(
    "slug, expected",
    [
        ("git@github.com:shakfu/pma.git", "shakfu/pma"),
        ("https://github.com/shakfu/pma", "shakfu/pma"),
        ("https://github.com/shakfu/pma.git/", "shakfu/pma"),
        ("git@gitlab.com:shakfu/pma.git", None),
    ],
)
def test_github_slug(slug: str, expected: str | None):
    assert add_todo.github_slug(slug) == expected


def test_skip_rules(repo: Path):
    assert reason(repo, is_fork=lambda s: True) == "owner/proj is a fork"
    assert reason(repo, is_fork=lambda s: None).startswith("could not read fork status")

    (repo / "docs").mkdir()
    (repo / "docs" / "TODO.md").write_text("# TODO\n")
    assert reason(repo) == "has docs/TODO.md"
    (repo / "docs" / "TODO.md").unlink()

    deep = repo / "a" / "b" / "c"
    deep.mkdir(parents=True)
    (deep / "TODO.md").write_text("# TODO\n")
    assert reason(repo) == "has a/b/c/TODO.md"
    (deep / "TODO.md").rename(deep / "TODO.old")
    (deep / "d").mkdir()
    (deep / "d" / "TODO.md").write_text("# TODO\n")
    assert reason(repo) is None, "deeper than NESTED_DEPTH is not searched"

    run(repo, "git", "checkout", "-qb", "feature")
    assert reason(repo) == "on feature, default is main"
    run(repo, "git", "checkout", "-q", "main")

    (repo / "f").write_text("y\n")
    run(repo, "git", "add", "f")
    run(repo, "git", "commit", "-qm", "local")
    assert reason(repo) == "1 unpushed commit(s) would be pushed too"


def test_gitignored_todo_is_skipped(repo: Path):
    (repo / ".gitignore").write_text("TODO.md\n")
    assert reason(repo) == "TODO.md is gitignored"


def test_apply_commits_only_todo_and_pushes(repo: Path):
    (repo / "README").write_text("changed\n")
    (repo / "staged").write_text("s\n")
    run(repo, "git", "add", "staged")

    assert add_todo.add_todo(repo, push=True) == "committed and pushed"

    assert (repo / "TODO.md").read_text() == add_todo.TEMPLATE
    assert run(repo, "git", "show", "--name-only", "--format=%s", "HEAD").split() == [
        "add", "TODO.md", "TODO.md",
    ]
    assert run(repo, "git", "rev-list", "--count", "@{u}..HEAD") == "0"
    status = run(repo, "git", "status", "--porcelain").splitlines()
    assert sorted(status) == [" M README", "A  staged"], "other changes untouched"


def test_failed_commit_removes_the_file(repo: Path):
    hook = repo / ".git" / "hooks" / "pre-commit"
    hook.write_text("#!/bin/sh\necho refused >&2\nexit 1\n")
    hook.chmod(0o755)

    outcome = add_todo.add_todo(repo, push=True)

    assert outcome.startswith("FAILED to commit") and "refused" in outcome
    assert not (repo / "TODO.md").exists()
    assert run(repo, "git", "status", "--porcelain") == ""


def test_dry_run_changes_nothing(repo: Path, monkeypatch: pytest.MonkeyPatch, capsys):
    monkeypatch.setattr(add_todo, "gh_is_fork", not_fork)
    monkeypatch.setattr(add_todo, "github_slug", lambda url: "owner/proj")

    assert add_todo.main(["--root", str(repo.parent), "proj"]) == 0

    assert capsys.readouterr().out.split() == ["proj", "would", "add,", "commit", "and", "push"]
    assert not (repo / "TODO.md").exists()
