//! Image-plan contract. These records are proposals, never permission to execute.
//! The builder must supply exact RPM closure and attestations before staging.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImagePlan {
    pub id: String,
    pub base_digest: String,
    pub metadata_digest: String,
    pub source: String,
    pub install_nevras: Vec<String>,
    pub remove_nevras: Vec<String>,
    pub created_at: u64,
    pub expires_at: u64,
}
impl ImagePlan {
    /// Revalidate against trusted current facts, never client-supplied facts.
    pub fn validate(
        &self,
        base: &str,
        metadata: &str,
        source: &str,
        now: u64,
    ) -> Result<(), &'static str> {
        if self.id.is_empty()
            || self.id.len() > 128
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("invalid plan identifier");
        }
        if !digest(&self.base_digest) || !digest(&self.metadata_digest) {
            return Err("digest required");
        }
        if self.base_digest != base || self.metadata_digest != metadata || self.source != source {
            return Err("stale plan or source policy changed");
        }
        if now < self.created_at
            || now >= self.expires_at
            || self.expires_at.saturating_sub(self.created_at) > 3600
        {
            return Err("plan expired or invalid lifetime");
        }
        if self.install_nevras.is_empty() && self.remove_nevras.is_empty() {
            return Err("empty plan");
        }
        for packages in [&self.install_nevras, &self.remove_nevras] {
            if packages.len() > 256 || packages.windows(2).any(|w| w[0] >= w[1]) {
                return Err("package lists must be bounded, sorted and unique");
            }
            if packages.iter().any(|p| {
                p.is_empty()
                    || p.len() > 256
                    || p.starts_with('-')
                    || !p
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._+:-".contains(&b))
            }) {
                return Err("invalid package identifier");
            }
        }
        if self
            .install_nevras
            .iter()
            .any(|p| self.remove_nevras.contains(p))
        {
            return Err("conflicting operations");
        }
        Ok(())
    }
}
fn digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    })
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageTransactionState {
    Created,
    Validating,
    Building,
    Testing,
    VerifyingSignature,
    ReadyToStage,
    Staged,
    AwaitingReboot,
    BootValidation,
    Succeeded,
    Failed,
    Cancelled,
    RollbackDetected,
}
impl ImageTransactionState {
    /// Transition validation does not certify external evidence. The broker must
    /// verify authorization, signatures and boot health before advancing state.
    pub fn can_transition(self, next: Self) -> bool {
        use ImageTransactionState::*;
        matches!(
            (self, next),
            (Created, Validating | Cancelled)
                | (Validating, Building | Failed | Cancelled)
                | (Building, Testing | Failed | Cancelled)
                | (Testing, VerifyingSignature | Failed)
                | (VerifyingSignature, ReadyToStage | Failed)
                | (ReadyToStage, Staged | Failed | Cancelled)
                | (Staged, AwaitingReboot | Failed)
                | (AwaitingReboot, BootValidation | RollbackDetected)
                | (BootValidation, Succeeded | Failed | RollbackDetected)
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn plan() -> ImagePlan {
        ImagePlan {
            id: "plan-1".into(),
            base_digest: format!("sha256:{}", "a".repeat(64)),
            metadata_digest: format!("sha256:{}", "b".repeat(64)),
            source: "fedora".into(),
            install_nevras: vec!["gcc-1:14-1.x86_64".into()],
            remove_nevras: vec![],
            created_at: 10,
            expires_at: 100,
        }
    }
    #[test]
    fn rejects_stale_and_expired_plans() {
        let p = plan();
        assert!(
            p.validate(&p.base_digest, &p.metadata_digest, &p.source, 20)
                .is_ok()
        );
        assert!(
            p.validate("changed", &p.metadata_digest, &p.source, 20)
                .is_err()
        );
        assert!(
            p.validate(&p.base_digest, &p.metadata_digest, &p.source, 100)
                .is_err()
        );
    }
    #[test]
    fn rejects_recipe_injection() {
        let mut p = plan();
        p.install_nevras = vec!["gcc\nRUN evil".into()];
        assert!(
            p.validate(&p.base_digest, &p.metadata_digest, &p.source, 20)
                .is_err()
        );
    }
    #[test]
    fn build_and_stage_cannot_claim_success() {
        use ImageTransactionState::*;
        assert!(!Building.can_transition(Succeeded));
        assert!(!Staged.can_transition(Succeeded));
        assert!(BootValidation.can_transition(Succeeded));
        assert!(!Succeeded.can_transition(Building));
    }
}
