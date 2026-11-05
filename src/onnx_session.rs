// How every ONNX Runtime inference call is run.
//
// All three models (toxicity, embeddings, NLI) take their run options from
// here, so a setting that matters for memory or speed cannot be applied to one
// model and forgotten on the others.

use ort::session::run_options::RunOptions;

/// ONNX Runtime's run-config key that shrinks the named arenas when a run ends.
const ARENA_SHRINKAGE_KEY: &str = "memory.enable_memory_arena_shrinkage";
/// The CPU arena (device id 0) — the only device Charcoal runs on.
const SHRINK_CPU_ARENA: &str = "cpu:0";

/// Run options for one inference call: shrink the CPU memory arena when the
/// run finishes (#427).
///
/// **Why.** ONNX Runtime's CPU arena grows to fit the largest batch it has
/// ever run and, by default, never gives that memory back. A scan scores each
/// account's posts as one batch, so the web server held its largest scan's
/// peak until the next deploy: staging went 0.72 -> 1.74 GB in one scan and
/// stayed there, prod sat at 3.8 GB for days. Railway bills memory held, so
/// that retained arena was most of the monthly bill.
///
/// **Why this and not the alternatives** — measured on Linux (glibc 2.39, the
/// Detoxify model, batches spread across threads as tokio's blocking pool
/// does), memory still held after the work, and time for the same work:
///
/// | configuration                         | held    | time   |
/// |---------------------------------------|---------|--------|
/// | default (arena on, never shrinks)     | 1416 MB | 42.4 s |
/// | arena on + shrink after each run      |  274 MB | 43.6 s |
/// | arena off                             |  464 MB | 60.8 s |
/// | mimalloc as global allocator, arena on| 1532 MB |   —    |
///
/// Turning the arena off frees memory too, but every run then maps and
/// page-faults its big tensors from scratch: 35-45% slower, on a gather phase
/// that is already CPU-bound on this model. A different allocator does
/// nothing while the arena is holding the memory. Shrinking keeps the arena's
/// speed inside a run and returns the memory between runs.
pub fn run_options() -> ort::Result<RunOptions> {
    let mut options = RunOptions::new()?;
    options.add_config_entry(ARENA_SHRINKAGE_KEY, SHRINK_CPU_ARENA)?;
    Ok(options)
}
