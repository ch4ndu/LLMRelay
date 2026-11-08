use anyhow::{bail, Result};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;

#[derive(Clone, Default)]
pub(crate) struct BootstrapIssuer(Arc<Mutex<Option<(String, Instant)>>>);

impl BootstrapIssuer {
    pub(crate) fn issue(&self) -> Result<String> {
        let secret = issue_secret();
        *self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("browser authentication unavailable"))? = Some((
            hash_secret(&secret),
            Instant::now() + Duration::from_secs(60),
        ));
        Ok(secret)
    }

    pub(crate) fn consume(&self, secret: &str) -> Result<()> {
        let mut pending = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("browser authentication unavailable"))?;
        let Some((hash, deadline)) = pending.as_ref() else {
            bail!("bootstrap credential is expired or already consumed")
        };
        if Instant::now() >= *deadline {
            *pending = None;
            bail!("bootstrap credential expired")
        }
        verify_secret(secret, hash)?;
        *pending = None;
        Ok(())
    }
}

pub fn issue_secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

pub fn hash_secret(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

pub fn verify_secret(secret: &str, expected_hex: &str) -> Result<()> {
    let actual = Sha256::digest(secret.as_bytes());
    let expected = hex::decode(expected_hex)?;
    if actual.as_slice().ct_eq(expected.as_slice()).unwrap_u8() != 1 {
        bail!("invalid role credential")
    }
    Ok(())
}

#[cfg(test)]
mod desktop_tests {
    use super::*;
    use crate::domain::{
        AttentionCategory, AttentionTarget, DesktopActivation, DesktopAttentionEntry,
        DesktopTarget, RoleKind,
    };

    #[test]
    fn desktop_issuer_expiry_reuse_and_exact_task_episode() {
        let issuer = BootstrapIssuer::default();
        let first = issuer.issue().unwrap();
        let second = issuer.issue().unwrap();
        assert!(issuer.consume(&first).is_err());
        issuer.consume(&second).unwrap();
        assert!(issuer.consume(&second).is_err());
        let expired = issuer.issue().unwrap();
        issuer.0.lock().unwrap().as_mut().unwrap().1 = Instant::now();
        assert!(issuer
            .consume(&expired)
            .unwrap_err()
            .to_string()
            .contains("expired"));
        let root = "a".repeat(64);
        let target = |task: &str, revision| AttentionTarget::RoleSettings {
            project_id: "p".into(),
            task_id: task.into(),
            role: RoleKind::Manager,
            settings_revision: revision,
        };
        let episode = |task: &str, revision| {
            crate::workflow::desktop_episode_key(
                &root,
                &target(task, revision).desktop_episode_source().unwrap(),
            )
        };
        assert_ne!(episode("task-a", 1), episode("task-b", 1));
        let activation = DesktopActivation {
            instance_key: root.clone(),
            attention_id: "reused-id".into(),
            episode_key: episode("task-a", 1),
        };
        activation.validate().unwrap();
        let entry = |revision| DesktopAttentionEntry {
            attention_id: "reused-id".into(),
            category: AttentionCategory::Blocked,
            episode_key: Some(episode("task-a", revision)),
            target: Some(target("task-a", revision)),
        };
        assert!(matches!(
            activation.resolve(&root, "1", "boot", vec![entry(1)]),
            DesktopTarget::Current { .. }
        ));
        assert!(matches!(
            activation.resolve(&root, "1", "boot", vec![entry(2)]),
            DesktopTarget::Stale { .. }
        ));
        assert!(matches!(
            activation.resolve(&root, "1", "boot", vec![entry(1), entry(2)]),
            DesktopTarget::Stale { .. }
        ));
        assert!(matches!(
            activation.resolve(&"b".repeat(64), "1", "boot", vec![entry(1)]),
            DesktopTarget::Stale { .. }
        ));
        let mut ambiguous = entry(1);
        ambiguous.episode_key = None;
        assert!(matches!(
            activation.resolve(&root, "1", "boot", vec![ambiguous]),
            DesktopTarget::Stale { .. }
        ));
    }
}
