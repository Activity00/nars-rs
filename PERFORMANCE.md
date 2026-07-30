# Performance baseline

All measurements below use the release profile on the local development
machine. They are regression baselines, not cross-machine performance claims.
Short runs vary with host scheduling, so system-test timings should be compared
across multiple warm runs before accepting or rejecting an optimization.

## Throughput harness

Run:

```bash
cargo run --release -p nars-core --example throughput -- 1000000 10000
```

The harness measures:

- empty working cycles;
- a text-input closed loop containing state, goal and operation execution;
- the same closed loop with sentences parsed once and reused;
- isolated present-belief input;
- isolated goal input.

Representative warm result on 2026-07-13:

```text
idle_cycles=1000000 elapsed_seconds=0.017318 cycles_per_second=57744639
text_agent_steps=10000 elapsed_seconds=1.476167 steps_per_second=6774
prepared_agent_steps=10000 elapsed_seconds=1.454800 steps_per_second=6874 concepts=3
belief_inputs=10000 elapsed_seconds=0.263093 inputs_per_second=38009
goal_inputs=10000 elapsed_seconds=0.015451 inputs_per_second=647200
```

`Reasoner::add_parsed_sentence` is intended for stable real-time sensor and goal
templates. The prepared sentence must come from the same reasoner's `Narsese`
instance because atom identifiers are interned per instance. For very small
atomic sentences, parsing is not the dominant closed-loop cost; operation
feedback, temporal learning and decision processing dominate.

## Upstream system loops

Representative release results:

| Test | Scale | Warm elapsed time |
| --- | ---: | ---: |
| Cartpole | 10,001 steps | 30.36 s |
| Alien | 20,001 steps | 30.30 s |
| Bandrobot | 10,001 steps | 0.10 s |

These tests execute the upstream-compatible environment logic and policy loop,
so they are the authoritative end-to-end regression checks. The synthetic
throughput example is useful for separating idle, input and decision costs but
does not replace them.

The July compatibility work added per-cycle declarative implication
processing and bounded operation-free goal lookahead. These account for most
of the increase from the earlier ~15 s Cartpole baseline. Learned state-link
lookahead is restricted to compact contingency tables and direct operation
reachability; removing those bounds roughly doubled Cartpole again and was
rejected. Further optimization should index fresh declarative premises and
reuse bounded decision-search scratch storage without weakening these
semantics.

The completed example-compatibility pass keeps new closure paths bounded.
Ordinary semantic inference still examines at most 80 related concepts per
selected belief. Revised-property relational closure considers at most 16
product relations, same-property event classification considers at most four
exemplars, and variable-goal grounding searches the inverted-atom neighborhood
rather than all 4096 concept slots. These bounds are required for the
10,001-step control tests and should be preserved by future refactors.

`scripts/evaluate_nal_examples.py` now runs the pinned upstream executable as
the truth baseline. This avoids optimizing toward stale comment annotations and
keeps correctness checks separate from throughput measurements.

## Optimization policy

Performance changes must preserve the fixed ONA behavior and system-test
outcomes. Prefer bounded reusable storage, single-pass selection, prepared
inputs and removal of provably discarded work. Reject changes that only look
allocation-efficient but regress the warm Cartpole or Alien loops.
