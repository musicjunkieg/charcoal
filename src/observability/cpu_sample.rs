//! Process CPU-time sampler for the gather phase (#343 Phase 0).
//!
//! We need to know how many cores ONNX inference actually keeps busy per
//! worker before deciding whether extra sessions or extra replicas are the
//! right lever. Railway's dashboard averages over minutes and hides the
//! gather's burstiness, so the process samples its own CPU time from
//! `/proc/self/stat` once a minute and logs `cpu_cores_busy` — the ratio of
//! CPU-seconds consumed to wall-seconds elapsed since the previous sample.
//! Only Linux exposes `/proc`; elsewhere the sampler logs one notice and
//! exits, so local macOS runs are unaffected.

use std::time::{Duration, Instant};

use tokio::task::JoinHandle;
use tracing::{info, warn};

/// How often to sample. One minute matches the spec's "once a minute" and is
/// coarse enough that the log stays readable for a 10-minute gather.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(60);

/// Linux `CLK_TCK` is 100 on every glibc target we ship to (Ubuntu 24.04 in
/// the Dockerfile). Hard-coding it avoids a `libc::sysconf` call and a new
/// dependency for a diagnostic.
const CLK_TCK: f64 = 100.0;

/// Extract `utime + stime` (clock ticks) from a `/proc/self/stat` line.
///
/// Field 2 (`comm`) is wrapped in parentheses and may itself contain spaces
/// and parentheses, so we split on the **last** `)` and count fields from
/// there: after the split, `state` is index 0, so `utime` (field 14 in the
/// man page's 1-based numbering) is index 11 and `stime` is index 12.
pub fn parse_proc_stat_cpu_ticks(stat: &str) -> Option<u64> {
    let after_comm = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

/// Dropping this stops the background sampler.
pub struct CpuSamplerGuard {
    handle: JoinHandle<()>,
}

impl Drop for CpuSamplerGuard {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Start sampling on the current tokio runtime. The first log line appears
/// after one interval; a gather shorter than that produces no samples,
/// which is fine — it is also not the case we are trying to measure.
pub fn spawn_cpu_sampler() -> CpuSamplerGuard {
    let handle = tokio::spawn(async {
        if !cfg!(target_os = "linux") {
            info!(
                metric = "cpu_cores_busy",
                "CPU sampler unsupported off Linux; skipping"
            );
            return;
        }
        let mut last_ticks = read_ticks();
        let mut last_at = Instant::now();
        loop {
            tokio::time::sleep(SAMPLE_INTERVAL).await;
            let now_ticks = read_ticks();
            let now = Instant::now();
            match (last_ticks, now_ticks) {
                (Some(prev), Some(cur)) => {
                    let cpu_secs = (cur.saturating_sub(prev)) as f64 / CLK_TCK;
                    let wall_secs = now.duration_since(last_at).as_secs_f64();
                    let cores_busy = if wall_secs > 0.0 {
                        cpu_secs / wall_secs
                    } else {
                        0.0
                    };
                    info!(
                        metric = "cpu_cores_busy",
                        cores_busy = format!("{cores_busy:.2}"),
                        cpu_secs = format!("{cpu_secs:.1}"),
                        wall_secs = format!("{wall_secs:.1}"),
                        "gather CPU sample (#343 Phase 0)"
                    );
                }
                _ => warn!(metric = "cpu_cores_busy", "could not read /proc/self/stat"),
            }
            last_ticks = now_ticks;
            last_at = now;
        }
    });
    CpuSamplerGuard { handle }
}

fn read_ticks() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    parse_proc_stat_cpu_ticks(&stat)
}
