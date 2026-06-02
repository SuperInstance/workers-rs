//! Human-readable conservation reports.

use crate::budget::EdgeBudget;
use crate::detector::Waste;
use crate::profiler::Profiler;

/// A conservation report summarizing profiler data and detected waste.
#[derive(Debug)]
pub struct Report {
    /// Plain-text lines of the report body.
    lines: Vec<String>,
}

impl Report {
    /// Generate a report from profiler data, budget, and detected wastes.
    pub fn generate(profiler: &Profiler, budget: &EdgeBudget, wastes: &[Waste]) -> Self {
        let mut lines = Vec::new();
        let init = profiler.init();

        // Header
        lines.push("═══ Edge Conservation Report ═══".into());
        lines.push(String::new());

        // Cold-start summary
        let budget_status = if budget.check_cold_start(init.wall_ms) {
            "✅ within budget"
        } else {
            "❌ over budget"
        };
        lines.push(format!(
            "Cold-start: {}ms (budget: {}ms) {}",
            init.wall_ms, budget.max_cold_start_ms, budget_status
        ));

        if init.bytes_loaded > 0 {
            let mb = init.bytes_loaded as f64 / (1024.0 * 1024.0);
            lines.push(format!(
                "  Global init loads {:.1} MB across {} resources.",
                mb, init.eager_loads
            ));
        }

        lines.push(String::new());

        // Per-request summary
        if !profiler.requests().is_empty() {
            lines.push(format!(
                "Requests profiled: {}",
                profiler.requests().len()
            ));
            lines.push(format!(
                "  Avg wall time:   {:.1}ms",
                profiler.avg_request_wall_ms()
            ));
            lines.push(format!(
                "  Avg fetch calls: {:.1}",
                profiler.avg_fetch_calls()
            ));

            let total_kv_reads: u32 = profiler.requests().iter().map(|r| r.kv_reads).sum();
            let total_kv_writes: u32 = profiler.requests().iter().map(|r| r.kv_writes).sum();
            lines.push(format!(
                "  Total KV reads:  {} | writes: {}",
                total_kv_reads, total_kv_writes
            ));

            // Sparse key usage
            for req in profiler.requests() {
                if req.kv_keys_total > 0 && !req.kv_keys_accessed.is_empty() {
                    let accessed = req.kv_keys_accessed.len();
                    let pct = accessed as f64 / req.kv_keys_total as f64 * 100.0;
                    lines.push(format!(
                        "  KV access: {}/{} keys ({:.0}%)",
                        accessed, req.kv_keys_total, pct
                    ));
                    break;
                }
            }
        }

        lines.push(String::new());

        // Wastes
        if wastes.is_empty() {
            lines.push("✅ No waste detected. Ship it.".into());
        } else {
            lines.push(format!("⚠️  {} waste(s) detected:", wastes.len()));
            lines.push(String::new());
            for (i, w) in wastes.iter().enumerate() {
                lines.push(format!("{}. [{}] {}", i + 1, w.kind, w.description));
                lines.push(format!("   → {}", w.suggestion));
                lines.push(format!("   Impact: {}", w.estimated_impact));
                lines.push(String::new());
            }
        }

        // Projected savings — synthesized from wastes
        if !wastes.is_empty() {
            lines.push("── Projected Savings ──".into());
            let projected_ms = Self::estimate_cold_start_savings(profiler, wastes);
            if projected_ms > 0 {
                lines.push(format!(
                    "Addressing the above could reduce cold-start by ~{}ms.",
                    projected_ms
                ));
            }
        }

        Self { lines }
    }

    /// Render the report as a plain-text string.
    pub fn to_text(&self) -> String {
        self.lines.join("\n")
    }

    fn estimate_cold_start_savings(profiler: &Profiler, wastes: &[Waste]) -> u64 {
        let init = profiler.init();
        let mut savings = 0u64;

        for w in wastes {
            match w.kind {
                crate::detector::WasteKind::EagerInit => {
                    // Rough: defer half of init time
                    savings += init.wall_ms / 2;
                }
                crate::detector::WasteKind::OversizedBundle => {
                    savings += init.wall_ms / 3;
                }
                crate::detector::WasteKind::SparseKvAccess => {
                    // Lazy-loading unused keys saves proportional init time
                    savings += init.wall_ms / 4;
                }
                _ => {}
            }
        }

        // Cap at the actual init time
        savings.min(init.wall_ms)
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::EdgeBudget;
    use crate::detector::Detector;

    #[test]
    fn report_on_clean_run() {
        let budget = EdgeBudget::default();
        let mut p = Profiler::new(&budget);

        p.record_init_start();
        p.record_init_end();

        p.record_request_start();
        p.record_kv_read(100);
        p.record_request_end();

        let d = Detector::new(&p, &budget);
        let wastes = d.detect();
        let report = Report::generate(&p, &budget, &wastes);

        let text = report.to_text();
        assert!(text.contains("No waste detected"));
    }

    #[test]
    fn report_shows_wastes() {
        let budget = EdgeBudget::strict(); // tight
        let mut p = Profiler::new(&budget);

        p.record_init_start();
        p.record_eager_load(3 * 1024 * 1024);
        p.record_eager_load(1024);
        p.record_init_end();

        p.record_request_start();
        p.record_kv_read_key("k1", 100);
        p.record_kv_read_key("k2", 100);
        p.record_kv_read_key("k3", 100);
        p.set_kv_total_keys(200);
        p.record_request_end();

        let d = Detector::new(&p, &budget);
        let wastes = d.detect();
        let report = Report::generate(&p, &budget, &wastes);

        let text = report.to_text();
        assert!(text.contains("waste(s) detected"));
        assert!(text.contains("Projected Savings"));
    }

    #[test]
    fn example_report_format() {
        // Simulate the example from the spec:
        // "Cold-start 340ms. Global init loads 2MB config. 90% of requests use 3/200 keys.
        //  Lazy load the rest → projected 45ms."
        let budget = EdgeBudget::builder()
            .cold_start_ms(400)
            .memory_mb(128)
            .cpu_time_per_request_ms(50)
            .subrequests(50)
            .build();

        let mut p = Profiler::new(&budget);

        p.record_init_start();
        // Simulate 340ms cold start — we can't actually sleep that long in tests,
        // so we'll set it manually after ending
        p.record_eager_load(2 * 1024 * 1024); // 2MB config
        p.record_init_end();
        p.set_init_wall_ms(340);

        p.record_request_start();
        p.record_kv_read_key("routes:home", 128);
        p.record_kv_read_key("routes:api", 64);
        p.record_kv_read_key("features:dark-mode", 32);
        p.set_kv_total_keys(200);
        p.record_request_end();

        let d = Detector::new(&p, &budget);
        let wastes = d.detect();
        let report = Report::generate(&p, &budget, &wastes);

        let text = report.to_text();
        assert!(text.contains("340ms"));
        assert!(text.contains("2.0 MB"));
        assert!(text.contains("3/200 keys"));
        assert!(text.contains("Projected Savings"));
    }
}
