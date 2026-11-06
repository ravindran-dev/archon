//! Native libflatpak bridge. Flatpak owns repository and dependency state.
//! Hosts must broker authorization before invoking the commit protocol.
pub const BRIDGE: &str = include_str!("flatpak_bridge.py");

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub action: String,
    pub package: String,
    pub repository: String,
}
impl Request {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !matches!(self.action.as_str(), "install" | "remove" | "update") {
            return Err("unsupported action");
        }
        let parts: Vec<_> = self.package.split('/').collect();
        if parts.len() != 4 || parts[0] != "app" || !parts[1..].iter().all(|s| identifier(s)) {
            return Err("invalid full application reference");
        }
        if !identifier(&self.repository) {
            return Err("invalid repository identifier");
        }
        Ok(())
    }
}
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_injection_and_unsafe_scopes() {
        for package in [
            "../etc/passwd",
            "app/--help/x86_64/stable",
            "runtime/a/x86_64/1",
            "app/a/x86_64/$(id)",
        ] {
            assert!(
                Request {
                    action: "install".into(),
                    package: package.into(),
                    repository: "flathub".into()
                }
                .validate()
                .is_err()
            );
        }
    }
    #[test]
    fn accepts_full_flatpak_identity() {
        assert!(
            Request {
                action: "install".into(),
                package: "app/org.gnome.Calculator/x86_64/stable".into(),
                repository: "flathub".into()
            }
            .validate()
            .is_ok()
        );
    }
}
