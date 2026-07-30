# ONA compatibility matrix

Baseline: OpenNARS for Applications commit `dc4efd0a`.

Status meanings:

- **Pass**: equivalent Rust behavior is covered by a passing test.
- **Partial**: the original assertion is covered, but the surrounding module is
  not yet complete.
- **Pending**: not yet ported or verified.

## Unit tests

| Upstream test | Status | Rust coverage |
|---|---:|---|
| Stamp_Test | Pass | stamp zip, overlap, duplicate and equality tests |
| PriorityQueue_Test | Pass | capacity, rejection, eviction, min/max tests |
| Table_Test | Pass | 120-entry ordering and implication revision |
| Memory_Test | Pass | both upstream input/conceptualize/find assertions are ported directly |
| OccurrenceTimeIndex_Test | Pass | 512-entry rollover and newest lookup |
| InvertedAtomIndex_Test | Pass | shared-atom chains and removal |
| Narsese_Test | Pass | upstream complex normalization/printing example |
| RuleTable_Test | Pass | upstream two-belief cycle passes and additionally proves the syllogistic conclusion; 235/22 counts match |
| Stack_Test | Pass | safe fixed-capacity LIFO covers pointer order, empty state, overflow and underflow |
| HashTable_Test | Pass | bounded chained table covers forced collisions, chain order, update, deletion, recycling and capacity |
| UDP_Test | Pass | ephemeral loopback port preserves the exact NUL-terminated datagram |

## System tests

| Upstream area | Status | Rust coverage |
|---|---:|---|
| Multistep / Multistep2 | Pass | ports both upstream schedules, including recursively chaining procedures learned more than 1000 cycles apart |
| Procedure learning | Pass | ports the upstream single-observation procedure test and executes the registered callback for the resulting goal |
| Sequence | Pass | ports the upstream success/failure schedule and selects `^1` for `a+b`, `^2` for `b`, and `^3` for `a` using negative anticipation |
| Alphabet | Pass | ports the complete 50-tick periodic input schedule and verifies cycle progression |
| Follow | Pass | deterministic motor babbling learns the two-action closed-loop policy and reaches upstream score 500 while preserving both safety assertions |
| Cartpole | Pass | runs the upstream 10,001-step continuous dynamics, 8-bin angle encoding, positive/negative `good` feedback, and two-action control loop |
| Alien | Pass | runs the upstream 20,001-step target tracking loop with `r0/l0/c0`, three competing operations, target relocation, and hit feedback |
| Bandrobot | Pass | runs the 10,001-step numeric-position delivery loop; parameterized operation construction, exploration arguments, callback products, and concrete feedback events are covered |
| Pong / Pong2 | Pass | runs both upstream 10,001-step rally environments, including resets, hit feedback, `ball_equal`, and left/right/stop control |
| Robot | Pass | runs the upstream 1,200-step 41x21 room environment with eight headings, three-sector vision, walls, doors, food spawning, collision-safe forward motion, and `eaten` goals |
| Testchamber | Pass | replaces the infinite keyboard loop with a deterministic script covering six positions, three switches, two lights, door gating, demonstrations, and autonomous multi-step goals |
| UDPNAR | Pass | receiver and cycle threads process the three upstream datagrams and execute `^left` |

## Network and metrics

- Raw UDP sender/receiver APIs preserve caller-supplied bytes, including the
  optional trailing NUL used by ONA.
- `UdpNar` owns a shared reasoner, independently cycling and receiving input,
  with deterministic shutdown and thread joining.
- Network input supports blank-cycle input, integer cycle counts, comments,
  `*reset`, beliefs, and goals.
- ONA's Graphite/StatsD counter format, `<path>:<value>|c`, is covered by an
  encoding and loopback-send test.

## Module-level audits beyond the upstream unit tests

- Memory now partitions procedural implications into the registered
  operation's one-based table, while non-operation temporal implications remain
  in table zero.
- Memory now applies ONA's existing-concept requirement to derived image,
  intersection, difference and temporal/dependent conjunction terms, and
  updates both source and target usage when storing implications.
- Continuous property events remain available as current beliefs but are
  excluded from occurrence indexing and temporal procedure mining.
- Temporal procedures containing a compound operation owned by another agent
  are rejected unless the operation subject is `{SELF}` or a variable, matching
  ONA's executable-operation gate and its target-concept side effect.
- Continuous-property beliefs whose value is a similarity or sequence remain
  available as current beliefs but are excluded from the cycling belief queue,
  matching ONA's specialized real-time filtering.
- The declarative rule engine interprets the fixed upstream `NAL.h` rather than
  generated C. Its derived-event path now applies ONA's filters for repeated
  atoms, nested higher-order statements, dependent variables in inheritance or
  similarity, right-nested junctions, invalid set operations, singleton
  independent/dependent variables, invalid higher-order variable introduction,
  and declarative conjunctions with a leftmost operation.
- Cycle-level special inference covers higher-order deduction, deduction with
  a remaining conjunctive condition, abduction, and anonymous analogy. The
  declarative implication cycle also applies recent premises each cycle, which
  covers the upstream penguin exception, numbers, asthma, and relational
  network examples.
- Temporal mining applies variable introduction before indexing learned
  procedures. Variable procedures are indexed separately from generic
  single-input procedures and instantiate concrete operation arguments for
  previously unseen objects.
- Operation execution clears superseded goals, injects operation feedback, and
  performs ONA's nested cycle before the outer decision cycle continues.
  Negative anticipation evidence is applied immediately, including repeated
  operation-free confirmation while the source belief spike remains active.
- Declarative and relational closure is bounded: ordinary semantic matching
  remains capped at 80 concepts, revised property events inspect at most 16
  relational products, and event classification uses at most four exact-property
  exemplars.
- Exact-property exemplar matching reconstructs an independent category
  observation from revised evidence and reproduces ONA's online duck/swan
  classification truth values. Relational category answers reject overlapping
  premises before combining stamps.
- Decision scoring uses the submitted goal truth and rejects operations whose
  combined recent goal outcomes have expectation below 0.5. Derived ordinary
  events may serve as temporal procedure contexts, matching ONA's event-stream
  learning behavior.
- ONA's `validation_concept` pointer/ID pair guards the OpenMP interval between
  lock-free rule calculation and the critical Memory write. Rust rule
  calculation and Memory insertion occur inside one exclusive `&mut Reasoner`;
  `UdpNar` serializes access through its reasoner mutex, so concept recycling
  cannot occur in that interval. A matching ID guard will be required if rule
  calculation is later made concurrent.
- Remaining rule audit work concerns broader example-suite output equivalence,
  not the original `RuleTable_Test`.

## Shell and questions

- The CLI now runs an interactive shell instead of a placeholder and accepts
  Narsese, decimal cycle counts, comments, `*reset`, and `quit`.
- `UDPNAR IP PORT timestep_ns` starts the network reasoner and accepts local
  shell input until EOF or `quit`.
- Passive questions search eternal beliefs, current/past event spikes, future
  predictions, declarative implications, and procedural implications.
- Query variables are unified against stored candidates and temporal answers
  are ranked using truth projected to the current reasoner time.
- Relational-image questions can compose a stored product relation with the
  properties describing one argument without retaining the otherwise-filtered
  intersection/image intermediates. This covers all 24 expected answers in
  `categoryformationanddetection.nal` and `example1.nal`.
- Active questions propagate variable-bearing subgoals through learned
  procedures and substitute their bindings into operation arguments; the
  upstream bedroom `^go` then `^say` scenario is covered.
- Variable goals first search the bounded inverted-atom neighborhood for the
  newest concrete matching belief. Fully grounded decision candidates win
  otherwise-equivalent comparisons, preventing execution of unresolved
  operation arguments such as `^go($1)`.
- Goal events are consumed from ONA's 30 bounded layers. No-operation
  implications derive goals into the next layer, and sequence goals advance to
  the first component whose recent belief does not satisfy the threshold.
- Recursive decision search crosses explicit operation-free state chains and
  performs inherited-property matching for relational contexts. Learned
  operation-free lookahead is bounded to compact contingency tables and paths
  that lead directly to an operation, preventing unbounded expansion in
  real-time control loops. The upstream `toothbrush`, `plandeep`, and
  `marshmallow` examples pass this path.
- Runtime Shell configuration now covers motor-babbling probability and
  operation limit, decision threshold, anticipation confidence, question
  priming, numeric similarity distance, restricted concept creation, current
  time, evidence IDs/imports, numeric values/spaces, operation names,
  arguments/stdin flags, `*query`, and `*concurrent`.
- `*stats`, `*concepts`, belief/goal queue dumps, inverted atom and occurrence
  time index dumps, and `*opconfig` return lock-independent structured
  snapshots which the CLI renders in ONA-compatible text form.

## Current Rust gates

```text
cargo test --workspace: 109 passed (105 core + 4 CLI), 0 failed
cargo test --release -p nars-core: 105 passed, 0 failed
cargo clippy --workspace --all-targets -- -D warnings: passed
ONA declarative rule count: 235
ONA reduction count: 22
NAL example expectations: 138/138 across 57 example files
```

The audit compares Rust answers and truth values with the pinned upstream
binary rather than relying solely on example comments, which are stale in
several files. `map2.nal` exceeds ONA's 64-slot heap term representation; both
binaries reject it, and the evaluator records that result as upstream-equivalent
instead of reproducing an out-of-bounds write.
