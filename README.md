# srvm

**The zero-config universal app launcher.** `cd` into any repository, type `srvm`, and your project is running — detection, dependency bootstrap, port conflicts, and `.env` loading handled for you.

Built for the era of AI-generated code: you can produce a full-stack repo in seconds, you shouldn't need DevOps knowledge to run it.

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

## What it does

- **Finds every app in the repo.** Discovery is bounded to conventional locations: the directory you are in, its direct `frontend`/`backend`/`client`/`server`/`web`/`api` children, and the immediate children of `apps/`, `packages/`, and `services/`. Bare `srvm` launches the whole independent set — each app on its own port with its own `[label]` — and a one-app repo keeps the plain single-app narration. A recognized root orchestrator (`Makefile`, `Procfile`, `docker compose`, turbo/nx) runs alone instead of double-launching the projects it already starts, and one `note` line tells you how to reach the sub-apps.
- **Detects** your stack from marker files — `package.json`, `deno.json`, `Makefile`, `manage.py`, `Gemfile`, `Cargo.toml`, `go.mod`, `compose.yaml`, and 15 more. Never evaluates your scripts; it issues the same command you would have typed.
- **Bootstraps dependencies like a teammate.** Missing `node_modules` (or one older than the lockfile) gets your package manager's install; a Python project with no virtualenv gets `<python> -m venv .venv` plus `pip install -r requirements.txt`; `bundle install`, `composer install`, and `mix deps.get` run when their conventional directories are absent. Only conventional untracked directories are written — tracked source files are never touched, and `--no-install` opts out.
- **Loads `.env` for you.** Parse-only `KEY=VALUE` (quotes, `export`, comments) from each app root, injected for variables you haven't set yourself — your shell environment always wins. If only `.env.example`/`.env.sample` exists, srvm says so instead of guessing. `BROWSER=none` is always set, so toolchains don't race srvm to open tabs.
- **Arbitrates ports** — if the port is taken, srvm shifts to the next free one and injects it the way each framework actually understands (`--port`, `-p`, `-a 127.0.0.1:<n>`, `runserver <n>`, …). Unknown commands get a best-effort `PORT` env var instead — but only when a start port exists (a `--port` flag, an inherited numeric `PORT`, or a known framework hint); srvm never invents a port for a fully opaque script. srvm never kills whatever is holding a port, and if the app still binds elsewhere the sniffed URL wins and srvm says so.
- **Finds your toolchain** even when it's not on `PATH` — bun, deno, pnpm, volta, asdf, mise, pyenv, rbenv shims are all searched. If `node`, `python`, `go`, or `cargo` is still missing, srvm downloads an official build, checks SHA-256, and puts that bin directory on the child `PATH`.
- **Serves static sites itself** — a repo that's just `index.html` and assets needs no runtime at all; a small HTTP server compiled into `srvm` handles it.
- **Cleans up completely** — Ctrl+C signals the whole process group, so `npm → sh → node` grandchildren can't leak.

## Static sites

When detection lands on a bare `index.html` — at the root or in a conventional `public/`, `www/`, or `site/` directory — `srvm` serves that directory itself, with no Node, Python, or other runtime required on the host. `--port`, `--no-open`, and the usual free-port walk all apply (default start `8000`); an ambient `PORT` environment variable is ignored because `srvm` owns the listener.

The built-in server is deliberately minimal:

- Binds `127.0.0.1` only and validates the `Host` header, so it never answers other machines or DNS-rebinding hostnames.
- GET and HEAD only; anything else gets `405` with `Allow: GET, HEAD`.
- No directory listing, no hidden files or dotfiles, no symlinks, no path traversal — requests are resolved relative to the served root.
- No SPA fallback: a missing `.js` (or anything else) is a real `404`.
- No transforms, template injection, live reload, uploads, or CGI; bytes are read fresh from disk on every request, so edits show up on refresh (`Cache-Control: no-store`).
- Directory requests without a trailing slash get a relative `308` redirect only when an `index.html` exists — a directory without one is a plain `404`, not a redirect to a dead URL. `Range` requests are ignored and full `200` responses are returned.

## Install

There is no published release yet — the first gate is a `v0.1.0-rc.1` prerelease, and stable promotion is a separate, explicitly approved step. Until then, build from source:

```console
$ cargo install --path .   # from a clone of this repository
```

Prebuilt archives for macOS, Linux, and Windows on both x64 and ARM64 ship with the release (`v0.1.0-rc.1` first), together with shell and PowerShell installers, a Homebrew formula, a Scoop manifest, a winget manifest, shell completions, and a man page.

`srvm` is one self-contained executable, not a statically linked one: macOS and Linux builds link the platform C library (Linux is GNU/glibc, not musl), and Windows static-links the MSVC CRT where supported. Runtimes srvm fetches for you still need the host linker/SDK that their ecosystems normally require.

## Usage

```console
$ srvm              # discover and run everything in this directory
$ srvm ~/some/repo  # or any directory
$ srvm --dry-run    # show every candidate and the effective launch set, run nothing
$ srvm --no-open    # don't open the app in a browser
$ srvm --port 4000  # start the free-port search at 4000
$ srvm --port 0     # let the OS pick any free port
$ srvm --select apps/web:package:dev   # run exactly one candidate (index, id, or unique name)
$ srvm --no-install # never run an install step, including bootstrap installs
$ srvm --all        # alias for the default set (compat)
```

**Bare `srvm` runs the project.** The default set deduplicates by (app root, ecosystem): a `frontend/` Vite app and a `backend/` Django app both start, while a `package.json` script plus a `vite` fallback in the same directory is still one app. `--dry-run` shows every per-root candidate and marks which ones the default set launches, without installing, downloading, spawning, or binding anything. If any app fails, everything is shut down and srvm exits non-zero; `--quiet` still reports failures.

Port notes: `--port` sets where the free-port *search* starts, not a hard requirement — a busy port just shifts the app forward. `--port 0` is OS-assigned per app; without `--port`, each app uses its own framework hint. `--dry-run` reports the planned start and injection without probing or binding anything. srvm's port reservations and fallback HTTP probes use IPv4 loopback (`127.0.0.1`); the app still controls its own bind address. There is a small race between srvm releasing its probe listener and the app binding: a non-cooperative app can still lose that race and fail, and when the app instead binds somewhere else successfully the URL it actually prints is adopted and reported — verification reconciles the outcome, it can't eliminate the race.

## Status

Early development — see [`PLAN.md`](PLAN.md) for the product development plan and milestone roadmap. M6.1 ("just run it") and the release-readiness gate are in progress; distribution (M7) has not started.

## Acknowledgements

Detection design inspired by [px0](https://github.com/px0-ai/px0) (MIT), whose `serve.go` rule table this project ports and extends.

## License

MIT — see [LICENSE](LICENSE).