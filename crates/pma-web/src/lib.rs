//! `pma web`: the TUI's views and actions in a browser, strictly local.
//!
//! It listens on 127.0.0.1 only, at a random port, behind a random token made
//! at each launch. The page trades the token for an `HttpOnly`,
//! `SameSite=Strict` cookie, and every data request needs that cookie. Each
//! request's `Host` must be a loopback name, against DNS rebinding; any port
//! is taken, so an SSH forward to another local port works. Each POST must
//! come from the origin its own `Host` names and send JSON, against
//! cross-site requests.
//! Rows, keys and actions come from `pma_core::screen` and `pma_core::keys`,
//! as the TUI's do. Design: `docs/dev/frontends.md`.

mod http;
mod page;

pub use page::page;

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use pma_core::api;
use pma_core::keys::{self, Act, Ask, Command, Tab};
use pma_core::report_runs::By;
use pma_core::screen::{self, Body, Item, Pick, Screen};
use pma_core::store::Store;

use http::{Request, Response};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const INDEX: &str = include_str!("../assets/index.html");
const SCRIPT: &str = include_str!("../assets/app.js");
const STYLE: &str = include_str!("../assets/app.css");

const COOKIE: &str = "pma_session";

pub struct Server {
    listener: TcpListener,
    ctx: Arc<Ctx>,
}

struct Ctx {
    port: u16,
    token: String,
    /// Opens the store for one request. A test passes its own.
    open: Box<dyn Fn() -> Result<Store> + Send + Sync>,
}

/// 32 random bytes as hex, from the kernel's generator.
fn token() -> Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

impl Server {
    /// Binds 127.0.0.1 at `port`; 0 lets the kernel pick one.
    pub fn bind(port: u16) -> Result<Server> {
        Server::bind_with(port, Box::new(Store::open_default))
    }

    pub fn bind_with(
        port: u16,
        open: Box<dyn Fn() -> Result<Store> + Send + Sync>,
    ) -> Result<Server> {
        let listener =
            TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("127.0.0.1:{port}: {e}"))?;
        let port = listener.local_addr()?.port();
        Ok(Server {
            listener,
            ctx: Arc::new(Ctx {
                port,
                token: token()?,
                open,
            }),
        })
    }

    pub fn port(&self) -> u16 {
        self.ctx.port
    }

    pub fn token(&self) -> &str {
        &self.ctx.token
    }

    /// The address to open. The token rides in the fragment, which a browser
    /// never sends in a request.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/#{}", self.ctx.port, self.ctx.token)
    }

    /// Serves until the process ends, one thread per connection.
    pub fn serve(self) -> Result<()> {
        for stream in self.listener.incoming() {
            let Ok(stream) = stream else { continue };
            let ctx = Arc::clone(&self.ctx);
            std::thread::spawn(move || handle(stream, &ctx));
        }
        Ok(())
    }
}

/// Equal without an early exit, so timing does not reveal a prefix.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

fn handle(mut stream: TcpStream, ctx: &Ctx) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let response = match http::read(&mut stream) {
        Err(status) => Response::status(status),
        Ok(req) => {
            if req.path == "/api/events" && req.method == "GET" && checked(&req, ctx).is_none() {
                let _ = events(&mut stream, ctx);
                return;
            }
            route(&req, ctx)
        }
    };
    let _ = http::write(&mut stream, &response);
}

/// Why a request is refused, if it is: the wrong `Host`; for a POST, another
/// origin or a body that is not JSON; for data, no session cookie.
fn checked(req: &Request, ctx: &Ctx) -> Option<Response> {
    let Some(host) = req.header("host").filter(|h| loopback(h)) else {
        return Some(Response::status(421));
    };
    if req.method == "POST" {
        if req.header("origin") != Some(&format!("http://{host}")) {
            return Some(Response::status(403));
        }
        if !req
            .header("content-type")
            .is_some_and(|t| t.starts_with("application/json"))
        {
            return Some(Response::status(415));
        }
    }
    if req.path.starts_with("/api/") && !req.cookie(COOKIE).is_some_and(|c| same(c, &ctx.token)) {
        return Some(Response::status(403));
    }
    None
}

/// `127.0.0.1:<port>` or `localhost:<port>`. A name an attacker controls,
/// such as one rebound to 127.0.0.1, is not one of them.
fn loopback(host: &str) -> bool {
    host.rsplit_once(':').is_some_and(|(name, port)| {
        matches!(name, "127.0.0.1" | "localhost") && port.parse::<u16>().is_ok()
    })
}

fn route(req: &Request, ctx: &Ctx) -> Response {
    if let Some(refused) = checked(req, ctx) {
        return refused;
    }
    let body = || -> std::result::Result<Value, Response> {
        serde_json::from_slice(&req.body).map_err(|_| Response::status(400))
    };
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => Response::new(200, "text/html; charset=utf-8", INDEX),
        ("GET", "/app.js") => Response::new(200, "text/javascript; charset=utf-8", SCRIPT),
        ("GET", "/app.css") => Response::new(200, "text/css; charset=utf-8", STYLE),
        ("POST", "/session") => {
            let Ok(b) = body() else {
                return Response::status(400);
            };
            match b["token"].as_str() {
                Some(t) if same(t, &ctx.token) => {
                    let mut r = Response::status(204);
                    r.body.clear();
                    r.extra.push((
                        "Set-Cookie",
                        format!("{COOKIE}={}; HttpOnly; SameSite=Strict; Path=/", ctx.token),
                    ));
                    r
                }
                _ => Response::status(403),
            }
        }
        ("GET", "/api/keys") => Response::json(200, &keys_json()),
        ("GET", "/api/screen") => {
            let Some(tab) = req.param("tab").as_deref().and_then(tab_named) else {
                return Response::status(400);
            };
            let by = req
                .param("by")
                .as_deref()
                .and_then(By::parse)
                .unwrap_or(By::Class);
            let project = req.param("project").unwrap_or_default();
            // A name `api::FINISHED` does not hold is every state.
            let named = req.param("finished").unwrap_or_default();
            let finished = api::FINISHED
                .into_iter()
                .flatten()
                .find(|s| s.name() == named);
            answer(ctx, |store| {
                Ok(screen_json(&screen::screen(api::query(
                    store,
                    tab.query(by, &project, finished),
                )?)))
            })
        }
        ("POST", "/api/ask") => {
            let Ok(b) = body() else {
                return Response::status(400);
            };
            let (Some(tab), Some(act), Some(pick)) = (
                b["tab"].as_str().and_then(tab_named),
                act_named(&b["act"]),
                pick_from(&b["pick"]),
            ) else {
                return Response::status(400);
            };
            answer(ctx, |store| {
                // As the TUI does: an act that cannot apply to the pick is
                // refused before the question, not after the answer.
                let confirm = keys::BINDINGS.iter().any(|b| {
                    b.command == Command::Act(act)
                        && matches!(b.ask, Ask::Confirm | Ask::CheckOrConfirm(_))
                });
                if confirm {
                    screen::action_in(
                        act,
                        &pick,
                        &rows(store, tab, act, &b)?,
                        "",
                        b["project"].as_str().unwrap_or(""),
                    )?;
                }
                let mut out = json!({
                    "question": screen::question(act, &pick),
                    "input": api::initial_input(store, act, &pick)?,
                });
                let chooses = keys::BINDINGS
                    .iter()
                    .any(|b| b.command == Command::Act(act) && matches!(b.ask, Ask::Choose(_)));
                let checks = keys::BINDINGS.iter().any(|b| {
                    b.command == Command::Act(act)
                        && matches!(b.ask, Ask::Check(_) | Ask::CheckOrConfirm(_))
                });
                if checks {
                    out["items"] = api::checklist(store, act, &pick)?
                        .into_iter()
                        .map(|(text, on)| json!({ "text": text, "on": on }))
                        .collect();
                }
                if chooses {
                    let c = api::choices(store, act)?;
                    out["options"] = c
                        .options
                        .iter()
                        .map(|o| json!({ "name": o.name, "reads": o.reads, "about": o.about }))
                        .collect();
                    out["flags"] = c
                        .flags
                        .iter()
                        .map(|f| json!({ "key": f.key, "arg": f.arg, "label": f.label }))
                        .collect();
                }
                Ok(out)
            })
        }
        ("POST", "/api/open") => {
            let Ok(b) = body() else {
                return Response::status(400);
            };
            let q = match pick_from(&b["pick"]) {
                Some(Pick::Run { id, .. }) => api::Query::Run { id },
                Some(Pick::Job { id, .. }) => api::Query::JobLog { id },
                Some(Pick::Task {
                    project, target, ..
                }) => api::Query::task(&project, &target),
                Some(Pick::Scan { id }) => api::Query::Scan { id },
                Some(Pick::Route { revision, .. }) => api::Query::RouteDoc { revision },
                Some(Pick::Campaign { name }) => api::Query::Campaign { name },
                _ => return Response::status(400),
            };
            answer(ctx, |store| {
                Ok(screen_json(&screen::screen(api::query(store, q)?)))
            })
        }
        ("POST", "/api/act") => {
            let Ok(b) = body() else {
                return Response::status(400);
            };
            let (Some(tab), Some(act), Some(pick)) = (
                b["tab"].as_str().and_then(tab_named),
                act_named(&b["act"]),
                pick_from(&b["pick"]),
            ) else {
                return Response::status(400);
            };
            let input = b["input"].as_str().unwrap_or("").to_string();
            answer(ctx, |store| {
                let action = screen::action_in(
                    act,
                    &pick,
                    &rows(store, tab, act, &b)?,
                    &input,
                    b["project"].as_str().unwrap_or(""),
                )?;
                let out = api::act(store, action)?;
                Ok(json!({ "said": out.said, "warnings": out.warnings }))
            })
        }
        (_, "/" | "/app.js" | "/app.css" | "/session" | "/api/keys" | "/api/screen")
        | (_, "/api/ask" | "/api/open" | "/api/act") => Response::status(405),
        _ => Response::status(404),
    }
}

/// The rows an act over many reads, as the store has them now rather than as
/// the page last drew them. Other acts read none.
fn rows(store: &Store, tab: Tab, act: Act, body: &Value) -> Result<Vec<Item>> {
    if !matches!(act, Act::ApproveRuns | Act::PushApproved) {
        return Ok(Vec::new());
    }
    let project = body["project"].as_str().unwrap_or("");
    Ok(
        match screen::screen(api::query(store, tab.query(By::Class, project, None))?).body {
            Body::Panes(panes) => panes.into_iter().flat_map(|p| p.items).collect(),
            Body::Detail(_) => Vec::new(),
        },
    )
}

/// Runs `f` against a store opened for this request. An error goes back as
/// JSON with status 400, for the page to show.
fn answer(ctx: &Ctx, f: impl FnOnce(&Store) -> Result<Value>) -> Response {
    let result = (ctx.open)().and_then(|store| f(&store));
    match result {
        Ok(v) => Response::json(200, &v),
        Err(e) => Response::json(400, &json!({ "error": e.to_string() })),
    }
}

/// A `change` whenever anything was committed to the store, read once a
/// second from its data version and carrying each run or job that reached a
/// state worth a notice; and a `tick` every 2 seconds while a job or a run is
/// under way, every 30 otherwise, until the page goes away. The page reloads
/// on either. Without a store to watch, the ticks alone.
fn events(stream: &mut TcpStream, ctx: &Ctx) -> std::io::Result<()> {
    http::start_events(stream)?;
    let store = (ctx.open)().ok();
    let mut watch = store.as_ref().and_then(|s| api::Watch::new(s).ok());
    let mut busy = match &watch {
        Some(w) => w.busy,
        None => (ctx.open)().and_then(|s| api::busy(&s)).unwrap_or(false),
    };
    // The first tick at once, so the page shows whether agents are working
    // before any cadence has passed.
    write!(stream, "event: tick\ndata: {{\"busy\":{busy}}}\n\n")?;
    stream.flush()?;
    let mut since_tick = std::time::Instant::now();
    loop {
        if let (Some(store), Some(w)) = (&store, watch.as_mut())
            && let Ok(Some(moved)) = w.poll(store)
        {
            busy = w.busy;
            write!(
                stream,
                "event: change\ndata: {}\n\n",
                json!({ "busy": busy, "transitions": moved.iter().map(transition_json).collect::<Vec<_>>() })
            )?;
            stream.flush()?;
        }
        if since_tick.elapsed() >= Duration::from_secs(if busy { 2 } else { 30 }) {
            if watch.is_none() {
                busy = (ctx.open)().and_then(|s| api::busy(&s)).unwrap_or(false);
            }
            write!(stream, "event: tick\ndata: {{\"busy\":{busy}}}\n\n")?;
            stream.flush()?;
            since_tick = std::time::Instant::now();
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn transition_json(t: &api::Transition) -> Value {
    json!({
        "kind": t.kind,
        "id": t.id,
        "project": t.project,
        "text": t.text,
        "state": t.state,
        "line": t.line(),
        "pick": pick_json(&t.pick()),
    })
}

fn tab_named(name: &str) -> Option<Tab> {
    Tab::ALL.into_iter().find(|t| t.title() == name)
}

fn act_named(v: &Value) -> Option<Act> {
    let name = v.as_str()?;
    Act::ALL.into_iter().find(|a| a.name() == name)
}

fn keys_json() -> Value {
    let bindings: Vec<Value> = keys::BINDINGS
        .iter()
        .map(|b| {
            let command = match b.command {
                Command::Tab(t) => json!({ "kind": "tab", "tab": t.title() }),
                Command::Act(a) => json!({
                    "kind": "act",
                    "act": a.name(),
                    "row": a.on_row(),
                    "weight": a.weight(),
                }),
                other => json!({ "kind": format!("{other:?}") }),
            };
            let ask = match b.ask {
                Ask::Nothing => json!({ "kind": "nothing" }),
                Ask::Confirm => json!({ "kind": "confirm" }),
                Ask::Text(label) => json!({ "kind": "text", "label": label }),
                Ask::Choose(label) => json!({ "kind": "choose", "label": label }),
                Ask::Check(label) => json!({ "kind": "check", "label": label }),
                Ask::CheckOrConfirm(label) => {
                    json!({ "kind": "check", "label": label, "confirm": true })
                }
            };
            json!({
                "key": b.key,
                "command": command,
                "tabs": b.tabs.iter().map(|t| t.title()).collect::<Vec<_>>(),
                "ask": ask,
                "label": b.label,
            })
        })
        .collect();
    json!({
        "tabs": Tab::ALL.iter().map(|t| t.title()).collect::<Vec<_>>(),
        "by": By::ALL.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        "finished": api::FINISHED
            .iter()
            .map(|s| s.map_or("all", |s| s.name()))
            .collect::<Vec<_>>(),
        "bindings": bindings,
    })
}

fn cell_json(c: &screen::Cell) -> Value {
    json!({ "text": c.text, "tone": c.tone.name(), "link": c.link })
}

fn screen_json(s: &Screen) -> Value {
    let body = match &s.body {
        Body::Detail(d) => json!({ "detail": {
            "title": d.title,
            "pick": pick_json(&d.pick),
            "acts": acts_json(&d.pick),
            "next": screen::next_act(&d.pick).map(Act::name),
            "badge": d.badge.as_ref().map(cell_json),
            "notice": d.notice,
            "fields": d.fields,
            "sections": d.sections.iter().map(|(title, kind, text)| json!({
                "title": title,
                "kind": match kind {
                    screen::Section::Prose => "prose",
                    screen::Section::Diff => "diff",
                    screen::Section::Log => "log",
                },
                "text": text,
            })).collect::<Vec<_>>(),
        }}),
        Body::Panes(panes) => json!({
            "panes": panes.iter().map(|p| json!({
                "title": p.title,
                "empty": p.empty,
                "columns": p.columns.iter().map(|c| json!({
                    "title": c.title,
                    "kind": match c.kind {
                        screen::Kind::Text => "text",
                        screen::Kind::Wide => "wide",
                        screen::Kind::Number => "number",
                        screen::Kind::Badge => "badge",
                    },
                })).collect::<Vec<_>>(),
                "items": p.items.iter().map(|i| json!({
                    "cells": i.cells.iter().map(cell_json).collect::<Vec<_>>(),
                    "pick": pick_json(&i.pick),
                    "acts": acts_json(&i.pick),
                    "next": screen::next_act(&i.pick).map(Act::name),
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        }),
    };
    json!({
        "tab": s.tab.map(|t| t.title()),
        "header": s.header,
        "summary": s.summary.iter().map(|(l, c)| json!([l, cell_json(c)])).collect::<Vec<_>>(),
        "body": body,
    })
}

/// The acts on a row that fit `pick`, by name.
fn acts_json(pick: &Pick) -> Value {
    Act::ALL
        .iter()
        .filter(|a| a.on_row() && screen::fits(**a, pick))
        .map(|a| a.name())
        .collect()
}

fn pick_json(p: &Pick) -> Value {
    match p {
        Pick::Nothing => json!({ "kind": "nothing" }),
        Pick::Task {
            project,
            target,
            retry,
        } => json!({ "kind": "task", "project": project, "target": target, "retry": retry }),
        Pick::Tasks { targets, retry } => {
            json!({ "kind": "tasks", "targets": targets, "retry": retry })
        }
        Pick::Manual { project } => json!({ "kind": "manual", "project": project }),
        Pick::Run {
            id,
            clean,
            fixing,
            state,
            project,
        } => {
            json!({ "kind": "run", "id": id, "clean": clean, "fixing": fixing, "state": state, "project": project })
        }
        Pick::Job { id, awaiting } => json!({ "kind": "job", "id": id, "awaiting": awaiting }),
        Pick::Instance { id, workflow, open } => {
            json!({ "kind": "instance", "id": id, "workflow": workflow, "open": open })
        }
        Pick::Note { id, text } => json!({ "kind": "note", "id": id, "text": text }),
        Pick::Campaign { name } => json!({ "kind": "campaign", "name": name }),
        Pick::Setting { key, value } => json!({ "kind": "setting", "key": key, "value": value }),
        Pick::Root { path } => json!({ "kind": "root", "path": path }),
        Pick::Agent { name } => json!({ "kind": "agent", "name": name }),
        Pick::Preset {
            name,
            agent,
            model,
            args,
        } => {
            json!({ "kind": "preset", "name": name, "agent": agent, "model": model, "args": args })
        }
        Pick::Route { revision, active } => {
            json!({ "kind": "route", "revision": revision, "active": active })
        }
        Pick::Scan { id } => json!({ "kind": "scan", "id": id }),
    }
}

/// A pick the page sent back. Anything malformed is `None`, so a request
/// that names no real subject is refused rather than guessed at.
fn pick_from(v: &Value) -> Option<Pick> {
    let s = |k: &str| v[k].as_str().map(String::from);
    let n = |k: &str| v[k].as_i64();
    let b = |k: &str| v[k].as_bool();
    Some(match v["kind"].as_str()? {
        "nothing" => Pick::Nothing,
        "task" => Pick::Task {
            project: s("project")?,
            target: s("target")?,
            retry: b("retry")?,
        },
        "tasks" => Pick::Tasks {
            targets: v["targets"]
                .as_array()?
                .iter()
                .map(|t| t.as_str().map(String::from))
                .collect::<Option<Vec<_>>>()
                .filter(|t| !t.is_empty())?,
            retry: b("retry")?,
        },
        "manual" => Pick::Manual {
            project: s("project")?,
        },
        "run" => Pick::Run {
            id: n("id")?,
            clean: b("clean")?,
            fixing: b("fixing")?,
            state: s("state")?,
            project: s("project")?,
        },
        "job" => Pick::Job {
            id: n("id")?,
            awaiting: b("awaiting")?,
        },
        "instance" => Pick::Instance {
            id: n("id")?,
            workflow: s("workflow")?,
            open: b("open")?,
        },
        "note" => Pick::Note {
            id: n("id")?,
            text: s("text")?,
        },
        "campaign" => Pick::Campaign { name: s("name")? },
        "setting" => Pick::Setting {
            key: s("key")?,
            value: s("value")?,
        },
        "root" => Pick::Root { path: s("path")? },
        "agent" => Pick::Agent { name: s("name")? },
        "preset" => Pick::Preset {
            name: s("name")?,
            agent: s("agent")?,
            model: match &v["model"] {
                Value::Null => None,
                m => Some(m.as_str()?.to_string()),
            },
            args: v["args"]
                .as_array()?
                .iter()
                .map(|a| a.as_str().map(String::from))
                .collect::<Option<Vec<_>>>()?,
        },
        "route" => Pick::Route {
            revision: n("revision")?,
            active: b("active")?,
        },
        "scan" => Pick::Scan { id: n("id")? },
        _ => return None,
    })
}

/// Opens the page in a Chrome or Chromium window with no browser controls:
/// an app window, from the first such browser found.
pub fn open_app(url: &str) -> Result<()> {
    const BROWSERS: [&str; 5] = [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    ];
    for program in BROWSERS {
        let spawned = std::process::Command::new(program)
            .arg(format!("--app={url}"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if spawned.is_ok() {
            return Ok(());
        }
    }
    Err("no Chrome or Chromium found for --app; open the address in a browser".into())
}

/// Opens the page in the user's browser, without waiting for it.
pub fn open_browser(url: &str) -> Result<()> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests;
