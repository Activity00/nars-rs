# nars-agent evaluations

The evaluation set is intentionally small and deterministic. Phase 1–5 verify
the control loop; Phase 6–8 use these tasks to measure whether NARS actually
adds value over a pure model loop.

## Running

```bash
cargo run -p nars-eval -- --provider scripted \
  --out target/eval-report.json --markdown-out target/eval-report.md
```

- `scripted` uses each task's `script` array for deterministic control-flow tests.
- `demo` uses the offline demo provider.
- `env` uses `NARS_AGENT_PROVIDER` for real-model evaluation.

The runner auto-approves pending calls by default; `deny_approvals=true` rejects them.
`pause_before_start=true` exercises pause/resume.

## Task format

```json
{
  "id": "rename_symbol",
  "category": "coding_tasks",
  "prompt": "Rename alpha to beta in src/lib.rs and run the tests.",
  "setup": {"fixture": "rename_symbol"},
  "limits": {"max_steps": 8, "max_model_calls": 6},
  "expect": {
    "final_answer_contains": ["beta"],
    "files_modified": ["src/lib.rs"],
    "tools_used": ["read_file", "apply_patch", "cargo_test"],
    "approval_required": true
  }
}
```

## Metrics per task

- completed / failed / denied;
- tool calls;
- model calls;
- NARS decisions and fallback count;
- errors and retries;
- approvals;
- incorrect modifications;
- successful resume after interruption;
- replay hash equality.

## Categories

| Directory | Purpose |
|---|---|
| `coding_tasks/` | read, search, patch, cargo check/test |
| `nars_tasks/` | tool failure recovery, priority, multi-step ordering |
| `recovery/` | pause, cancel, crash/restart, snapshot restore |
| `regression/` | approval denial, path escape, invalid IR |

## Status

The runner is implemented in `apps/nars-eval`; fixtures live in
`evals/fixtures`. The current set has six tasks: rename, compile-error repair,
plan ordering, tool-failure recovery, pause/resume, and approval denial.
Reports are written outside the repository by default (`target/eval-report.json`).
