// Drives `pma web` in headless Chromium over the DevTools protocol: tabs,
// the address, one-click open, the `z` chooser, prompts, the usage log,
// console errors. Reads the store through `pma`, so it needs the fixture's
// PMA_HOME, as web-check.sh exports it.
// Expects a project `alpha` whose TODO.md has "add greeting" at line 5, as
// web-fixture.sh makes. Adds a note to the store.
//
//   node scripts/web-check.mjs <token-url>
//
// <token-url> is what `pma web --no-open` prints. $CHROME overrides the
// browser; the default is Playwright's cached headless shell.
import { spawn, execFileSync } from "node:child_process";
import { mkdtempSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

const [url] = process.argv.slice(2);
const chrome = process.env.CHROME
  || `${homedir()}/Library/Caches/ms-playwright/chromium_headless_shell-1234/chrome-headless-shell-mac-arm64/chrome-headless-shell`;
const origin = new URL(url).origin;
const profile = mkdtempSync(join(process.env.TMPDIR || tmpdir(), "cdp-"));
const proc = spawn(chrome, ["--headless", "--remote-debugging-port=9333", `--user-data-dir=${profile}`, "--window-size=1280,900", "about:blank"], { stdio: "ignore" });
const wait = (ms) => new Promise((r) => setTimeout(r, ms));

let target;
for (let i = 0; i < 50 && !target; i++) {
  try {
    const list = await (await fetch("http://127.0.0.1:9333/json/list")).json();
    target = list.find((t) => t.type === "page");
  } catch (_) { await wait(200); }
}
const ws = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((r) => ws.addEventListener("open", r));
let seq = 0;
const pending = new Map();
const errors = [];
ws.addEventListener("message", (m) => {
  const d = JSON.parse(m.data);
  if (d.id && pending.has(d.id)) { pending.get(d.id)(d); pending.delete(d.id); }
  if (d.method === "Runtime.exceptionThrown") errors.push(d.params.exceptionDetails.exception?.description || d.params.exceptionDetails.text);
  if (d.method === "Runtime.consoleAPICalled" && d.params.type === "error") errors.push(d.params.args.map((a) => a.value ?? a.description).join(" "));
});
const send = (method, params = {}) => new Promise((r) => { const id = ++seq; pending.set(id, r); ws.send(JSON.stringify({ id, method, params })); });
const js = async (expr) => {
  const r = await send("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true });
  if (r.result.exceptionDetails) return `THREW ${r.result.exceptionDetails.exception?.description}`;
  return r.result.result.value;
};
const key = async (k, text) => {
  await send("Input.dispatchKeyEvent", { type: "keyDown", key: k, text, code: k.length === 1 ? `Key${k.toUpperCase()}` : k, windowsVirtualKeyCode: k === "Enter" ? 13 : k === "ArrowDown" ? 40 : k === "Escape" ? 27 : 0 });
  await send("Input.dispatchKeyEvent", { type: "keyUp", key: k });
};
const type = async (s) => { for (const c of s) await key(c, c); };
// A failure sets the exit status, so `make web-check` fails with it.
const check = (name, ok, got) => {
  if (!ok) process.exitCode = 1;
  console.log(`${ok ? "PASS" : "FAIL"} ${name}${ok ? "" : `: ${JSON.stringify(got)}`}`);
};
const state = () => js(`({ title: document.getElementById("title").textContent, busy: document.getElementById("busy").textContent, search: location.search, rows: document.querySelectorAll("tbody tr").length, projects: [...document.querySelectorAll(".project-item")].map(b => b.textContent), dialog: document.getElementById("dialog").open, detail: !!document.querySelector(".detail") })`);

await send("Runtime.enable");
await send("Page.enable");
await send("Page.navigate", { url });
await wait(2500);

let s = await state();
check("opens on the inbox", s.title === "Inbox" && s.rows > 0, s);
check("footer leaves Connecting", s.busy === "Idle" || s.busy === "Agents working", s);
check("sidebar lists projects", s.projects.some((p) => p.includes("alpha")), s);
check("address names the tab", s.search === "?tab=inbox", s);

await js(`document.querySelector('[data-tab=matrix]').click()`);
await wait(800);
s = await state();
check("a tab click enters the address", s.search === "?tab=matrix" && s.title === "Matrix", s);
await js(`history.back()`);
await wait(800);
s = await state();
check("Back returns to the inbox", s.search === "?tab=inbox" && s.title === "Inbox", s);
await js(`history.forward()`);
await wait(800);
s = await state();
check("Forward returns to the matrix", s.search === "?tab=matrix" && s.title === "Matrix", s);

await send("Page.navigate", { url: `${origin}/?tab=project&project=alpha` });
await wait(2000);
s = await state();
check("a reload keeps the project view", s.title === "alpha" && s.search === "?tab=project&project=alpha", s);
const len = await js(`history.length`);

// One click on a task in its project opens it.
// The task's row, not a run of it: found by its pick in the page's state.
await js(`(() => {
  const panes = S.screens.project.body.panes;
  const pi = panes.findIndex((p) => p.items.some((i) => i.pick.kind === "task" && i.cells.some((c) => c.text.includes("add greeting"))));
  const ii = panes[pi].items.findIndex((i) => i.pick.kind === "task" && i.cells.some((c) => c.text.includes("add greeting")));
  document.querySelectorAll(".card")[pi].querySelectorAll("tbody tr")[ii].click();
})()`);
await wait(1200);
s = await state();
check("one click opens a task", s.detail && s.search.includes("open=task%3Aalpha%3A5"), s);
await send("Page.reload");
await wait(2000);
s = await state();
check("a reload keeps the open task", s.detail && s.title === "alpha", s);
check("a reload adds no history entry", (await js(`history.length`)) === len + 1, { len, now: await js(`history.length`) });
await key("Escape");
await wait(600);

// A checkbox marks a task; d asks to dispatch the marked ones together.
// Cancelled, so the runs the next script reads stay as they are.
const boxes = await js(`document.querySelectorAll("td.mark input").length`);
check("task rows carry a checkbox", boxes >= 2, boxes);
await js(`[...document.querySelectorAll("td.mark input")].slice(0, 2).forEach((b) => b.click())`);
await wait(300);
const marked = await js(`({
  checked: document.querySelectorAll("td.mark input:checked").length,
  button: [...document.querySelectorAll("#toolbar button")].some((b) => b.textContent.startsWith("Dispatch 2 marked")),
})`);
check("two marks show in the toolbar", marked.checked === 2 && marked.button, marked);
await js(`document.activeElement && document.activeElement.blur()`);
await key("d", "d");
await wait(800);
const asked = await js(`document.getElementById("dialog").open ? document.getElementById("dialog").textContent : ""`);
check("d asks to dispatch the marked tasks", asked.includes("2 tasks"), asked);
await key("Escape");
await wait(300);
check("cancelling keeps the marks", (await js(`S.marks.length`)) === 2, await js(`S.marks`));
await key("Escape");
await wait(300);
check("Escape clears the marks", (await js(`document.querySelectorAll("td.mark input:checked").length`)) === 0, null);

// z: the chooser filters by name and description and aligns its columns.
await js(`document.activeElement && document.activeElement.blur()`);
await key("z", "z");
await wait(1000);
s = await state();
check("z opens the chooser", s.dialog, s);
const all = await js(`document.querySelectorAll(".option").length`);
const lefts = await js(`[...new Set([...document.querySelectorAll(".option-about")].map(e => Math.round(e.getBoundingClientRect().left)))]`);
check("descriptions line up", Array.isArray(lefts) && lefts.length === 1, lefts);
check("the filter has focus", (await js(`document.activeElement.type`)) === "search", await js(`document.activeElement.outerHTML`));
const first = await js(`[...document.querySelectorAll(".option-name")].map(e => e.textContent)`);
const probe = first[first.length - 1].slice(0, 3);
await type(probe);
await wait(300);
const shown = await js(`[...document.querySelectorAll(".option:not([hidden]) .option-name")].map(e => e.textContent)`);
// Fuzzy over name and description, best first, as the TUI's chooser.
check(`typing "${probe}" filters`, shown.length >= 1 && shown.length < all && shown[0].toLowerCase().includes(probe.toLowerCase()), { all, shown });
check("the first match is chosen", (await js(`document.querySelector("input[name=choice]:checked").value`)) === shown[0], shown);
await key("Escape");
await wait(500);

// History holds the report and the scans; setup lists agents first and
// the library's panes. A tab key is a usage event.
const titles = () => js(`[...document.querySelectorAll(".card-head h2")].map(h => h.textContent)`);
await send("Page.navigate", { url: `${origin}/?tab=inbox` });
await wait(1500);
await key("4", "4");
await wait(1200);
let t = await titles();
check("4 opens history with the scans", (await state()).search === "?tab=history"
  && t[0].startsWith("Runs by") && t.includes("Changed at the last scan") && t.includes("Recent scans"), t);
await key("5", "5");
await wait(1200);
t = await titles();
check("5 opens setup, agents first", t[0] === "Agents" && t.includes("Workflow revisions") && t.includes("Campaigns"), t);
await send("Page.navigate", { url: `${origin}/?tab=report` });
await wait(1200);
check("a retired tab in the address opens the inbox", (await state()).title === "Inbox", await state());
await wait(500);
const pma = new URL("../target/debug/pma", import.meta.url).pathname;
let usage = "";
try { usage = execFileSync(pma, ["usage", "--since", "1"]).toString(); } catch (e) { usage = String(e); }
check("tab keys reach the usage log", /history\s+web\s+1/.test(usage) && /setup\s+web\s+1/.test(usage), usage);
check("the usage log names no project", !usage.includes("alpha"), usage);

// Enter in a text prompt submits it rather than cancelling. A note shows
// on the inbox once there is one, one row more.
await send("Page.navigate", { url: `${origin}/?tab=inbox` });
await wait(1500);
const before = (await state()).rows;
const add = await js(`S.keys.bindings.find(b => b.command.act === "AddNote")?.key`);
await key(add, add);
await wait(800);
await type("hello from cdp");
await key("Enter", "\r");
await wait(1500);
s = await state();
check("Enter submits a prompt", s.rows === before + 1, { before, s });

check("no console errors", errors.length === 0, errors);
ws.close();
proc.kill();
