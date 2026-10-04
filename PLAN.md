# srvm — Product Development Plan

> **One-liner:** `srvm` is a single-binary, zero-config CLI that detects how any repository is meant to run, launches it, absorbs the boring failures (missing deps, busy ports), and gets out of the way.

This plan merges the original project brief with lessons distilled from building the same feature inside px0 (`serve.go`, `docs/internals/dev-server.md`). Where the brief and the px0 experience conflict, the px0-informed decisions below win — each divergence is flagged explicitly.

---

## 1. Product framing

### 1.1 What srvm is

A **launcher**, full stop. px0's `-s` proved the concept of marker-file detection + supervised launch inside a read-only code viewer. srvm extracts that idea into a dedicated tool whose entire job is the run lifecycle: detect → provision → arbitrate port → spawn → supervise → surface URL → clean shutdown. No viewer, no editor, no panes.

### 1.2 Divergences from the original brief (locked decisions)

| Brief said | Decision | Why |
|---|---|---|
| "LaunchPad (`lp`)" branding | **`srvm`** everywhere | Consistent binary/repo name; cache dirs derive from it |
| `~/.launchpad/runtimes/` cache | **Platform cache dir** (`dirs::cache_dir()/srvm`: `~/Library/Caches/srvm`, `~/.cache/srvm`, `%LOCALAPPDATA%\srvm`) | OS conventions; only needed once runtime fetching lands (M5) |
| "Never install runtimes; embed a version manager day 1" | **Phased** — v0.1 resolves toolchains from PATH + shim dirs (like px0); auto-fetch is M5 | Runtime download is the highest-risk component; sequencing it after the launcher works avoids a big-bang first release. The seam is designed in from day 1 (§5.4) so M5 is additive |
| Both frontend + backend launched in one repo | **Single best match now; architected for N** | `detect()` returns `Vec<Spec>`; `Supervisor` owns `Vec<Child>`; v0.1 launches `specs[0]`, `--all` lands in M6 without a rewrite |
| Language "Go or Rust" | **Rust** | User's call; every px0 mechanism has a Rust equivalent (§4.2) |
| Table claiming "Vercel CLI" comparison vs `lp` | Keep the positioning, drop the stale table | Same pitch: no Docker daemon, no host toolchains required (eventually), silent port shifting |

### 1.3 What carries over from px0 `-s` (the proven parts)

- **Pure detection, zero evaluation** — read marker files only, emit a fixed command the user would have typed. Never evaluate repo script bodies.
- **First-match ordered rules** with fall-through when a marker's binary is missing.
- **`BROWSER=none`** in child env so toolchains don't race srvm to open tabs.
- **URL sniffing** — scan child output line-by-line for loopback URLs; regexes already designed (`serveURLRe`, `serveBareRe`, `normalizeServeURL`); **probe-hint fallback** — HEAD the framework's default port after ~12s of silence.
- **Process-group kill** — Unix: own pgid + `SIGTERM` → grace → `SIGKILL`; Windows: `taskkill /F /T` or Job Objects. No orphan grandchildren.
- **Auto-install** — `node_modules` missing + JS spec → `<pm> install` first (15-min cap), streamed like server output.
- **Early-death classification** — non-zero exit < 3s = `failed` + last ~12 log lines; no auto-restart.
- **Output flood collapse** — repeated-pattern lines shown twice then folded into `· N more`.
- **Static-site fallback** — bare `index.html` repo gets an embedded file server; guaranteed landing zone.

---

## 2. UX spec

### 2.1 Happy path (revised from brief — real narration style)

```console
$ srvm
  srvm 0.1.0
  workspace  ~/projects/ai-app
  serve      next dev (node)
  step       installing dependencies — npm install
  port       3000 busy → 3001
  app        http://localhost:3001

  ctrl-c to stop
```

Deviation from the brief's mock: no emojis in output (px0 house style: `uiKV`/`uiStatus` key-value narration), runtime-download lines only appear once M5 lands, and multi-stack detection isn't claimed.

### 2.2 CLI surface

| Invocation | Behavior |
|---|---|
| `srvm` | detect + launch in `.` |
| `srvm <dir>` | detect + launch in `<dir>` |
| `srvm --dry-run [dir]` | print detection result(s) + resolved commands; exit 0. **Primary testing/debugging surface — build in M1** |
| `srvm --no-open` | don't auto-open the app URL in a browser |
| `srvm --port N` | arbitration starts at N instead of the spec's hint |
| `srvm --select <tool\|n>` | when multiple stacks detected, pick one (v0.1 errors listing candidates without it) |
| `srvm --no-install` | never run the spec's install step |
| `srvm -v/--verbose` | child output unfiltered (skip flood-collapse) |
| `srvm --quiet`, `--no-color` | narration controls (port px0's `uiQuiet`/`uiForcedColor`) |
| `srvm --version` | print and exit |
| `srvm --all` | **reserved for M6** — accept the flag, error "not yet supported" |

No subcommands in v0.1 — the bare invocation IS the product. (If a `doctor`/`runtimes` management subcommand is wanted later, `clap` subcommands bolt on cleanly.)

### 2.3 Output contract

- States per child: `installing → starting → running`, terminal `exited|failed`.
- First URL line in output wins; `0.0.0.0`/`::1` normalize to `127.0.0.1`.
- On adoption: print `app  <url>` and `open` it unless `--no-open`.
- `failed` prints spec name + error + last ~12 log lines (ANSI-stripped).
- All child stdout/stderr echoes dimmed and indented under srvm's narration; consecutive lines normalizing to the same key (strip `"quoted"`, `/paths`, `numbers`) print twice then collapse to `· N more like the above`. Full tail always kept in the ring buffer for the failure dump.

---

## 3. Detection engine (M1 — the core asset)

Port px0's full ordered rule table. **Order is semantics** — first match wins, so meta-frameworks precede their bundlers and heavyweight fallbacks come last.

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
| 22 | static | `index.html` at root | embedded file server | 8000 |

Supporting machinery to port (all in `serve.go`, tests in `serve_test.go`):

- `ServeSpec { name, tool, cmd, install, url_hint, is_static, port_env, port_args }` — **new fields** `port_env`/`port_args` for arbitration (§3.2)
- `pickPM`: `packageManager` field → lockfile (`bun.lock[b]`/`pnpm-lock|workspace`/`yarn.lock|.yarnrc.yml`/`package-lock|npm-shrinkwrap`) → ubiquity order npm/pnpm/yarn/bun; `execCmd` per-PM runner (npx/pnpm exec/bun x/yarn)
- `pickScript`: exact list then sorted `dev[:_-]*`
- `fwBins`: dependency→args+port table (ordered as above)
- `fileTargets`: col-0 `name:` parser skipping recipes/comments/`VAR :=`/`.PHONY`/patterns
- `pyPrefix`/`pyDeps`/`pyAppModule`/`pyToolBin`: venv `.venv|venv|env` (POSIX+Windows layouts) → `uv|poetry|pipenv run` by lockfile → `python3|python`
- `serveBinDirs`: `PATH` + `~/.bun/bin`, `~/.deno/bin`, `~/.local/share/pnpm`, `~/.volta/bin`, `~/.asdf/shims`, `~/.local/share/mise/shims`, `~/.pyenv/shims`, `~/.rbenv/shims` + Windows (`%LOCALAPPDATA%\pnpm`, `%USERPROFILE%\.bun|.deno\bin`, `%ProgramFiles%\nodejs`)
- `serveFileContains` (1 MB probe cap), `readJSONC`/`stripJSONC` (comments + trailing commas)

**Missing-binary fall-through is the key invariant**: a marker whose toolchain can't be resolved returns `None`, letting the next rule try — including per-rule inside one ecosystem (`bundle`→`jekyll`, binstub→`bundle`).

### 3.1 Detection result

`detect(root) -> Vec<Spec>` — collect **all** matching rules (not just first); `launch` uses `specs[0]` or `--select` pick; empty vec → friendly "no servable app detected" listing what was looked for.

### 3.2 Port override matrix (new — px0 never rewrote ports)

Each spec gains how to inject a shifted port. Where both env and args exist, **args win** (explicit beats ambient):

| Stack | Mechanism |
|---|---|
| npm/PM scripts | `PORT=<n>` env (universal convention; react-scripts/vite/next honor it) — plus `PORT` is harmless when ignored |
| deno task | `PORT` env |
| wrangler | `--port <n>` arg (or `PORT`) |
| vite/astro/next/nuxt/remix binaries | `--port <n>` arg where supported; else `PORT` env |
| django | append port arg: `runserver <n>` |
| uvicorn | `--port <n>` |
| flask | `--port <n>` (or `FLASK_RUN_PORT`) |
| rails | `-p <n>` |
| jekyll | `-P <n>` |
| rackup | `-p <n>` |
| hugo | `--port <n>` |
| mkdocs | `-a 127.0.0.1:<n>` |
| phoenix | `PORT` env |
| laravel | `--port=<n>` |
| trunk | `--port <n>` |
| cargo/go/procfile/make/just/task | `PORT` env only (can't know) |
| compose | none — skip arbitration, warn |
| static | ours — bind directly |

**Arbitration algorithm**: probe `TcpListener::bind(("127.0.0.1", port))` — success → use it; `AddrInUse` → walk `port+1 ..= port+100`; still none → OS-assigned `:0`. Inject via spec mechanism → then **verify against sniffed URL**; if child bound elsewhere anyway, adopt the sniffed URL and print a note ("app reports <url>, override ignored").

---

## 4. Architecture

### 4.1 Crate layout (single binary crate, `cargo init --bin`)

```
src/
  main.rs        CLI parse (clap), orchestration, signal handling
  detect/
    mod.rs       detect() -> Vec<Spec>; ServeSpec, ServeRule
    js.rs        pickPM, pmTable, execCmd, pickScript, fwBins, rulePackageJSON/ruleDeno/ruleWrangler
    targets.rs   fileTargets, targetSpec, ruleMake/ruleJust/ruleTaskfile, ruleProcfile
    python.rs    pyPrefix, pyDeps, pyAppModule, pyToolBin, ruleDjango/Uvicorn/Flask
    misc.rs      rails/jekyll/rackup/hugo/mkdocs/phoenix/laravel/trunk/cargo/go/compose/static
    probe.rs     fileExists, dirExists, fileContains, readJSONC, stripJSONC
    binpath.rs   lookPathIn, binDirs (PATH + shim dirs), cached
  ports.rs       probe/arbitrate/inject
  supervise/
    mod.rs       Supervisor { children: Vec<Child> } — N-ready from day 1
    child.rs     Child { spec, proc, state, url, ring, done } — spawn, install step, wait, classify exit
    pump.rs      io pump: child stdout/stderr → ring buffer + dim echo + line scanner → url
    scan.rs      URL regexes (serveURLRe, serveBareRe, normalize), probeHint equivalent
    kill.rs      process-group teardown: #[cfg(unix)] setsid+killpg SIGTERM→SIGKILL; #[cfg(windows)] Job Object / taskkill /T
    ring.rs      tail ring buffer (64 KB)
    collapse.rs  noiseKey normalization + flood folding (port of dimWriter)
  runtime/
    mod.rs       ToolchainResolver trait — v0.1 PathResolver; seam for M5 fetchers
    path.rs      resolve tool name → absolute path via binpath
  open.rs        openBrowser port: $BROWSER, darwin open, windows rundll32/cmd start, WSL branch + xdg-open/sensible-browser/gio/chrome fallbacks
  ui.rs          narration primitives (kv/status/bullet/hint), color + NO_COLOR/--no-color, quiet
  staticsrv.rs   embedded file server for rule 22 (tiny_http or ~100-line hand-rolled std::net)
```

### 4.2 Dependency policy

Lean like px0; every dep must justify itself:

| Crate | For |
|---|---|
| `clap` (derive) | CLI surface |
| `serde`, `serde_json` | package.json/deno.json parsing (+ `json5` or hand-ported `stripJSONC` for .jsonc) |
| `regex` | URL sniffing, noise collapse, fileTargets |
| `anyhow` | error paths (keep `thiserror` for lib-grade errors if split later) |
| `ctrlc` | SIGINT/SIGTERM handling |
| `dirs` | platform cache dir (needed at M5; cheap to add now) |
| `tiny_http` *(evaluate)* | static fallback; alternative is a minimal hand-rolled responder |
| `tempfile`, `assert_cmd` *(dev)* | fixture + CLI tests |

**Deliberately avoided**: `tokio` (threads + channels suffice — px0 used goroutines the same way; async buys nothing here and doubles conceptual weight), `mise`/`asdf` linking (M5 is a downloader, not an embedded manager).

### 4.3 Supervisor model (N-ready)

```rust
struct Supervisor { children: Vec<Child> }
struct Child {
    spec: ServeSpec,
    proc: Option<process::Child>,   // install or server
    state: State,                   // Installing|Starting|Running|Exited|Failed
    url: Option<String>, target: Option<Url>,
    ring: Ring, done: Receiver<()>, stopped: AtomicBool,
}
```

v0.1 runs `children[0]` only; `--select` picks index. Threads: one pump thread per child stream (or one merged `stderr+stdout` pipe like px0: `cmd.stderr(Stdio::piped())`→`stdout` merge via `Command::new` + `[Stdio]` — px0 merged by pointing both at one writer; in Rust spawn one thread per stream feeding one `Mutex<Pump>`), one probe-hint timer thread, main thread waits on children + signals.

**Stop sequence**: `stopped=true` → SIGTERM to pgid (unix) / taskkill /T (win) → 1.5s grace → SIGKILL. Static server: graceful shutdown of the listener. Race: if stop arrives during spawn, kill immediately after `proc` assigned (px0's closed race — replicate).

### 4.4 The toolchain-resolution seam (for M5)

```rust
trait ToolchainResolver {
    /// Resolve a tool name ("node", "python3", "cargo") to an executable path.
    fn resolve(&self, tool: &str, root: &Path) -> Option<PathBuf>;
}
```

- **v0.1**: `PathResolver` — binpath dirs only. Missing → error with *actionable* message ("node not found — install it, or wait for `srvm` auto-fetch in a future release"). Rules already return `None` on missing bins so detection degrades rather than dies.
- **M5**: `FetchingResolver` wrapping `PathResolver` — on miss, download official dist (nodejs.org, python-build-standalone, go.dev, rustup-init) into `cache_dir()/srvm/runtimes/<tool>/<ver>/`, verify SHA-256, return shim path. Detectors consult a `wanted_version(root)` hint (`.nvmrc`, `packageManager`, `.python-version`, `rust-toolchain.toml`, `go.mod` `go` directive) — **design the trait to take that hint now** so M5 needs no refactor.

---

## 5. Milestones

| Milestone | Status | Deliverable | Exit criteria |
|---|---|---|---|
| **M0** Scaffold | ✅ Done | `cargo init`, clap CLI skeleton, CI (fmt/clippy/test on macOS+Linux+Windows), LICENSE/README/PLAN | `cargo build` green on CI |
| **M1** Detection | ✅ Done | `probe.rs`, `binpath.rs`, all 22 rules, `detect() -> Vec<Spec>`, `--dry-run`, `--select` | Full rule table covered by fixture tests (tempdir markers + `StubResolver` — implemented in `detect::tests`, 8 fixtures exercising every rule family + missing-binary fall-through); `--dry-run` correct on a fixture matrix |
| **M2** Supervisor | ✅ Done | spawn + `BROWSER=none`, install step (15-min cap, Ctrl+C-safe), pump→ring+ANSI-dim+collapse (`--no-color`/`NO_COLOR`), URL scan + probeHint (hand-rolled HEAD over `TcpStream`), full `open_browser` port (`$BROWSER` w/ `%s`, WSL, Linux fallback chain), Ctrl+C group kill, exit classification | Remaining from exit criteria: real-toolchain e2e (`vite`/`python -m http.server`) and orphaned-grandchild assertion are not yet automated; early-death tail IS tested (`supervisor_reports_early_failure_tail`) |
| **M3** Port arbitration | 🔶 Spec-side only | probe/walk/inject per matrix, override verify + "ignored" note | Occupied-port test (hold 3000, assert app lands on 3001 and announced URL matches reality). *Gap: `PortInjection` is populated on every spec and `--port` is parsed, but `spawn` doesn't yet probe or inject* |
| **M4** Static fallback | ⬜ Stub | embedded file server for `index.html` repos | `srvm` in a bare-HTML dir serves it. *Currently `is_static` bails; detection returns the spec (name `srvm static .`) correctly* |
| **M5** Runtime fetch | ⬜ Not started | `FetchingResolver`: node + python-build-standalone first, then go/rust; checksum verify; `.nvmrc`/`.python-version`/`rust-toolchain.toml`/`go.mod` hints | `srvm` on a PATH-scrubbed env (mise-style test) boots a Node and a Python app |
| **M6** Multi-stack | ⬜ Flag reserved | `--all` + `--select`; per-child log prefixes (`[next]`, `[api]`); supervisor already N-shaped | Next+FastAPI fixture launches both, each gets its own URL line. *`--select` already works; `--all` errors cleanly* |
| **M7** Distribution | ⬜ Not started | release CI matrix via `cargo-dist` (or manual goreleaser-style), install.sh, brew/scoop/winget taps, shell completions, man page | One-command install on all three OSes |

### Current status snapshot (as of 2026-10-04, `dev/akriti`)

**Done:** M0–M2. The launcher works end-to-end on PATH-resolved toolchains: `srvm [dir]` detects, installs deps if missing, spawns supervised, sniffs/probes the URL, opens the browser, and tears down the process tree on Ctrl+C. 30 tests green (25 unit incl. full rule-table fixtures, 5 integration). Deviations worth noting: supervision is a `run()` function over one spec rather than a `Supervisor` struct of `Vec<Child>` — the M6 refactor must introduce that type (still additive, but a refactor, not just wiring).

**Next up:** M3 is the highest-value gap — the injection metadata exists on every spec but is never applied (`spawn` doesn't probe or set `PORT`/args, `--port` is a no-op). Then M4's embedded static server is a small, isolated piece.

Sequencing rationale: M1+M2 is the product's spine and is directly port-verifiable; M3 differentiates (nobody else does silent shifting); M5 is the riskiest and benefits most from a stable foundation.

---

## 6. Testing strategy

- **Unit**: `fileTargets` parser, `pickScript`/`pickPM`, `stripJSONC`, URL regexes against real captured lines (Vite/Next/Django/Uvicorn/Phoenix/ANSI — copy px0's cases), `noiseKey` collapse, port-walk logic against a held socket.
- **Fixture** (the `serve_test.go` pattern): `tempdir` + marker files + stub binaries (shell scripts on unix, `.bat`/tiny `.exe` on Windows) injected via a private `binDirs` override — detection never depends on the host toolchain.
- **Integration**: `assert_cmd` on `--dry-run` matrix; gated "e2e" tests (`#[ignore]` + env flag) that really spawn `python3 -m http.server`-equivalents.
- **Verification per change**: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `cargo build --release`.

---

## 7. Risks & edge cases

| Risk | Mitigation |
|---|---|
| Windows process-tree kill is weaker than unix pgid | Use Job Objects (`windows-rs`) with `KILL_ON_JOB_CLOSE`, fallback `taskkill /F /T` — decide in M2 |
| `PORT` ignored by a framework | Per-spec `port_args` where available; post-inject verification against sniffed URL + honest "override ignored" note |
| Runtime fetch trust (M5) | Pin SHA-256s per release; HTTPS only; verify before exec; document provenance per dist |
| `.cmd`/`.bat` shims on Windows can't take signals | Same as px0: kill the tree, never just the shim PID |
| Monorepo false positives (root package.json but real app is deeper) | `--select` + `--dry-run` escape hatches; `turbo.json`/`nx.json` rules help; recursive subdir detection is a possible M6+ exploration — **not** v0.1 |
| Repos with several matching rules where first is wrong | `detect()` returns all; `--dry-run` shows the full ranked list so `--select` is discoverable |
| Port race between probe and bind | Arbitrate → inject → verify-by-sniff covers it; the probe listener is dropped before spawn |
| AI-generated repos often lack lockfiles/scripts entirely | `fwBins` dep table + static fallback catch most; M5 covers missing PMs themselves |

## 8. Explicit non-goals (v1)

- Web UI / embedded app panes (the px0 iframe-proxy idea is **obsolete** — srvm is a CLI; if a dashboard is ever wanted, a TUI via `ratatui` or thin local web UI is a separate product decision)
- Config files (`srvm.toml`) — the repo is the config; escape hatches are flags
- Docker/VM isolation — srvm launches on the host by design
- Telemetry (px0 had opt-out anonymous events; srvm ships without any — revisit only with explicit user demand)
- Auto-restart / crash-loop supervision — fail loudly once
- Plugin architecture

## 9. Open questions for the maintainer

1. **`--port` semantics**: arbitration *start* (current spec) vs *exact* port with failure if busy? Plan implements start; exact could be `--port!` or `--exact-port` later.
2. **Static server binding**: serve on `127.0.0.1` only (px0 posture) or honor `--host`? Recommend loopback-only v0.1 — expose flag if requested.
3. **Name collision check**: `srvm` is short for "serve 'em"; verify crates.io/`brew` name availability before M7 publish — have `srv`/`srve`/`srvup` as backups.
4. **Minimum Rust version**: suggest MSRV = latest stable at M0; edition 2024 (check `cargo` default).
