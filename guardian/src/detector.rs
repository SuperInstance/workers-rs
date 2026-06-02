//! Waste detection heuristics.
//!
//! The detector looks at profiler data through the lens of a budget and
//! produces a list of [`Waste`] items — concrete things you can fix.

use crate::budget::EdgeBudget;
use crate::profiler::Profiler;

/// Category of detected waste.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WasteKind {
    /// Init code that runs on every cold start but is only needed by a small
    /// fraction of requests.
    EagerInit,
    /// The WASM bundle (or a large data file) is bigger than necessary.
    OversizedBundle,
    /// Sequential `fetch()` calls that could be fired in parallel with
    /// `Promise::all` or similar.
    SequentialFetches,
    /// KV reads for data that hasn't changed since last read (or is rarely
    /// used).
    UnchangedKvReads,
    /// More KV keys loaded than actually accessed — indicates a sparse-access
    /// pattern.
    SparseKvAccess,
    /// Cold-start wall time exceeds the budget.
    ColdStartOverBudget,
    /// Per-request CPU time exceeds the budget.
    CpuTimeOverBudget,
    /// Subrequest count exceeds the budget.
    SubrequestOverBudget,
    /// Memory usage exceeds the budget.
    MemoryOverBudget,
}

impl std::fmt::Display for WasteKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WasteKind::EagerInit => write!(f, "eager-init"),
            WasteKind::OversizedBundle => write!(f, "oversized-bundle"),
            WasteKind::SequentialFetches => write!(f, "sequential-fetches"),
            WasteKind::UnchangedKvReads => write!(f, "unchanged-kv-reads"),
            WasteKind::SparseKvAccess => write!(f, "sparse-kv-access"),
            WasteKind::ColdStartOverBudget => write!(f, "cold-start-over-budget"),
            WasteKind::CpuTimeOverBudget => write!(f, "cpu-time-over-budget"),
            WasteKind::SubrequestOverBudget => write!(f, "subrequest-over-budget"),
            WasteKind::MemoryOverBudget => write!(f, "memory-over-budget"),
        }
    }
}

/// A single detected waste item with an actionable suggestion.
#[derive(Debug, Clone)]
pub struct Waste {
    pub kind: WasteKind,
    /// Human-readable description of what was detected.
    pub description: String,
    /// Suggested fix.
    pub suggestion: String,
    /// Estimated impact (e.g. "saves ~45ms per cold start").
    pub estimated_impact: String,
}

/// Runs all waste-detection heuristics.
pub struct Detector<'a> {
    profiler: &'a Profiler,
    budget: &'a EdgeBudget,
    /// Threshold: if fewer than this fraction of requests use an eagerly-loaded
    /// resource, flag it.
    sparse_threshold: f64,
    /// Threshold: if the init phase loads more than this many bytes, flag it.
    oversized_bundle_bytes: u64,
}

impl<'a> Detector<'a> {
    pub fn new(profiler: &'a Profiler, budget: &'a EdgeBudget) -> Self {
        Self {
            profiler,
            budget,
            sparse_threshold: 0.10,
            oversized_bundle_bytes: 2 * 1024 * 1024, // 2 MB
        }
    }

    /// Set the fraction threshold for sparse-access detection (default 0.10).
    pub fn with_sparse_threshold(mut self, t: f64) -> Self {
        self.sparse_threshold = t;
        self
    }

    /// Set the bundle-size threshold in bytes (default 2 MB).
    pub fn with_bundle_threshold(mut self, bytes: u64) -> Self {
        self.oversized_bundle_bytes = bytes;
        self
    }

    /// Run all detection heuristics and return the list of wastes found.
    pub fn detect(&self) -> Vec<Waste> {
        let mut wastes = Vec::new();

        self.detect_cold_start(&mut wastes);
        self.detect_eager_init(&mut wastes);
        self.detect_oversized_bundle(&mut wastes);
        self.detect_sequential_fetches(&mut wastes);
        self.detect_unchanged_kv(&mut wastes);
        self.detect_sparse_kv(&mut wastes);
        self.detect_cpu_over_budget(&mut wastes);
        self.detect_memory_over_budget(&mut wastes);
        self.detect_subrequest_over_budget(&mut wastes);

        wastes
    }

    // ---- individual heuristics ------------------------------------------

    fn detect_cold_start(&self, out: &mut Vec<Waste>) {
        let init = self.profiler.init();
        if init.wall_ms > self.budget.max_cold_start_ms {
            out.push(Waste {
                kind: WasteKind::ColdStartOverBudget,
                description: format!(
                    "Cold-start wall time is {}ms, budget is {}ms.",
                    init.wall_ms, self.budget.max_cold_start_ms
                ),
                suggestion: "Defer non-critical init to first-use. Lazy-load config, dictionaries, and large data structures.".into(),
                estimated_impact: format!(
                    "Reducing init by {}ms would bring you within budget.",
                    init.wall_ms.saturating_sub(self.budget.max_cold_start_ms)
                ),
            });
        }
    }

    fn detect_eager_init(&self, out: &mut Vec<Waste>) {
        let init = self.profiler.init();
        if init.eager_loads == 0 {
            return;
        }
        let total_requests = self.profiler.requests().len();
        if total_requests == 0 {
            return;
        }
        let keys_used: std::collections::HashSet<&str> = self
            .profiler
            .requests()
            .iter()
            .flat_map(|r| r.kv_keys_accessed.iter().map(|s| s.as_str()))
            .collect();

        // Only flag if there are more eager loads than distinct keys used
        // AND the ratio is below the sparse threshold
        if keys_used.is_empty() || init.eager_loads as usize <= keys_used.len() {
            return;
        }

        let usage_ratio = keys_used.len() as f64 / init.eager_loads as f64;

        if usage_ratio < self.sparse_threshold {
            out.push(Waste {
                kind: WasteKind::EagerInit,
                description: format!(
                    "Init eagerly loads {} resources, but only {} ({:.0}%) are used per request.",
                    init.eager_loads,
                    keys_used.len(),
                    usage_ratio * 100.0
                ),
                suggestion: "Switch to lazy loading. Wrap expensive init in `std::sync::OnceLock` or `lazy_static!` so it only runs when first needed.".into(),
                estimated_impact: format!(
                    "Deferring {} unused loads could save ~{}ms per cold start.",
                    init.eager_loads - keys_used.len() as u32,
                    init.wall_ms / 2
                ),
            });
        }
    }

    fn detect_oversized_bundle(&self, out: &mut Vec<Waste>) {
        let init = self.profiler.init();
        if init.bytes_loaded > self.oversized_bundle_bytes {
            let mb = init.bytes_loaded as f64 / (1024.0 * 1024.0);
            out.push(Waste {
                kind: WasteKind::OversizedBundle,
                description: format!(
                    "Init loads {:.1} MB of data. Large bundles increase cold-start time and memory pressure.",
                    mb
                ),
                suggestion: "Split data into small hot paths and lazy-loaded cold paths. Consider compressing static data or moving it to KV/R2.".into(),
                estimated_impact: format!(
                    "Shaving {:.0}% could save ~{}ms cold-start time.",
                    50.0,
                    init.wall_ms / 2
                ),
            });
        }
    }

    fn detect_sequential_fetches(&self, out: &mut Vec<Waste>) {
        let total_chains: u32 = self
            .profiler
            .requests()
            .iter()
            .map(|r| r.sequential_fetch_chains)
            .sum();

        let total_fetches: u32 = self
            .profiler
            .requests()
            .iter()
            .map(|r| r.fetch_calls)
            .sum();

        if total_chains > 0 && total_fetches > 1 {
            let avg_chain = total_chains as f64 / self.profiler.requests().len() as f64;
            out.push(Waste {
                kind: WasteKind::SequentialFetches,
                description: format!(
                    "{} sequential fetch chains detected across {} requests (avg {:.1}/req). Total fetches: {}.",
                    total_chains,
                    self.profiler.requests().len(),
                    avg_chain,
                    total_fetches
                ),
                suggestion: "Fire independent fetches concurrently using `Promise::all` or `futures::join!`. Only chain when request B depends on response A.".into(),
                estimated_impact: format!(
                    "Parallelizing could cut fetch latency by ~{}% for independent calls.",
                    50
                ),
            });
        }
    }

    fn detect_unchanged_kv(&self, out: &mut Vec<Waste>) {
        let total_unchanged: u32 = self
            .profiler
            .requests()
            .iter()
            .map(|r| r.kv_unchanged_reads)
            .sum();

        let total_reads: u32 = self
            .profiler
            .requests()
            .iter()
            .map(|r| r.kv_reads)
            .sum();

        if total_unchanged > 0 && total_reads > 0 {
            let ratio = total_unchanged as f64 / total_reads as f64;
            if ratio > 0.3 {
                out.push(Waste {
                    kind: WasteKind::UnchangedKvReads,
                    description: format!(
                        "{}/{} KV reads ({:.0}%) returned unchanged data.",
                        total_unchanged,
                        total_reads,
                        ratio * 100.0
                    ),
                    suggestion: "Cache KV values in-memory with a TTL. Skip re-reading keys that haven't expired. Consider using KV `cacheTtl` option.".into(),
                    estimated_impact: format!(
                        "Eliminating {} redundant reads would save ~{}ms per request.",
                        total_unchanged,
                        total_unchanged * 2 // rough estimate: ~2ms per KV read
                    ),
                });
            }
        }
    }

    fn detect_sparse_kv(&self, out: &mut Vec<Waste>) {
        // Look at requests that set kv_keys_total
        for req in self.profiler.requests() {
            if req.kv_keys_total > 0 && !req.kv_keys_accessed.is_empty() {
                let accessed = req.kv_keys_accessed.len() as u32;
                let ratio = accessed as f64 / req.kv_keys_total as f64;
                if ratio < 0.15 && req.kv_keys_total > 20 {
                    out.push(Waste {
                        kind: WasteKind::SparseKvAccess,
                        description: format!(
                            "Loaded {} KV keys but only accessed {} ({:.0}%) per request.",
                            req.kv_keys_total,
                            accessed,
                            ratio * 100.0
                        ),
                        suggestion: "Load only the keys you need. Use a manifest index and lazy-load the rest on demand.".into(),
                        estimated_impact: format!(
                            "Loading {} instead of {} keys could save ~{}KB per request.",
                            accessed,
                            req.kv_keys_total,
                            (req.kv_keys_total - accessed) * 256 // rough: 256 bytes/key
                        ),
                    });
                }
            }
        }
    }

    fn detect_cpu_over_budget(&self, out: &mut Vec<Waste>) {
        for (i, req) in self.profiler.requests().iter().enumerate() {
            if req.cpu_ms > self.budget.max_cpu_time_per_request_ms {
                out.push(Waste {
                    kind: WasteKind::CpuTimeOverBudget,
                    description: format!(
                        "Request #{} used {}ms CPU time, budget is {}ms.",
                        i + 1,
                        req.cpu_ms,
                        self.budget.max_cpu_time_per_request_ms
                    ),
                    suggestion: "Profile hot paths. Consider caching computation results, reducing JSON parsing, or offloading heavy work to Durable Objects.".into(),
                    estimated_impact: format!(
                        "Cutting CPU time by {}ms meets the budget.",
                        req.cpu_ms.saturating_sub(self.budget.max_cpu_time_per_request_ms)
                    ),
                });
            }
        }
    }

    fn detect_memory_over_budget(&self, out: &mut Vec<Waste>) {
        let init = self.profiler.init();
        if init.peak_memory_mb > self.budget.max_memory_mb {
            out.push(Waste {
                kind: WasteKind::MemoryOverBudget,
                description: format!(
                    "Init peak memory is {}MB, budget is {}MB.",
                    init.peak_memory_mb, self.budget.max_memory_mb
                ),
                suggestion: "Reduce in-memory data structures. Stream large payloads instead of buffering. Use KV or R2 for large datasets.".into(),
                estimated_impact: "Reducing memory avoids OOM kills and improves cold-start performance.".into(),
            });
        }

        for (i, req) in self.profiler.requests().iter().enumerate() {
            if req.peak_memory_mb > self.budget.max_memory_mb {
                out.push(Waste {
                    kind: WasteKind::MemoryOverBudget,
                    description: format!(
                        "Request #{} peak memory is {}MB, budget is {}MB.",
                        i + 1,
                        req.peak_memory_mb,
                        self.budget.max_memory_mb
                    ),
                    suggestion: "Avoid buffering entire response bodies. Stream processing or paginate large results.".into(),
                    estimated_impact: format!(
                        "Reducing by {}MB avoids memory pressure.",
                        req.peak_memory_mb.saturating_sub(self.budget.max_memory_mb)
                    ),
                });
            }
        }
    }

    fn detect_subrequest_over_budget(&self, out: &mut Vec<Waste>) {
        for (i, req) in self.profiler.requests().iter().enumerate() {
            if req.fetch_calls > self.budget.max_subrequests {
                out.push(Waste {
                    kind: WasteKind::SubrequestOverBudget,
                    description: format!(
                        "Request #{} made {} fetch calls, budget is {}.",
                        i + 1,
                        req.fetch_calls,
                        self.budget.max_subrequests
                    ),
                    suggestion: "Batch API calls. Use GraphQL field selection. Cache responses. Merge sequential fetches into one.".into(),
                    estimated_impact: format!(
                        "Reducing from {} to {} fetches stays within limits.",
                        req.fetch_calls,
                        self.budget.max_subrequests
                    ),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::EdgeBudget;

    fn make_profiler_with_init() -> Profiler {
        let budget = EdgeBudget::default();
        let mut p = Profiler::new(&budget);
        p.record_init_start();
        p.record_eager_load(3 * 1024 * 1024); // 3 MB — oversized
        p.record_eager_load(1024);
        p.record_init_end();
        p
    }

    #[test]
    fn detects_oversized_bundle() {
        let p = make_profiler_with_init();
        let budget = EdgeBudget::default();
        let d = Detector::new(&p, &budget);
        let wastes = d.detect();
        assert!(wastes.iter().any(|w| w.kind == WasteKind::OversizedBundle));
    }

    #[test]
    fn detects_sequential_fetches() {
        let budget = EdgeBudget::default();
        let mut p = Profiler::new(&budget);

        p.record_request_start();
        p.record_fetch_call();
        // Simulate a gap (sequential)
        std::thread::sleep(std::time::Duration::from_millis(10));
        p.record_fetch_call();
        p.record_request_end();

        let d = Detector::new(&p, &budget);
        let wastes = d.detect();
        assert!(wastes.iter().any(|w| w.kind == WasteKind::SequentialFetches));
    }

    #[test]
    fn detects_sparse_kv() {
        let budget = EdgeBudget::default();
        let mut p = Profiler::new(&budget);

        p.record_request_start();
        p.record_kv_read_key("key1", 100);
        p.record_kv_read_key("key2", 100);
        p.record_kv_read_key("key3", 100);
        p.set_kv_total_keys(200);
        p.record_request_end();

        let d = Detector::new(&p, &budget);
        let wastes = d.detect();
        assert!(wastes.iter().any(|w| w.kind == WasteKind::SparseKvAccess));
    }

    #[test]
    fn no_false_positives_on_clean_run() {
        let budget = EdgeBudget::default();
        let mut p = Profiler::new(&budget);

        p.record_init_start();
        p.record_eager_load(1024); // tiny
        p.record_init_end();

        p.record_request_start();
        p.record_kv_read(100);
        p.record_request_end();

        let d = Detector::new(&p, &budget);
        let wastes = d.detect();
        // Should not flag anything major
        assert!(!wastes.iter().any(|w| w.kind == WasteKind::OversizedBundle));
        assert!(!wastes.iter().any(|w| w.kind == WasteKind::SequentialFetches));
    }
}
