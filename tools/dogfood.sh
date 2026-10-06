#!/usr/bin/env bash
# Dogfood pass: run srvm against real-world repository shapes and record
# friction. Fixtures prove the machine; this proves the promise.
#
# Usage: tools/dogfood.sh /path/to/srvm [outdir]
#
# Shapes (each: --dry-run purity check, then a real boot, then Ctrl+C):
#   1. fullstack — frontend/ + backend/ with no lockfile or node_modules
#   2. static — a bare HTML directory
#   3. rust-cli — a Cargo binary that serves to prove it compiled
#   4. django — manage.py + requirements.txt
#   5. monorepo — root orchestrator over two conventional sub-apps (--all)
#   6. dotenv-sample — a repo with only .env.example
set -uo pipefail

SRVM="${1:?usage: dogfood.sh /path/to/srvm [outdir]}"
ROOT="${2:-$(mktemp -d /tmp/srvm-dogfood.XXXXXX)}"
mkdir -p "$ROOT"
PASS=0
FAIL=0
DEADLINE_DRY=30
DEADLINE_BOOT=240

log() { echo "  $*" >> "$ROOT/summary.log"; }

# --- shape constructors -----------------------------------------------------

shape_fullstack() {
  local dir="$1"
  mkdir -p "$dir/frontend" "$dir/backend"
  cat > "$dir/frontend/package.json" <<'EOF'
{"name":"frontend","scripts":{"dev":"node index.js"}}
EOF
  cat > "$dir/frontend/index.js" <<'EOF'
const http = require("http");
const s = http.createServer((q, r) => r.end("frontend"));
s.listen(Number(process.env.PORT || 0), "127.0.0.1",
  () => console.log(`ready on http://127.0.0.1:${s.address().port}/`));
EOF
  cat > "$dir/backend/requirements.txt" <<'EOF'
Flask>=3.0,<4
EOF
  cat > "$dir/backend/app.py" <<'EOF'
from flask import Flask
app = Flask(__name__)

@app.route("/")
def index():
    return "backend"
EOF
}

shape_static() {
  local dir="$1"
  mkdir -p "$dir"
  echo '<html>dogfood-static</html>' > "$dir/index.html"
}

shape_rust_cli() {
  local dir="$1"
  mkdir -p "$dir/src"
  cat > "$dir/Cargo.toml" <<'EOF'
[package]
name = "dogfood"
version = "0.1.0"
edition = "2021"
EOF
  cat > "$dir/src/main.rs" <<'EOF'
use std::io::{Read, Write};
use std::net::TcpListener;
fn main() {
    let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(0);
    let l = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let p = l.local_addr().unwrap().port();
    println!("ready on http://127.0.0.1:{p}/");
    loop {
        let (mut s, _) = l.accept().unwrap();
        std::thread::spawn(move || {
            let mut b = [0u8; 512];
            let _ = s.read(&mut b);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\ncli");
        });
    }
}
EOF
}

shape_django() {
  local dir="$1"
  mkdir -p "$dir/mysite"
  echo 'Django>=5.2,<5.3' > "$dir/requirements.txt"
  cat > "$dir/manage.py" <<'EOF'
#!/usr/bin/env python
import os, sys

def main():
    os.environ.setdefault("DJANGO_SETTINGS_MODULE", "mysite.settings")
    from django.core.management import execute_from_command_line
    execute_from_command_line(sys.argv)

if __name__ == "__main__":
    main()
EOF
  cat > "$dir/mysite/__init__.py" <<'EOF'
EOF
  cat > "$dir/mysite/settings.py" <<'EOF'
SECRET_KEY = "dogfood"
DEBUG = True
ALLOWED_HOSTS = ["*"]
ROOT_URLCONF = "mysite.urls"
INSTALLED_APPS = ["django.contrib.staticfiles"]
MIDDLEWARE = []
DEFAULT_AUTO_FIELD = "django.db.models.BigAutoField"
STATIC_URL = "static/"
EOF
  cat > "$dir/mysite/urls.py" <<'EOF'
from django.http import HttpResponse
from django.urls import path

urlpatterns = [path("", lambda request: HttpResponse("django"))]
EOF
}

shape_monorepo() {
  local dir="$1"
  shape_fullstack "$dir"
}

shape_dotenv_sample() {
  local dir="$1"
  mkdir -p "$dir"
  cat > "$dir/.env.example" <<'EOF'
PORT=3000
EOF
}

# --- runner -----------------------------------------------------------------

snapshot() { # dir -> sorted file list on stdout
  (cd "$1" && find . -type f | sort)
}

with_timeout() { # seconds cmd...
  local secs="$1"; shift
  "$@" &
  local pid=$!
  (sleep "$secs"; kill "$pid" 2>/dev/null) &
  local killer=$!
  wait "$pid"
  local code=$?
  kill "$killer" 2>/dev/null
  wait "$killer" 2>/dev/null
  return "$code"
}

run_shape() { # name [extra srvm args...]
  local name="$1"; shift
  local dir="$ROOT/$name"
  local log="$ROOT/$name.log"
  : > "$log"
  echo "== $name ==" | tee -a "$ROOT/summary.log"

  local before after
  before="$(snapshot "$dir")"
  if ! with_timeout "$DEADLINE_DRY" "$SRVM" --dry-run "$@" "$dir" > "$log" 2>&1; then
    log "FAIL: dry-run errored ($(tail -1 "$log"))"; FAIL=$((FAIL+1)); return
  fi
  after="$(snapshot "$dir")"
  if [ "$before" != "$after" ]; then
    log "FAIL: dry-run modified the repo"; FAIL=$((FAIL+1)); return
  fi
  log "dry-run pure and clean"

  "$SRVM" --no-open --no-color "$@" "$dir" > "$log" 2>&1 &
  local pid=$!
  local url=""
  for _ in $(seq 1 $((DEADLINE_BOOT * 2))); do
    url="$(grep -o 'http://127.0.0.1:[0-9]*' "$log" | tail -1 || true)"
    [ -n "$url" ] && break
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.5
  done
  if [ -z "$url" ]; then
    log "FAIL: no URL announced; tail: $(tail -8 "$log" | tr '\n' '|')"; FAIL=$((FAIL+1))
    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
    return
  fi
  if ! curl -fsS --max-time 5 "$url" | grep -q .; then
    log "FAIL: announced $url but GET failed"; FAIL=$((FAIL+1))
    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
    return
  fi
  log "served at $url"

  kill -INT "$pid" 2>/dev/null
  local waited=0
  while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 20 ]; do
    sleep 0.25; waited=$((waited+1))
  done
  if kill -0 "$pid" 2>/dev/null; then
    log "FAIL: srvm survived Ctrl+C"; FAIL=$((FAIL+1)); kill -9 "$pid" 2>/dev/null
    return
  fi
  log "Ctrl+C teardown clean"; PASS=$((PASS+1))
}

main() {
  shape_fullstack "$ROOT/fullstack"
  shape_static "$ROOT/static"
  shape_rust_cli "$ROOT/rust-cli"
  shape_django "$ROOT/django"
  shape_monorepo "$ROOT/monorepo"
  shape_dotenv_sample "$ROOT/dotenv-sample"

  run_shape fullstack
  run_shape static
  run_shape rust-cli
  run_shape django
  run_shape monorepo --all

  # The .env.example-only repo: no apps to boot; srvm must explain itself and
  # exit non-zero rather than hang or panic.
  echo "== dotenv-sample ==" | tee -a "$ROOT/summary.log"
  local log="$ROOT/dotenv-sample.log"
  if with_timeout "$DEADLINE_DRY" "$SRVM" "$ROOT/dotenv-sample" > "$log" 2>&1; then
    log "FAIL: bare .env.example-only repo exited 0"; FAIL=$((FAIL+1))
  elif grep -q "panicked" "$log"; then
    log "FAIL: srvm panicked on a config-only repo"; FAIL=$((FAIL+1))
  elif [ -s "$log" ]; then
    log "explained itself and exited non-zero ($(head -1 "$log"))"; PASS=$((PASS+1))
  else
    log "FAIL: empty output"; FAIL=$((FAIL+1))
  fi

  echo "PASS=$PASS FAIL=$FAIL log=$ROOT/summary.log"
  [ "$FAIL" -eq 0 ]
}

main "$@"
