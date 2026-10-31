use archon_types::PackageMeta;

/// One resolved package plus why it's in the plan (targets vs pulled-in
/// dependencies are displayed differently, like pacman does).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedPackage {
    pub meta: PackageMeta,
    /// Empty for explicit targets; otherwise the packages that pulled
    /// this one in.
    pub required_by: Vec<String>,
}

/// The resolver's output: everything needed to show the user a complete,
/// truthful "this is what will happen" screen before anything runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallPlan {
    /// Official packages, install order. Handed to `pacman -S --needed`
    /// in one shot (pacman re-verifies; we never bypass it).
    pub official: Vec<PlannedPackage>,
    /// AUR packages in build order: every entry's depends/makedepends are
    /// satisfied by `official` plus the entries before it.
    pub aur: Vec<PlannedPackage>,
}

impl InstallPlan {
    pub fn is_empty(&self) -> bool {
        self.official.is_empty() && self.aur.is_empty()
    }

    pub fn package_count(&self) -> usize {
        self.official.len() + self.aur.len()
    }
}
