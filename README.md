# nars-rs

Rust implementation of OpenNARS for Applications (ONA), aimed at real-time
reasoning and embeddable agent systems.

中文读者可从 [中文入门与实现指南](docs/PROJECT_GUIDE_ZH.md) 开始，内容涵盖
OpenNARS/ONA 生态、Narsese、推理机制、系统架构、源码导览、运行方式与二次开发。

The compatibility baseline is upstream ONA commit `dc4efd0a` from 2025-05-16.
The original source snapshot is kept under `upstream/ona` so every ported rule
and test can be checked against a fixed, auditable reference.

## nars-agent

`nars-agent` is a development agent built on top of `nars-core`. NARS owns goals,
facts, priorities, historical tool outcomes, and the next-action decision. A model
only produces a validated **Agent IR** proposal; a policy-checked tool broker
performs real work. The model can never call a tool directly. Tools can be built-in, MCP stdio, or
MCP Streamable HTTP. Sessions persist as JSONL or SQLite. The web UI can switch
between all runs or a single run, including runs restored after a restart. Sessions
can be branched (with recorded lineage, a nested branch tree, and parent
navigation), renamed, and deleted. Branch runs can be compared with their
parent source run across status, tools, fallbacks, files, and final answer,
with selectable runs on both sides. Non-Rust projects verify through a
policy-gated `run_command` and workspace verification discovery. Runs in the
same session receive a bounded transcript of earlier completed turns, and each
workspace keeps a durable NARS memory (concepts, beliefs, learned implications)
that is loaded at run start and written back when a run finishes. Agent-level
facts (tool outcomes, plan progress, user preferences) are promoted into a
cross-workspace profile and seeded into every run; the web UI can inspect both
through the Memory panel. Tool outcomes are learned back into NARS memory and
can override the model's preferred candidate or introduce a tool the model did
not propose; the eval reports the override rate, the NARS-pickup count, and a
model-top baseline.

```text
user message → model (Agent IR) → Rust validation → NARS goals/facts
             → NARS decision → approval → tool broker → observation
             → NARS facts → next step
```

- Architecture decisions:
  [0001](docs/decisions/0001-agent-architecture.md),
  [0002](docs/decisions/0002-agent-ir.md),
  [0003](docs/decisions/0003-tool-protocol.md),
  [0004](docs/decisions/0004-task-plan.md),
  [0005](docs/decisions/0005-mcp.md),
  [0006](docs/decisions/0006-evaluation.md),
  [0007](docs/decisions/0007-session-branching.md),
  [0008](docs/decisions/0008-session-lifecycle.md),
  [0009](docs/decisions/0009-run-comparison.md),
  [0010](docs/decisions/0010-generic-verification.md),
  [0011](docs/decisions/0011-session-context.md),
  [0012](docs/decisions/0012-long-term-memory.md),
  [0013](docs/decisions/0013-memory-maintenance.md),
  [0014](docs/decisions/0014-profile-memory.md),
  [0015](docs/decisions/0015-decision-learning.md)
- Architecture overview: [docs/architecture/overview.md](docs/architecture/overview.md)
- Event protocol: [docs/protocol/events.md](docs/protocol/events.md)
- 中文快速开始: [docs/agent/QUICKSTART_ZH.md](docs/agent/QUICKSTART_ZH.md)

```bash
# 1. Offline demo (no API key): demo provider, real read-only tool loop
cargo run -p narsd -- --approval ask

# 2. Real model (OpenAI-compatible endpoint)
NARS_AGENT_PROVIDER=openai NARS_AGENT_API_KEY=sk-... NARS_AGENT_MODEL=gpt-4.1-mini \
  cargo run -p narsd

# 3. Ollama
NARS_AGENT_PROVIDER=ollama NARS_AGENT_MODEL=qwen3 cargo run -p narsd

# 4. Web UI in development (proxies /api to :8787)
cd web && npm install && npm run dev     # http://localhost:5173

# 5. Or build the UI once and let narsd serve it at :8787
cd web && npm run build && cd .. && cargo run -p narsd

# 6. SQLite session storage
cargo run -p narsd -- --sqlite .nars-agent/sessions.db
```

Run the deterministic evaluation set (fixtures, scripted model, real runtime):

```bash
cargo run -p nars-eval -- --provider scripted \
  --out target/eval-report.json --markdown-out target/eval-report.md
```

Quality gates: `./scripts/agent-check.sh` runs fmt, agent tests, the deterministic
evaluation, and the web build; `--full` adds all workspace release tests. CI uses
[.github/workflows/nars-agent.yml](.github/workflows/nars-agent.yml).

The focused test suite for the agent stack:

```bash
cargo test -p nars-agent-ir -p nars-model -p nars-tools \
           -p nars-session -p nars-agent-runtime -p nars-server
```

## Current status

The foundational truth-value, evidential-stamp, term encoding, Narsese parsing,
variable system, bounded queues/tables, concept memory, atom/time indices,
event/implication inference, and the complete 235-rule/22-reduction declarative
NAL table are implemented. The integrated reasoner performs multi-cycle
inference, learns temporal procedures from event streams, registers Agent
operation callbacks, selects actions through recursive subgoals, and revises
procedures with ONA-compatible negative anticipation when expected outcomes do
not occur. Deterministic motor babbling, operation-event feedback, and closed
loop policy learning are implemented for online Agent exploration. The
continuous Cartpole control loop also runs at the upstream 10,001-step
evaluation scale with positive and negative feedback. The 20,001-step Alien
tracking environment exercises competing left/right/shoot operations and
hit-driven learning. Pong and Pong2 run their upstream 10,001-step evaluations,
including rally resets and the three-action left/right/stop variant. The
runtime now supports configured motor-babbling arguments and passes concrete
`({SELF} * argument)` products to callbacks. Bandrobot runs its 10,001-step
numeric perception and four-action delivery loop. The Robot evaluation covers
a 41x21 room map, eight headings, sector vision, collision-safe forward motion,
dynamic food and reward feedback over 1,200 steps. The remaining control
policies include a deterministic Testchamber harness covering six locations,
three switches, two linked lights, a gated door, and multi-step autonomous
goals. The runtime also provides raw UDP transport, a continuously cycling
`UdpNar` service with clean thread shutdown, NUL-terminated Narsese datagram
compatibility, and StatsD counter output. A usable interactive shell and
`UDPNAR` command are included, with passive querying of eternal beliefs,
temporal events, predictions, and implication tables. Advanced shell commands,
along with deeper control/memory compatibility, are still being ported. Active
questions now bind variables across learned subgoals into concrete operation
arguments, while goal processing consumes the bounded 30-layer queue and
decomposes sequence goals by their first unfulfilled component. Runtime Shell
commands configure decision and anticipation thresholds, exploration,
question priming, evidence stamps, numeric spaces, concept restrictions, and
operation slots without restarting the process. Statistics, concepts, event
queues, semantic/time indexes, and operation configuration are exposed as
structured snapshots and through their corresponding Shell inspection commands.
Stable real-time sensor and goal templates can be parsed once and submitted
through `Reasoner::add_parsed_sentence`, avoiding text processing in the
control-loop API while retaining normal input semantics.
Safe fixed-capacity stack and explicitly chained hash-table equivalents cover
the remaining upstream low-level allocation, collision and recycling tests
without carrying C pointer-pool behavior into the public runtime.
Procedural implication storage is partitioned by registered operation slot as
in ONA, keeping table zero for temporal implications without an operation.
Derived compound knowledge follows ONA's existing-concept filters, while
continuous numeric properties are kept out of the temporal occurrence/mining
path to bound irrelevant real-time correlations.
Derived rule conclusions also pass ONA's structural validity filters before
entering Memory, covering nested higher-order statements, variable occurrence,
junction shape, set operations, and operation-bearing declarative conditions.
Cycle-level higher-order special inference, per-cycle declarative implication
application, negative-goal outcome suppression, relational-image question
composition, and bounded operation-free goal-chain search are also implemented.
Temporal variable introduction transfers learned procedures to unseen operation
arguments, operation feedback runs through ONA-compatible nested cycles, and
negative confirmations are applied immediately while their source spikes remain
active. Same-property exemplar classification, revised-property relational
closure, overlap-safe relational queries, and concrete-first variable-goal
planning cover the remaining category, relational-network, and active-question
behaviors. The executable upstream audit now matches all 138 annotated
expectations across 57 `.nal` files. The evaluator uses the pinned upstream
binary as the authoritative truth baseline because several example comments are
stale; `map2.nal` is recorded as an upstream-equivalent fixed-capacity rejection.

```bash
cargo test --workspace
cargo run -p nars-cli
cargo run --release -p nars-core --example throughput -- 1000000 10000
```

See [UPSTREAM.md](UPSTREAM.md) for the compatibility contract and test policy.
See [COMPATIBILITY.md](COMPATIBILITY.md) for the current upstream-test matrix.
See [PERFORMANCE.md](PERFORMANCE.md) for the reproducible throughput and
system-loop baselines.
