use crate::package::PackageMeta;

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// The backing store couldn't be read at all (corrupt sync db, AUR RPC
    /// unreachable, ...). Distinct from "package not found", which is *not*
    /// an error at this layer — the resolver decides what missing means.
    #[error("provider backend failure: {0}")]
    Backend(#[from] Box<dyn std::error::Error + Send + Sync>),
}

/// A source of package metadata the resolver can query.
///
/// Implemented by `archon-repo` (sync databases), `archon-aur` (AUR RPC),
/// and by in-memory mocks in resolver tests. Queries are batched so the AUR
/// implementation can answer N names with a single RPC round-trip.
pub trait PackageProvider {
    /// Human-readable label for error messages ("core", "extra", "aur").
    fn label(&self) -> &str;

    /// Exact-name lookup. Names with no match are simply absent from the
    /// result; order is not guaranteed.
    fn get(&self, names: &[String]) -> Result<Vec<PackageMeta>, ProviderError>;

    /// Find packages that satisfy `dep_name` via their `provides` array
    /// (virtual packages: `libgl`, `sh`, `jdk`, ...). Exact-name matches
    /// are handled by [`PackageProvider::get`] and need not be repeated here.
    fn find_providers(&self, dep_name: &str) -> Result<Vec<PackageMeta>, ProviderError>;
}
