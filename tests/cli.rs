use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

/// A scratch directory unique to one test, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("pma-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn project(&self, name: &str, todo: &str) -> PathBuf {
        let dir = self.0.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("TODO.md"), todo).unwrap();
        dir
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn pma(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pma"))
        .args(args)
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn clean_file_exits_zero_silently() {
    let s = Scratch::new("clean");
    let dir = s.project("a", "# TODO\n\n## High\n\n- [ ] thing\n");
    let out = pma(&["lint".as_ref(), dir.as_os_str()]);
    assert!(out.status.success());
    assert_eq!(stdout(&out), "");
    assert!(out.stderr.is_empty());
}

#[test]
fn warnings_alone_exit_zero() {
    let s = Scratch::new("warn");
    let dir = s.project("a", "# TODO\n\n## Low\n\n- [x] finished\n");
    let out = pma(&["lint".as_ref(), dir.as_os_str()]);
    assert!(out.status.success());
    let todo = dir.join("TODO.md");
    assert_eq!(
        stdout(&out),
        format!(
            "{}:5: warning: finished item; move it to `## Done`\n",
            todo.display()
        )
    );
}

#[test]
fn errors_in_any_file_exit_one_and_every_file_is_checked() {
    let s = Scratch::new("errors");
    let bad = s.project("bad", "## High\n");
    let good = s.project("good", "# TODO\n");
    let missing = s.0.join("missing");
    fs::create_dir_all(&missing).unwrap();

    let out = pma(&[
        "lint".as_ref(),
        bad.as_os_str(),
        good.as_os_str(),
        missing.as_os_str(),
    ]);
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    assert!(
        text.contains(&format!("{}:1: error:", bad.join("TODO.md").display())),
        "{text}"
    );
    assert!(
        text.contains(&format!("{}: error:", missing.join("TODO.md").display())),
        "{text}"
    );
    assert!(!text.contains(&good.display().to_string()), "{text}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "2 errors, 0 warnings\n"
    );
}
