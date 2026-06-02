//! Edge Conservation Guardian
//!
//! Tracks serverless function resource usage and enforces cold-start budgets
//! for Cloudflare Workers (workers-rs).
//!
//! # Quick start
//!
//! ```
//! use guardian::{EdgeBudget, Profiler, Detector, Report};
//!
//! let budget = EdgeBudget::default();
//! let mut profiler = Profiler::new(&budget);
//!
//! profiler.record_init_start();
//! // ... your init code ...
//! profiler.record_init_end();
//!
//! profiler.record_request_start();
//! // ... handle request ...
//! profiler.record_fetch_call();
//! profiler.record_kv_read(2048);
//! profiler.record_request_end();
//!
//! let detector = Detector::new(&profiler, &budget);
//! let wastes = detector.detect();
//!
//! let report = Report::generate(&profiler, &budget, &wastes);
//! println!("{}", report);
//! ```

mod budget;
mod detector;
mod profiler;
mod report;

pub use budget::{CpuTimeResult, EdgeBudget, ResourceUsage};
pub use detector::{Detector, Waste, WasteKind};
pub use profiler::{InitPhase, PerRequestProfile, Profiler};
pub use report::Report;
