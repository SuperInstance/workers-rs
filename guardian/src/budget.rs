//! Budget definitions for edge function resource limits.

/// Resource consumption snapshot returned after checking a budget.
#[derive(Debug, Clone)]
pub struct ResourceUsage {
    pub cold_start_ms: u64,
    pub memory_mb: u64,
    pub cpu_time_ms: u64,
    pub subrequests: u32,
}

impl ResourceUsage {
    pub fn new() -> Self {
        Self {
            cold_start_ms: 0,
            memory_mb: 0,
            cpu_time_ms: 0,
            subrequests: 0,
        }
    }
}

impl Default for ResourceUsage {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of checking CPU time against the budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuTimeResult {
    /// Still within the per-request CPU budget.
    WithinBudget,
    /// Approaching the limit (≥ 80% consumed).
    ApproachingLimit { used_ms: u64, limit_ms: u64 },
    /// Budget exceeded.
    Exceeded { used_ms: u64, limit_ms: u64 },
}

/// Resource budget for an edge function.
///
/// Defines upper bounds for cold-start wall time, memory, per-request CPU
/// time, and outbound subrequests.
#[derive(Debug, Clone)]
pub struct EdgeBudget {
    /// Maximum cold-start wall time in milliseconds.
    pub max_cold_start_ms: u64,
    /// Maximum memory usage in megabytes.
    pub max_memory_mb: u64,
    /// Maximum CPU time per request in milliseconds.
    pub max_cpu_time_per_request_ms: u64,
    /// Maximum number of outbound subrequests (fetch calls).
    pub max_subrequests: u32,
}

impl EdgeBudget {
    /// Create a budget tuned for a typical Cloudflare Worker on the
    /// [bundled plan](https://developers.cloudflare.com/workers/platform/limits/).
    pub fn default_worker() -> Self {
        Self {
            max_cold_start_ms: 400,
            max_memory_mb: 128,
            max_cpu_time_per_request_ms: 50,
            max_subrequests: 50,
        }
    }

    /// Stricter budget for latency-sensitive endpoints.
    pub fn strict() -> Self {
        Self {
            max_cold_start_ms: 100,
            max_memory_mb: 64,
            max_cpu_time_per_request_ms: 10,
            max_subrequests: 10,
        }
    }

    /// Generous budget for heavy processing tasks.
    pub fn generous() -> Self {
        Self {
            max_cold_start_ms: 800,
            max_memory_mb: 256,
            max_cpu_time_per_request_ms: 150,
            max_subrequests: 100,
        }
    }

    /// Build a custom budget.
    pub fn builder() -> EdgeBudgetBuilder {
        EdgeBudgetBuilder {
            inner: Self::default_worker(),
        }
    }

    // --- budget checks ---------------------------------------------------

    /// Check whether a measured cold-start time is within budget.
    pub fn check_cold_start(&self, measured_ms: u64) -> bool {
        measured_ms <= self.max_cold_start_ms
    }

    /// Check memory usage against the budget.
    pub fn check_memory(&self, used_mb: u64) -> bool {
        used_mb <= self.max_memory_mb
    }

    /// Check per-request CPU time. Returns a graded result so callers can
    /// warn before hard-failing.
    pub fn check_cpu_time(&self, used_ms: u64) -> CpuTimeResult {
        let ratio = used_ms as f64 / self.max_cpu_time_per_request_ms as f64;
        if ratio > 1.0 {
            CpuTimeResult::Exceeded {
                used_ms,
                limit_ms: self.max_cpu_time_per_request_ms,
            }
        } else if ratio >= 0.8 {
            CpuTimeResult::ApproachingLimit {
                used_ms,
                limit_ms: self.max_cpu_time_per_request_ms,
            }
        } else {
            CpuTimeResult::WithinBudget
        }
    }

    /// Check subrequest count against the budget.
    pub fn check_subrequests(&self, count: u32) -> bool {
        count <= self.max_subrequests
    }
}

impl Default for EdgeBudget {
    fn default() -> Self {
        Self::default_worker()
    }
}

/// Fluent builder for [`EdgeBudget`].
pub struct EdgeBudgetBuilder {
    inner: EdgeBudget,
}

impl EdgeBudgetBuilder {
    pub fn cold_start_ms(mut self, ms: u64) -> Self {
        self.inner.max_cold_start_ms = ms;
        self
    }

    pub fn memory_mb(mut self, mb: u64) -> Self {
        self.inner.max_memory_mb = mb;
        self
    }

    pub fn cpu_time_per_request_ms(mut self, ms: u64) -> Self {
        self.inner.max_cpu_time_per_request_ms = ms;
        self
    }

    pub fn subrequests(mut self, n: u32) -> Self {
        self.inner.max_subrequests = n;
        self
    }

    pub fn build(self) -> EdgeBudget {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_budget_is_reasonable() {
        let b = EdgeBudget::default();
        assert!(b.check_cold_start(300));
        assert!(!b.check_cold_start(500));
        assert!(b.check_memory(100));
        assert!(b.check_subrequests(40));
    }

    #[test]
    fn cpu_time_grading() {
        let b = EdgeBudget::default(); // 50 ms limit
        assert_eq!(b.check_cpu_time(10), CpuTimeResult::WithinBudget);
        assert!(matches!(
            b.check_cpu_time(42),
            CpuTimeResult::ApproachingLimit { .. }
        ));
        assert!(matches!(b.check_cpu_time(60), CpuTimeResult::Exceeded { .. }));
    }

    #[test]
    fn builder_custom() {
        let b = EdgeBudget::builder()
            .cold_start_ms(50)
            .memory_mb(32)
            .cpu_time_per_request_ms(5)
            .subrequests(3)
            .build();

        assert_eq!(b.max_cold_start_ms, 50);
        assert_eq!(b.max_memory_mb, 32);
        assert_eq!(b.max_cpu_time_per_request_ms, 5);
        assert_eq!(b.max_subrequests, 3);
    }
}
