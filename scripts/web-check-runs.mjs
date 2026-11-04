// Drives the run paths of `pma web` in headless Chromium: a run's detail,
// the toolbar and More, the commit prompt and its notice, reject, a
// bundle's approval checklist, the activity filter, 720 px. Needs a fresh
// web-fixture.sh store: it commits run #1, rejects run #2 and approves run
// #3, so a second pass fails.
//
//   node scripts/web-check-runs.mjs <token-url> <dir>/root/alpha
//
// $CHROME overrides the browser, as in web-check.mjs.
import { spawn, execFileSync } from "node:child_process";
import { mkdtempSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

const [url, clone] = process.argv.slice(2);
const chrome = process.env.CHROME
  || `${homedir()}/Library/Caches/ms-playwright/chromium_headless_shell-1234/chrome-headless-shell-mac-arm64/chrome-headless-shell`;
const origin = new URL(url).origin;
const profile = mkdtempSync(join(process.env.TMPDIR || tmpdir(), "cdp-"));
const proc = spawn(chrome, ["--headless", "--remote-debugging-port=9336", `--user-data-dir=${profile}`, "--window-size=1100,900", "about:blank"], { stdio: "ignore" });
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
let target;
for (let i = 0; i < 50 && !target; i++) {
  try { target = (await (await fetch("http://127.0.0.1:9336/json/list")).json()).find((t) => t.type === "page"); } catch { await wait(200); }
}
const ws = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((r) => ws.addEventListener("open", r));
let seq = 0; const pending = new Map(); const errors = [];
ws.addEventListener("message", (m) => {
  const d = JSON.parse(m.data);
  if (pending.has(d.id)) { pending.get(d.id)(d); pending.delete(d.id); }
  if (d.method === "Runtime.exceptionThrown") errors.push(d.params.exceptionDetails.exception?.description || d.params.exceptionDetails.text);
  if (d.method === "Runtime.consoleAPICalled" && d.params.type === "error") errors.push(d.params.args.map((a) => a.value ?? a.description).join(" "));
});
const send = (method, params = {}) => new Promise((r) => { const id = ++seq; pending.set(id, r); ws.send(JSON.stringify({ id, method, params })); });
const js = async (e) => {
  const r = await send("Runtime.evaluate", { expression: e, awaitPromise: true, returnByValue: true });
  return r.result.exceptionDetails ? `THREW ${r.result.exceptionDetails.exception?.description}` : r.result.result.value;
};
const key = async (k, text) => {
  await send("Input.dispatchKeyEvent", { type: "keyDown", key: k, text, windowsVirtualKeyCode: k === "Enter" ? 13 : 0 });
  await send("Input.dispatchKeyEvent", { type: "keyUp", key: k });
};
// A failure sets the exit status, so `make web-check` fails with it.
const check = (name, ok, got) => {
  if (!ok) process.exitCode = 1;
  console.log(`${ok ? "PASS" : "FAIL"} ${name}${ok ? "" : `: ${JSON.stringify(got)}`}`);
};
const toolbar = () => js(`(() => { const t = document.getElementById("toolbar");
  return { shown: [...t.children].filter(c => c.tagName !== "SPAN").map(c => c.tagName === "DETAILS" ? "More" : c.textContent.replace(/\\s+/g, " ").trim()),
    more: [...t.querySelectorAll(".more-menu button")].map(b => b.textContent.replace(/\\s+/g, " ").trim()),
    height: Math.round(t.getBoundingClientRect().height), fits: t.scrollWidth <= t.clientWidth }; })()`);
const toasts = () => js(`[...document.querySelectorAll(".toast p")].map(p => p.textContent)`);
const waitFor = async (expr, ms = 20000) => { for (let i = 0; i < ms / 250; i++) { if (await js(expr)) return true; await wait(250); } return false; };

await send("Runtime.enable");
await send("Page.navigate", { url });
await wait(2500);

// A run's detail, opened by one click.
await js(`[...document.querySelectorAll("tbody tr")].find(r => r.textContent.includes("#1")).click()`);
await waitFor(`!!document.querySelector(".detail")`);
check("one click opens run #1", (await js(`location.search`)).includes("open=run%3A1"), await js(`location.search`));
check("the detail shows the diff", await js(`!!document.querySelector(".diff") && document.querySelector(".diff").textContent.includes("hello.txt")`), null);
let tb = await toolbar();
console.log(`  toolbar: ${tb.shown.join(" | ")}; More: ${tb.more.join(" | ")}; ${tb.height}px`);
check("the toolbar is one row", tb.fits && tb.height < 50, tb);
check("Approve, Reject and Push stay out of More", ["Approve", "Reject", "Push"].every((n) => tb.shown.some((s) => s.startsWith(n))), tb);
check("Commit is under More", tb.more.some((s) => s.startsWith("Commit")), tb);
await js(`document.querySelector(".more summary").click()`);
await wait(300);
check("More opens", await js(`document.querySelector(".more").open && document.querySelector(".more-menu").getBoundingClientRect().height > 0`), null);
await js(`document.querySelector(".more").open = false`);

// The commit prompt starts from the task text; the subject typed is the commit's.
await js(`document.activeElement && document.activeElement.blur()`);
await key("m", "m");
await waitFor(`!document.getElementById("cmdline").classList.contains("hidden")`);
check("m prompts for the subject on the bottom line", (await js(`document.getElementById("cmd-input").value`)) === "add greeting", await js(`document.getElementById("cmd-input").value`));
await js(`document.getElementById("cmd-input").value = "Add the greeting"`);
await key("Enter", "\r");
const committed = await waitFor(`[...document.querySelectorAll(".toast p")].some(p => p.textContent.includes("committed in its clone, push next"))`, 30000);
check("a commit notice names the next step", committed, await toasts());
let subject = "";
try { subject = execFileSync("git", ["-C", clone, "log", "-1", "--format=%s"]).toString().trim(); } catch (e) { subject = String(e); }
check("the commit has the typed subject", subject === "Add the greeting", subject);
// The notice's Open button opens the run.
await js(`[...document.querySelectorAll(".toast")].find(t => t.textContent.includes("push next")).querySelector("button.act").click()`);
await wait(1500);
check("the notice's Open opens the run", await js(`!!document.querySelector(".detail") && document.querySelector(".detail").textContent.includes("committed")`), await js(`location.search`));
await key("Escape");
await wait(800);

// Reject #2 from its detail, and see what it did.
await send("Page.navigate", { url: `${origin}/?tab=inbox&open=run:2` });
await waitFor(`!!document.querySelector(".detail")`);
check("the address opens run #2", await js(`document.querySelector(".detail h2").textContent.includes("second task")`), null);
await js(`document.activeElement && document.activeElement.blur()`);
await key("x", "x");
await waitFor(`document.getElementById("dialog").open`);
await js(`document.getElementById("dialog-ok").click()`);
const rejected = await waitFor(`[...document.querySelectorAll(".toast p")].some(p => p.textContent.startsWith("Run #2 rejected"))`);
check("a reject says what it did", rejected, await toasts());
console.log(`  ${(await toasts()).find((t) => t.startsWith("Run #2")) || ""}`);

// Approving bundle #3 lists its items, checked; the one unchecked is left
// open, as `pma review 3` shows.
await send("Page.navigate", { url: `${origin}/?tab=inbox&open=run:3` });
await waitFor(`!!document.querySelector(".detail")`);
await js(`document.activeElement && document.activeElement.blur()`);
await key("a", "a");
await waitFor(`document.getElementById("dialog").open`);
const items = await js(`[...document.querySelectorAll(".checklist input")].map(i => i.checked)`);
check("approval lists the bundle's items, checked", JSON.stringify(items) === "[true,true]", items);
await js(`document.querySelectorAll(".checklist input")[1].click()`);
await js(`document.getElementById("dialog-ok").click()`);
await waitFor(`[...document.querySelectorAll(".toast p")].some(p => p.textContent.includes("done"))`);
const pma = new URL("../target/debug/pma", import.meta.url).pathname;
let review = "";
try { review = execFileSync(pma, ["review", "3"]).toString(); } catch (e) { review = String(e); }
check("the unchecked item is left open", review.startsWith("#3 approved") && review.includes("2. fourth task  (left open)"), review);

// Activity lists it under Rejected.
await send("Page.navigate", { url: `${origin}/?tab=activity&finished=rejected` });
await wait(1500);
const rows = await js(`[...document.querySelectorAll(".card")].find(c => c.textContent.includes("Finished runs: rejected"))?.querySelectorAll("tbody tr").length`);
check("activity lists the rejected run", rows === 1, rows);
check("the filter button shows the state", await js(`[...document.querySelectorAll("#toolbar button[aria-pressed=true]")].map(b => b.textContent)[0] === "Rejected"`), null);

// 720 px: no horizontal page scroll.
await send("Emulation.setDeviceMetricsOverride", { width: 720, height: 900, deviceScaleFactor: 1, mobile: false });
await send("Page.navigate", { url: `${origin}/?tab=inbox` });
await wait(1500);
const w = await js(`({ page: document.documentElement.scrollWidth, view: innerWidth })`);
check("720 px has no horizontal scroll", w.page <= w.view, w);

check("no console errors", errors.length === 0, errors);
ws.close(); proc.kill();
