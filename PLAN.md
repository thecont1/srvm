# srvm — Product Development Plan

> **One-liner:** `srvm` is a single-binary, zero-config CLI that detects how any repository is meant to run, launches it, absorbs the boring failures (missing deps, busy ports), and gets out of the way.

This plan merges the original project brief with the locked design decisions below. Where the brief and a locked decision conflict, the decision wins — each divergence is flagged explicitly.

---

## 0. North star contract

**After `cd`, bare `srvm` runs the project — zero decisions.** No command to recall, no directory to pick, no dependency to install, no port to free, no `.env` to source. Every flag is an escape hatch, never a requirement; when two options are equal, the one that removes a user decision wins over the one that adds a flag.

1. **The user never names a command, runtime, package manager, or port.** `srvm` alone is the complete invocation for the common case.
2. **The repo is the config.** Conventional locations (`frontend/`, `backend/`, `apps/*`, `packages/*`, `services/*`, `.env`, lockfiles, version-hint files) carry the intent. There is no `srvm.toml`.
3. **Bootstrap like a teammate would.** Missing `.venv`, stale `node_modules`, absent `vendor/`/`deps/` — srvm performs the conventional, untracked, ecosystem-standard setup so clone → run works.
4. **One lifecycle, honest exits.** Every app shares shutdown; failures are reported in the app's own words (tail + spec name); nothing leaks.
5. **Reliability is part of simplicity.** Runtime-cache integrity and complete process-tree teardown are invisible until they fail — they are release blockers, not polish.

---

## 1. Product framing

### 1.1 What srvm is

A **launcher**, full stop. srvm's entire job is the run lifecycle: detect → provision → arbitrate port → spawn → supervise → surface URL → clean shutdown. Marker-file detection produces a fixed command the user would have typed; srvm supervises that process and absorbs the boring failures. No viewer, no editor, no panes.

### 1.2 Divergences from the original brief (locked decisions)

| Brief said | Decision | Why |
|---|---|---|
| "LaunchPad (`lp`)" branding | **`srvm`** everywhere | Consistent binary/repo name; cache dirs derive from it |
| `~/.launchpad/runtimes/` cache | **Platform cache dir** (`dirs::cache_dir()/srvm`, override `SRVM_CACHE_DIR`) | Runtime archives are cached here since M5; the launcher remains stateless outside provisioning |
| "Never install runtimes; embed a version manager day 1" | **Phased** — PATH + shim resolution first; archive fetching implemented in M5 | Detection stays read-only; launch-time provisioning follows the seam in §4.4 |
| Both frontend + backend launched in one repo | **Bare `srvm` launches the whole independent set; `--all` is a compat alias; `--select` picks one** | Reverses the earlier "single best match" lock (M6.1). Running the project means running all of it; `launch::default_set` dedups by (canonical app root, family) and a recognized root orchestrator runs alone (§4.5) |
| Language "Go or Rust" | **Rust** | User's call; the design maps cleanly onto Rust threads + channels (§4.3) |
| Comparison table using the old product name | **Independent launcher positioning** | No viewer or Docker daemon required for ordinary launch paths; supported missing runtimes can be fetched |

### 1.3 Core design invariants

- **Pure detection, zero evaluation** — read marker files only, emit a fixed command the user would have typed. Never evaluate repo script bodies.
- **Ordered candidate collection** — rules emit ranked launch candidates; default launch chooses the first. Candidates are not necessarily independent apps.
- **`BROWSER=none`** in child env so toolchains don't race srvm to open tabs.
- **URL sniffing** — scan child output line-by-line for loopback URLs (`URL_RE`/`BARE_RE`/`normalize_url` in `supervise/scan.rs`); **probe-hint fallback** — HEAD the selected port, or framework hint when no port was injected, after ~12s without a URL.
- **Process-group teardown** — Unix children get their own pgid; Windows uses `taskkill /F /T`. The timeout path can escalate Unix SIGTERM to SIGKILL, but signal-driven shutdown still needs the stronger lifecycle guarantees described in §4.3.
- **Auto-install** — `node_modules` missing + JS spec → `<pm> install` first (15-min cap), streamed like server output.
- **Early-death classification** — non-zero exit < 3s = `failed` + last ~12 log lines. No continuous crash-loop restart; unannounced failures after port injection have a bounded startup retry exception (§3.2).
- **Output flood collapse** — repeated-pattern lines shown twice then folded into `· N more`.
- **Static-site fallback** — bare `index.html` repo gets an embedded file server; guaranteed landing zone.
- **Bounded conventional discovery** — the selected root, direct `frontend`/`backend`/`client`/`server`/`web`/`api`, and immediate children of `apps`/`packages`/`services` (§3.3). No symlink traversal, nothing outside the canonical workspace, hidden/vendor/generated directories skipped, hard cap of 128 candidate roots reported as a diagnostic instead of silent truncation.
- **Bootstrap-only repo writes** — a missing `.venv`, a `node_modules` that predates the lockfile, an absent `vendor/`/`deps/` get the conventional, untracked, ecosystem-standard setup, so clone → run works. Tracked source files are never touched and `--no-install` remains the escape hatch.
- **`.env` is parse-only** — `KEY=VALUE` (+`export`, quotes, comments, ~64 KiB cap) read from each app root and injected for unset vars only, so the OS environment always wins. Never evaluated, never echoed. `BROWSER=none` is always injected last.

---

## 2. UX spec

### 2.1 Happy path (revised from brief — real narration style)

```console
$ cd ~/projects/ai-app && srvm
  srvm 0.1.0
  workspace  ~/projects/ai-app
  serve      [frontend] npm run dev (npm)
  serve      [backend] python3 manage.py runserver (python)
  step       [frontend] installing dependencies — npm install
  step       [backend] installing dependencies — .venv/bin/pip install -r requirements.txt
  port       [frontend] 5173
  port       [backend] 8000
  app        http://localhost:5173
  app        http://localhost:8000

  ctrl-c to stop
```

Bare `srvm` launches the whole independent set: one `serve`/`app` line per app, `[label]` only when the set has more than one, and a single `ctrl-c to stop` that stops everything. A repo with one candidate keeps the unlabeled narration (`srvm 0.1.0` / `workspace` / `serve` / `step` / `port` / `app`) unchanged.

Deviations from the brief's mock: key-value narration uses aligned lowercase labels without decorative symbols. Runtime-download lines (M5) and bootstrap-install lines (M6.1) appear only when they are needed. When a recognized root orchestrator suppresses visible sub-apps, one `note` line points at them (`orchestrated by make:dev; --select <id> for one app`).

### 2.2 CLI surface

| Invocation | Behavior |
|---|---|
| `srvm` | discover + launch everything in `.` — the default set (§4.5) |
| `srvm <dir>` | discover + launch everything in `<dir>` |
| `srvm --dry-run [dir]` | print detection result(s) + resolved commands; exit 0. **Primary testing/debugging surface — build in M1** |
| `srvm --no-open` | don't auto-open the app URL in a browser |
| `srvm --port N` | arbitration starts at N instead of the spec's hint; `--port 0` picks an OS-assigned free port |
| `srvm --select <id>` | launch exactly one candidate: 1-based dry-run index, qualified id (`apps/web:package:dev`), or a bare name/tool when it is unambiguous — an ambiguous bare name errors with the qualified options |
| `srvm --no-install` | never run any install step, including bootstrap installs |
| `srvm -v/--verbose` | child output unfiltered (skip flood-collapse) |
| `srvm --quiet`, `--no-color` | narration controls (`--quiet` suppresses narration; `--no-color`/`NO_COLOR` disables ANSI) |
| `srvm --version` | print and exit |
| `srvm --all` | compat alias for the default set — identical behavior, kept for docs and muscle memory; conflicts with `--select` |

No subcommands in v0.1 — the bare invocation IS the product. (If a `doctor`/`runtimes` management subcommand is wanted later, `clap` subcommands bolt on cleanly.)

### 2.3 Output contract

- Lifecycle phases per launch: install if needed → start → wait/announce URL → exit/fail. These are control flow in `serve_attempt`, shared by the single and multi-app paths; there is no explicit per-child state machine.
- The first sniffed child URL wins; a later sniffed URL may replace an earlier probe-hint announcement. `0.0.0.0`/`::1` normalize to `127.0.0.1`.
- On adoption: print `app  <url>` and `open` it unless `--no-open`.
- `failed` prints spec name + error + last ~12 log lines (ANSI-stripped).
- All child stdout/stderr echoes dimmed and indented under srvm's narration; consecutive lines normalizing to the same key (strip `"quoted"`, `/paths`, `numbers`) print twice then collapse to `· N more like the above`. Full tail always kept in the ring buffer for the failure dump.

---

## 3. Detection engine (M1 — the core asset)

The detection engine is a fixed ordered rule table. **Order is semantics** — rules collect ranked candidates, and within a root the first surviving candidate wins its family; some ecosystem rules also choose only their highest-priority command. Meta-frameworks precede bundlers and heavyweight fallbacks come last. The table itself is unchanged by M6.1: discovery (§3.3) decides which roots it runs against.

| # | Rule | Markers | Command | Port hint |
|---|---|---|---|---|
| 1 | deno | `deno.json[c]` tasks `dev/serve/start` | `deno task <t>` | — |
| 2 | package.json | scripts `dev/serve/develop/start/preview`, else `dev[:_-]*` (alpha-first) | `<pm> run <s>` | — |
| | | `turbo.json`/`nx.json` w/o script | `<pmExec> turbo run dev` / `nx run-many -t dev` | — |
| | | framework deps (astro→next→nuxt→ng→remix→gatsby→docusaurus→hexo→wrangler→vite) | `<pmExec> <bin> dev` | per-dep |
| 3 | wrangler | `wrangler.toml\|json[c]` | `wrangler dev` | 8787 |
| 4 | Procfile | `Procfile` + foreman/overmind/hivemind | `<tool> start` | — |
| 5 | make | `Makefile\|GNUmakefile\|makefile` targets `dev/serve/server/run/start` | `make <t>` | — |
| 6 | just | `justfile\|Justfile\|.justfile` | `just <t>` | — |
| 7 | task | `Taskfile.y[a]ml` `tasks:` map | `task <t>` | — |
| 8 | django | `manage.py` | `<py> manage.py runserver` | 8000 |
| 9 | uvicorn | `uvicorn\|fastapi` in py deps + `main/app/server/wsgi/asgi.py` | `uvicorn <mod>:app --reload` | 8000 |
| 10 | flask | `flask` in py deps + app module | `flask --app <mod> run` | 5000 |
| 11 | rails | `bin/rails` exec, or `config/application.rb` + `bundle` | `[bin/]rails server` | 3000 |
| 12 | jekyll | `_config.yml` + jekyll in `Gemfile` | `bundle exec jekyll serve` | 4000 |
| 13 | rackup | `config.ru` + rack in `Gemfile` | `bundle exec rackup` | 9292 |
| 14 | hugo | `hugo.toml\|yaml\|json` or `config.*` w/ `baseURL` | `hugo server` | 1313 |
| 15 | mkdocs | `mkdocs.y[a]ml` | `mkdocs serve` | 8000 |
| 16 | phoenix | `mix.exs` containing `phoenix` | `mix phx.server` | 4000 |
| 17 | laravel | `composer.json` + `artisan` | `php artisan serve` | 8000 |
| 18 | trunk | `Cargo.toml` + `index.html` | `trunk serve` | 8080 |
| 19 | cargo | `Cargo.toml` + `src/main.rs` | `cargo run` | — |
| 20 | go | `go.mod` + `main.go`, or exactly one `cmd/*/main.go` | `go run .` / `go run ./cmd/<x>` | — |
| 21 | compose | `compose.y[a]ml`/`docker-compose.y[a]ml` | `docker compose up` | — |
| 22 | static | `index.html` at the candidate root, or in `public/`, `www/`, `site/` when nothing else matches anywhere | embedded file server | 8000 |

Supporting machinery (implemented; names match `src/detect/`):

- `ServeSpec { name, tool, command, install, url_hint, is_static, port }` — `port` is a `PortInjection` plan for arbitration (§3.2)
- `pick_pm`: `packageManager` field → lockfile (`bun.lock[b]`/`pnpm-lock|workspace`/`yarn.lock|.yarnrc.yml`/`package-lock|npm-shrinkwrap`) → ubiquity order npm/pnpm/yarn/bun; `PackageManager::exec_command` per-PM runner (npx/pnpm exec/bun x/yarn)
- `pick_script`: exact list then sorted `dev[:_-]*`
- `framework_bins`/`script_framework`: dependency→args+port table (ordered as above); conservative exact-token script-body recognition
- `file_targets`: col-0 `name:` parser skipping recipes/comments/`VAR :=`/`.PHONY`/patterns
- `py_command`/`py_deps_contain`/`py_app_module`/`py_tool_name`: venv `.venv|venv|env` (POSIX+Windows layouts) → `uv|poetry|pipenv run` by lockfile → `python3|python`
- `look_path`/`bin_dirs`: `PATH` + `~/.bun/bin`, `~/.deno/bin`, `~/.local/share/pnpm`, `~/.volta/bin`, `~/.asdf/shims`, `~/.local/share/mise/shims`, `~/.pyenv/shims`, `~/.rbenv/shims` + Windows (`%LOCALAPPDATA%\pnpm`, `%USERPROFILE%\.bun|.deno\bin`, `%ProgramFiles%\nodejs`)
- `file_contains` (probe-cap), `read_jsonc`/`strip_jsonc` (comments + trailing commas)

**Availability fall-through is the key invariant**: a marker whose tool cannot be resolved or fetched returns `None`, letting later rules try. `AvailabilityResolver` treats supported fetchable runtimes as available without downloading them; `PathResolver` alone searches existing tools. This distinction keeps dry-run read-only while permitting M5 provisioning on launch.

### 3.1 Detection result

`detect(root) -> Vec<ServeSpec>` collects ranked matches at the selected root; ordinary launch uses `specs[0]` or the `--select` pick. An empty vector produces a launch error; dry-run instead prints the no-match explanation and rule-family list. `--dry-run` currently prints all candidates before selection and does not apply `--select`. Multiple matches can be alternative launch routes for the same app (for example, a package script, a wrapper target, and static fallback); `--all` therefore spawns `launch::launch_set` (§4.5), never the raw vector. The Go rule's special `cmd/*/main.go` lookup is not general subdirectory discovery.

### 3.2 Port override matrix (implemented in M3)

Each spec carries a `PortInjection` plan: `Args` templates replace literal `{port}` with the selected port, `Env` sets one child environment pair, `Listener` gives the static server an owned socket, and `None` leaves ports unchanged. The spec selects one mechanism; argument-injection specs do not consult ambient `PORT`.

| Stack | Mechanism |
|---|---|
| npm/PM script exactly matching a known framework command (`vite`, `next dev`, …) | forwarded args per the framework row — npm needs a `--` separator, pnpm/yarn/bun take them directly |
| other npm/PM script bodies, deno task, turbo/nx | `PORT` env — best-effort convention, honored by many dev servers, harmless when ignored |
| wrangler | `--port <n>` |
| astro/next/nuxt/ng/docusaurus/hexo/vite binaries | `--port <n>` (gatsby `-p`) |
| remix dev | `PORT` env — classic Remix `--port` only sets the HMR port, not the app port |
| django | append port arg: `runserver <n>` |
| uvicorn | `--port <n>` |
| flask | `--port <n>` |
| rails | `-p <n>` |
| jekyll | `-P <n>` |
| rackup | `-p <n>` |
| hugo | `--port <n>` |
| mkdocs | `-a 127.0.0.1:<n>` |
| phoenix | `PORT` env |
| laravel | `--port=<n>` |
| trunk | `--port <n>` |
| cargo/go/procfile/make/just/task | `PORT` env only (can't know) |
| compose | none — warn when an override is requested (`--port` or a port hint exists) |
| static | owned in-process listener — srvm binds and serves directly; ambient `PORT` is ignored |

Script-body recognition is deliberately conservative: an exact `framework-bin + args` token match (plus bare `vite`) earns the framework's hint and args forwarding; anything else (`vite --host`, `next dev -p 5000`, compound shell) stays an opaque `PORT`-env spec.

**Arbitration algorithm**: explicit `--port` first (including `0`), then an inherited port value for Env-injection specs, then the spec's `url_hint`. Bind on `127.0.0.1`; `AddrInUse` or `PermissionDenied` walks through `start+100`, then falls back to OS-assigned `:0`. Other bind errors abort. Static serving keeps the listener. Dynamic launch drops it immediately before spawn because arbitrary apps cannot inherit it. Sniffed URLs remain authoritative: an app reporting a different port gets an override-ignored warning. Hint probing uses the selected port, never the stale default. `--port` never kills whatever already holds a port.

**Bounded startup retries**: after an injected-port launch exits non-zero without announcing a URL, srvm allows at most two retries, re-reserving from the selected port plus one (saturating at 65535). A failure within three seconds retries without checking occupancy; a slower failure retries only if a bind probe returns `AddrInUse`. This is a lost-handoff heuristic, not proof of a collision: unrelated quick failures can execute three times before their error is returned. Install commands are not repeated by this server retry loop. This is not general auto-restart supervision.

---

### 3.3 Conventional discovery (M6.1)

`workspace::discover(root)` turns one directory into a bounded, located candidate list. The rule table above runs unchanged against each discovered root; discovery only decides *which roots* it sees.

| Order | Root |
|---|---|
| 1 | the canonicalized selected root |
| 2 | direct children named `frontend`, `backend`, `client`, `server`, `web`, `api` (exact lowercase names) |
| 3 | immediate children of `apps/`, `packages/`, `services/` — **sorted by name**, directories only |

Every candidate carries its canonical app root and its path relative to the workspace root, so a child app keeps root-qualified labels, per-app cwd, per-app hints, and per-app `.env`.

**Bounds.** Hidden directories (leading `.`) and generated/vendor directories (`node_modules`, `vendor`, `target`, `dist`, `build`, `out`, `.venv`, `venv`, `env`, `deps`, `__pycache__`) are skipped. Symlinks are never traversed (`DirEntry::file_type()` only), so discovery cannot escape the canonical workspace. At most 128 candidate roots are visited; hitting the cap prints a diagnostic naming the limit — truncation is never silent. An oversized *structured* marker (`package.json`, `deno.json`) errors explicitly, while a malformed optional child marker produces a path-qualified warning without aborting discovery; root-level detection errors keep their existing behavior.

**Static fallback.** `public/`, `www/`, and `site/` are probed for `index.html` only after every other rule came up empty, and only the first one found is used. `dist/`/`build/` stay excluded in v0.1 — generated output is ambiguous about which app produced it.

---

## 4. Architecture

### 4.1 Crate layout (binary entry point plus library modules)

```text
src/
  main.rs        delegates to cli::run()
  lib.rs         exports bootstrap, cli, detect, dotenv, launch, ports, runtime,
                 staticsrv, supervise, workspace
  cli.rs         clap, dry-run, candidate selection, runtime preparation, launch
  workspace.rs   bounded conventional discovery -> located candidates
  launch.rs      Family classification, (root, family) dedup, default/select sets
  dotenv.rs      parse-only .env reader (no eval), OS-env-precedence injection
  bootstrap.rs   per-stack bootstrap installs and their stamps (M6.1b)
  detect/
    mod.rs       ServeSpec/CommandSpec/PortInjection; ToolResolver, PathResolver,
                 AvailabilityResolver; ranked detect()/detect_with()
    js.rs        package managers/scripts/frameworks; deno and wrangler rules
    targets.rs   Procfile/make/just/task rules and target parsing
    python.rs    Django/Uvicorn/Flask rules, venv and wrapper selection
    misc.rs      Ruby/docs/Elixir/PHP/Rust/Go/Compose/static rules
    probe.rs     bounded marker reads, lossy dependency reads, JSONC
    binpath.rs   PATH/shim lookup and spawn-time executable resolution
  ports.rs       requested-port resolution, reservation, injection, URL port parsing
  supervise/
    mod.rs       single-spec run(), install/server/static lifecycle, signal handler,
                 bounded handoff retries, HTTP probing, URL announcements
    pump.rs      stdout/stderr byte-line reads, lossy UTF-8, ring, echo, URL channel
    scan.rs      URL regexes, normalization, ANSI stripping
    kill.rs      Unix process groups; Windows taskkill; timeout escalation helper
    ring.rs      64 KB failure-tail ring
    collapse.rs  repeated-output folding
    open.rs      browser opening, BROWSER override, platform/WSL fallbacks
  runtime/
    mod.rs       exports runtime kinds, fetch entry point, platform mappings
    fetch.rs     HTTP, cache, offline fallback, staged runtime installation
    archive.rs   checksum verification, extraction, component merge, tool lookup
    hint.rs      project version/channel hints
    node.rs      Node release/archive selection and checksums
    python.rs    python-build-standalone asset selection
    go.rs        Go release/archive selection
    rust.rs      Rust channel parsing and rustc/cargo/rust-std filenames
  staticsrv.rs   confined loopback HTTP serving; 4 workers and bounded socket queue
```

### 4.2 Dependency policy

Lean — every dep must justify itself:

| Crate | For |
|---|---|
| `clap` (derive) | CLI surface |
| `serde`, `serde_json` | package.json/deno.json parsing (+ `strip_jsonc` for .jsonc) |
| `regex` | URL sniffing, noise collapse, file_targets |
| `anyhow` | error paths (keep `thiserror` for lib-grade errors if split later) |
| `ctrlc` | SIGINT/SIGTERM handling |
| `dirs` | platform runtime cache directory |
| `httparse` | request parsing for the embedded static server |
| `cap-std` | capability-relative opens confining static paths to the served root (no symlink/traversal escape) |
| `libc` | Unix process-group setup/signals |
| `ureq` | synchronous HTTPS with bounded reads/timeouts |
| `sha2` | archive SHA-256 verification |
| `tar`, `flate2`, `zip` | runtime archive extraction |
| `tempfile`, `assert_cmd`, `predicates` *(dev)* | fixture + CLI tests |

**Deliberately avoided**: `tokio` (threads + channels suffice; async buys nothing here and doubles conceptual weight), `mise`/`asdf` linking (M5 is a downloader, not an embedded manager).

### 4.3 Supervisor model (M2 + M6)

**Single spec:** `supervise::run(root, spec, options, path_prepend)` installs dependencies, then supervises one server process, or blocks in the embedded static server. Each child stream has a pump thread; the supervising thread polls exit/URL events and performs the hint probe.

**Multi-stack (M6):** `supervise::run_many(root, &[LaunchItem], options)` takes the launch set from `launch::launch_set` (§4.5). It allocates ports first — every reservation is held while the rest are selected, so siblings never receive the same port — then runs installs sequentially (runtime cache staging is not concurrency-safe), then spawns one supervising thread per app sharing `run_server`/`serve_attempt` with the single path. Output lines carry a `[label]` prefix (`label = spec.name`, `#N` on collisions); the browser opens and `ctrl-c to stop` prints once, for the first announcement. Policy: an app exiting 0 leaves the others running; any failure sets `SHUTDOWN`, every worker terminates its tree, and srvm exits non-zero once the siblings are reaped (5s bound). Static specs never reach `run_many`.

**Shutdown:** live PIDs sit in `CURRENT_CHILDREN: Vec<u32>`; the signal handler sets `SHUTDOWN`, sends Unix SIGTERM to every recorded process group or runs Windows `taskkill /T /F`, then exits 130. Static mode first sets its stop flag for graceful shutdown. The install-timeout path uses up to 1.5s grace and Unix SIGKILL escalation; the signal path does not share that bounded wait/escalation, and no stop-token recheck closes the spawn-to-PID-registration window. Arbitrary nested and TERM-resistant descendant cleanup is still not established by tests.

### 4.5 Launch-set policy (`src/launch.rs`)

`default_set(&[Candidate]) -> Set` is the product's center of gravity: it decides what bare `srvm` runs, and `--all` is only an alias for it. Each candidate is classified into a `Family` (Js, Python, Ruby, Docs, Elixir, Php, Rust, Go, Orchestrator, Static) by name prefix (`package:`, `go:`, `make:`, `just:`, `task:`) then exact name, falling back on the tool.

1. **Root orchestrator precedence.** If the workspace root produced a recognized orchestrator (the existing `Orchestrator` family plus conservative `turbo`/`nx` package-script wrappers, recognized by bounded token inspection of the detected command — never by evaluating the script body), the default set is exactly that one candidate. Visible sub-apps stay in `--dry-run` and stay reachable through `--select`, and one `note` line says so.
2. **Otherwise, dedup by (canonical app root, family).** `apps/web` and `apps/admin` both JS → two apps, on distinct ports. Two JS matches in one directory (`package:dev` script plus a `vite` fallback), or a JS app next to a Makefile in the same directory, remain one app — later matches in that root are alternative launchers for the same app.
3. **Orchestrators and statics never join a mixed set.** A `Makefile`/`compose`/static candidate inside a sub-root is dropped when any real app exists; if no app survives, the first orchestrator runs alone; if there is still nothing, the first static candidate runs alone.

The set's size selects the execution path: one entry takes the ordinary single-app lifecycle, two or more take `supervise::run_many`. `--select` always resolves to exactly one candidate. Orchestrator precedence is also what keeps bare `srvm` in a monorepo from starting the same work twice.

### 4.4 Implemented runtime-resolution and provisioning flow (M5)

1. `detect::ToolResolver` is the lookup seam. `PathResolver` searches existing executables; `AvailabilityResolver` also accepts names supported by `runtime::can_fetch`. Neither downloads anything during detection.
2. `cli::prepare_runtimes` inspects the selected command and optional install command. Existing repo-local program paths contribute their parent directory; off-PATH resolved tools contribute their bin directory; missing supported runtime kinds call `runtime::fetch_if_missing`.
3. Fetchers select a version from project hints, download archives, verify SHA-256 before extraction, and stage under `cache_dir()/srvm/runtimes/<kind>/<version>.partial` before publishing the completed version directory. `SRVM_CACHE_DIR` overrides the cache base. `.srvm-ok` plus expected executable lookup identifies reusable installations.
4. Tool directories are prepended to the child PATH so descendants resolve them too. Spawn-time `resolve_for_spawn` resolves bare commands to executable paths; on Windows it honors launchable PATHEXT entries rather than selecting extensionless shell scripts or PowerShell files.

Supported names are `node`/`npm`/`npx`, `python`/`python3`, `go`, and `cargo`/`rustc`. Other package managers and framework executables still need to be available through existing tool lookup or project tooling. M5 does not install arbitrary framework dependencies merely because it can fetch a language runtime.

Version hints: `.nvmrc` / `.node-version`, `.python-version`, `go.mod` (`toolchain` exact; `go` language line as major.minor), and `rust-toolchain.toml` before `rust-toolchain`. With no hint, online selection uses Node LTS and stable Python, Go, and Rust releases.

Rust downloads the official **rustc, cargo, and rust-std** component archives rather than executing `rustup-init`. The intended installation is one prefix containing both binaries and the target standard library. Real component-layout/compilation validation remains a follow-up (§5.1); do not equate the stub boot test with a working fetched compiler. Non-`X.Y.Z` manifest versions (including nightly/beta) are rejected by the current selector.

**Cache behavior:** the initial release index/channel request is still attempted on every preparation. If that request fails, all four fetchers can reuse a completed cached version. Numeric hints constrain matching; Node LTS aliases fall back to the newest cached version without validating LTS metadata. If no usable match exists, the original fetch error is returned. This is offline fallback, not an offline-first or universally hint-equivalent cache policy.

**Upstream formats (M5 recon, 2026-10; not a live-download test):**

- **Node**: `GET nodejs.org/dist/index.json` → array newest-first, `{version:"vX.Y.Z", files:[...], lts, npm}`. `GET /dist/{ver}/SHASUMS256.txt` → `<sha256>  <name>` lines covering every artifact. Archive URL `/dist/{ver}/node-{ver}-{plat}.{ext}` — note the `files` tags ≠ filename tokens (`osx-arm64-tar` → `darwin-arm64.tar.gz`, `win-x64-zip` → `win-x64.zip`); map via the SHASUMS filename, not the `files` tag. `.tar.gz` exists for all unix platforms (no xz dep needed); Windows has `.zip` and a bare `win-x64/node.exe` (no npm).
- **Python**: `GET api.github.com/repos/astral-sh/python-build-standalone/releases/latest` → date-tagged releases, every asset carries `digest: "sha256:…"` inline (plus a `SHA256SUMS` asset). Assets: `cpython-{ver}+{tag}-{triple}-install_only[_stripped].tar.gz` — triples `aarch64|x86_64-apple-darwin`, `x86_64|aarch64-unknown-linux-gnu|musl` (plus `x86_64_v2/v3/v4` microarch variants), `x86_64|aarch64-pc-windows-msvc`; `-freethreaded` variants exist and must be filtered out. One release ships all maintained CPython lines (3.10–3.15) — pick the asset matching `wanted_version`.
- **Implemented archive dependencies**: `ureq` (sync HTTP — fits the no-tokio policy), `sha2`, `tar`+`flate2` (+`zip` for win Node). No xz/zstd needed if `.tar.gz`/`install_only` artifacts are chosen consistently.
- **Plumbing is implemented**: PATH-prepend covers child and descendant lookup, and spawn-time resolution covers Windows shims. Detection's availability check need not carry a resolved executable path in every spec.

---

## 5. Milestones

| Milestone | Status | Deliverable | Exit criteria |
|---|---|---|---|
| **M0** Scaffold | Implemented / merged | `cargo init`, clap CLI skeleton, CI (fmt/clippy/test on macOS+Linux+Windows), LICENSE/README/PLAN | Debug compilation, fmt, all-target Clippy, and tests pass in the three-OS CI matrix; release packaging is M7 |
| **M1** Detection | Implemented / merged | `probe.rs`, `binpath.rs`, all 22 rules, `detect() -> Vec<ServeSpec>`, `--dry-run`, `--select` | Full rule table covered by fixture tests (tempdir markers + `StubResolver` — implemented in `detect::tests`, 8 fixtures exercising every rule family + missing-binary fall-through); `--dry-run` correct on a fixture matrix |
| **M2** Supervisor | Implemented / merged | spawn + `BROWSER=none`, install step (15-min cap, registered PID included in signal handling), pump→ring+ANSI-dim+collapse (`--no-color`/`NO_COLOR`), URL scan + `probe_hint` (hand-rolled HEAD over `TcpStream`), `open_browser` (`$BROWSER` w/ `%s`, WSL, Linux fallback chain), Ctrl+C group kill, exit classification | Early failure tails and fixture shutdown are covered; real-framework e2e and arbitrary nested/TERM-resistant descendant cleanup remain follow-ups (§5.1) |
| **M3** Port arbitration | Implemented / merged | `ports.rs`: requested/explicit/inherited/hint resolution, bounded probe-walk listener reservation, `{port}` template + env injection, sniffed-URL verify + "ignored" note, hardened HTTP probe hint | Occupied-port integration tests assert a real listener lands on the shifted port and the announced URL matches reality (see `tests/ports.rs`; 12s probe fallback exercised once) |
| **M4** Static fallback | Implemented / merged | `staticsrv.rs`: in-process loopback HTTP server on the M3-reserved `TcpListener` (no drop/rebind), httparse + cap-std confinement, 4 bounded workers, GET/HEAD only | `srvm` in a bare-HTML dir serves it with an empty `PATH`; `tests/staticsrv.rs` covers MIME, traversal/symlink/dotfile denial (real sibling secrets), Host validation, request limits + 408 deadline, index-gated redirects, streaming/range semantics, read-only roots, `--no-open` positive/negative control, and signal shutdown |
| **M5** Runtime fetch | Implemented / merged | Node, Python, Go, and rustc+cargo+rust-std archives; checksum-before-extract, cache/offline fallback, PATH prepend | Four PATH-scrubbed fixture boots pass in CI, including sequential Rust component-layout and pre-publish-prefix validation. Live upstream archive/toolchain boots remain an M7 release gate (§5.1). No rustup-init |
| **M6** Multi-stack | Implemented / merged (PR #2) | `launch.rs` family classification, `run_many` with live-PID registry + shared shutdown, `--all`, per-app labels/URLs/ports, dry-run `launch` listing | `tests/multi.rs`: two labeled apps on distinct ports, `--port N` ascending allocation, sibling-failure teardown, Ctrl+C reaps both (unix), one-app and dry-run paths; single-stack output unchanged |
| **M6.1** "Just run it" | Implemented / merged (PR #3) | `workspace.rs` bounded conventional discovery, `launch::default_set` (bare `srvm` runs the whole independent set), per-app roots/cwd/hints/PATH, qualified `--select` ids, parse-only `.env` injection, ordered bootstrap installs (`bootstrap.rs` stamps a Python `.venv`, JS staleness follows the package manager's marker, `vendor/`/`deps/` gate Composer/Bundler/Mix) | Acceptance suite in §2/§5.2: frontend+backend, two JS apps, orchestrator-vs-sub-apps, spaces/non-ASCII paths, symlink/generated/cap limits, per-app cwd/hints/install, `.env` behavior, per-stack bootstrap (fixture stubs), dry-run purity, unified teardown |
| **R** Release readiness | Implemented / merged (`da1cd0e`, PR #4) | Per-version runtime-cache locks with post-lock cache recheck; sequential Rust component-prefix assembly and pre-publish validation; Go historical index; Unix TERM→SIGKILL teardown escalation and signal-owned exit status; declared MSRV 1.89 | Regression coverage for Rust prefix assembly, resistant descendants, and startup cancellation is merged. Six-native MSRV proof, real upstream archives/toolchain boots, and Windows console-event teardown remain M7 native-execution gates (§5.1) |
| **M7** Distribution | In progress (`dev/diparati`) | Six native execution hosts; opt-in live-upstream/runtime proof; pinned `cargo-dist`; GitHub archives, checksums, provenance, shell/PowerShell installers, Homebrew/Scoop/winget manifests, crates.io packaging, completions, and man page | Green six-host build/test/smoke + MSRV; archive/install/manifest validation; real-repo dogfood; explicit `v0.1.0-rc.1` approval and clean-machine RC matrix, followed by separately approved stable promotion. No tag or publication is authorized by this plan alone |

The R verification tail intentionally folds into M7 because the native runners are shared: prove `cargo +1.89.0 check --locked --all-targets` on all six targets, Windows console Ctrl+Break cleanup before guard fallback, actual upstream archive layouts, and bounded live Node/Python/Go/Rust boots. Fixture coverage is not a substitute for those gates.

### Current status snapshot (verified on 2026-10-06)

**Implemented and merged:** M0–M6.1 and milestone R. [PR #2](https://github.com/thecont1/srvm/pull/2), [PR #3](https://github.com/thecont1/srvm/pull/3), and [PR #4](https://github.com/thecont1/srvm/pull/4) are merged to `main`; R's merge commit is `da1cd0e8ff43edef91f38e9329673066cf1f2a43`. The working branch `dev/diparati` was clean at the same SHA when this reconciliation began.

**Verified CI:** [run 37449406383](https://github.com/thecont1/srvm/actions/runs/37449406383) succeeded for the merge on Ubuntu, macOS, and Windows. Baseline CI runs fmt, clippy, and tests; M7 must add the six native execution targets and distribution/live-upstream gates.

**M6.1 and R status:** M6.1 is merged (PR #3); per-app `--no-install`/`--port` overrides, static alongside apps, and JSON dry-run remain explicitly deferred post-v0.1. R is merged (PR #4): per-version runtime-cache locks, sequential Rust component-prefix assembly and pre-publish validation, Go historical index, bounded Unix teardown escalation, and signal-owned exit status are implemented. M5 fixture coverage includes sequential Rust component layout. Do not describe this fixture coverage as proof against live upstream distributions.

**Next:** M7 is in progress on `dev/diparati` (draft PR #5). The M7 gate owns residual R evidence: six-native MSRV, live upstream archive layouts and toolchain boots, and Windows console-event teardown. No release/tag or published channels are asserted at this baseline; see §5.1 and the approved execution plan for approval boundaries.

**M7 implemented so far (`dev/diparati`):**

- **Windows console-event regression:** `tests/lifecycle_windows.rs` spawns srvm with `CREATE_NEW_PROCESS_GROUP`, sends a real `CTRL_BREAK_EVENT` via `GenerateConsoleCtrlEvent`, and asserts exit 130 plus app/descendant/listener death before the test guard's `taskkill` fallback. The fixture gained `hold-break`/`hold-break-child` modes whose handlers swallow the event, so the deaths prove srvm's own teardown; the stub is the fixture copied to `npm.exe` so no `cmd.exe` layer takes the event. Job Objects remain the armed sub-gate if the runner evidence shows `taskkill` insufficient.
- **Opt-in live suite:** `tests/live.rs` behind `--features live` — real Rust component archives compiling a std server, real Node + registry PM install, python-build-standalone venv + Django, real Go compile/run, and nested `--all` (node+flask). Every test runs in an isolated `HOME`/`SRVM_CACHE_DIR` and a PATH farm of symlinks to infra tools only (sh/cc/ld/…), so host language runtimes cannot bypass the fetch path; each asserts an HTTP 200 on the announced port and a fetched runtime tree in the isolated cache.
- **Release pipeline:** `dist-workspace.toml` pins cargo-dist 0.30.2 with the six-runner map (`macos-15-intel`/`macos-15`, `ubuntu-22.04`/`-arm`, `windows-2022`/`windows-11-arm` — no cross-compile), GitHub provenance attestations, `publish-prereleases = false` (an rc tag can never touch stable package feeds; Homebrew updates only on the separately approved stable), `pr-run-mode = "upload"` (PRs build all six artifacts without publishing), `msvc-crt-static = true`, and shell/powershell/homebrew installers at `CARGO_HOME` (user-level, no sudo). The generated `release.yml` is committed and reviewed. Windows-arm64 notes: cargo-dist ships no native windows-arm64 binary (dist.exe runs x64-emulated and misreports the host), and the image's preinstalled rustup resolves x64 toolchains under emulation — `github-build-setup` forces `1.97.1-aarch64-pc-windows-msvc` as the default, drops the repo `rust-toolchain.toml` for that job, and pins `RUSTUP_TOOLCHAIN` through a cygpath-converted `GITHUB_ENV` (Git Bash cannot address the raw Windows path); 0.28.0 refused win→win builds outright, hence the 0.30.2 pin. **Verified 2026-10-06:** all three workflows green on `d4c5107` (CI 3-OS; Native six-runner; Release six-target). The downloaded aarch64-pc-windows-msvc release artifact was inspected: PE machine `0xAA64` (genuine ARM64) and its checksum sidecar matches the zip exactly.
- **Toolchain:** `rust-toolchain.toml` pins 1.97.1 (the locally verified compiler) for dev, CI, and release builds — independent of the declared 1.89 MSRV, which `native.yml` proves with `cargo +1.89.0 check --locked --all-targets` on all six runners.
- **CLI assets:** `cli::command()` factory + `examples/generate_cli_assets.rs` (dev-only, clap_complete/clap_mangen) emit bash/zsh/fish/powershell completions and `srvm.1` from the single schema; outputs committed under `assets/`.
- **Manifest generators:** `tools/gen_manifests.py` derives Scoop (x64+arm64) and winget multi-file manifests purely from the release manifest plus its `.sha256` sidecars; validated against a synthetic manifest (`tools/testdata/`). Submission to the bucket/winget-pkgs is an approval-gated external step.
- **Installer verification (host):** a real `dist build` produced the archive, checksum sidecars, installer, and Homebrew formula; the shell installer passed a clean-room install from a local URL into a temp `CARGO_HOME` (`srvm 0.1.0` runs), and a byte-flipped archive was rejected with `ERROR: checksum mismatch` (exit 1, nothing installed). Caveat: the installer only verifies when a `sha256sum` binary exists on PATH; stock macOS lacks one and falls back to a warning — tracked as a hardening consideration.
- **crates.io:** the sparse index 404s for `srvm` (name available); `cargo package --locked` and `publish --dry-run` pass on a clean tree. Publishing itself is approval-gated and uses no token until the stable step.
- **Live gate friction log:** go1.21.13 (resolved from a `go 1.21` directive) builds binaries without the Mach-O `LC_UUID` load command, which dyld on macOS ≥15 refuses to launch — the Go smoke pins `go 1.24` where upstream emits it. A project that itself pins `go 1.21` on macOS will hit the same upstream wall; srvm reports the child's failure tail. The Rust smoke passed against srvm's pinned rustc 1.99.0 archives. The fetch client's original 180s global timeout failed against the ~150MB rustc archive on a real link; it is now a 20-minute global budget with a 60s per-body-read stall detector.
- **Dogfood recorded:** 6/6 shapes pass on `aarch64-apple-darwin` (fullstack without lockfile, bare static site, Rust CLI, Django clone, monorepo `--all`, `.env.example`-only); dry-run purity, real serve, and Ctrl+C teardown all verified.
- **Signing/notarization assessment:** no Apple Developer ID or Windows code-signing certificate is available to this project, so v0.1 artifacts are unsigned; GitHub provenance attestations are attached but attestations are not platform signatures. macOS Gatekeeper and Windows SmartScreen will treat first-run binaries as unknown until signing credentials are provisioned (a post-v0.1 decision — do not block RC on it, but do not claim signed artifacts).

### 5.1 Native-execution gates carried from R into M7

The following are release-blocking acceptance gates. CI must use the six native runners below; fixture success alone does not close them.

- **Windows console teardown:** add the Windows-only regression in `tests/lifecycle_windows.rs`. Deliver a real `CTRL_C_EVENT` or `CTRL_BREAK_EVENT` to a spawned srvm attached to a console, then prove the app/descendant PIDs and listener are gone before the test guard's `taskkill` fallback. If this demonstrates `taskkill /T /F` is insufficient, stop and review a Job Objects sub-gate before implementing it.
- **Live upstream formats and toolchains:** opt-in, bounded real-archive validation for Rust component assembly, Go ZIP layout, and python-build-standalone Windows `python.exe`/`python3` naming. Run isolated `HOME`, `PATH`, and `SRVM_CACHE_DIR`; keep host linkers/SDKs, but prevent installed host language tools from bypassing srvm's runtime fetch. Smokes: Node+package-manager launch, Python venv bootstrap + Django, Go compile/run, Rust stdlib compile/run, and nested `--all`. Use pinned versions where upstream still serves the requested architecture; report unsupported combinations as blocked rather than passing by fixture.
- **Six-host MSRV proof:** run `cargo +1.89.0 check --locked --all-targets` on each native target. Change `rust-version` only if this evidence requires it.
- **Native release execution:** each of the six target jobs builds, tests, and runs its own produced binary and smoke suite; cross-compilation-only success is not an execution pass. Runner map: `x86_64-apple-darwin` → `macos-15-intel`; `aarch64-apple-darwin` → `macos-15`; `x86_64-unknown-linux-gnu` → `ubuntu-22.04`; `aarch64-unknown-linux-gnu` → `ubuntu-22.04-arm`; `x86_64-pc-windows-msvc` → `windows-2022`; `aarch64-pc-windows-msvc` → `windows-11-arm`.
- **Bounds and evidence:** every live test has explicit timeouts and isolated temporary HOME/cache. Keep fixture suite as default fast coverage; gate RC on an explicit opt-in native job and preserve its logs/artifact manifest.

Already closed by R and its regressions: realistic sequential Rust prefix assembly/validation as an implementation regression, TERM-resistant descendants on Unix, and cancel-during-startup windows. Those fixture/Unix proofs do not substitute for actual upstream archives, six-host MSRV, or the real Windows console event.

**Deferred after v0.1 — do not implement in M7:** per-app `--no-install`/`--port` overrides, static-alongside-apps, JSON dry-run, and musl. Linux support is GNU/glibc.


### 5.2 Launch-set and discovery decisions (M6 + M6.1)

| Gate | Decision |
|---|---|
| Discovery | Bounded conventional locations (§3.3): selected root, `frontend`/`backend`/`client`/`server`/`web`/`api`, immediate children of `apps`/`packages`/`services`; sorted sub-roots; 128-root cap; no symlink traversal; hidden/vendor/generated skipped |
| Default set | Bare `srvm` runs `launch::default_set`: dedup by (canonical app root, family); a recognized root orchestrator runs alone with a `note`; `--all` is a compat alias; `--select` picks exactly one |
| `--all` + `--select` | clap `conflicts_with`; `--all` is a no-op alias for the default set; `--select` accepts a 1-based index, a qualified id (`apps/web:package:dev`), or an unambiguous bare name/tool and errors with the qualified options when ambiguous |
| `--port N` across apps | App 1 starts at N, each later app starts one past the previous selection; `--port 0` is OS-assigned per app; without `--port`, each app's own hint/inherited port, with held reservations resolving duplicates |
| Sibling failure / aggregate exit | Exit 0 keeps siblings running; any failure shuts every app down and srvm exits non-zero. Exit 0 only when every app exited 0 |
| Browser | Opens once, for the first announced URL; `--no-open` honored |
| Provisioning | `prepare_runtimes`, bootstrap installs, and `.env` injection run sequentially, per app root, before any server starts; installs finish before any port is reserved |
| Bootstrap writes | Only conventional untracked dirs (`.venv`, `node_modules`, `vendor/`, `deps/`); a stamp written after the last step succeeds, so repeat runs stay cheap and a partial bootstrap never looks complete; `--no-install` opts out |
| Static coexistence | Not supported; static serves only when nothing else is detected — a root `index.html` or a conventional asset dir (`public/`, `www/`, `site/`) |

**Deferred from M6/M6.1:** per-app `--no-install`/`--port` overrides; static alongside apps; `--json` dry-run output for agents; stronger spawn-registration-window and TERM-resistant descendant tests (§5.1). Windows Ctrl+C multi-app teardown relies on `taskkill /T /F` per registered PID and is exercised only by the unix-gated test.

### 5.3 Settled sub-gate designs (M6.1)

**Shared-JS-workspace declaration parser.** A shared JS install is recognized only from declarations, never inferred:

- `package.json` `"workspaces"`, as an array of patterns or `{"packages": [patterns]}`.
- `pnpm-workspace.yaml`, top-level `packages:` list items only (`- 'apps/*'`), parsed line-wise — no general YAML.

Patterns support literal segments, `*`, and `**`. An app root is owned by the nearest ancestor inside the workspace that declares a workspace whose patterns match the app root's relative path; the install then runs once at that ancestor with the ancestor's package manager and the app root's own install is suppressed. Nested declarations, unparseable structure, or patterns that do not match produce a warning and fall back to per-root installs — srvm never guesses.

**Bootstrap stamp formats (implemented).** One versioned, tab-separated line written into the untracked directory it describes:

`srvm-bootstrap-v1<TAB><purpose><TAB><source-rel-path><TAB>sha256:<hex>`

for example `<app>/.venv/.srvm-bootstrap` holding `python-requirements` + `requirements.txt` + the digest. Multiple sources hash as a sorted list of `<rel>\0<sha256>\n` records. A missing or unparsable stamp means *unknown, do not reinstall*; a mismatching digest means stale → reinstall. JS staleness is deliberately **not** stamped: it compares the lockfile mtime with the package manager's own marker (`node_modules/.package-lock.json`, `.modules.yaml`, `.bun-install`), and a missing marker next to an existing `node_modules` is unknown rather than stale.

**Windows Job Objects.** Not implemented. `taskkill /T /F` per registered PID remains the mechanism; Job Objects become an explicit reviewed sub-gate only if a Windows regression proves the current teardown insufficient.

**MSRV.** `rust-version = "1.89"` is proposed because it enables `File::try_lock` for the runtime cache lock without a new dependency; it is verified with `cargo +1.89.0 check --locked --all-targets` on every supported host before being set, and revised only from evidence.

---

## 6. Testing strategy

- **Unit**: `file_targets` parser, `pick_script`/`pick_pm`, `strip_jsonc`, URL regexes against real captured lines (Vite/Next/Django/Uvicorn/Phoenix/ANSI), `noise_key` collapse, port-walk logic against a held socket.
- **Fixture**: `tempdir` + marker files + stub binaries (shell scripts on unix, `.bat`/tiny `.exe` on Windows) injected via a private `bin_dirs`/`ToolResolver` override — detection never depends on the host toolchain.
- **Integration (implemented)**: CLI subprocess fixtures, a rustc-built HTTP fixture for port arbitration/handoff retries, confined static-server tests, and PATH-scrubbed local-archive boots for all four runtime kinds. Live frameworks/upstream downloads are not part of the default suite.
- **Follow-up coverage**: bounded real-framework smoke tests, realistic sequential Rust component extraction plus fetched compilation, and deeper descendant-cancellation tests (§5.1).
- **Verification for code changes**: focused regressions first, then `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, and a local `cargo build --release` gate when appropriate. CI currently runs the first three on macOS/Linux/Windows. Documentation-only edits require diff review and `git diff --check`, not a Rust suite rerun.

---

## 7. Risks & edge cases

| Risk | Mitigation |
|---|---|
| Windows process-tree kill is weaker than Unix pgid | Current implementation uses `taskkill /F /T` per registered PID, not Job Objects. Multi-app Ctrl+C teardown is only tested on unix; stronger Windows ownership remains a design choice |
| `PORT` ignored by a framework | Per-spec args injection where available; post-inject verification against sniffed URL + honest "override ignored" note |
| Runtime fetch trust (M5) | HTTPS distributions/checksum metadata (loopback HTTP allowed for fixtures), SHA-256 before extraction, staged cache publication; upstream-supplied checksums are integrity checks, not independent publisher authentication |
| `.cmd`/`.bat` shims on Windows can't take signals | Kill the tree, never just the shim PID |
| Monorepo false positives or apps below the selected root | Discovery is bounded to conventional locations (§3.3), and orchestrator precedence (§4.5) keeps turbo/nx/compose/Procfile from double-launching the sub-apps they already run. `--dry-run` lists every per-root candidate plus effective launch membership, and `--select` reaches suppressed ones. Anything outside the conventional locations stays invisible by design |
| Repos with several matching rules where first is wrong | `detect()` returns all; `--dry-run` shows the full ranked list so `--select` is discoverable |
| Port race between reservation and child bind | At most two startup retries after an injected-port, unannounced nonzero exit; quick failures use a heuristic, slower ones require an occupied endpoint (§3.2). Report the actual sniffed URL when the app binds elsewhere |
| Concurrent provisioning into one cache version | A bounded `File::try_lock` (MSRV 1.89) on a stable per-runtime/platform/version lock file, with the post-acquire recheck and unique owned staging dirs added under milestone R; `--all` still prepares runtimes and installs sequentially |
| Rust component layout differs from synthetic fixtures | A failing three-archive regression lands first, then the assembled prefix is validated (rustc, cargo, compiler libs, `lib/rustlib/<target>/lib`) before the cache marker is published — never by scanning previously assembled directories. Milestone R owns this; a stub cargo boot is not proof |
| AI-generated repos lack lockfiles/scripts or tools | Framework-dependency detection and static fallback cover some cases; M5 fetches the supported runtime names in §4.4, not every package manager or framework dependency |

## 8. Explicit non-goals (v1)

- Web UI / embedded app panes (srvm is a CLI; if a dashboard is ever wanted, a TUI via `ratatui` or thin local web UI is a separate product decision)
- Config files (`srvm.toml`) — the repo is the config; escape hatches are flags
- Docker/VM isolation — srvm launches on the host by design
- Telemetry (srvm ships without any — revisit only with explicit user demand)
- Continuous auto-restart / crash-loop supervision — only the bounded startup-handoff heuristic in §3.2 is implemented; exhausted attempts return the failure
- Plugin architecture
- JSON dry-run output (`--json`) for agents — recorded, post-v0.1
- musl targets — Linux ships GNU/glibc in v0.1
- Arbitrary script bodies or evaluated repo commands — detection and `.env` handling are parse-only
- Daemon mode / supervised auto-restart

## 9. Open questions for the maintainer

1. **`--port` semantics**: settled — it is the arbitration *start* (with `0` meaning OS-assigned). An exact-port flag (`--exact-port`) could be added later if requested.
2. **Static server binding**: settled — loopback `127.0.0.1` only, no `--host` flag in v0.1; expose it only if requested.
3. **Name collision check**: `srvm` is short for "serve 'em"; verify crates.io/`brew` name availability before M7 publish — have `srv`/`srve`/`srvup` as backups.
4. **Minimum Rust version**: settled in proposal — `rust-version = "1.89"`, set only after `cargo +1.89.0 check --locked --all-targets` passes on every supported host, then revised only from evidence (§5.3).
5. **Subdirectory discovery**: settled — bounded conventional discovery replaces root-only detection (§3.3), bare `srvm` runs the resulting independent set, and `--all` survives as a compat alias (§4.5).
6. **Registry identity**: the crates.io name is still unverified (the API returned 403 during research). Confirm name availability, publisher identity, and tap/bucket ownership before M7 publishes; `srv`/`srve`/`srvup` remain the documented backups. No rename or reservation happens without explicit approval.
