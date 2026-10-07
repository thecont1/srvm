# srvm

**`cd` into any repository, type `srvm`, and your project is running.** No
reading the repo's docs for the right command, no remembering whether this
one wanted `bun run dev` or `cargo run` — `srvm` figures it out, installs
what's missing, picks a free port, and hands you the URL.

Built for the era of AI-generated code: you can produce a full-stack repo in
seconds. You shouldn't need DevOps knowledge to run it.

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

- **Finds your app** — even when a repo holds several (`frontend/` +
  `backend/`, `apps/*`, `packages/*`), and starts all of them with their own
  ports and labels.
- **Knows your stack** — Node, Python, Rust, Go, Ruby, PHP, Elixir, and more,
  detected from the files you already have. It never runs your scripts to
  figure this out; it issues the same command you would have typed.
- **Installs what's missing** — `npm install`, a Python `.venv` + `pip`,
  `bundle install`… whatever the project expects, in the places it expects.
- **Reads `.env` for you** — without touching variables you've already set.
- **Picks a free port** — and if the app lands somewhere else anyway, it
  tells you the real URL.
- **Fetches runtimes** — no `node` or `python`? srvm downloads an official,
  checksum-verified build and uses it just for your app.
- **Serves plain static sites itself** — no runtime needed at all.
- **Cleans up fully** — Ctrl+C stops the whole tree, nothing leaks.

## Install

```console
$ cargo install srvm
```

That's the whole thing if you have Rust. Other channels:

```console
# Homebrew
$ brew install thecont1/srvm/srvm

# Scoop (Windows)
$ scoop bucket add srvm https://github.com/thecont1/scoop-bucket
$ scoop install srvm

# winget (Windows)
$ winget install thecont1.srvm

# Prebuilt binary without a toolchain (macOS/Linux front door —
# checksum verification is enforced even where sha256sum is missing)
$ curl -sSfL https://raw.githubusercontent.com/thecont1/srvm/main/tools/install.sh | sh

# Windows (PowerShell)
$ powershell -ExecutionPolicy Bypass -c "irm https://github.com/thecont1/srvm/releases/latest/download/srvm-installer.ps1 | iex"
```

**Unsigned binaries:** the project has no signing certificate, so the first
run is gated by the OS. On macOS, right-click → Open (or
`xattr -d com.apple.quarantine "$(command -v srvm)"`); on Windows, SmartScreen
→ More info → Run anyway. Every download is SHA-256 verified during install,
and `gh attestation verify <archive> --repo thecont1/srvm` proves provenance.

## Use

```console
$ srvm              # run everything this directory contains
$ srvm ~/some/repo  # or any directory
$ srvm --dry-run    # show what it would launch, run nothing
$ srvm --no-open    # don't open the app in a browser
$ srvm --port 4000  # start the free-port search at 4000
$ srvm --port 0     # let the OS pick
$ srvm --select apps/web:package:dev  # run exactly one app
$ srvm --no-install # never install anything
```

Every flag is an escape hatch, never a requirement — the default is to just
work.

## How it works

Detection, discovery bounds, bootstrap rules, port arbitration, runtime
fetching, the built-in static server, process teardown, and the release
pipeline are documented in [`TECH-SPEC.md`](TECH-SPEC.md). The milestone
gate ledger is in [`PLAN.md`](PLAN.md).

## License

MIT — see [LICENSE](LICENSE).
