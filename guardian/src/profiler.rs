//! Per-request and init-phase profiling.

use crate::budget::EdgeBudget;

/// Summary of the global init (cold-start) phase.
#[derive(Debug, Clone)]
pub struct InitPhase {
    /// Wall time of the init phase in milliseconds.
    pub wall_ms: u64,
    /// Estimated peak memory during init in MB.
    pub peak_memory_mb: u64,
    /// Number of lazy values or config maps loaded eagerly.
    pub eager_loads: u32,
    /// Estimated bytes loaded during init.
    pub bytes_loaded: u64,
}

impl Default for InitPhase {
    fn default() -> Self {
        Self::new()
    }
}

impl InitPhase {
    pub fn new() -> Self {
        Self {
            wall_ms: 0,
            peak_memory_mb: 0,
            eager_loads: 0,
            bytes_loaded: 0,
        }
    }
}

/// Per-request resource profile.
#[derive(Debug, Clone)]
pub struct PerRequestProfile {
    /// Wall-clock time in milliseconds.
    pub wall_ms: u64,
    /// CPU time in milliseconds.
    pub cpu_ms: u64,
    /// Peak memory usage in MB during this request.
    pub peak_memory_mb: u64,
    /// Total bytes allocated (heap) during this request.
    pub alloc_bytes: u64,
    /// Number of `fetch()` calls made.
    pub fetch_calls: u32,
    /// Number of KV reads.
    pub kv_reads: u32,
    /// Number of KV writes.
    pub kv_writes: u32,
    /// Total bytes read from KV.
    pub kv_read_bytes: u64,
    /// Total bytes written to KV.
    pub kv_write_bytes: u64,
    /// Keys accessed — used for the detector's "sparse key usage" heuristic.
    pub kv_keys_accessed: Vec<String>,
    /// All keys present in the eagerly-loaded config map (if applicable).
    pub kv_keys_total: u32,
    /// Number of sequential fetch chains detected.
    pub sequential_fetch_chains: u32,
    /// Number of KV reads where the value hadn't changed since last read.
    pub kv_unchanged_reads: u32,
}

impl Default for PerRequestProfile {
    fn default() -> Self {
        Self::new()
    }
}

impl PerRequestProfile {
    pub fn new() -> Self {
        Self {
            wall_ms: 0,
            cpu_ms: 0,
            peak_memory_mb: 0,
            alloc_bytes: 0,
            fetch_calls: 0,
            kv_reads: 0,
            kv_writes: 0,
            kv_read_bytes: 0,
            kv_write_bytes: 0,
            kv_keys_accessed: Vec::new(),
            kv_keys_total: 0,
            sequential_fetch_chains: 0,
            kv_unchanged_reads: 0,
        }
    }
}

/// High-level profiler that accumulates measurements across the init phase
/// and one or more requests.
#[derive(Debug)]
pub struct Profiler {
    budget: EdgeBudget,
    init: InitPhase,
    requests: Vec<PerRequestProfile>,
    /// In-flight request being accumulated.
    current: Option<PerRequestProfile>,
    init_start_ns: Option<u128>,
    request_start_ns: Option<u128>,
    fetch_timestamps_ns: Vec<u128>,
    kv_cache: std::collections::HashMap<String, u64>,
}

impl Profiler {
    /// Create a new profiler bound to the given budget.
    pub fn new(budget: &EdgeBudget) -> Self {
        Self {
            budget: budget.clone(),
            init: InitPhase::new(),
            requests: Vec::new(),
            current: None,
            init_start_ns: None,
            request_start_ns: None,
            fetch_timestamps_ns: Vec::new(),
            kv_cache: std::collections::HashMap::new(),
        }
    }

    fn with_current<F>(&mut self, f: F)
    where
        F: FnOnce(&mut PerRequestProfile),
    {
        if let Some(ref mut req) = self.current {
            f(req);
        }
    }

    // ---- init phase -----------------------------------------------------

    /// Mark the start of the global init / cold-start phase.
    pub fn record_init_start(&mut self) {
        self.init_start_ns = Some(Self::now_ns());
    }

    /// Mark the end of the init phase.
    pub fn record_init_end(&mut self) {
        if let Some(start) = self.init_start_ns {
            self.init.wall_ms = ((Self::now_ns() - start) / 1_000_000) as u64;
        }
    }

    /// Record an eager load during init.
    pub fn record_eager_load(&mut self, bytes: u64) {
        self.init.eager_loads += 1;
        self.init.bytes_loaded += bytes;
    }

    /// Set peak memory observed during init.
    pub fn set_init_peak_memory_mb(&mut self, mb: u64) {
        self.init.peak_memory_mb = mb;
    }

    /// Override the init-phase wall time.
    pub fn set_init_wall_ms(&mut self, ms: u64) {
        self.init.wall_ms = ms;
    }

    // ---- per-request ----------------------------------------------------

    /// Mark the start of a request handler.
    pub fn record_request_start(&mut self) {
        self.request_start_ns = Some(Self::now_ns());
        self.fetch_timestamps_ns.clear();
        self.current = Some(PerRequestProfile::new());
    }

    /// Mark the end of the current request and commit the profile.
    pub fn record_request_end(&mut self) {
        if let Some(start) = self.request_start_ns.take() {
            let wall_ms = ((Self::now_ns() - start) / 1_000_000) as u64;
            if let Some(mut profile) = self.current.take() {
                profile.wall_ms = wall_ms;
                if profile.cpu_ms == 0 {
                    profile.cpu_ms = wall_ms;
                }
                self.requests.push(profile);
            }
        }
    }

    /// Record a `fetch()` call.
    pub fn record_fetch_call(&mut self) {
        let now = Self::now_ns();
        if let Some(last) = self.fetch_timestamps_ns.last() {
            if now - *last > 5_000_000 {
                self.with_current(|req| req.sequential_fetch_chains += 1);
            }
        }
        self.fetch_timestamps_ns.push(now);
        self.with_current(|req| req.fetch_calls += 1);
    }

    /// Record a KV read.
    pub fn record_kv_read(&mut self, value_bytes: u64) {
        self.with_current(|req| {
            req.kv_reads += 1;
            req.kv_read_bytes += value_bytes;
        });
    }

    /// Record a KV read for a specific key.
    pub fn record_kv_read_key(&mut self, key: &str, value_bytes: u64) {
        self.with_current(|req| {
            req.kv_reads += 1;
            req.kv_read_bytes += value_bytes;
            req.kv_keys_accessed.push(key.to_string());
        });
    }

    /// Record a KV read with value hash for unchanged-read detection.
    pub fn record_kv_read_checked(&mut self, key: &str, value_bytes: u64, value_hash: u64) -> bool {
        let unchanged = self
            .kv_cache
            .get(key)
            .map_or(false, |&prev| prev == value_hash);
        self.kv_cache.insert(key.to_string(), value_hash);
        self.with_current(|req| {
            req.kv_reads += 1;
            req.kv_read_bytes += value_bytes;
            req.kv_keys_accessed.push(key.to_string());
            if unchanged {
                req.kv_unchanged_reads += 1;
            }
        });
        unchanged
    }

    /// Record a KV write.
    pub fn record_kv_write(&mut self, value_bytes: u64) {
        self.with_current(|req| {
            req.kv_writes += 1;
            req.kv_write_bytes += value_bytes;
        });
    }

    /// Set peak memory for the current request.
    pub fn set_request_peak_memory_mb(&mut self, mb: u64) {
        self.with_current(|req| req.peak_memory_mb = mb);
    }

    /// Set total alloc bytes for the current request.
    pub fn set_request_alloc_bytes(&mut self, bytes: u64) {
        self.with_current(|req| req.alloc_bytes = bytes);
    }

    /// Override CPU time for the current request.
    pub fn set_request_cpu_ms(&mut self, ms: u64) {
        self.with_current(|req| req.cpu_ms = ms);
    }

    /// Record the total number of keys in an eagerly-loaded config map.
    pub fn set_kv_total_keys(&mut self, total: u32) {
        self.with_current(|req| req.kv_keys_total = total);
    }

    // ---- accessors ------------------------------------------------------

    pub fn budget(&self) -> &EdgeBudget {
        &self.budget
    }

    pub fn init(&self) -> &InitPhase {
        &self.init
    }

    pub fn requests(&self) -> &[PerRequestProfile] {
        &self.requests
    }

    /// Latest completed request, or `None`.
    pub fn last_request(&self) -> Option<&PerRequestProfile> {
        self.requests.last()
    }

    /// Average wall time across all recorded requests.
    pub fn avg_request_wall_ms(&self) -> f64 {
        if self.requests.is_empty() {
            return 0.0;
        }
        self.requests.iter().map(|r| r.wall_ms as f64).sum::<f64>() / self.requests.len() as f64
    }

    /// Average number of fetch calls across all recorded requests.
    pub fn avg_fetch_calls(&self) -> f64 {
        if self.requests.is_empty() {
            return 0.0;
        }
        self.requests.iter().map(|r| r.fetch_calls as f64).sum::<f64>() / self.requests.len() as f64
    }

    // ---- internal -------------------------------------------------------

    fn now_ns() -> u128 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiler_records_init() {
        let budget = EdgeBudget::default();
        let mut p = Profiler::new(&budget);
        p.record_init_start();
        p.record_eager_load(1024 * 1024);
        p.record_eager_load(512 * 1024);
        p.record_init_end();

        let init = p.init();
        assert!(init.wall_ms <= 10);
        assert_eq!(init.eager_loads, 2);
        assert_eq!(init.bytes_loaded, 1024 * 1024 + 512 * 1024);
    }

    #[test]
    fn profiler_records_request() {
        let budget = EdgeBudget::default();
        let mut p = Profiler::new(&budget);

        p.record_request_start();
        p.record_fetch_call();
        p.record_kv_read(256);
        p.record_kv_write(128);
        p.record_request_end();

        let req = p.last_request().unwrap();
        assert_eq!(req.fetch_calls, 1);
        assert_eq!(req.kv_reads, 1);
        assert_eq!(req.kv_read_bytes, 256);
        assert_eq!(req.kv_writes, 1);
        assert_eq!(req.kv_write_bytes, 128);
    }

    #[test]
    fn profiler_averages() {
        let budget = EdgeBudget::default();
        let mut p = Profiler::new(&budget);

        p.record_request_start();
        p.record_fetch_call();
        p.record_request_end();

        p.record_request_start();
        p.record_fetch_call();
        p.record_fetch_call();
        p.record_request_end();

        assert_eq!(p.requests().len(), 2);
        assert!((p.avg_fetch_calls() - 1.5).abs() < f64::EPSILON);
    }
}
