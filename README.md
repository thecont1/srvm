# srvm

**The zero-config universal app launcher.** `cd` into any repository, type `srvm`, and your app is running — runtime detection, dependency install, and port conflicts handled for you.

Built for the era of AI-generated code: you can produce a full-stack repo in seconds, you shouldn't need DevOps knowledge to run it.

```console
$ srvm
  srvm 0.1.0
  workspace  ~/projects/ai-app
  serve      next dev (node)
  installing npm install …
  app        http://localhost:3000

  ctrl-c to stop
```

## What it does

- **Detects** your stack from marker files — `package.json`, `deno.json`, `Makefile`, `manage.py`, `Gemfile`, `Cargo.toml`, `go.mod`, `compose.yaml`, and 15 more. Never evaluates your scripts; it issues the same command you would have typed.
- **Installs** missing dependencies (`npm install`, etc.) when the workspace lacks them.
- **Arbitrates ports** — if the port is taken, srvm shifts to the next free one and injects it the way each framework actually understands (`--port`, `-p`, `-a 127.0.0.1:<n>`, `runserver <n>`, …). Unknown commands get a best-effort `PORT` env var instead — but only when a start port exists (a `--port` flag, an inherited numeric `PORT`, or a known framework hint); srvm never invents a port for a fully opaque script. srvm never kills whatever is holding a port, and if the app still binds elsewhere the sniffed URL wins and srvm says so.
- **Finds your toolchain** even when it's not on `PATH` — bun, deno, pnpm, volta, asdf, mise, pyenv, rbenv shims are all searched.
- **Cleans up completely** — Ctrl+C signals the whole process group, so `npm → sh → node` grandchildren can't leak.

## Install

```console
$ cargo install srvm        # once published
```

Prebuilt binaries for macOS, Linux, and Windows ship with each release. `srvm` itself is a single static binary with no runtime requirements.

## Usage

```console
$ srvm              # detect and launch the app in this directory
$ srvm ~/some/repo  # or any directory
$ srvm --dry-run    # show what would be launched, don't launch it
$ srvm --no-open    # don't open the app in a browser
$ srvm --port 4000  # start the free-port search at 4000
$ srvm --port 0     # let the OS pick any free port
```

Port notes: `--port` sets where the free-port *search* starts, not a hard requirement — a busy port just shifts the app forward. `--dry-run` reports the planned start and injection without probing or binding anything. srvm's port reservations and fallback HTTP probes use IPv4 loopback (`127.0.0.1`); the app still controls its own bind address. There is a small race between srvm releasing its probe listener and the app binding: a non-cooperative app can still lose that race and fail, and when the app instead binds somewhere else successfully the URL it actually prints is adopted and reported — verification reconciles the outcome, it can't eliminate the race.

## Status

Early development — see [`PLAN.md`](PLAN.md) for the product development plan and milestone roadmap.

## Acknowledgements

Detection design inspired by [px0](https://github.com/px0-ai/px0) (MIT), whose `serve.go` rule table this project ports and extends.

## License

MIT — see [LICENSE](LICENSE).
