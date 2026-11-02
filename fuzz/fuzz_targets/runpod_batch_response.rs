//! Fuzz target: the parser for RunPod's classifier responses (#420).
//!
//! The response body comes from a remote service, so it is untrusted input.
//! Invariant: for ANY byte string, `parse_batch_response` returns Ok or Err
//! and never panics, hangs or overflows — a bad body must fail one batch,
//! never crash the scan worker.
#![no_main]

use charcoal::toxicity::runpod_cope_b::RunPodCopeBClient;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The client reads the body as text, so feed it what reqwest would:
    // lossy UTF-8 rather than skipping inputs that are not valid UTF-8.
    let body = String::from_utf8_lossy(data);
    let _ = RunPodCopeBClient::parse_batch_response(&body, 0);
});
