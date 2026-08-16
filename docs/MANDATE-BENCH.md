# Mandate solver load benchmark

Measures whether the synchronous Mandate pathfinder becomes a Tokio runtime
bottleneck as liquidity size and concurrency grow. No production behaviour was
changed to produce these numbers.

Reproduce:

```sh
./scripts/mandate-bench.sh                              # worst-case matrix + timeout experiment
BASE_TOKENS=4 OUT=/tmp/mandate-bench-prod ./scripts/mandate-bench.sh   # production-shaped matrix
./scripts/mandate-bench-probe.sh                        # isolation + out-of-process /healthz probe
```

## Environment

| | |
|---|---|
| Machine | Apple M4 Pro (Mac16,7), 14 cores, 24 GB, macOS 26.1 |
| Build | `--release`, rustc 1.97.1 |
| Server | `solvers ... --log=warn --max-concurrent-requests 32 --request-timeout 10s` |
| Client | `crates/solvers/examples/mandate_bench.rs`, same machine, loopback |

Client and server share the machine, so client CPU is included in contention.
Absolute RPS is therefore a lower bound; the comparisons between rows are the
point, not the absolute throughput.

## Fixture

`POST /mandate/solve` with a constant intent (1 WETH → BUY, floor 1 wei, one
allowlisted router) and a liquidity set of `N` constant-product pools: 2 direct
WETH/BUY pools plus, for each intermediate token, a WETH→T and a T→BUY pool.
`max-hops = 1`.

Two config shapes, because they turn out to measure different things:

- **Worst case** — every intermediate is a configured base token, so all `N`
  entries sit on a candidate path (5000 entries → 2499 base tokens → 2500 path
  candidates per request).
- **Production-shaped** (`BASE_TOKENS=4`) — a fixed 4-token base set, as a real
  config has. The extra entries are still parsed, converted, and allowlist
  filtered; they just are not path candidates.

## Results — production-shaped config (4 base tokens)

| entries | client concurrency | success | 503 | 504 | other | RPS | p50 | p95 | p99 | max | server cores |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 10 | 1 | 2000 | 0 | 0 | 0 | 13973 | 0.06 ms | 0.11 ms | 0.14 ms | 0.28 ms | 0.7 |
| 10 | 32 | 4000 | 0 | 0 | 0 | 122956 | 0.23 ms | 0.42 ms | 0.59 ms | 1.41 ms | 3.7 |
| 100 | 1 | 1000 | 0 | 0 | 0 | 4674 | 0.21 ms | 0.24 ms | 0.29 ms | 0.34 ms | 0.8 |
| 100 | 32 | 2000 | 0 | 0 | 0 | 44410 | 0.64 ms | 1.14 ms | 2.13 ms | 4.93 ms | 5.9 |
| 500 | 32 | 600 | 0 | 0 | 0 | 10818 | 2.67 ms | 4.49 ms | 5.82 ms | 7.42 ms | 7.1 |
| 1000 | 32 | 300 | 0 | 0 | 0 | 5506 | 5.10 ms | 8.54 ms | 10.72 ms | 12.14 ms | 6.8 |
| 5000 | 32 | 100 | 0 | 0 | 0 | 976 | 26.66 ms | 43.31 ms | 50.18 ms | 67.40 ms | 7.1 |
| 500 | **128** (limit 32) | 405 | 168 | 0 | 27 | 13742 | 7.11 ms | 13.33 ms | 17.80 ms | 19.43 ms | 6.4 |

No 400s and no 413s in any scenario (payloads are valid and 1.8 MB at 5000
entries, well under the 10 MB limit). The 27 "other" are client-side connection
errors, discussed below.

## Results — worst-case config (base tokens grow with the fixture)

| entries | base tokens | client concurrency | success | 503 | 504 | other | RPS | p50 | p95 | p99 | max | server cores |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 10 | 4 | 1 | 2000 | 0 | 0 | 0 | 14809 | 0.06 ms | 0.09 ms | 0.12 ms | 0.27 ms | 0.6 |
| 10 | 4 | 32 | 4000 | 0 | 0 | 0 | 128796 | 0.22 ms | 0.39 ms | 0.59 ms | 1.69 ms | 3.8 |
| 100 | 49 | 1 | 1000 | 0 | 0 | 0 | 2485 | 0.40 ms | 0.45 ms | 0.48 ms | 0.65 ms | 0.9 |
| 100 | 49 | 32 | 2000 | 0 | 0 | 0 | 24276 | 1.22 ms | 2.14 ms | 2.91 ms | 3.58 ms | 9.2 |
| 500 | 249 | 32 | 600 | 0 | 0 | 0 | 1789 | 16.06 ms | 26.16 ms | 29.81 ms | 35.76 ms | 11.0 |
| 1000 | 499 | 32 | 300 | 0 | 0 | 0 | 413 | 69.51 ms | 123.56 ms | 156.14 ms | 168.01 ms | 10.2 |
| 5000 | 2499 | 32 | 100 | 0 | 0 | 0 | 4.2 | 6836.94 ms | 10104.89 ms | 10420.24 ms | 10421.53 ms | 5.9 |
| 500 | 249 | **128** (limit 32) | 237 | 212 | 0 | 151 | 4239 | 11.24 ms | 41.10 ms | 62.78 ms | 80.89 ms | 8.7 |

## What actually costs the time

Two controls separate parsing from path finding. Both use the same server and
the same client.

| scenario | base tokens | payload entries | concurrency | p50 | `/healthz` p99 (separate process) |
|---|---|---|---|---|---|
| idle | 4 | — | — | — | 0.72 ms |
| parse-heavy | 4 | 5000 (1.8 MB) | 1 | 8.85 ms | 0.61 ms |
| parse-heavy | 4 | 5000 (1.8 MB) | 32 | 15.99 ms | 5.00 ms |
| route-heavy | 2499 | 10 (4 KB) | 1 | 508.31 ms | 516.24 ms |
| route-heavy | 2499 | 10 (4 KB) | 32 | 7068.23 ms | 9491.40 ms |
| full | 2499 | 5000 | 1 | 521.78 ms | 0.50 ms |
| full, allowlist matches nothing | 2499 | 5000 | 1 | 533.57 ms | 0.49 ms |

Parsing a 1.8 MB body costs ~9 ms. A 4 KB body against 2499 base tokens costs
~508 ms. The cost tracks the number of **path candidates**, which is the base
token count, not the size of the liquidity payload.

The "allowlist matches nothing" control is *not* faster (533 vs 522 ms): with an
empty liquidity set the router still enumerates and walks all 2500 candidates,
it just finds nothing at each one. The expense is candidate enumeration itself,
not pool math.

## Runtime responsiveness

`/healthz` sits outside the Mandate concurrency limiter. Probed once at a time
from a separate `curl` process, so the load client's own runtime cannot confound
it:

| state | `/healthz` p50 | p99 | max |
|---|---|---|---|
| idle | 0.41 ms | 0.72 ms | 0.72 ms |
| parse-heavy load, c=32 | 0.35 ms | 5.00 ms | 5.00 ms |
| production-shaped, 5000 entries, c=32 | 6.50 ms | 17.10 ms | — |
| route-heavy, **c=1** | 0.34 ms | 516.24 ms | 526.28 ms |
| route-heavy, c=32 | 0.43 ms | 9491.40 ms | 9491.40 ms |

The c=1 row is the important one. **A single** in-flight CPU-bound solve, on a
14-core machine with 13 idle workers, pushes health-check p99 to 516 ms — almost
exactly one solve duration. The median stays at 0.34 ms, so it is a tail effect:
most probes land on a free worker, and the occasional probe that lands behind
the busy one waits for the entire solve, because a synchronous poll is not
preemptible and a task already parked on that worker cannot always be stolen.

At c=32 (more CPU-bound requests than cores) the effect stops being a tail: p99
9.5 s against solves that take ~7 s.

## Timeout experiment

Worst-case config, 5000 entries, `--request-timeout 200ms`, client concurrency 8:

| requests | 504 | 200 | p50 | p99 |
|---|---|---|---|---|
| 48 | **0** | 48 | 1409 ms | 1847 ms |

**Every request returned 200, ~7× past its deadline. Not one 504.** The layer
ordering is correct and the unit tests pass; the deadline simply cannot fire.
`tower::timeout` checks its timer only after polling the inner future, and the
handler completes body parse and route finding in a single uninterrupted poll,
so it returns `Ready` before the timer is ever consulted. The deadline is only
enforceable where the handler actually awaits.

Consequences measured:

- CPU consumed in the 3 s after the last response: **0.00 s**. Nothing runs on
  past the response — the work finishes first, then the response goes out late.
- A lightweight request right after the burst: p50 0.12 ms. No lingering
  degradation.

So the failure mode is not "abandoned work keeps burning CPU". It is "the
request deadline silently does not apply to the requests that need it most."

## Scaling behavior

With base tokens fixed (production shape), latency against liquidity entries at
c=32 is 0.23 → 0.64 → 2.67 → 5.10 → 26.66 ms for 10 → 100 → 500 → 1000 → 5000
entries: roughly linear in entries, at a low slope, and server CPU stays flat at
~7 of 14 cores.

With base tokens growing (worst case), the same axis gives 0.22 → 1.22 → 16.06 →
69.51 → 6836.94 ms. The last step is far steeper than the 5× change in input:
per-request CPU rises from ~0.5 s at c=1 to ~1.4 s at c=32 for the same work, so
that final jump is contention, not just per-request cost. This is a small
benchmark on one machine — it says the growth is steep, not what its complexity
class is.

Overload behaves as designed in both shapes: at client concurrency 128 against a
limit of 32, requests are shed with 503 rather than queued, and p50 for the
requests that are served stays in line with the c=32 row.

## Known measurement gaps

- 27–151 requests per 128-concurrency run failed client-side with reqwest
  "request" errors (0 status). The server logs show only the 503 classifications
  — no panics, no 500s. The likely mechanism is the server answering 503 before
  the client finished uploading its body, turning into a reset on the client;
  this was not confirmed at packet level.
- Client and server share the machine.
- Only constant-product liquidity is exercised. Weighted, stable, and
  concentrated pools have different per-pool math.
- No flamegraph. Which part of candidate enumeration dominates is inferred from
  the controls above, not from a profiler.
