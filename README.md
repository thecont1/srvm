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

### Installing a prebuilt binary

`v0.1.0-rc.1` is the first release carrying prebuilt archives for macOS, Linux, and Windows on both x64 and ARM64, alongside the shell and PowerShell installers:

```console
# Linux and macOS
$ curl --proto '=https' --tlsv1.2 -LsSf https://github.com/thecont1/srvm/releases/download/v0.1.0-rc.1/srvm-installer.sh | sh

# Windows
$ powershell -ExecutionPolicy Bypass -c "irm https://github.com/thecont1/srvm/releases/download/v0.1.0-rc.1/srvm-installer.ps1 | iex"

# Homebrew (the formula reaches the tap at the stable release)
$ brew install thecont1/srvm/srvm
```

Those commands name the prerelease tag on purpose: GitHub's `releases/latest` skips prereleases, so it would resolve to nothing until a stable release exists. Once one does, `releases/latest` works and the `/download/<tag>` segment can go.

**On macOS, use the checked front door.** The generated installer verifies the download with `sha256sum`, which stock macOS does not provide — without it the installer prints a note and installs unverified. [`tools/install.sh`](tools/install.sh) supplies the command it is looking for, so the installer's own comparison runs and a tampered archive is refused, and then runs that same installer unchanged:

```console
$ curl -sSfL https://raw.githubusercontent.com/thecont1/srvm/main/tools/install.sh | sh
```

**The binaries are unsigned**, and this project has no signing certificate, so first runs are gated by the OS. macOS Gatekeeper blocks the binary once: right-click it in Finder and choose Open, or run `xattr -d com.apple.quarantine "$(command -v srvm)"`. Windows SmartScreen shows "Windows protected your PC": pick More info, then Run anyway. Every archive carries a `.sha256` sidecar and a GitHub artifact attestation, so `gh attestation verify <archive> --repo thecont1/srvm` confirms what you downloaded without trusting the download itself.

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

M0–M7 are implemented and merged (`main` is `507d1c8`, from PR #5). The release-candidate phase is under way: the installer's checksum gap on stock macOS is closed by [`tools/install.sh`](tools/install.sh), which supplies the `sha256sum` the installer looks for so that a tampered download is refused instead of warned through; the `RC verify` workflow installs the *published* artifacts into a throwaway prefix and smokes them on all six native hosts; and crates.io publication runs through a manual, token-based workflow until the crate exists and trusted publishing can take over. The external channels exist as empty repositories (`thecont1/homebrew-srvm`, `thecont1/scoop-bucket`) and stay dormant while `publish-prereleases = false`. What still gates `v0.1.0-rc.1`: a green `RC verify` run against the published artifacts, the dogfood pass recorded on the RC binary, winget validation on both Windows architectures, and the explicit tag approval. No release is published yet — see [`PLAN.md`](PLAN.md) for the gates and approval sequence.

## Acknowledgements

Detection design inspired by [px0](https://github.com/px0-ai/px0) (MIT), whose `serve.go` rule table this project ports and extends.

## License

MIT — see [LICENSE](LICENSE).
