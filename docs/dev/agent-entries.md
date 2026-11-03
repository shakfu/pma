# Agent entries for `settings.toml`

Drafted 2026-11-02 from each agent's `--help` and source; none has run under pma yet. Append them to `~/.config/pma/settings.toml` once a `pma` with schema 49 is installed: an older `pma` rejects `interactive_args`, and an invalid file stops every command. The TODO items "Test `<agent>` as a pma worker" check each one.

`opencode` keeps its seeded entry and gains one line:

```toml
interactive_args = ["{dir}", "--auto", "--prompt", "{prompt}"]
```

`omp` is not installed and has no `interactive_args`.

```toml
[agents.codex]
command = "codex"
args = ["exec", "-C", "{dir}", "-s", "workspace-write", "-m", "{model}", "{extra}", "--", "{prompt}"]
interactive_args = ["-C", "{dir}", "--approve-for-me", "-m", "{model}", "{extra}", "--", "{prompt}"]
parse = "text-tail"
reports_cost = false
enforces_budget = false
sandbox = false
resumes = false

[agents.pi]
command = "pi"
args = ["-p", "--model", "{model}", "{extra}", "--", "{prompt}"]
interactive_args = ["--model", "{model}", "{extra}", "--", "{prompt}"]
parse = "text-tail"
reports_cost = false
enforces_budget = false
sandbox = false
resumes = false

[agents.gilda]
command = "gilda"
args = ["-p", "{prompt}", "--json", "-C", "{dir}", "-m", "{model}", "--permissions", "auto", "{extra}"]
interactive_args = ["-C", "{dir}", "-m", "{model}", "--permissions", "auto", "{extra}", "--", "{prompt}"]
parse = "json:text:usage.cost:error"
reports_cost = true
enforces_budget = false
sandbox = false
resumes = false

[agents.minima]
command = "minima"
args = ["--root", "{dir}", "-m", "{model}", "--json", "{extra}", "-p", "{prompt}"]
interactive_args = ["--root", "{dir}", "-m", "{model}", "{extra}", "--", "{prompt}"]
parse = "json:text:cost:error"
reports_cost = true
enforces_budget = false
sandbox = false
resumes = false

[agents.hax]
command = "hax"
args = ["--json", "--model={model}", "{extra}", "--", "{prompt}"]
interactive_args = ["--model={model}", "{extra}", "--", "{prompt}"]
parse = "json:text:cost:error"
reports_cost = true
enforces_budget = false
sandbox = false
resumes = false

[agents.myra]
command = "myra"
args = ["-C", "{dir}", "-m", "{model}", "--permissions", "auto", "{extra}", "-p", "{prompt}", "--json"]
interactive_args = ["-C", "{dir}", "-m", "{model}", "--permissions", "auto", "{extra}", "--", "{prompt}"]
parse = "json:text:cost:error"
reports_cost = true
enforces_budget = false
sandbox = false
resumes = false

[agents.hilda]
command = "hilda"
args = ["-m", "{model}", "-M", "yolo", "{extra}", "-p", "{prompt}", "--json"]
interactive_args = ["-m", "{model}", "-M", "yolo", "{extra}", "--", "{prompt}"]
parse = "json:result:usage.cost:error"
reports_cost = true
enforces_budget = false
sandbox = false
resumes = false
```
