use super::*;

#[test]
fn changed_resume_request_does_not_poison_retained_session() {
    assert_eq!(
            permanent_resume_rejection_category(&anyhow!(
                "resume input differs from the persisted invocation; create a fresh role or review request"
            )),
            None
        );
    assert_eq!(
        permanent_resume_rejection_category(&anyhow!("session has no persisted invocation input")),
        Some("invocation_provenance_missing")
    );
}

#[test]
fn typed_compatibility_precedes_frozen_identity_string_fallback() {
    let error = crate::provider_compatibility::BundleSet::embedded()
        .resolve(
            crate::domain::Provider::Claude,
            "unknown",
            crate::domain::RoleKind::Manager,
        )
        .unwrap_err();
    let contextual = anyhow::Error::new(error).context("frozen runtime identity changed");
    assert_eq!(
        permanent_resume_rejection_category(&contextual),
        Some("provider_compatibility_unsupported")
    );
}
