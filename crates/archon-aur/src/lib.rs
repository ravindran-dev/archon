//! Client for the AUR RPC (v5), <https://aur.archlinux.org/rpc/>.
//!
//! Endpoints used:
//! - `GET /rpc/v5/info?arg[]=a&arg[]=b`   — batch metadata lookup
//! - `GET /rpc/v5/search/{name}?by=provides` — virtual-package resolution
//!
//! The base URL is injectable so tests can point the client at a local mock
//! server instead of the real AUR. Metadata responses are cached per client
//! instance: the resolver may ask about the same package several times while
//! walking the graph, and we don't want to hammer the AUR (which rate-limits
//! by IP).

mod client;
mod model;

pub use client::{AurClient, AurError};
pub use model::AurPackage;

pub const DEFAULT_AUR_URL: &str = "https://aur.archlinux.org";
