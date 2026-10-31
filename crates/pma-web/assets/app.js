// pma web. Draws the screens the server sends: tables with typed columns,
// the selected row's actions in the toolbar, a run or a job in place of the
// tables when opened, toasts for outcomes and for what changed. What a
// row is, what a key does to it and what a state means come from the server
// (pma_core::screen and ::keys), as they do for the TUI. Text from the store
// is only ever set as textContent, never parsed as HTML.
"use strict";

const TABS = {
  inbox: { label: "Inbox", about: "What waits on you, what is under way, and what agents can take." },
  projects: { label: "Projects", about: "Every project, worst first, by what needs attention." },
  activity: { label: "Activity", about: "Jobs, running agents and workflow instances, newest first." },
  matrix: { label: "Matrix", about: "Every open task, by urgency and importance." },
  library: { label: "Library", about: "Workflow revisions and campaigns: what can be run." },
  report: { label: "Report", about: "What dispatching produced." },
  notes: { label: "Notes", about: "Notes that belong to no one project." },
  settings: { label: "Settings", about: "Settings, roots, agents, presets and route revisions." },
  project: { label: "Project", about: "" },
};

const S = {
  keys: null,
  tab: "inbox",
  by: 1,
  finished: 0, // the place in keys.finished the activity tab filters to
  screens: {},
  pane: {},
  sel: {},
  detail: null, // an open run or job: { screen, pick, acts }, over the tab
  project: "", // the project the project view shows
  previous: "inbox", // the view it was opened from
  projects: [], // every project, worst first: { name, tier, state, tone }
  filter: "",
  restoring: false, // the view is being set from the address
  marks: [], // task picks marked on this view, which dispatch takes together
};

const $ = (id) => document.getElementById(id);

function el(tag, attrs, ...children) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === null || v === undefined || v === false) continue;
    if (k === "class") e.className = v;
    else if (k.startsWith("on")) e.addEventListener(k.slice(2), v);
    else e.setAttribute(k, v === true ? "" : v);
  }
  for (const c of children.flat()) {
    if (c === null || c === undefined) continue;
    e.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return e;
}

// Inline markdown an agent writes: **bold**, which may hold `code`, and
// `code`. Built as nodes.
function codeSpans(text) {
  const out = [];
  let last = 0;
  for (const m of text.matchAll(/`([^`]+)`/g)) {
    if (m.index > last) out.push(text.slice(last, m.index));
    out.push(el("code", {}, m[1]));
    last = m.index + m[0].length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

function inline(text) {
  const out = [];
  let last = 0;
  for (const m of text.matchAll(/\*\*(.+?)\*\*/g)) {
    if (m.index > last) out.push(...codeSpans(text.slice(last, m.index)));
    out.push(el("strong", {}, ...codeSpans(m[1])));
    last = m.index + m[0].length;
  }
  if (last < text.length) out.push(...codeSpans(text.slice(last)));
  return out;
}

async function api(path, body) {
  const opt = body === undefined
    ? {}
    : { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) };
  const r = await fetch(path, opt);
  let data = null;
  try { data = await r.json(); } catch (_) { /* a status with no JSON */ }
  if (!r.ok) throw new Error((data && data.error) || `${r.status} ${r.statusText}`);
  return data;
}

// The binding `key` has on the current tab for the row in hand: one for
// that tab alone wins over one for every tab; among those, an act that fits
// the row, then a command that is not an act, then the first. As
// pma_core::keys::lookup.
function lookup(key) {
  const fit = S.keys.bindings.filter((b) => b.key === key);
  const onTab = fit.filter((b) => b.tabs.includes(S.tab));
  const cands = onTab.length ? onTab : fit.filter((b) => b.tabs.length === 0);
  const fits = S.marks.length && !S.detail ? currentActs().concat(MARK_ACTS) : currentActs();
  return cands.find((b) => b.command.kind === "act" && fits.includes(b.command.act))
    || cands.find((b) => b.command.kind !== "act")
    || cands[0];
}

function onTab() {
  return S.keys.bindings.filter((b) => b.tabs.length === 0 || b.tabs.includes(S.tab));
}

function acts(row) {
  const seen = new Set();
  return onTab().filter((b) => {
    if (b.command.kind !== "act" || b.command.row !== row || seen.has(b.command.act)) return false;
    seen.add(b.command.act);
    return true;
  });
}

// A row's acts: the one that moves it on (`next`, from the server), the
// primary and danger ones, then the rest under More, so a run's dozen acts
// do not wrap the toolbar.
function rowButtons(toolbar, own, next) {
  const shown = (b) => b.command.act === next || ["primary", "danger"].includes(b.command.weight);
  // The next act is drawn primary unless it is a danger one, such as push.
  const lift = (b) => (b.command.act === next && b.command.weight === "default" ? "primary" : "");
  for (const b of own.filter(shown)) toolbar.append(button(b, lift(b)));
  const rest = own.filter((b) => !shown(b));
  if (!rest.length) return;
  toolbar.append(el("details", { class: "more" },
    el("summary", { class: "button" }, "More"),
    el("div", { class: "more-menu", role: "group", "aria-label": "More actions" }, rest.map((b) => button(b)))));
}

function panes() {
  const s = S.screens[S.tab];
  return s && s.body && s.body.panes ? s.body.panes : [];
}

function selected() {
  const p = S.pane[S.tab] || 0;
  const pane = panes()[p];
  const i = (S.sel[S.tab] || [])[p];
  return pane && pane.items[i] ? { pane, item: pane.items[i] } : null;
}

// What an act is about: the open detail, else the selected row.
function currentPick() {
  if (S.detail) return S.detail.pick;
  const s = selected();
  return s ? s.item.pick : { kind: "nothing" };
}

// ---------------------------------------------------------------- marks

const sameTask = (a, b) => (a.kind === "task" && b.kind === "task" && a.target === b.target)
  || (a.kind === "manual" && b.kind === "manual" && a.project === b.project);
const MARK_ACTS = ["Dispatch", "DispatchPush", "DispatchInteractive"];
const markable = (pick) => pick.kind === "task" || (pick.kind === "manual" && S.tab === "projects");
const isMarked = (pick) => S.marks.some((m) => sameTask(m, pick));

function toggleMark(pick) {
  if (!markable(pick)) {
    toast("Only a task or a project can be marked.", "warn");
    return;
  }
  S.marks = isMarked(pick) ? S.marks.filter((m) => !sameTask(m, pick)) : S.marks.concat(pick);
  draw();
}

// The marked tasks as one pick, as `screen::marked` builds it.
function markedPick() {
  if (S.marks[0].kind === "manual") return { kind: "projects", names: S.marks.map((m) => m.project) };
  return { kind: "tasks", targets: S.marks.map((m) => m.target), retry: S.marks.some((m) => m.retry) };
}

function currentActs() {
  if (S.detail) return S.detail.acts;
  const s = selected();
  return s ? s.item.acts : [];
}

// ---------------------------------------------------------------- address

// The view lives in the address, so a reload or Back returns to it:
// `?tab=project&project=x&open=run:52`. Not in the fragment, which carries
// the launch token.
function address() {
  const q = new URLSearchParams({ tab: S.tab });
  if (S.tab === "project") q.set("project", S.project);
  if (S.tab === "report" && S.by) q.set("by", S.keys.by[S.by]);
  if (S.tab === "activity" && S.finished) q.set("finished", S.keys.finished[S.finished]);
  const p = S.detail && S.detail.pick;
  const open = p && { run: p.id, job: p.id, scan: p.id, route: p.revision, campaign: p.name, task: p.target }[p.kind];
  if (open !== undefined && open !== null) q.set("open", `${p.kind}:${open}`);
  return `/?${q}`;
}

// Each new view is a history entry. Not while the address is being read:
// that would add entries on a reload and drop the forward ones on Back.
function remember() {
  if (S.restoring) return;
  const url = address();
  if (url !== location.pathname + location.search) history.pushState(null, "", url);
}

// The pick `open=kind:value` names, with only what /api/open reads; the
// detail it returns carries the full pick.
function pickNamed(open) {
  const i = open.indexOf(":");
  const kind = open.slice(0, i);
  const v = open.slice(i + 1);
  const n = Number(v);
  if (i < 0 || !v) return null;
  if (["run", "job", "scan"].includes(kind) && !Number.isInteger(n)) return null;
  return {
    run: { kind, id: n, clean: false, fixing: false, state: "", project: "" },
    job: { kind, id: n, awaiting: false },
    scan: { kind, id: n },
    route: { kind, revision: n, active: false },
    campaign: { kind, name: v },
    task: { kind, project: v.split(":")[0], target: v, retry: false },
  }[kind] || null;
}

// Sets the view from the address; unknown values fall back to the inbox.
async function restore() {
  const q = new URLSearchParams(location.search);
  const tab = q.get("tab");
  const tabs = S.keys.tabs;
  S.tab = tab && tabs.includes(tab) && TABS[tab] ? tab : "inbox";
  S.project = q.get("project") || "";
  if (S.tab === "project" && !S.project) S.tab = "inbox";
  const by = S.keys.by.indexOf(q.get("by"));
  S.by = by >= 0 ? by : 1;
  S.finished = Math.max(S.keys.finished.indexOf(q.get("finished")), 0);
  S.detail = null;
  const open = q.get("open") && pickNamed(q.get("open"));
  S.restoring = true;
  try {
    await Promise.all([load(), loadProjects()]);
    if (open) await openDetail(open);
  } finally {
    S.restoring = false;
  }
  // What could not be shown, such as a run since removed, leaves the address.
  history.replaceState(null, "", address());
}

// ---------------------------------------------------------------- loading

async function load() {
  const by = S.keys.by[S.by];
  const project = encodeURIComponent(S.project);
  const finished = S.keys.finished[S.finished];
  try {
    S.screens[S.tab] = await api(`/api/screen?tab=${S.tab}&by=${by}&project=${project}&finished=${finished}`);
  } catch (e) {
    S.screens[S.tab] = { error: e.message };
  }
  const ps = panes();
  const sel = S.sel[S.tab] || [];
  S.sel[S.tab] = ps.map((p, i) => Math.min(sel[i] || 0, Math.max(p.items.length - 1, 0)));
  let p = S.pane[S.tab] || 0;
  if (p >= ps.length || !ps[p].items.length) {
    const first = ps.findIndex((x) => x.items.length);
    p = first >= 0 ? first : 0;
  }
  S.pane[S.tab] = p;
  // A mark on a row no longer shown would be dispatched unseen.
  const shown = ps.flatMap((x) => x.items.map((i) => i.pick));
  S.marks = S.marks.filter((m) => shown.some((x) => sameTask(m, x)));
  draw();
}

// ---------------------------------------------------------------- drawing

function drawNav() {
  const nav = $("nav");
  nav.replaceChildren();
  const tabKeys = Object.fromEntries(
    S.keys.bindings.filter((b) => b.command.kind === "tab").map((b) => [b.command.tab, b]),
  );
  const views = el("div", { class: "nav-group" });
  for (const [tab, meta] of Object.entries(TABS)) {
    const b = tabKeys[tab];
    if (!b) continue;
    views.append(el("button", {
      class: "nav-item",
      type: "button",
      "data-tab": tab,
      "aria-keyshortcuts": b.key,
      onclick: () => run(b),
    }, el("span", {}, meta.label), el("kbd", {}, b.key)));
  }
  nav.append(views);
  // Acts that apply on every view live here rather than on each toolbar.
  $("anywhere").replaceChildren(...S.keys.bindings
    .filter((b) => b.command.kind === "act" && !b.command.row && b.tabs.length === 0)
    .map((b) => button(b, "small")));
  const projects = el("div", { class: "nav-group projects" },
    el("h2", { id: "projects-heading" }, "Projects"),
    el("input", {
      id: "project-filter",
      class: "filter",
      type: "search",
      placeholder: "Filter projects",
      "aria-label": "Filter projects",
      oninput: (e) => { S.filter = e.target.value.toLowerCase(); drawProjects(); },
    }),
    el("div", { id: "project-list", role: "list", "aria-labelledby": "projects-heading" }));
  nav.append(projects);
}

function drawProjects() {
  const list = $("project-list");
  if (!list) return;
  list.replaceChildren();
  const shown = S.projects.filter((p) => p.name.toLowerCase().includes(S.filter));
  for (const p of shown) {
    const current = S.tab === "project" && S.project === p.name;
    list.append(el("button", {
      class: "nav-item project-item",
      type: "button",
      role: "listitem",
      title: `${p.name}: ${p.state}`,
      "aria-current": current ? "page" : "false",
      onclick: () => openProject(p.name),
    }, el("span", { class: "project-name" }, el("span", { class: `dot tone-${p.tone}`, "aria-hidden": "true" }), p.name),
       el("span", { class: "tier" }, p.tier === "-" ? "" : `T${p.tier}`)));
  }
  if (!shown.length) list.append(el("p", { class: "empty small" }, S.projects.length ? "No match." : "No projects scanned."));
}

// Every project for the sidebar, from the status view: worst first.
async function loadProjects() {
  try {
    const s = await api("/api/screen?tab=projects");
    const items = (s.body.panes && s.body.panes[0] ? s.body.panes[0].items : []);
    S.projects = items.map((it) => ({
      name: it.cells[0].text,
      tier: it.cells[1].text,
      state: it.cells[2].text,
      tone: it.cells[2].tone,
    }));
  } catch (_) {
    S.projects = [];
  }
  drawProjects();
}

function openProject(name) {
  S.marks = [];
  if (S.tab !== "project") S.previous = S.tab;
  S.tab = "project";
  S.project = name;
  S.detail = null;
  load();
}

// Opens the run or job `pick` names over the tab, from a row, a toast or a
// key.
// Row kinds a detail opens for anywhere; a task opens in its project.
const OPENABLE = ["run", "job", "scan", "route", "campaign"];

function opens(pick) {
  return OPENABLE.includes(pick.kind) || (pick.kind === "task" && S.tab === "project");
}

async function openDetail(pick) {
  if (!OPENABLE.includes(pick.kind) && pick.kind !== "task") return;
  try {
    const screen = await api("/api/open", { pick });
    const d = screen.body.detail;
    S.detail = { screen, pick: d.pick, acts: d.acts };
    $("content").scrollTop = 0;
  } catch (e) {
    toast(e.message, "bad");
  }
  draw();
}

function button(b, extraClass) {
  const weight = b.command.weight && b.command.weight !== "default" ? b.command.weight : "";
  return el("button", {
    class: `button ${weight} ${extraClass || ""}`.trim(),
    type: "button",
    "aria-keyshortcuts": b.key,
    onclick: () => run(b),
  }, label(b.label), el("kbd", {}, b.key));
}

function label(text) {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

function draw() {
  for (const b of document.querySelectorAll(".nav-item")) {
    b.setAttribute("aria-current", b.dataset.tab === S.tab ? "page" : "false");
  }
  const meta = TABS[S.tab];
  const s = S.screens[S.tab];
  const title = S.tab === "project" ? S.project : meta.label;
  $("title").textContent = title;
  $("subtitle").textContent = s && s.header
    ? (S.tab === "project" ? s.header.replace(`${S.project}  `, "") : s.header)
    : meta.about;
  document.title = `${title} - pma`;
  remember();
  drawProjects();

  const toolbar = $("toolbar");
  toolbar.replaceChildren();
  const content = $("content");
  content.replaceChildren();
  content.className = "content";
  if (S.detail) {
    drawDetail(toolbar, content);
    return;
  }
  if (S.tab === "report") {
    S.keys.by.forEach((name, i) => {
      toolbar.append(el("button", {
        class: `button ${i === S.by ? "primary" : ""}`,
        type: "button",
        "aria-pressed": String(i === S.by),
        onclick: () => { S.by = i; load(); },
      }, `By ${name}`));
    });
  }
  if (S.tab === "activity") {
    S.keys.finished.forEach((name, i) => {
      toolbar.append(el("button", {
        class: `button ${i === S.finished ? "primary" : ""}`,
        type: "button",
        "aria-pressed": String(i === S.finished),
        onclick: () => { S.finished = i; load(); },
      }, label(name)));
    });
  }
  for (const b of acts(false).filter((b) => b.tabs.length)) toolbar.append(button(b));
  if (S.marks.length) {
    const dispatch = acts(true).find((b) => b.command.act === "Dispatch");
    const back = onTab().find((b) => b.command.kind === "Back");
    toolbar.append(el("span", { class: "sep", "aria-hidden": "true" }));
    if (dispatch) {
      toolbar.append(el("button", { class: "button primary", type: "button", onclick: () => act(dispatch) },
        `Dispatch ${S.marks.length} marked`, el("kbd", {}, dispatch.key)));
    }
    const run = acts(false).find((b) => b.command.act === "Run");
    if (run && S.tab === "projects") {
      toolbar.append(el("button", { class: "button primary", type: "button", onclick: () => act(run) },
        `Run on ${S.marks.length} marked`, el("kbd", {}, run.key)));
    }
    toolbar.append(el("button", { class: "button", type: "button", onclick: () => { S.marks = []; draw(); } },
      "Clear marks", back ? el("kbd", {}, back.key) : null));
  }
  // The selected row's acts, after the view's: what the key would do to it.
  // With tasks marked, dispatch is theirs, above.
  const sel = selected();
  if (sel) {
    const fit = sel.item.acts.filter((a) => !(S.marks.length && MARK_ACTS.includes(a)));
    const own = acts(true).filter((b) => fit.includes(b.command.act));
    const proj = onTab().find((b) => b.command.kind === "OpenProject");
    const open = onTab().find((b) => b.command.kind === "Open");
    const named = sel.item.pick.project;
    const openable = opens(sel.item.pick);
    if (own.length || (proj && named && S.tab !== "project") || (open && openable)) {
      toolbar.append(el("span", { class: "sep", "aria-hidden": "true" }));
    }
    if (open && openable) {
      const what = { run: "Open run", job: "Open log", task: "Open task" }[sel.item.pick.kind] || "Open";
      toolbar.append(el("button", { class: "button primary", type: "button", onclick: () => run(open) },
        what, el("kbd", {}, "Enter")));
    }
    rowButtons(toolbar, own, sel.item.next);
    if (proj && named && S.tab !== "project") {
      // Enter opens it too, when the row is a task.
      toolbar.append(el("button", { class: `button ${openable ? "" : "primary"}`, type: "button", onclick: () => run(proj) },
        `Open ${named}`, el("kbd", {}, openable ? proj.key : "Enter")));
    }
  }

  if (!s) {
    content.append(el("p", { class: "empty" }, "Loading..."));
  } else if (s.error) {
    content.append(el("div", { class: "error-box", role: "alert" }, s.error));
  } else {
    if (s.summary && s.summary.length) {
      content.append(el("dl", { class: "stats" }, s.summary.map(([k, c]) =>
        el("div", { class: "stat" }, el("dt", {}, label(k)), el("dd", { class: `tone-${c.tone}` }, ...inline(c.text))))));
    }
    const ps = panes();
    if (S.tab === "projects" && ps.length > 1) {
      // One table a page: four side by side each scroll both ways.
      const at = S.pane[S.tab] || 0;
      content.className = "content paged";
      content.append(el("div", { class: "pages", role: "tablist", "aria-label": "Tables" }, ps.map((p, pi) =>
        el("button", {
          class: "page-tab",
          type: "button",
          role: "tab",
          id: `page-${pi}`,
          "aria-selected": String(pi === at),
          "aria-controls": `card-${pi}`,
          onclick: () => { S.pane[S.tab] = pi; draw(); },
        }, p.title, el("span", { class: "count" }, p.items.length)))));
      content.append(card(ps[at], at));
    } else {
      if (S.tab === "matrix" && ps.length === 4) content.className = "content grid";
      ps.forEach((p, pi) => content.append(card(p, pi)));
    }
  }
}

// A run or a job in place of the tables: its facts, then its sections, with
// the acts that fit it in the toolbar beside Back.
function drawDetail(toolbar, content) {
  const s = S.detail.screen;
  const dt = s.body.detail;
  const back = onTab().find((b) => b.command.kind === "Back");
  toolbar.append(el("button", { class: "button", type: "button", onclick: () => run(back) }, "Back", el("kbd", {}, "Esc")));
  const own = acts(true).filter((b) => S.detail.acts.includes(b.command.act));
  if (own.length) toolbar.append(el("span", { class: "sep", "aria-hidden": "true" }));
  rowButtons(toolbar, own, dt.next);
  const head = el("header", { class: "detail-head" },
    el("p", { class: "kicker" },
      dt.badge ? el("span", { class: `pill tone-${dt.badge.tone}` }, dt.badge.text) : null,
      s.header || null),
    el("h2", {}, ...inline(dt.title)));
  const box = el("article", { class: "detail" }, head);
  if (dt.notice) box.append(el("div", { class: "notice", role: "alert" }, dt.notice));
  if (dt.fields.length) box.append(fieldList(dt.fields));
  for (const sec of dt.sections) {
    const body = sec.kind === "diff" ? diffNode(sec.text)
      : sec.kind === "log" ? el("pre", { class: "log" }, sec.text)
      : el("div", { class: "prose" }, sec.text.split("\n").flatMap((l, i) => (i ? ["\n", ...inline(l)] : inline(l))));
    box.append(el("section", { class: "section" }, el("h3", {}, sec.title), body));
  }
  content.className = "content detail-view";
  content.append(box);
}

function cellNode(c, col) {
  const node = shownNode(c, col);
  // A link opens GitHub in a new tab; the click does not also select the row.
  if (!c.link) return node;
  return el("a", { class: "out", href: c.link, target: "_blank", rel: "noopener noreferrer", onclick: (e) => e.stopPropagation() }, node);
}

function shownNode(c, col) {
  if (col.kind === "badge" && c.text) return el("span", { class: `pill tone-${c.tone}` }, c.text);
  const span = el("span", { class: `tone-${c.tone}` }, ...inline(c.text));
  if (col.kind === "wide") {
    span.classList.add("clamp");
    span.title = c.text;
  }
  return span;
}

function card(p, pi) {
  const focused = pi === (S.pane[S.tab] || 0);
  const id = `pane-${pi}`;
  const box = el("section", { class: `card ${focused ? "focused" : ""}`, id: `card-${pi}`, "aria-labelledby": id },
    el("header", { class: "card-head" }, el("h2", { id }, p.title), el("span", { class: "count" }, p.items.length)));
  if (!p.items.length) {
    box.append(el("p", { class: "empty" }, p.empty));
    return box;
  }
  // A column no row fills says nothing; it is left out.
  const shown = p.columns.map((_, i) => i).filter((i) => p.items.some((it) => it.cells[i] && it.cells[i].text));
  // A checkbox per task, where the view binds marking.
  const marks = onTab().some((b) => b.command.kind === "Mark") && p.items.some((it) => markable(it.pick));
  const head = el("tr", {},
    marks ? el("th", { scope: "col", class: "mark" }, el("span", { class: "visually-hidden" }, "Marked")) : null,
    shown.map((i) => el("th", { scope: "col", class: p.columns[i].kind }, p.columns[i].title)));
  const body = el("tbody");
  const foot = el("tfoot");
  p.items.forEach((item, ii) => {
    const sel = focused && ii === S.sel[S.tab][pi];
    const total = item.cells.every((c) => c.tone === "strong");
    // A row with a detail opens on one click; any other row is selected.
    const box = marks && markable(item.pick)
      ? el("input", {
        type: "checkbox",
        "aria-label": `Mark ${item.pick.target || item.pick.project}`,
        onclick: (e) => { e.stopPropagation(); toggleMark(item.pick); },
      })
      : null;
    if (box) box.checked = isMarked(item.pick);
    const tr = el("tr", {
      class: box && box.checked ? "marked" : "",
      "aria-selected": String(sel),
      onclick: () => { select(pi, ii); if (opens(item.pick)) run(lookup("Enter")); },
    },
    markable ? el("td", { class: "mark" }, box) : null,
    shown.map((i) => el("td", { class: p.columns[i].kind }, cellNode(item.cells[i], p.columns[i]))));
    (total ? foot : body).append(tr);
    if (sel) requestAnimationFrame(() => tr.scrollIntoView({ block: "nearest" }));
  });
  box.append(el("div", { class: "table-wrap" }, el("table", {}, el("thead", {}, head), body, foot)));
  return box;
}

function select(pi, ii) {
  S.pane[S.tab] = pi;
  S.sel[S.tab][pi] = ii;
  draw();
}

function fieldList(pairs) {
  return el("dl", { class: "fields" }, pairs.flatMap(([k, v]) => [el("dt", {}, k), el("dd", {}, v)]));
}

function diffNode(text) {
  return el("div", { class: "diff", role: "region", "aria-label": "Diff" }, text.split("\n").map((l) => {
    const cls = l.startsWith("+++") || l.startsWith("---") || l.startsWith("diff ") ? "meta"
      : l.startsWith("+") ? "add"
      : l.startsWith("-") ? "del"
      : l.startsWith("@@") ? "hunk" : "";
    return el("div", { class: cls }, l || " ");
  }));
}

// ---------------------------------------------------------------- feedback

// `open` is a pick: the toast gets a button that opens it, and stays longer.
function toast(text, tone, open) {
  const t = el("div", { class: `toast ${tone}`, role: tone === "bad" ? "alert" : "status" },
    el("p", {}, text),
    open ? el("button", { class: "button act", type: "button", onclick: () => { t.remove(); openDetail(open); } },
      open.kind === "run" ? "Open run" : "Open log") : null,
    el("button", { type: "button", "aria-label": "Dismiss", onclick: () => t.remove() }, "Dismiss"));
  $("toasts").append(t);
  setTimeout(() => t.remove(), tone === "bad" || open ? 15000 : 6000);
  return t;
}

// The list an act offers: one radio per option, a checkbox per flag. Its
// `read` gives the input line the chosen option and flags make.
function chooser(asked, name) {
  const box = el("div", { class: "chooser" });
  const list = el("div", { class: "options", role: "radiogroup", "aria-label": name });
  asked.options.forEach((o, i) => list.append(el("label", { class: "option", "data-name": o.name.toLowerCase() },
    el("input", { type: "radio", name: "choice", value: o.name, checked: i === 0 }),
    el("span", { class: "option-name" }, o.name),
    el("span", { class: "option-about" }, ...inline(o.about)))));
  const rows = [...list.children];
  const shown = () => rows.filter((r) => !r.hidden);
  const choose = (r) => {
    if (!r) return;
    r.querySelector("input").checked = true;
    r.scrollIntoView({ block: "nearest" });
  };
  // Typing filters by name; the arrows move among what is shown.
  const filter = el("input", {
    type: "search",
    class: "filter",
    placeholder: "Filter by name",
    "aria-label": "Filter by name",
    oninput: () => {
      const f = filter.value.toLowerCase();
      for (const r of rows) r.hidden = !r.dataset.name.includes(f);
      const c = box.querySelector("input[name=choice]:checked");
      if (!c || c.closest(".option").hidden) choose(shown()[0]);
    },
    onkeydown: (e) => {
      if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
      e.preventDefault();
      const v = shown();
      const at = v.findIndex((r) => r.querySelector("input").checked);
      choose(v[Math.max(0, Math.min(v.length - 1, at + (e.key === "ArrowDown" ? 1 : -1)))]);
    },
  });
  const flags = el("div", { class: "flags" }, asked.flags.map((f) => el("label", { class: "flag" },
    el("input", { type: "checkbox", value: f.arg }), f.label)));
  box.append(filter, list, flags);
  box.read = () => {
    const chosen = box.querySelector("input[name=choice]:checked");
    if (!chosen || chosen.closest(".option").hidden) return null;
    const on = [...box.querySelectorAll(".flags input:checked")].map((i) => i.value);
    return [chosen.value, ...on].join(" ");
  };
  return box;
}

// Rows to check, numbered from 1; `read` gives the numbers left unchecked,
// as `keys::Ask::Check` takes them.
function checklist(items, name) {
  const box = el("fieldset", { class: "options checklist" },
    el("legend", { class: "visually-hidden" }, name),
    items.map((it, i) => el("label", { class: "option" },
      el("input", { type: "checkbox", value: String(i + 1), checked: it.on }),
      el("span", {}, `${i + 1}. `, ...inline(it.text)))));
  box.read = () => [...box.querySelectorAll("input:not(:checked)")].map((i) => i.value).join(",");
  return box;
}

// One dialog serves a question, a prompt and a list. Resolves to the input's
// value, what `read` gives, true for OK, or null when cancelled.
function dialog({ title, text, field, input, ok, okClass, cancel, read }) {
  const d = $("dialog");
  $("dialog-title").textContent = title;
  const t = $("dialog-text");
  t.replaceChildren();
  if (text instanceof Node) t.append(text);
  else if (text) t.textContent = text;
  $("dialog-label").classList.toggle("hidden", field === undefined);
  $("dialog-label-text").textContent = field || "";
  $("dialog-input").value = input || "";
  $("dialog-cancel").classList.toggle("hidden", !cancel);
  const okb = $("dialog-ok");
  okb.textContent = ok || "OK";
  okb.className = `button ${okClass || "primary"}`;
  d.returnValue = "";
  d.showModal();
  const search = t.querySelector("input[type=search]");
  if (field !== undefined) $("dialog-input").focus();
  else if (search) search.focus();
  else okb.focus();
  return new Promise((resolve) => {
    d.addEventListener("close", () => {
      if (d.returnValue !== "ok") return resolve(null);
      if (read) return resolve(read());
      resolve(field === undefined ? true : $("dialog-input").value);
    }, { once: true });
  });
}

function shortcuts() {
  const grid = el("div", { class: "shortcuts" });
  const group = (title, filter) => {
    const seen = new Map();
    for (const b of onTab().filter(filter)) {
      const k = `${b.command.kind}:${b.command.act || b.command.tab || ""}:${b.label}`;
      if (seen.has(k)) seen.get(k).keys.push(b.key);
      else seen.set(k, { label: b.label, keys: [b.key] });
    }
    if (!seen.size) return;
    grid.append(el("h3", {}, title));
    for (const { label: raw, keys } of seen.values()) {
      const tab = Object.entries(TABS).find(([t]) => t === raw);
      const l = tab ? tab[1].label : raw;
      grid.append(el("div", {}, el("span", {}, label(l)), el("span", {}, keys.map((k) => el("kbd", {}, k)))));
    }
  };
  group("Views", (b) => b.command.kind === "tab");
  group("Move", (b) => !["tab", "act", "Quit"].includes(b.command.kind));
  group(`On ${S.tab === "project" ? S.project : TABS[S.tab].label}`, (b) => b.command.kind === "act" && b.tabs.length > 0);
  group("Anywhere", (b) => b.command.kind === "act" && b.tabs.length === 0);
  return grid;
}

// ---------------------------------------------------------------- acting

async function act(b) {
  const name = b.command.act;
  const overMarks = MARK_ACTS.includes(name) || (name === "Run" && S.tab === "projects");
  const marked = overMarks && S.marks.length > 0 && !S.detail;
  // A decision from a run's detail: the next ready run follows it.
  let decided = S.detail && b.command.decides ? S.detail.pick.id : null;
  const pick = marked ? markedPick() : currentPick();
  let input = "";
  try {
    if (b.ask.kind !== "nothing") {
      const asked = await api("/api/ask", { tab: S.tab, act: name, pick, project: S.project });
      if (b.ask.kind === "confirm" || (b.ask.confirm && !asked.items.length)) {
        const yes = await dialog({
          title: label(asked.question),
          ok: label(b.label),
          okClass: b.command.weight === "danger" ? "danger" : "primary",
          cancel: true,
        });
        if (!yes) return;
      } else if (b.ask.kind === "check") {
        // No rows, as for a run that is not a bundle: nothing to ask.
        if (asked.items.length) {
          const node = checklist(asked.items, label(b.ask.label));
          const left = await dialog({ title: label(b.label), text: node, ok: label(b.label), cancel: true, read: node.read });
          if (left === null) return;
          input = left;
        }
      } else if (b.ask.kind === "choose") {
        const node = chooser(asked, label(b.ask.label));
        const chosen = await dialog({ title: label(b.label), text: node, ok: label(b.label), cancel: true, read: node.read });
        if (chosen === null) return;
        input = chosen;
      } else {
        const typed = await dialog({ title: label(b.label), field: label(b.ask.label), input: asked.input, ok: label(b.label), cancel: true });
        if (typed === null) return;
        input = typed;
      }
    }
    const pending = toast(`${label(b.label)}...`, "accent");
    const out = await api("/api/act", { tab: S.tab, act: name, pick, input, project: S.project })
      .finally(() => pending.remove());
    if (marked) S.marks = [];
    const lines = out.said.concat(out.warnings.map((w) => `Warning: ${w}`));
    toast(lines.length ? lines.join("\n") : `${label(b.label)}: done.`, out.warnings.length ? "warn" : "good");
  } catch (e) {
    toast(e.message, "bad");
    decided = null;
  }
  // The detail's run or job has moved on; the tab shows where it went.
  S.detail = null;
  await Promise.all([load(), loadProjects()]);
  // Not over a detail opened meanwhile, as from a notice.
  if (decided !== null && !S.detail) {
    const next = panes().flatMap((p) => p.items).map((i) => i.pick)
      .find((p) => p.kind === "run" && p.state === "ready" && p.id !== decided);
    if (next) await openDetail(next);
  }
}

async function run(b) {
  if (!b) return;
  const t = S.tab;
  const p = S.pane[t] || 0;
  const sel = S.sel[t] || [];
  const len = (panes()[p] || { items: [] }).items.length;
  const move = (i) => { if (len) sel[p] = i; };
  switch (b.command.kind) {
    case "tab":
      S.tab = b.command.tab;
      S.detail = null;
      S.marks = [];
      return load();
    case "Down": move(Math.min((sel[p] || 0) + 1, len - 1)); break;
    case "Up": move(Math.max((sel[p] || 0) - 1, 0)); break;
    case "First": move(0); break;
    case "Last": move(len - 1); break;
    case "NextPane": S.pane[t] = (p + 1) % Math.max(panes().length, 1); break;
    case "PrevPane": S.pane[t] = (p + panes().length - 1) % Math.max(panes().length, 1); break;
    case "Back":
      if (S.detail) {
        S.detail = null;
      } else if (S.marks.length) {
        S.marks = [];
      } else if (S.tab === "project") {
        S.tab = S.previous;
        return load();
      }
      break;
    case "OpenProject": {
      const named = currentPick().project;
      if (named) return openProject(named);
      toast("Select a row that names a project.", "warn");
      break;
    }
    case "ReportBy":
      S.by = (S.by + 1) % S.keys.by.length;
      return load();
    case "Finished":
      S.finished = (S.finished + 1) % S.keys.finished.length;
      return load();
    case "Help":
      await dialog({ title: "Keyboard shortcuts", text: shortcuts(), ok: "Close" });
      break;
    case "Quit": toast("Close the browser tab to leave. pma web runs until Ctrl-C in its terminal.", "accent"); break;
    case "Open": {
      const pick = currentPick();
      if (OPENABLE.includes(pick.kind)) return openDetail(pick);
      if (pick.kind === "task" && S.tab === "project") return openDetail(pick);
      if (pick.project) return openProject(pick.project);
      break;
    }
    case "Mark": return toggleMark(currentPick());
    case "act": return act(b);
  }
  draw();
}

function onKey(e) {
  if ($("dialog").open) return;
  if (e.ctrlKey || e.metaKey || e.altKey) return;
  if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;
  // Tab moves between panes only where the table binds it and nothing has
  // focus; from a button or a link it moves the browser's focus as usual.
  if (e.key === "Tab" && e.target !== document.body) return;
  const b = lookup(e.key === "Tab" && e.shiftKey ? "BackTab" : e.key);
  if (!b) return;
  if (e.key === "Enter" && e.target instanceof HTMLButtonElement) return;
  // Space on a focused button or checkbox presses it, as the browser does.
  if (e.key === " " && (e.target instanceof HTMLButtonElement || e.target instanceof HTMLInputElement)) return;
  // Over a detail, only its own acts and Back apply; the tables beneath
  // are not what the eye is on.
  if (S.detail && !(b.command.kind === "Back" || b.command.kind === "Help"
    || (b.command.kind === "act" && S.detail.acts.includes(b.command.act)))) return;
  e.preventDefault();
  run(b);
}

function setBusy(text, cls) {
  const b = $("busy");
  b.textContent = text;
  b.className = `busy ${cls}`;
}

async function start() {
  const token = location.hash.slice(1);
  if (token) {
    await fetch("/session", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ token }),
    });
    history.replaceState(null, "", location.pathname + location.search);
  }
  try {
    S.keys = await api("/api/keys");
  } catch (_) {
    $("content").replaceChildren(el("div", { class: "error-box", role: "alert" },
      "Not signed in. Open the address `pma web` printed in its terminal; it carries a one-time token."));
    setBusy("Not signed in", "lost");
    return;
  }
  drawNav();
  $("help-button").addEventListener("click", () => run(lookup("?")));
  document.addEventListener("keydown", onKey);
  // Enter in a field submits with OK. Left to the browser, the form's
  // default button is the first one, Cancel.
  $("dialog-form").addEventListener("keydown", (e) => {
    if (e.key !== "Enter" || !(e.target instanceof HTMLInputElement)) return;
    e.preventDefault();
    $("dialog-form").requestSubmit($("dialog-ok"));
  });
  window.addEventListener("popstate", () => { if (!$("dialog").open) restore(); });
  await restore();
  const events = new EventSource("/api/events");
  const heard = (e) => {
    let d = {};
    try { d = JSON.parse(e.data); } catch (_) { /* keep the defaults */ }
    setBusy(d.busy ? "Agents working" : "Idle", d.busy ? "working" : "");
    for (const t of d.transitions || []) toast(t.line, transitionTone(t), t.pick);
    refresh();
  };
  events.addEventListener("tick", heard);
  events.addEventListener("change", heard);
  events.addEventListener("error", () => setBusy("Disconnected", "lost"));
}

// Reloads what is on screen: the tab, the project list, and an open run or
// job, whose state may have moved. Not under a dialog, which holds a
// question the reload would pull from under the user.
async function refresh() {
  if ($("dialog").open) return;
  const open = S.detail ? S.detail.pick : null;
  await Promise.all([load(), loadProjects()]);
  if (open && S.detail) {
    try {
      const screen = await api("/api/open", { pick: open });
      const d = screen.body.detail;
      S.detail = { screen, pick: d.pick, acts: d.acts };
      draw();
    } catch (_) { /* the row may be gone; the page keeps what it showed */ }
  }
}

function transitionTone(t) {
  if (t.state === "failed" || t.state === "stopped" || t.state === "blocked") return "bad";
  if (t.state === "pushed" || t.state === "merged" || t.state === "applied" || t.state === "done") return "good";
  return "accent";
}

start();
