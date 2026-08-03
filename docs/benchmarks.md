# Benchmarks — `myagent` on SWE-bench Verified

This page documents the only benchmark number we publish, how it was produced, and what it
does and does not mean. If you only read one line:

> **`myagent` driving `deepseek-v4-pro` resolved a median of 17/30 (56.7%) on a 30-instance
> subset of SWE-bench Verified, across eight runs graded by the official SWE-bench Docker
> harness, with the grading test withheld from the engine. Individual runs ranged from 16 to
> 19 resolved (53.3%–63.3%).**

We quote the **median**, not our best run. A single run of this subset is worth ±3 instances
of noise, so any one number — including our 19/30 — would overstate what you should expect.

## What was measured

| | |
|---|---|
| Engine | `myagent` (this repository), Rust |
| Model | `deepseek-v4-pro`, temperature 0 |
| Task set | 30 instances of SWE-bench Verified across 8 repositories (see below) |
| Ruler | **No test leakage** — the engine got a working per-repo test environment and a generic "run the tests" affordance, but never saw the `FAIL_TO_PASS` grading test, and `test.patch` was never applied during a run |
| Grading | Official `swebench.harness.run_evaluation` in Docker |
| Run config | Offline, no web search, no memory, `--max-turns 40` |
| Runs | 8 graded runs between 2026-07-12 and 2026-07-15 |

### Results distribution

| Resolved / 30 | Runs |
|---|---|
| 16 | 3 |
| 17 | 2 |
| 18 | 2 |
| 19 | 1 |

Median 17 (56.7%) · mean 17.1 (57.1%) · range 16–19 (53.3%–63.3%).

### Per-repository breakdown (representative run, 17/30)

| Repository | Resolved / Total |
|---|---|
| pylint | 2 / 2 |
| flask | 1 / 1 |
| sympy | 4 / 5 |
| pytest | 3 / 4 |
| django | 6 / 10 |
| xarray | 1 / 3 |
| requests | 0 / 1 |
| sphinx | 0 / 4 |
| **Total** | **17 / 30** |

In that run, 25 of 30 instances produced a non-empty patch; of those, 17 were correct (68%
precision). 5 produced no patch at all — the engine explored but never committed to a fix.
Zero harness errors. Measured engine cost was roughly $3–6 for the full 30 instances.

## Limitations — read these before comparing

1. **This is a 30-instance subset, not the full 500.** It is not directly comparable to the
   public SWE-bench Verified leaderboard. Numbers on a hand-composed subset are always
   easier to move than numbers on the full set.
2. **The subset is host-friendly.** It deliberately excludes repositories with heavy C
   extensions (matplotlib, scikit-learn, astropy) that could not be built reliably in our
   host environment. Those are not obviously easier or harder, but their absence means the
   subset is not a uniform random sample.
3. **Composition:** 20 single-file and 10 multi-file gold patches across 8 repositories.
4. **The engine changed between runs.** The eight runs span four days of engine iteration,
   so they are replicates of a moving target rather than eight repeats of one frozen build.
   That is part of why we report a range and a median rather than a single figure.
5. **The score is the engine *and* the model together.** A different model behind the same
   engine will score differently. `deepseek-v4-pro` is a mid-tier model by design — the point
   of this number is "a mid-tier, pay-as-you-go model is enough for everyday work," not "we
   beat frontier agents."

For reference, frontier-model agents score in the 60–70%+ range on the *full* 500-instance
set. A mid-tier model at 56.7% on this subset is a credible result for its tier and price,
and nothing more than that.

## Checking our work

We publish the exact 30 instance IDs in [`evals/swebench/fair30_ids.json`](../evals/swebench/fair30_ids.json),
and the full method above: official SWE-bench-Verified instances, unmodified gold tests,
official Docker grading, temperature 0, and the model named for each run.

We deliberately do **not** ship a one-click reproduction script. Our runner is a set of
working scripts wired to one specific host (local per-repo virtualenvs, machine-specific
paths, a particular Docker setup), and we have not verified them end-to-end on a clean
machine — shipping them as a "recipe" would waste your time more than it would help.
To check our number, run the same 30 instances through your own harness (or through
AgentLoom) with the model above and official grading.

## Why we publish this at all

Plenty of agent projects publish a benchmark number with no method, no subset disclosure and
no way to check it. We would rather publish a smaller, honest number with the exact subset
and the full method disclosed. If you run the same 30 instances and get something materially
different, please open an issue — that is useful to us.
