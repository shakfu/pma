//! The project a command is typed in, and what its arguments name there.
//! Design: `docs/dev/cli-context.md`.

use std::path::{Path, PathBuf};

use crate::store::{ProjectRow, Result, Store};

/// Names the context project, over the cwd.
pub const VAR: &str = "PMA_PROJECT";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    pub project: String,
    pub path: PathBuf,
    /// `cwd` or `PMA_PROJECT`: how the project was found, for help to say.
    pub source: &'static str,
}

/// `$PMA_PROJECT`, else the project whose directory holds the cwd.
pub fn detect(store: &Store) -> Result<Option<Context>> {
    let rows = store.projects()?;
    if let Ok(name) = std::env::var(VAR)
        && !name.is_empty()
    {
        let row = rows
            .iter()
            .find(|r| r.name == name)
            .ok_or_else(|| format!("{VAR} names no project `{name}`"))?;
        return Ok(Some(Context {
            project: row.name.clone(),
            path: row.path.clone(),
            source: VAR,
        }));
    }
    let cwd = std::env::current_dir()?;
    Ok(within(&rows, &cwd).map(|r| Context {
        project: r.name.clone(),
        path: r.path.clone(),
        source: "cwd",
    }))
}

/// The present project whose directory is `dir` or holds it; the deepest,
/// for a project inside another.
pub fn within<'a>(rows: &'a [ProjectRow], dir: &Path) -> Option<&'a ProjectRow> {
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    rows.iter()
        .filter(|r| r.absent_since.is_none())
        .filter(|r| {
            let p = r.path.canonicalize().unwrap_or_else(|_| r.path.clone());
            dir.starts_with(&p)
        })
        .max_by_key(|r| r.path.components().count())
}

/// A project or target argument as the context reads it: `.` is the
/// project, `:12` its line 12. Anything else is as typed.
pub fn reference(arg: &str, ctx: Option<&Context>) -> std::result::Result<String, String> {
    let needs = || format!("`{arg}` needs a project: run inside one, or set {VAR}");
    match arg {
        "." => ctx.map(|c| c.project.clone()).ok_or_else(needs),
        a if a.starts_with(':') && a.len() > 1 => {
            ctx.map(|c| format!("{}{a}", c.project)).ok_or_else(needs)
        }
        a => Ok(a.to_string()),
    }
}

/// `names` with each reference read, or the context project when none is
/// named and nothing else selects: no tag and no `-A`.
pub fn scope(
    names: &[String],
    tagged: bool,
    all_projects: bool,
    ctx: Option<&Context>,
) -> std::result::Result<Vec<String>, String> {
    let read: Vec<String> = names
        .iter()
        .map(|n| reference(n, ctx))
        .collect::<std::result::Result<_, _>>()?;
    Ok(match (read.is_empty() && !tagged && !all_projects, ctx) {
        (true, Some(c)) => vec![c.project.clone()],
        _ => read,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context {
            project: "argdec".into(),
            path: "/p/argdec".into(),
            source: "cwd",
        }
    }

    #[test]
    fn references_read_against_the_context() {
        let c = ctx();
        assert_eq!(reference(".", Some(&c)), Ok("argdec".into()));
        assert_eq!(reference(":12", Some(&c)), Ok("argdec:12".into()));
        assert_eq!(reference("spore:3", Some(&c)), Ok("spore:3".into()));
        assert!(reference(".", None).unwrap_err().contains(VAR));
        assert!(reference(":ci", None).is_err());
        assert_eq!(reference(":", None), Ok(":".into()));
    }

    #[test]
    fn a_scope_falls_back_to_the_context_unless_something_selects() {
        let c = ctx();
        let s = |names: &[&str], tagged, all, ctx| {
            let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
            scope(&names, tagged, all, ctx)
        };
        assert_eq!(s(&[], false, false, Some(&c)), Ok(vec!["argdec".into()]));
        assert_eq!(
            s(&["spore"], false, false, Some(&c)),
            Ok(vec!["spore".into()])
        );
        assert_eq!(s(&[], true, false, Some(&c)), Ok(vec![]), "a tag overrides");
        assert_eq!(s(&[], false, true, Some(&c)), Ok(vec![]), "-A ignores it");
        assert_eq!(s(&[], false, false, None), Ok(vec![]));
        assert_eq!(
            s(&[".", "spore"], false, false, Some(&c)),
            Ok(vec!["argdec".into(), "spore".into()])
        );
    }
}
