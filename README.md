# srvm

**`srvm` launches an app from within any project's directory or repo.** 
No reading the repo's docs for the right command, no remembering whether 
this one wanted `bun run dev` or `cargo run`. Let `srvm` figure it out, 
install whatever is missing, pick a free port on localhost, and load it
in your browser. Give it a second, maybe?

Built for the era of AI-generated code: you can produce a full-stack repo in
seconds. You shouldn't need DevOps knowledge to run it.

## What it looks like

### A single-app repo

The common case — one project at the root, one URL back.

```console
$ cd ~/projects/my-app && srvm
  srvm 0.1.2
  workspace  ~/projects/my-app
  serve      npm run dev (npm)
  step       installing dependencies — npm install
  port       5173
  step       starting — npm run dev --port 5173
  app        http://localhost:5173

  ctrl-c to stop
```

### A repo with a frontend and a backend

Each app gets its own label, port, and URL. srvm installs both before
launching either, and stops both on Ctrl+C.

```console
$ cd ~/projects/ai-app && srvm
  srvm 0.1.2
  workspace  ~/projects/ai-app
  serve      [frontend] npm run dev (npm)
  serve      [backend] python3 manage.py runserver (python)
  step       [frontend] installing dependencies — npm install
  step       [backend] installing dependencies — .venv/bin/pip install -r requirements.txt
  port       [frontend] 5173
  port       [backend] 8000
  step       [frontend] starting — npm run dev --port 5173
  step       [backend] starting — python3 manage.py runserver 8000
  app        [frontend] http://localhost:5173
  app        [backend] http://localhost:8000

  ctrl-c to stop
```

### A directory with nothing to run

srvm reports the directory and stops — no panic, no stack trace. It only
runs what it can positively identify as a project.

```console
$ cd ~/projects/empty && srvm
Error: no servable app detected in /Users/home/projects/empty
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
- **Fetches runtimes** — no `node`, `python`, `go`, or `cargo` on your
  machine? srvm downloads an official, checksum-verified build and uses it
  just for your app.
- **Serves plain static sites itself** — no runtime needed at all.
- **Cleans up fully** — Ctrl+C stops the whole tree, nothing leaks.

## Install

```console
# Any platform with a Rust toolchain (1.89 or newer; `rustup update` if you have an older one)
$ cargo install srvm
```

Other channels:

```console
# macOS / Linux — Homebrew
$ brew install thecont1/srvm/srvm

# Windows — Scoop
$ scoop bucket add srvm https://github.com/thecont1/scoop-bucket
$ scoop install srvm

# Windows — winget
$ winget install thecont1.srvm

# macOS / Linux — prebuilt binary, no toolchain
# (checksum verification is enforced even where sha256sum is missing)
$ curl -sSfL https://raw.githubusercontent.com/thecont1/srvm/main/tools/install.sh | sh

# Windows — PowerShell
$ powershell -ExecutionPolicy Bypass -c "irm https://github.com/thecont1/srvm/releases/latest/download/srvm-installer.ps1 | iex"
```

The binaries are unsigned. Installs via `cargo`, `brew`, `scoop`, or the
shell installer are unaffected; if you download an archive through a browser
or install via PowerShell, the OS may gate the first run — on macOS run
`xattr -d com.apple.quarantine "$(command -v srvm)"`, on Windows pick
More info → Run anyway in the SmartScreen dialog. Every archive carries a
`.sha256` sidecar for end-to-end integrity checks. Build-provenance
attestation (`gh attestation verify <archive> --repo thecont1/srvm`) is
also published for the build pipeline — that one needs the `gh` CLI signed
in and is intended for maintainers and auditors, not for everyday use.

## Use

```console
$ srvm                       # run everything this directory contains
$ srvm <path>                # run a different directory
$ srvm --dry-run             # show what it would launch; run nothing
$ srvm --no-open             # don't open the app in a browser
$ srvm --port <N>            # start the free-port search at N (0 = OS picks a free one)
$ srvm --select <id>         # run exactly one app: 1-based number, qualified id, or unique name
$ srvm --no-install          # never install anything; assume the project is already set up
$ srvm -v / --verbose        # show more diagnostic detail
$ srvm --quiet               # suppress non-essential output
$ srvm --no-color            # disable colored output
$ srvm --all                 # alias for the default set
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
