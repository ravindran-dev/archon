//! Sandboxed `makepkg` execution via bubblewrap.
//!
//! We delegate all the hard parts — namespace setup, unprivileged bind
//! mounts, capability dropping — to `bwrap`, a well-audited setuid-free
//! sandboxing tool already shipped by Arch. archon's job is just building
//! the right policy and driving `makepkg` through it; we do not reimplement
//! namespace plumbing or package extraction ourselves.
//!
//! The build runs in two stages so network access can be scoped tightly:
//!
//! 1. [`BuildStage::Fetch`] — `makepkg --nobuild`: download and extract
//!    sources declared in `source=()`. Network is allowed (nothing else
//!    can be fetched without it); filesystem writes are still confined to
//!    the package directory.
//! 2. [`BuildStage::Build`] — `makepkg --noextract`: run `build()` and
//!    `package()` against what Stage 1 already fetched. Network is fully
//!    unshared — a compromised build script has no egress at all here.
//!
//! Both stages skip `makepkg`'s own dependency install (`--nodeps`):
//! archon's resolver already routed `depends`/`makedepends` through
//! `pacman -S` before the sandbox ever starts, so nothing inside the
//! sandbox needs to invoke pacman or touch the system package database.

mod policy;
mod runner;

pub use policy::SandboxPolicy;
pub use runner::{BuildStage, SandboxError, SandboxRunner};
