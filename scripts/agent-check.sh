#!/usr/bin/env bash
# Local quality gate for the agent stack.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "==> cargo fmt --check"
cargo fmt --all -- --check

echo "==> agent stack tests"
cargo test -p nars-agent-ir \
           -p nars-model \
           -p nars-tools \
           -p nars-session \
           -p nars-agent-runtime \
           -p nars-server \
           -p nars-eval

echo "==> deterministic evaluation"
cargo run -p nars-eval -- \
  --provider scripted \
  --out target/eval-report.json \
  --markdown-out target/eval-report.md

if command -v npm >/dev/null 2>&1; then
  echo "==> web build"
  (cd web && npm run build)
else
  echo "==> npm not found; skipping web build"
fi

if [[ "${1:-}" == "--full" ]]; then
  echo "==> full workspace release tests"
  cargo test --workspace --release
fi

echo "agent check passed"
