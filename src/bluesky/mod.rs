// Bluesky API client — unauthenticated public API access.
//
// Built on reqwest and atrium-api types. Each submodule handles one area of
// the AT Protocol API surface. All endpoints are public (read-only).

pub mod amplification;
pub mod client;
pub mod followers;
pub mod likes;
pub mod posts;
pub mod profiles;
pub mod replies;
