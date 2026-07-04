# How Conceptify starts up

A walkthrough of what happens between double-clicking Conceptify.app and the
app being "ready". No Rust knowledge assumed. File references are to
`src-tauri/src/`.

## The two binaries

The repo builds two separate programs from one Cargo *workspace* (Rust's word
for a multi-package repo):

- **`conceptify-app`** — the desktop app itself (the code in `src-tauri/`).
  The bundler wraps it up and ships it as `Conceptify.app`.
- **`conceptify`** — the CLI (in `crates/conceptify-cli/`). A tiny standalone
  program that talks to the app over HTTP. It never runs "inside" the app.

This doc is about the first one.

## Step 0 — the entry point (`main.rs` → `lib.rs`)

Every Rust program starts at a function called `main()`. Ours is three lines
(`main.rs`): it just calls `run()` in `lib.rs`. That split is a Tauri
convention (the real logic lives in a library so tests and mobile builds can
reuse it) — `lib.rs` is where startup actually happens.

## Step 1 — building the Tauri app

`run()` uses a *builder*: you start with a default app object and chain
configuration onto it before launching. In order, we attach:

1. **single-instance plugin** (registered first on purpose — it must run
   before anything else). If you launch Conceptify while it's already
   running, the second process notices, tells the first one "show and focus
   your window", and exits immediately. You never get two copies.
2. **opener plugin** — utility for opening files/URLs in the default browser
   (used later for "open artifact in browser").
3. **window-state plugin** — saves window size/position to disk and restores
   them on the next launch.
4. **Command handlers** — the list of Rust functions the frontend
   (JavaScript) is allowed to call. In Tauri, the UI is a web page in a
   native window; when it needs something from the native side it "invokes a
   command", which is just a Rust function marked `#[tauri::command]` (that
   `#[...]` is an *attribute* — a label that tells Tauri to expose the
   function over its internal JS↔Rust bridge).

## Step 2 — the `setup` hook (the important part)

`setup` is a function Tauri calls exactly once, after the app object exists
but before any window appears. Ours does two things, in a deliberate order:

**First, the database.** `db::init()` (in `db/mod.rs`):

- opens (or creates) the SQLite file at
  `~/Library/Application Support/conceptify/conceptify.db`
- switches it to WAL mode (a journaling mode that's safer under concurrent
  reads/writes)
- runs *migrations*: numbered schema-change scripts. SQLite stores which
  number it's at, so on every launch we just apply any it hasn't seen.
  A fresh machine gets all of them (creating the `projects`, `threads`,
  `artifacts`, `comments`, `follow_up_runs`, `settings` tables); an
  up-to-date machine gets none. That's why launch is idempotent — running
  the app repeatedly never re-creates or damages anything.

The opened connection is then put into **managed state** via `app.manage(db)`.
Managed state is Tauri's built-in "global object store": you park a value in
it once, and any command handler or server code can ask for it later by type.
The value we park is an `Arc<Mutex<Connection>>` — Rust-ese for "a shared,
lockable handle": `Arc` lets many parts of the program hold the same thing at
once, `Mutex` makes them take turns actually using it. That's how one SQLite
connection is safely shared between the HTTP server and UI commands.

**Second, the HTTP API.** `tauri::async_runtime::spawn(server::start(...))`
starts the web server as a background task — "spawn" means "run this
concurrently, don't wait for it". The window can therefore appear instantly
while the server boots in parallel. `server::start()` (in `server/mod.rs`):

1. **Loads or creates the auth token** — a random secret written to
   `~/Library/Application Support/conceptify/token` with permissions `0600`
   (only your user can read it). Every API request except `/health` must
   present this token; that's what stops other software on the machine from
   poking the API. The token persists across launches.
2. **Binds a port.** Tries `127.0.0.1:4477`. If it's taken, it probes
   whatever is on it: if that's *another Conceptify* (it answers `/health`
   with our signature), this server politely stands down — the other
   instance owns the API. If it's some unrelated program, we try
   4478, 4479, … up to 4487.
3. **Writes the port file** — the winning port number goes into
   `~/Library/Application Support/conceptify/port`, which is how the CLI
   finds us without guessing.
4. **Serves.** The route table (`server/routes.rs`) is attached and the
   server runs for the life of the app. Handlers get an `ApiState` bundle
   containing the DB handle and an `AppHandle` — the latter lets any HTTP
   handler fire a Tauri *event* into the webview, which is how "an agent
   hit the API" turns into "the UI updated live".

If any of this fails (no free port, unwritable token file), the server logs
and gives up **without crashing the app** — the window still opens.

## Step 3 — the window

Tauri creates the `main` window per `tauri.conf.json`. What loads into it
depends on how you launched:

- **`just dev`** — the window loads `http://localhost:1420`, Vite's dev
  server, so frontend edits hot-reload instantly.
- **Built app** — the window loads the compiled frontend files baked into
  the bundle (`dist/`). No local web server involved; "localhost:1420" does
  not exist in production.

## After startup: lifecycle quirks worth knowing

- **Closing the window doesn't quit.** The close click is intercepted and
  the window merely hides; the process (and the HTTP API) stays alive. This
  is deliberate: agents and the CLI can talk to Conceptify with no window
  open.
- **Getting the window back**: click the Dock icon (macOS sends a "Reopen"
  event, which we handle by re-showing the window), or launch the app again
  (single-instance focuses the existing one), or `conceptify status` via the
  CLI (which launches/focuses through `open -a`).
- **Actually quitting**: menu Quit / Cmd-Q. That's a different event than
  closing the window, and nothing intercepts it.

## One-glance summary

```
main() → run()
  ├─ plugins: single-instance, opener, window-state
  ├─ setup hook:
  │    ├─ db::init()  → open/create SQLite (WAL) + run pending migrations
  │    ├─ app.manage(db)                → share DB handle app-wide
  │    └─ spawn server::start()         → token → bind 4477(+fallback)
  │                                        → write port file → serve /api/v1
  └─ window opens (dev: Vite; prod: bundled files)
       └─ close = hide; Dock click = re-show; Cmd-Q = quit
```
