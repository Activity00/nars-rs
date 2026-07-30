# Upstream compatibility contract

## Baseline

- Project: OpenNARS for Applications (ONA)
- Repository: https://github.com/opennars/OpenNARS-for-Applications
- Commit: `dc4efd0a` (`Update: accelerated negation handling for implication tables`)
- Commit date: 2025-05-16
- License: MIT
- Snapshot: `upstream/ona`

The Rust implementation targets behavioral compatibility with this exact
revision before intentionally introducing extensions.

## Test policy

The upstream suite consists of C unit tests, system tests, Narsese examples,
and Python-driven evaluation tests. Each upstream assertion must either:

1. run as an equivalent native Rust test;
2. run as a black-box differential test against the upstream executable; or
3. be explicitly classified as an environment integration test.

The upstream UDP loopback test currently fails in the local sandbox after all
preceding in-memory tests pass. It is therefore tracked separately as a network
integration test; it is not removed from the compatibility target.

## Compatibility layers

1. Data structures: Truth, Stamp, Term, Event, tables, indices and queues.
2. Language: complete Narsese parsing, normalization and printing.
3. Logic: generated NAL rule table and all truth functions.
4. Runtime: Memory, Cycle, Decision, operations and shell behavior.
5. Systems: examples, simulated agents, UDP NAR and evaluation metrics.

