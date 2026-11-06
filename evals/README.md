# nars-agent evaluations

The evaluation set is intentionally small and deterministic. Phase 1–5 verify
the control loop; Phase 6–8 use these tasks to measure whether NARS actually
adds value over a pure model loop.

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

The harness is not wired yet. The integration tests in
`crates/nars-agent-runtime/tests/agent_loop.rs` and
`crates/nars-server/tests/api.rs` cover the same mechanisms with scripted
providers; a runner will consume these JSON files in Phase 8.
