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
- **Arbitrates ports** — if the default port is taken, srvm shifts to the next free one and injects it into the app's environment. No prompts, no errors.
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
$ srvm --port 4000  # start arbitration at a specific port
```

## Status

Early development — see [`PLAN.md`](PLAN.md) for the product development plan and milestone roadmap.

## Acknowledgements

Detection design inspired by [px0](https://github.com/px0-ai/px0) (MIT), whose `serve.go` rule table this project ports and extends.

## License

MIT — see [LICENSE](LICENSE).
