# Foldex benchmarks

A repeatable comparison of Foldex against other Windows file managers. The
suite is built so that adding a competitor or a new scenario means adding one
file, not rewriting the plan.

**Status:** plan. Nothing here has been measured yet. The figures in
`web/src/data/benchmarks.ts` are placeholders until a run under
[`results/`](results/README.md) replaces them.

## What we compare

| ID | App | Role |
| :--- | :--- | :--- |
| `explorer` | [Windows 11 File Explorer](apps/explorer.md) | Baseline, default settings |
| `foldex` | [Foldex](apps/foldex.md) | Ours |

Other file managers can be added later. See [Adding a file manager](#adding-a-file-manager).

## What we measure

| ID | Scenario | Headline metric |
| :--- | :--- | :--- |
| `S1` | [Startup](scenarios/S1-startup.md) | Launch → folder fully listed |
| `S2` | [Folder listing](scenarios/S2-folder-listing.md) | Navigate → all items listed (100 to 1M) |
| `S3` | [Stress](scenarios/S3-stress.md) | Longest UI stall |
| `S4` | [Resource use](scenarios/S4-resources.md) | Private bytes, idle CPU, leak slope |
| `S5` | [Search and filtering](scenarios/S5-search-filter.md) | Keystroke → filtered list; recursive search complete |

Network locations are out of scope for this suite.

## Layout

```
benchmarks/
  README.md          this index
  methodology.md     fairness rules, environment, cache states, statistics
  metrics.md         every metric, its ID and exact definition
  fixtures.md        the generated test corpus
  harness.md         the measuring tool (fxbench): observers, adapters, results
  apps/              one file per file manager (the adapter contract)
    _template.md
  scenarios/         one file per scenario family
    _template.md
  results/           one folder per run: machine, versions, raw runs, summary
```

## Adding a file manager

1. Copy [`apps/_template.md`](apps/_template.md) to `apps/<id>.md`.
2. Fill in every section. Anything you have not confirmed on a real machine
   gets a ⚠ mark and an entry under **Verify in pilot**.
3. Give each scenario a readiness signal for the app, or mark it `N/A` with
   the reason ("no current-folder filter"). Never fill a gap with a guess.
4. Add the app to the table above and to `apps.toml` in the harness once it
   exists ([harness.md](harness.md#adapters)).
5. Run the pilot checks ([methodology.md](methodology.md#pilot)) before the
   app appears in any published result.

## Adding a scenario

1. Copy [`scenarios/_template.md`](scenarios/_template.md).
2. Use metric IDs from [metrics.md](metrics.md). Add a new metric there first if
   none fits, so every scenario uses one shared vocabulary.
3. Use fixtures from [fixtures.md](fixtures.md) or add a new fixture set there
   with a generator rule and a ground-truth manifest.
4. List what each app needs (command, signal, or `N/A`) in the scenario's
   per-app table.

## Run order

1. **Pilot** – confirm every ⚠ capability in `apps/*.md`; check the readiness
   signals against screen capture ([methodology.md](methodology.md#pilot)).
2. **Fixtures** – `fxbench fixtures generate` on the benchmark volume.
3. **Run** – `fxbench run --suite full`, with all apps interleaved.
4. **Summarize** – `fxbench summarize results/<run>`.
5. **Publish** – `fxbench export web results/<run>` updates
   `web/src/data/benchmarks.ts`, including the machine and method text.
