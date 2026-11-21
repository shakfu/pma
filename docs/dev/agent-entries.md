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

## Relays under minos, 2026-11-20

Under `integration = minos`, these lines let `minosb -agent-spec` run each CLI once per turn and resume its session, so `I` and `T` work from the room. Add them to the entry above. `relay` is the absolute path of the spec in a minos checkout. The contract, and which installed binaries still lack `--resume`, are in minos `docs/one-shot-agents.md`. `{resume}` and `{text}` pass through pma for `minosb` to fill; `-p -` reads the text from stdin, which keeps it out of `ps`.

```toml
# [agents.gilda]
relay = "/path/to/minos/agents/gilda.json"
relay_args = ["{resume}", "-p", "-", "--json", "-C", "{dir}", "-m", "{model}", "--permissions", "auto", "{extra}"]

# [agents.minima]
relay = "/path/to/minos/agents/minima.json"
relay_args = ["--root", "{dir}", "-m", "{model}", "--json", "{extra}", "{resume}", "-p", "{text}"]

# [agents.hax]: text on stdin
relay = "/path/to/minos/agents/hax.json"
relay_args = ["{resume}", "--json", "--model={model}", "{extra}"]

# [agents.myra]
relay = "/path/to/minos/agents/myra.json"
relay_args = ["{resume}", "-C", "{dir}", "-m", "{model}", "--permissions", "auto", "{extra}", "-p", "{text}", "--json"]

# [agents.hilda]
relay = "/path/to/minos/agents/hilda.json"
relay_args = ["{resume}", "-m", "{model}", "-M", "yolo", "{extra}", "-p", "-", "--json"]
```

The report reads every JSON record with the entry's `parse` and sums their costs. Each of the five prints one result per process, costed for that turn only (minos-43, from each CLI's source, 2026-11-20); `claude`'s `total_cost_usd` is a session total instead, so its last record stands. `pma integrate` requires `minosb.agent-spec` once any worker's `relay` names a spec.

### ACP relays, opt-in

One-shot specs are the default relay. Use `relay = "acp"` only to record each permission request in the review, and its denials in the `--push` gate (minos `docs/acp.md`). Each CLI then runs as one Agent Client Protocol process per run, not one per turn.

```toml
# [agents.gilda]
relay = "acp"
relay_args = ["--acp", "-C", "{dir}", "-m", "{model}", "--permissions", "ask", "--context", "200000", "{extra}"]

# [agents.minima]
relay = "acp"
relay_args = ["--acp", "--root", "{dir}", "-m", "{model}", "{extra}"]

# [agents.hax]
relay = "acp"
relay_args = ["--acp", "--model={model}", "{extra}"]

# [agents.hilda]
relay = "acp"
relay_args = ["--acp", "-m", "{model}", "-M", "ask", "--context-window", "128000", "{extra}"]
```

minosb answers every permission request with `deny`. minosb's policy applies only to requests the agent sends. `hax`, `minima` and `hilda` (whose `--acp` defaults to `-M ask`) ask before any write; gilda asks only with `--permissions ask`, since its default `auto` writes in the working directory and runs shell without asking (minos-65, real model, 2026-11-21). So under ACP all four can only read; pma has no setting yet to pass `-permission allow`. gilda and hilda report cost only once they know the context window; the price list gives it for a priced model, and `--context`/`--context-window` give it otherwise. Set them to the model's window, or drop them. Each line above was checked only to start and answer `initialize` (2026-11-21); none has run a real task.
