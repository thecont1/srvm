---
name: srvm
description: Invariants, locked product decisions, and verification gates for srvm — the zero-config universal app launcher. Use whenever implementing, reviewing, or changing this repository.
---

# srvm Working Contract

Zero-config universal app launcher: a single Rust binary. `cd` into any repo, type `srvm`, app is running.

The approved execution plan lives at `~/.devin/plans/plan-3b58f317421f618d.md` — it is the source of truth for sequencing and scope. PLAN.md documents the architecture and milestone gates.

## North Star

After `cd`, bare `srvm` runs the project — zero decisions. Every flag is an escape hatch, never a requirement. When two options are equal, pick the one that removes a user decision over the one that adds a flag. Reliability is part of simplicity: invisible machinery (runtime cache, process teardown) is a release blocker, not polish.

## Invariants — never violate

- **Detection is pure and read-only.** Marker files only; never evaluate repository scripts during detection.
- **Discovery is bounded to conventional locations**: selected root; `frontend`, `backend`, `client`, `server`, `web`, `api`; immediate children of `apps`, `packages`, `services`. No symlink traversal, nothing outside the canonical workspace.
- **Repo writes are bootstrap-only.** `.venv`, `node_modules`, `vendor/`, `deps/` and similar conventional untracked dirs; never touch tracked source files.
- **`.env` is parse-only** (no eval), injected for unset vars only — OS env wins. `BROWSER=none` is always injected into children.
- **No config files, daemons, telemetry, or arbitrary script evaluation.**
- **Exit honesty.** 130 on signal shutdown; original nonzero error on failure; nothing leaked.

## Locked Product Decisions

- Bare `srvm` launches the whole independent set when >1 app exists; `--all` is a compat alias; `--select` picks one.
- Dedup key: (canonical app root, ecosystem family) — two JS apps in different dirs both launch; overlapping routes in one dir do not.
- A recognized root orchestrator runs alone under the default set; sub-apps stay selectable.
- Installs run before port reservation; reservations live only across the immediate spawn handoff.
- Six native release targets with execution tests: macOS/Linux/Windows × x64/ARM64. Linux = GNU/glibc.
- All channels ship together at v0.1 (GitHub archives, shell+PowerShell installers, crates.io, Homebrew tap, Scoop bucket, winget); `v0.1.0-rc.1` gates stable.
- Proposed MSRV `rust-version = "1.89"` (enables `File::try_lock`); adjust only from evidence.

## Workflow

1. Write a failing regression before every fix; observe it fail.
2. Per-tranche gates, all must pass:
   - `cargo fmt --check`
   - `cargo clippy --locked --all-targets -- -D warnings`
   - `cargo test --locked`
   - `cargo build --locked --release`
   - `git diff --check`
3. Windows parity is real: `.cmd`/PATHEXT spawning, process trees, console Ctrl+C all have platform consequences. Never ship `cfg(unix)`-only logic that leaves Windows silently broken.
4. Three design sub-gates must be settled before implementing them: the shared-JS-workspace declaration parser, Windows Job Objects (only if regressions demand), and bootstrap stamp formats.

## Authority Boundaries

- Feature branch, logical commits, push only when asked.
- Never tag, publish, create secrets/taps/buckets, or submit external manifests without explicit approval. Plan gates are checklists, not standing permission.
- No publication claims before artifacts exist; no invented URLs.
