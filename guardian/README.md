# Edge Conservation Guardian

Track serverless function resource usage. Enforce cold-start budgets. Find waste before your users do.

## What it does

Guardian profiles your Cloudflare Worker across two dimensions:

1. **Init phase** — what happens at cold start (eager loads, config parsing, WASM init)
2. **Per-request** — wall time, CPU time, fetch calls, KV reads/writes

Then it runs heuristics to find waste:

- Init code that runs every cold start but only 5% of requests need it
- Oversized WASM bundles or config blobs
- Sequential `fetch()` calls that should be parallel
- KV reads for data that hasn't changed
- Sparse key access (loading 200 keys, using 3)

And prints a report:

```
═══ Edge Conservation Report ═══

Cold-start: 340ms (budget: 400ms) ✅ within budget
  Global init loads 2.0 MB across 1 resources.

Requests profiled: 1
  Avg wall time:   12.0ms
  Avg fetch calls: 0.0
  Total KV reads:  3 | writes: 0
  KV access: 3/200 keys (2%)

⚠️  1 waste(s) detected:

1. [sparse-kv-access] Loaded 200 KV keys but only accessed 3 (2%) per request.
   → Load only the keys you need. Use a manifest index and lazy-load the rest on demand.
   Impact: Loading 3 instead of 200 keys could save ~50432KB per request.

── Projected Savings ──
Addressing the above could reduce cold-start by ~85ms.
```

## Usage

```rust
use guardian::{EdgeBudget, Profiler, Detector, Report};

// Define your budget
let budget = EdgeBudget::builder()
    .cold_start_ms(400)
    .memory_mb(128)
    .cpu_time_per_request_ms(50)
    .subrequests(50)
    .build();

let mut profiler = Profiler::new(&budget);

// --- Init phase ---
profiler.record_init_start();
// load your config, init WASM, etc.
profiler.record_eager_load(2 * 1024 * 1024); // 2 MB config file
profiler.record_init_end();

// --- Request handler ---
profiler.record_request_start();

// ... your handler logic ...
profiler.record_fetch_call();
profiler.record_kv_read_key("config:feature-flags", 2048);
profiler.record_kv_write(512);

profiler.record_request_end();

// --- Analyze ---
let detector = Detector::new(&profiler, &budget);
let wastes = detector.detect();

let report = Report::generate(&profiler, &budget, &wastes);
println!("{}", report);
```

## Presets

```rust
// Default Cloudflare Worker limits
let budget = EdgeBudget::default();

// Tight latency endpoints
let budget = EdgeBudget::strict();   // 100ms cold start, 64MB, 10ms CPU

// Heavy processing
let budget = EdgeBudget::generous(); // 800ms cold start, 256MB, 150ms CPU
```

## Detection heuristics

| Waste | What it finds |
|---|---|
| `eager-init` | Resources loaded at cold start but rarely used |
| `oversized-bundle` | Init loading >2 MB of data |
| `sequential-fetches` | Fetch calls spaced >5ms apart (not parallelized) |
| `unchanged-kv-reads` | KV reads returning the same value as last time |
| `sparse-kv-access` | Loading many KV keys, accessing few |
| `cold-start-over-budget` | Init exceeds budget |
| `cpu-time-over-budget` | Request CPU exceeds budget |
| `memory-over-budget` | Memory exceeds budget |
| `subrequest-over-budget` | Fetch count exceeds budget |

## Running tests

```bash
cd guardian
cargo test
```

## Why

Cold starts are the silent killer of serverless UX. You write a Worker, it's fast locally, then in production it takes 400ms because you loaded a 2 MB config file into memory at startup for feature flags that only 3 out of 200 are ever read.

Guardian makes that visible. Instrument once, get a report, fix the waste.

## License

MIT OR Apache-2.0
