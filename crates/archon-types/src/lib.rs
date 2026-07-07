//! Shared vocabulary for archon.
//!
//! Every other crate speaks in these types: `archon-repo` and `archon-aur`
//! produce [`PackageMeta`], `archon-resolver` consumes it through the
//! [`PackageProvider`] trait. Keeping this crate dependency-light means the
//! resolver can be tested with plain in-memory mocks and never needs a live
//! Arch system.

mod dependency;
mod package;
mod provider;
mod version;

pub use dependency::{ConstraintOp, Dependency, DependencyParseError, VersionConstraint};
pub use package::{PackageMeta, PackageSource};
pub use provider::{PackageProvider, ProviderError};
pub use version::{Version, VersionParseError};
