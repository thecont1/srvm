# srvm — technical specification

The mechanics behind `srvm`, for contributors and the curious. The README
covers what it does and how to get it; this file covers how it works.

## Design rules

- **Detection is pure and read-only.** Stack detection reads marker files
  (`package.json`, `deno.json`, `Makefile`, `manage.py`, `Gemfile`,
  `Cargo.toml`, `go.mod`, `compose.yaml`, and ~15 more) and never evaluates
  repository scripts. It issues the same command a human would have typed.
- **Discovery is bounded.** Candidates come only from conventional locations:
  the selected directory, its direct `frontend`/`backend`/`client`/`server`/
  `web`/`api` children, and the immediate children of `apps/`, `packages/`,
  and `services/`. No symlink traversal, nothing outside the canonical
  workspace.
- **Repo writes are bootstrap-only.** srvm writes conventional untracked
  directories only — `.venv`, `node_modules`, `vendor/`, `deps/` — and never
  touches tracked source files. `--no-install` disables even that.
- **`.env` is parse-only.** `KEY=VALUE` syntax with quotes, `export` prefixes,
  and comments; injected only for variables not already set — the shell
  environment always wins. `.env.example`/`.env.sample` alone produce a note,
  not guesses. `BROWSER=none` is always injected into children.
- **No config files, daemons, telemetry, or arbitrary script evaluation.**
- **Exit honesty.** 130 on signal shutdown; the child's original nonzero code
  on failure; nothing left running.

## Launch-set selection

- Bare `srvm` launches the whole independent set; `--all` is a compatibility
  alias; `--select` runs exactly one candidate (index, `path:ecosystem:script`
  id, or unique name).
- Deduplication key: `(canonical app root, ecosystem family)` — two JS apps
  in different directories both launch, while a `package.json` script and a
  fallback probe in the same directory collapse to one app.
- A recognized root orchestrator (`Makefile`, `Procfile`, `docker compose`,
  turbo/nx) runs alone by default; sub-apps stay selectable and a `note`
  line says how to reach them.
- Each app gets its own cwd, port hint, `PATH`, label, and URL. If any app
  fails, the whole set is shut down and srvm exits nonzero; `--quiet` still
  reports failures.
- `--dry-run` reports every per-root candidate, marks the effective launch
  set, and shows planned ports/injection — without installing, downloading,
  spawning, or binding anything.

## Dependency bootstrap

- JS ecosystems: run the package manager's install when `node_modules` is
  absent or older than the lockfile (staleness follows the manager's marker).
- Python: `<python> -m venv .venv` plus `.venv/bin/pip install
  -r requirements.txt` when the project has no virtualenv; a stamp prevents
  repeat installs.
- `bundle install`, `composer install`, and `mix deps.get` gate on their
  conventional directories (`vendor/`, `deps/`).
- All installs run before port reservation, sequentially per app.

## Port arbitration

- `--port N` sets where the free-port *search* starts; a busy port shifts the
  app forward rather than failing. `--port 0` is OS-assigned per app. Without
  `--port`, each app uses its own framework hint.
- Reservations are held only across the immediate spawn handoff. A
  reservation stolen mid-handoff retries above every port already selected
  for the launch, never over a sibling's.
- The selected port is injected the way each framework understands:
  `--port`, `-p`, `-a 127.0.0.1:<n>`, `runserver <n>`, etc. Unknown commands
  get a best-effort `PORT` env var — only when a start port exists (`--port`,
  an inherited numeric `PORT`, or a known hint); srvm never invents a port
  for a fully opaque script and never kills whatever holds a port.
- Reservations and fallback HTTP probes use IPv4 loopback (`127.0.0.1`); the
  app controls its own bind address.
- Honest reconciliation: a small race exists between srvm releasing its probe
  listener and the app binding. If the app binds elsewhere, the URL it
  actually prints is adopted and reported; if it loses the race it fails
  visibly rather than silently.

## Toolchain discovery and runtime fetch

- PATH search covers bun, deno, pnpm, volta, asdf, mise, pyenv, and rbenv
  shims — a runtime off `PATH` is still found.
- If `node`, `python`, `go`, or `cargo` is genuinely missing, srvm downloads
  an official build into a platform cache, verifies SHA-256, and puts that
  bin directory on the child `PATH`. Go resolves pinned versions through the
  historical release index (`?mode=json&include=all`).
- The runtime cache is per-version locked (`File::try_lock`), the prefix is
  validated before publish, and a post-lock recheck avoids duplicate work.

## Built-in static server

A repo that's just `index.html` and assets (root or conventional `public/`,
`www/`, `site/`) is served by a small HTTP server compiled into srvm — no
runtime required. `--port`, `--no-open`, and the free-port walk apply
(default start `8000`); ambient `PORT` is ignored because srvm owns the
listener.

Deliberately minimal:

- Binds `127.0.0.1` only and validates the `Host` header — never answers
  other machines or DNS-rebinding hostnames.
- GET and HEAD only; everything else gets `405 Allow: GET, HEAD`.
- No directory listing, dotfiles, symlinks, or path traversal — requests
  resolve relative to the served root.
- No SPA fallback: missing files are real `404`s.
- No transforms, template injection, live reload, uploads, or CGI. Bytes are
  read fresh per request (`Cache-Control: no-store`) so edits appear on
  refresh.
- Trailing-slash-less directory requests get a relative `308` only when an
  `index.html` exists. `Range` requests are ignored with full `200`s.

## Process supervision

- Children run in their own process group; Ctrl+C (or teardown) signals the
  whole group so `npm → sh → node` grandchildren can't leak.
- Unix teardown escalates TERM → SIGKILL for resistant descendants where a
  grace period exists; Windows uses `taskkill /T` on the tree.
- Exit status is signal-owned: 130 on Ctrl+C shutdown, the child's real code
  otherwise.

## Distribution and release engineering

- Six native targets: macOS/Linux/Windows × x64/ARM64, built on real runners
  (no cross). Linux is GNU/glibc, not musl; Windows static-links the MSVC
  CRT. ARM64 builds assert the produced PE machine (`0xAA64`) so an emulated
  build can never pass silently.
- MSRV `rust-version = "1.89"` (enables `File::try_lock`).
- Releases are cargo-dist 0.30.2 generated (`release.yml`): per-target
  archives + `.sha256` sidecars, shell/PowerShell installers, build
  attestations, `dist-manifest.json`, source tarball, `srvm.rb` formula.
- `tools/install.sh` is the checked front door: it supplies a `sha256sum`
  shim from `shasum`/`openssl` where the platform lacks one, so the official
  installer's own checksum verification always runs, then invokes that
  installer unchanged — one install path.
- `tools/gen_manifests.py` regenerates the Scoop manifest and the three
  winget manifests from `dist-manifest.json` + checksum sidecars.
- `examples/generate_cli_assets.rs` regenerates shell completions and the
  man page from the single `srvm::command()` schema — maintainer-only, kept
  out of the published crate via `exclude`.
- Workflows: `native.yml` (six-runner build/test/smoke with MSRV check),
  `rc-verify.yml` (installs *published* artifacts on all six hosts and
  smokes them), `manifest-verify.yml` (schema-checks winget manifests and
  performs the download/sha256/unpack/run they describe on Windows x64 +
  ARM64), `crates-publish.yml` (manual dispatch; publishes via crates.io
  trusted publishing over OIDC — no stored token), `release.yml` (dist
  publish + Homebrew tap push on stable tags only).
- The published crate is trimmed by `Cargo.toml` `exclude` (56 files): no
  workflows, plan, skill, dist config, or release tooling ships in the
  crates.io package.
- Verified at `v0.1.0`: six-runner artifact install matrix, winget manifest
  schema + real-install smoke on both Windows architectures, 6/6 real-repo
  dogfood, `cargo install --locked srvm`, `brew install thecont1/srvm/srvm`.

The milestone-by-milestone gate ledger lives in [`PLAN.md`](PLAN.md).
