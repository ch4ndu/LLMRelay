use crate::snapshot::SnapshotManifest;
use crate::store::Store;
use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ReviewDispatch {
    pub request_id: String,
    pub attempt_id: String,
    pub role: crate::domain::RoleKind,
    pub candidate_hash: String,
    pub prompt: String,
    pub handoff: serde_json::Value,
    pub state: String,
}

#[derive(Clone)]
pub struct ReviewService {
    store: Store,
    snapshots_root: PathBuf,
}

impl ReviewService {
    pub fn new(store: Store, artifacts: PathBuf) -> Self {
        Self {
            store,
            snapshots_root: artifacts.join("snapshots"),
        }
    }

    pub fn freeze(&self, attempt_id: &str, kind: &str) -> Result<serde_json::Value> {
        if !["plan", "candidate", "checkpoint", "accepted"].contains(&kind) {
            bail!("unsupported snapshot kind")
        }
        let freeze_id = self.reserve_freeze(attempt_id, kind)?;
        match self.freeze_reserved(attempt_id, kind, &freeze_id) {
            Ok(value) => Ok(value),
            Err(error) => {
                let connection = self.store.lock()?;
                connection.execute(
                    "UPDATE freeze_intents SET state='abandoned',error=?1,updated_at=?2 WHERE id=?3 AND state IN ('reserved','capturing')",
                    params![format!("{error:#}"), Utc::now().to_rfc3339(), freeze_id],
                )?;
                Err(error)
            }
        }
    }

    fn reserve_freeze(&self, attempt_id: &str, kind: &str) -> Result<String> {
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active_writer: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id WHERE rg.attempt_id=?1 AND rg.role='implementer' AND s.status NOT IN ('exited','launch_failed'))",
            params![attempt_id],
            |row| row.get(0),
        )?;
        if active_writer {
            bail!("snapshot cannot be frozen while the implementer writer is active or unresolved")
        }
        let source_generation: Option<String> = match kind {
            "plan" => crate::store::eligible_manager_plan(&transaction, attempt_id, true)?
                .map(|candidate| candidate.generation_id),
            "candidate" => crate::store::eligible_implementer_candidate(&transaction, attempt_id, true)?.map(|(_, generation, _)| generation),
            "checkpoint" => transaction.query_row("SELECT rg.id FROM role_generations rg JOIN sessions s ON s.role_generation_id=rg.id JOIN role_settings rs ON rs.effective_generation_id=rg.id AND rs.role='implementer' WHERE rg.attempt_id=?1 AND rg.role='implementer' AND s.status='exited' AND json_extract(s.exit_json,'$.process_group_quiescent')=1 ORDER BY rg.generation DESC LIMIT 1",params![attempt_id],|row|row.get(0)).optional()?,
            "accepted" => None,
            _ => unreachable!(),
        };
        if kind != "accepted" && source_generation.is_none() {
            bail!("snapshot kind {kind} lacks a current quiescent source generation")
        }
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        transaction.execute("INSERT INTO freeze_intents(id,attempt_id,kind,source_role_generation_id,state,created_at,updated_at) VALUES(?1,?2,?3,?4,'capturing',?5,?5)",params![id,attempt_id,kind,source_generation,now])?;
        transaction.commit()?;
        Ok(id)
    }

    fn freeze_reserved(
        &self,
        attempt_id: &str,
        kind: &str,
        freeze_id: &str,
    ) -> Result<serde_json::Value> {
        let (workspace_path, repository_path, repository_identity, attempt_base, active_writer): (
            String,
            String,
            String,
            String,
            bool,
        ) = {
            let connection = self.store.lock()?;
            connection.query_row("SELECT w.path,p.repository_path,p.repository_identity,a.base_revision,EXISTS(SELECT 1 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id WHERE rg.attempt_id=a.id AND rg.role='implementer' AND s.status NOT IN ('exited','launch_failed'))
                FROM attempts a JOIN tasks t ON t.id=a.task_id JOIN projects p ON p.id=t.project_id JOIN workspaces w ON w.attempt_id=a.id WHERE a.id=?1",
                params![attempt_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)))?
        };
        if active_writer {
            bail!("snapshot cannot be frozen while the implementer writer is active or unresolved")
        }
        if kind == "plan" {
            let (result_id, generation_id, plan, base, task_version): (
                String,
                String,
                String,
                String,
                i64,
            ) = {
                let connection = self.store.lock()?;
                let candidate = crate::store::eligible_manager_plan(&connection, attempt_id, true)?
                    .ok_or_else(||anyhow!("plan freeze requires a causally current quiescent manager plan_ready result with metadata.plan"))?;
                (
                    candidate.result_id,
                    candidate.generation_id,
                    candidate.plan,
                    candidate.base_revision,
                    candidate.task_version,
                )
            };
            if plan.trim().is_empty() || plan.len() > 256 * 1024 {
                bail!("reported plan must be non-empty and at most 256 KiB")
            }
            let hash = hex::encode(Sha256::digest(plan.as_bytes()));
            let previously_rejected: bool = {
                let connection = self.store.lock()?;
                connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM review_requests
                     WHERE attempt_id=?1 AND review_kind='plan' AND delivery_state='finished'
                       AND verdict IN ('request_changes','needs_rework') AND candidate_hash=?2)",
                    params![attempt_id, hash],
                    |row| row.get(0),
                )?
            };
            if previously_rejected {
                bail!("plan freeze cannot repeat a previously rejected plan candidate")
            }
            let snapshot_id = uuid::Uuid::new_v4().to_string();
            let destination = self.snapshots_root.join(&snapshot_id);
            std::fs::create_dir_all(&destination)?;
            crate::config::atomic_write(&destination.join("plan.md"), plan.as_bytes())?;
            let now = Utc::now().to_rfc3339();
            let mut connection = self.store.lock()?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let publication_plan_hash: Option<Option<String>> = transaction
                .query_row(
                    "SELECT a.plan_hash FROM freeze_intents fi
                 JOIN role_results rr ON rr.role_generation_id=fi.source_role_generation_id
                 JOIN role_generations rg ON rg.id=rr.role_generation_id
                 JOIN attempts a ON a.id=rg.attempt_id
                 JOIN role_settings rs ON rs.effective_generation_id=rg.id AND rs.role='manager'
                 JOIN sessions s ON s.id=rr.session_id AND s.role_generation_id=rg.id
                 WHERE fi.id=?1 AND fi.attempt_id=?2 AND fi.kind='plan' AND fi.state='capturing'
                   AND rr.id=?3 AND rr.role_generation_id=?4 AND rr.outcome='plan_ready'
                   AND rr.consumed_at IS NULL AND a.phase='planning'
                   AND (a.plan_hash IS NULL OR (
                     a.plan_approved_at IS NULL AND a.candidate_hash IS NULL
                     AND a.accepted_snapshot_id IS NULL
                     AND EXISTS(SELECT 1 FROM review_requests latest_finished
                       WHERE latest_finished.id=(
                         SELECT latest.id FROM review_requests latest
                         WHERE latest.attempt_id=a.id AND latest.review_kind='plan'
                           AND latest.delivery_state='finished'
                         ORDER BY latest.rowid DESC LIMIT 1)
                       AND latest_finished.verdict IN ('request_changes','needs_rework')
                       AND latest_finished.candidate_hash=a.plan_hash
                       AND json_extract(rr.metadata_json,'$.rejected_plan_review_request_id')=latest_finished.id)))
                   AND (s.status='exited' OR (s.status='running' AND s.readiness_state='idle_candidate'))
                   AND (NOT EXISTS(SELECT 1 FROM review_requests rejected
                          WHERE rejected.attempt_id=a.id AND rejected.review_kind='plan'
                            AND rejected.delivery_state='finished'
                            AND rejected.verdict IN ('request_changes','needs_rework'))
                        OR json_extract(rr.metadata_json,'$.rejected_plan_review_request_id')=(
                          SELECT latest.id FROM review_requests latest
                          WHERE latest.attempt_id=a.id AND latest.review_kind='plan'
                            AND latest.delivery_state='finished'
                            AND latest.verdict IN ('request_changes','needs_rework')
                          ORDER BY latest.rowid DESC LIMIT 1))
                   AND NOT EXISTS(SELECT 1 FROM review_requests rejected_hash
                     WHERE rejected_hash.attempt_id=a.id AND rejected_hash.review_kind='plan'
                       AND rejected_hash.delivery_state='finished'
                       AND rejected_hash.verdict IN ('request_changes','needs_rework')
                       AND rejected_hash.candidate_hash=?5)",
                    params![freeze_id, attempt_id, result_id, generation_id, hash],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?;
            let Some(replaced_rejected_plan_hash) = publication_plan_hash else {
                bail!(
                    "plan freeze authority or post-rejection causal identity changed before publication"
                )
            };
            let mut manifest = serde_json::json!({"schema":1,"kind":"plan","role_result_id":result_id,"bytes":plan.len(),"hash":hash});
            if let Some(replaced) = replaced_rejected_plan_hash.as_deref() {
                manifest["replaced_rejected_plan_hash"] = serde_json::json!(replaced);
            }
            let (workspace_id, settings_revision): (String, i64) = transaction.query_row(
                "SELECT w.id,rg.config_revision FROM workspaces w JOIN role_generations rg ON rg.attempt_id=w.attempt_id WHERE w.attempt_id=?1 AND rg.id=?2",
                params![attempt_id,generation_id],
                |row| Ok((row.get(0)?,row.get(1)?)),
            )?;
            let existing:Option<String>=transaction.query_row("SELECT id FROM snapshots WHERE attempt_id=?1 AND kind='plan' AND manifest_hash=?2",params![attempt_id,hash],|row|row.get(0)).optional()?;
            let persisted_id = existing.unwrap_or(snapshot_id);
            transaction.execute("INSERT OR IGNORE INTO snapshots(id,attempt_id,kind,snapshot_base,manifest_hash,manifest_json,complete,created_at,original_base,candidate_head,source_role_generation_id,source_settings_revision,workspace_id,workspace_hash) VALUES(?1,?2,'plan',?3,?4,?5,1,?6,?3,?3,?7,?8,?9,?4)",
                params![persisted_id,attempt_id,base,hash,manifest.to_string(),now,generation_id,settings_revision,workspace_id])?;
            if transaction.execute(
                "UPDATE attempts SET plan_hash=?1,updated_at=?2
                 WHERE id=?3 AND phase='planning' AND plan_hash IS ?4
                   AND (?4 IS NULL OR (plan_approved_at IS NULL AND candidate_hash IS NULL
                     AND accepted_snapshot_id IS NULL))",
                params![hash, now, attempt_id, replaced_rejected_plan_hash],
            )? != 1
            {
                bail!("plan freeze attempt changed before publication")
            }
            if transaction.execute(
                "UPDATE role_results SET consumed_at=?1 WHERE id=?2 AND consumed_at IS NULL",
                params![now, result_id],
            )? != 1
            {
                bail!("plan freeze result changed before publication")
            }
            transaction.execute("UPDATE controls SET state='superseded',updated_at=?1 WHERE attempt_id=?2 AND kind='transition_proposal' AND state='proposed'",params![now,attempt_id])?;
            let proposal_id = uuid::Uuid::new_v4().to_string();
            let proposal = serde_json::json!({"phase":"plan_review","evidence":["current manager plan_ready result frozen by service"],"source_phase":"planning","plan_hash":hash.clone(),"candidate_hash":null,"role_generation_id":generation_id.clone(),"expected_task_version":task_version});
            transaction.execute("INSERT INTO controls(id,attempt_id,role_generation_id,kind,state,expected_version,payload_json,created_at,updated_at) VALUES(?1,?2,?3,'transition_proposal','proposed',?4,?5,?6,?6)",params![proposal_id,attempt_id,generation_id,task_version,proposal.to_string(),now])?;
            if transaction.execute("UPDATE freeze_intents SET state='complete',result_snapshot_id=?1,updated_at=?2 WHERE id=?3 AND state='capturing'",params![persisted_id,now,freeze_id])? != 1 {
                bail!("plan freeze intent changed before publication")
            }
            transaction.commit()?;
            return Ok(
                serde_json::json!({"snapshot_id":persisted_id,"kind":"plan","manifest_hash":hash,"manifest":manifest}),
            );
        }
        if kind != "checkpoint" {
            let connection = self.store.lock()?;
            let authorized: bool = if kind == "candidate" {
                crate::store::eligible_implementer_candidate(&connection, attempt_id, true)?
                    .is_some()
            } else {
                connection.query_row("SELECT EXISTS(SELECT 1 FROM review_requests r JOIN attempts a ON a.id=r.attempt_id WHERE a.id=?1 AND r.review_kind='final' AND r.candidate_hash=a.candidate_hash AND r.verdict='approved' AND r.delivery_state='finished')",params![attempt_id],|row|row.get(0))?
            };
            if !authorized {
                bail!("snapshot kind {kind} lacks its authenticated quiescent role/review gate")
            }
        }
        let mut repository = crate::workspace::inspect(Path::new(&repository_path))?;
        if repository.identity != repository_identity {
            bail!("physical repository identity changed")
        }
        if !crate::workspace::commit_exists(&repository, &attempt_base)? {
            bail!("original attempt base is no longer reachable")
        }
        repository.head = attempt_base;
        let snapshot_id = uuid::Uuid::new_v4().to_string();
        let destination = self.snapshots_root.join(&snapshot_id);
        let (manifest, hash) =
            crate::snapshot::capture(&repository, Path::new(&workspace_path), &destination)?;
        let now = Utc::now().to_rfc3339();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<String> = transaction
            .query_row(
                "SELECT id FROM snapshots WHERE attempt_id=?1 AND kind=?2 AND manifest_hash=?3",
                params![attempt_id, kind, hash],
                |row| row.get(0),
            )
            .optional()?;
        let persisted_id = existing.unwrap_or(snapshot_id);
        let (original_base, workspace_id, source_generation, settings_revision): (String, String, Option<String>, Option<i64>) = transaction.query_row("SELECT a.base_revision,w.id,fi.source_role_generation_id,rg.config_revision FROM freeze_intents fi JOIN attempts a ON a.id=fi.attempt_id JOIN workspaces w ON w.attempt_id=a.id LEFT JOIN role_generations rg ON rg.id=fi.source_role_generation_id WHERE fi.id=?1 AND fi.state='capturing'",params![freeze_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?;
        transaction.execute("INSERT OR IGNORE INTO snapshots(id,attempt_id,kind,snapshot_base,manifest_hash,manifest_json,complete,created_at,original_base,candidate_head,source_role_generation_id,source_settings_revision,workspace_id,workspace_hash) VALUES(?1,?2,?3,?4,?5,?6,1,?7,?8,?9,?10,?11,?12,?5)",
            params![persisted_id,attempt_id,kind,manifest.snapshot_base,hash,serde_json::to_string(&manifest)?,now,original_base,manifest.candidate_head,source_generation,settings_revision,workspace_id])?;
        if kind == "accepted" {
            let reviewed: Option<String> = transaction.query_row(
                "SELECT candidate_hash FROM attempts WHERE id=?1",
                params![attempt_id],
                |row| row.get(0),
            )?;
            if reviewed.as_deref() != Some(hash.as_str()) {
                bail!("live workspace bytes drifted from the reviewed candidate")
            }
            transaction.execute(
                "UPDATE attempts SET accepted_snapshot_id=?1,updated_at=?2 WHERE id=?3",
                params![persisted_id, now, attempt_id],
            )?;
        } else if kind == "candidate" {
            let (result_id, _, _) =
                crate::store::eligible_implementer_candidate(&transaction, attempt_id, true)?
                    .ok_or_else(|| anyhow!("candidate source changed before publication"))?;
            if transaction.execute(
                "UPDATE role_results SET consumed_at=?1 WHERE id=?2 AND consumed_at IS NULL",
                params![now, result_id],
            )? != 1
            {
                bail!("candidate result changed before publication")
            }
            transaction.execute(
                "UPDATE attempts SET candidate_hash=?1,updated_at=?2 WHERE id=?3",
                params![hash, now, attempt_id],
            )?;
        }
        transaction.execute("UPDATE freeze_intents SET state='complete',result_snapshot_id=?1,updated_at=?2 WHERE id=?3 AND state='capturing'",params![persisted_id,now,freeze_id])?;
        transaction.commit()?;
        Ok(
            serde_json::json!({"snapshot_id":persisted_id,"kind":kind,"manifest_hash":hash,"manifest":manifest}),
        )
    }

    pub fn verify(&self, snapshot_id: &str) -> Result<bool> {
        let (kind, manifest_hash, manifest_json, workspace_path, repository_path): (
            String,
            String,
            String,
            String,
            String,
        ) = {
            let connection = self.store.lock()?;
            connection.query_row("SELECT s.kind,s.manifest_hash,s.manifest_json,w.path,p.repository_path FROM snapshots s JOIN attempts a ON a.id=s.attempt_id JOIN tasks t ON t.id=a.task_id JOIN projects p ON p.id=t.project_id JOIN workspaces w ON w.attempt_id=a.id WHERE s.id=?1",params![snapshot_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)))?
        };
        if kind == "plan" {
            let bytes = std::fs::read(self.snapshots_root.join(snapshot_id).join("plan.md"))?;
            return Ok(hex::encode(Sha256::digest(bytes)) == manifest_hash);
        }
        let expected: SnapshotManifest = serde_json::from_str(&manifest_json)?;
        let temp = self
            .snapshots_root
            .join(format!("verify-{}", uuid::Uuid::new_v4()));
        let mut repository = crate::workspace::inspect(Path::new(&repository_path))?;
        repository.head = if expected.original_base.is_empty() {
            expected.snapshot_base.clone()
        } else {
            expected.original_base.clone()
        };
        let (observed, _) =
            crate::snapshot::capture(&repository, Path::new(&workspace_path), &temp)?;
        std::fs::remove_dir_all(temp)?;
        Ok(serde_json::to_vec(&expected)? == serde_json::to_vec(&observed)?)
    }

    pub fn materialize_rework(&self, snapshot_id: &str, destination: &Path) -> Result<()> {
        let (manifest_json, repository_path): (String, String) = {
            let connection = self.store.lock()?;
            connection.query_row("SELECT s.manifest_json,p.repository_path FROM snapshots s JOIN attempts a ON a.id=s.attempt_id JOIN tasks t ON t.id=a.task_id JOIN projects p ON p.id=t.project_id WHERE s.id=?1",params![snapshot_id],|row|Ok((row.get(0)?,row.get(1)?)))
                .optional()?.ok_or_else(||anyhow!("unknown snapshot"))?
        };
        let manifest: SnapshotManifest = serde_json::from_str(&manifest_json)?;
        let repository = crate::workspace::inspect(Path::new(&repository_path))?;
        crate::workspace::create_detached_worktree(
            &repository,
            destination,
            &manifest.snapshot_base,
        )?;
        crate::snapshot::materialize(
            &manifest,
            &self.snapshots_root.join(snapshot_id),
            destination,
        )
    }

    pub fn verify_materialized(&self, snapshot_id: &str, destination: &Path) -> Result<bool> {
        let manifest_json: String = {
            let connection = self.store.lock()?;
            connection.query_row("SELECT manifest_json FROM snapshots WHERE id=?1 AND kind='accepted' AND complete=1",params![snapshot_id],|row|row.get(0))?
        };
        let manifest: SnapshotManifest = serde_json::from_str(&manifest_json)?;
        crate::snapshot::verify_materialized(&manifest, destination)
    }

    /// Verifies a rework child's workspace against its intent's source. An
    /// accepted source keeps the accepted-snapshot verification above. A
    /// candidate source verifies only as the exact rejected candidate recorded
    /// by one terminal replan of the intent's parent; it never gains accepted
    /// authority.
    pub fn verify_rework_source(&self, intent_id: &str, destination: &Path) -> Result<bool> {
        let (snapshot_id, kind, terminal_manifest): (String, String, Option<String>) = {
            let connection = self.store.lock()?;
            connection.query_row(
                "SELECT s.id,s.kind,
                   (SELECT source.manifest_json FROM attempts child
                    JOIN snapshots source ON source.id=ri.snapshot_id
                      AND source.attempt_id=ri.parent_attempt_id
                      AND source.kind='candidate' AND source.complete=1
                    JOIN audit_events provenance
                      ON provenance.event_code='rework.terminal_replan.created'
                      AND provenance.entity_id=child.id
                      AND json_extract(provenance.detail_json,'$.intent_id')=ri.id
                      AND json_extract(provenance.detail_json,'$.source_snapshot_id')=source.id
                      AND json_extract(provenance.detail_json,'$.candidate_hash')=source.manifest_hash
                    JOIN review_requests terminal
                      ON terminal.id=json_extract(provenance.detail_json,'$.terminal_review.review_request_id')
                      AND terminal.attempt_id=ri.parent_attempt_id
                      AND terminal.review_kind IN ('code','final')
                      AND terminal.delivery_state='finished'
                      AND terminal.verdict IN ('request_changes','needs_rework')
                      AND terminal.candidate_hash=source.manifest_hash
                    JOIN role_results result
                      ON result.id=json_extract(provenance.detail_json,'$.terminal_review.role_result_id')
                      AND result.consumed_at IS NOT NULL AND result.outcome=terminal.verdict
                      AND result.role_generation_id=terminal.role_generation_id
                      AND result.session_id=terminal.session_id
                      AND json_extract(result.metadata_json,'$.review_request_id')=terminal.id
                    WHERE child.id=ri.new_attempt_id
                      AND child.parent_attempt_id=ri.parent_attempt_id
                      AND child.accepted_snapshot_id IS NULL AND ri.carry_plan_approval=0
                      AND (SELECT COUNT(*) FROM audit_events other
                           WHERE other.event_code='rework.terminal_replan.created'
                             AND other.entity_id=child.id)=1
                      AND (SELECT COUNT(*) FROM role_results other
                           WHERE json_extract(other.metadata_json,'$.review_request_id')=terminal.id)=1)
                 FROM rework_intents ri JOIN snapshots s ON s.id=ri.snapshot_id WHERE ri.id=?1",
                params![intent_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?
        };
        if kind == "accepted" {
            return self.verify_materialized(&snapshot_id, destination);
        }
        let manifest_json = terminal_manifest.ok_or_else(|| {
            anyhow!("the rework source is not the exact rejected candidate of a recorded terminal replan")
        })?;
        let manifest: SnapshotManifest = serde_json::from_str(&manifest_json)?;
        crate::snapshot::verify_materialized(&manifest, destination)
    }

    pub fn reserve_request(
        &self,
        attempt_id: &str,
        kind: &str,
        prompt: &str,
        handoff: serde_json::Value,
    ) -> Result<ReviewDispatch> {
        let role = match kind {
            "plan" => crate::domain::RoleKind::PlanReviewer,
            "code" => crate::domain::RoleKind::CodeReviewer,
            "final" => crate::domain::RoleKind::FinalReviewer,
            _ => bail!("invalid review kind"),
        };
        if prompt.trim().is_empty()
            || prompt.len() > 256 * 1024
            || !handoff.is_object()
            || serde_json::to_vec(&handoff)?.len() > 256 * 1024
        {
            bail!("review prompt and structured handoff are required")
        }
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let candidate:Option<String>=transaction.query_row("SELECT CASE WHEN ?1='plan' THEN plan_hash ELSE candidate_hash END FROM attempts WHERE id=?2",params![kind,attempt_id],|row|row.get(0))?;
        let candidate = candidate.ok_or_else(|| anyhow!("review candidate is not frozen"))?;
        let existing=transaction.query_row(
            "SELECT id,prompt_hash,handoff_hash,delivery_state,prompt_text,handoff_json FROM review_requests WHERE attempt_id=?1 AND review_kind=?2 AND delivery_state IN ('reserved','launching','delivered','ambiguous')",
            params![attempt_id,kind],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,Option<String>>(4)?,row.get::<_,Option<String>>(5)?))).optional()?;
        // Decided before any reviewer binding can change: in the final repair
        // round a code review runs only as the receipt's dedicated recheck,
        // including one that is already launching, delivered or ambiguous.
        let mut retained_recheck = false;
        if kind == "code" {
            match final_repair_code_review_binding(
                &transaction,
                attempt_id,
                existing
                    .as_ref()
                    .map(|request| (request.0.as_str(), request.3 != "reserved")),
            )? {
                RecheckBinding::Held(reason) => {
                    hold_final_repair_recheck(
                        &transaction,
                        attempt_id,
                        &reason,
                        &Utc::now().to_rfc3339(),
                    )?;
                    transaction.commit()?;
                    bail!("final-repair recheck held: {reason}")
                }
                RecheckBinding::Retained => retained_recheck = true,
                RecheckBinding::Ordinary => {}
            }
        }
        // Before a reviewer session exists for this request, the reviewer
        // moves to its newest activated profile; a request is never launched
        // for reviewer settings that were replaced. The dedicated recheck
        // keeps its receipt's retained reviewer instead.
        if !retained_recheck
            && existing
                .as_ref()
                .is_none_or(|request| request.3 == "reserved")
        {
            let now = Utc::now().to_rfc3339();
            if let crate::trip::ReadOnlyProfileBoundary::Pending { message, .. } =
                crate::trip::materialize_read_only_profile(&transaction, attempt_id, role, &now)?
            {
                bail!("reviewer profile change pending: {message}")
            }
        }
        if let Some((id, stored_prompt, stored_handoff, state, prompt_text, handoff_json)) =
            existing
        {
            if stored_prompt != crate::store::json_hash(&prompt)?
                || stored_handoff != crate::store::json_hash(&handoff)?
            {
                bail!("active review request must resume with identical prompt and handoff")
            }
            transaction.commit()?;
            return Ok(ReviewDispatch {
                request_id: id,
                attempt_id: attempt_id.into(),
                role,
                candidate_hash: candidate,
                prompt: prompt_text.unwrap_or_else(|| prompt.into()),
                handoff: handoff_json
                    .map(|value| serde_json::from_str(&value))
                    .transpose()?
                    .unwrap_or(handoff),
                state,
            });
        }
        if kind == "code" {
            let now = Utc::now().to_rfc3339();
            match final_repair_recheck_lane(
                &transaction,
                attempt_id,
                &candidate,
                prompt,
                &handoff,
                &now,
            )? {
                RecheckLane::Ordinary => {}
                RecheckLane::Dispatch(request_id, state) => {
                    transaction.commit()?;
                    return Ok(ReviewDispatch {
                        request_id,
                        attempt_id: attempt_id.into(),
                        role,
                        candidate_hash: candidate,
                        prompt: prompt.into(),
                        handoff,
                        state,
                    });
                }
                RecheckLane::Closed => {
                    transaction.commit()?;
                    bail!("the final-repair recheck ended without a verdict; the attempt has no further code review")
                }
            }
        }
        let budget:(i64,i64,i64)=transaction.query_row("SELECT initial_allowance,extension_allowance,spent FROM review_budgets WHERE attempt_id=?1 AND review_kind=?2",params![attempt_id,kind],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
        if budget.2 >= budget.0 + budget.1 {
            let now = Utc::now().to_rfc3339();
            transaction.execute("UPDATE tasks SET attention='needs_review_budget',version=version+1,updated_at=?1 WHERE id=(SELECT task_id FROM attempts WHERE id=?2)",params![now,attempt_id])?;
            transaction.execute(
                "UPDATE attempts SET status='needs_input',updated_at=?1 WHERE id=?2",
                params![now, attempt_id],
            )?;
            transaction.commit()?;
            bail!("review budget exhausted")
        }
        let id = insert_review_request(
            &transaction,
            attempt_id,
            kind,
            &candidate,
            prompt,
            &handoff,
            &Utc::now().to_rfc3339(),
        )?;
        transaction.commit()?;
        Ok(ReviewDispatch {
            request_id: id,
            attempt_id: attempt_id.into(),
            role,
            candidate_hash: candidate,
            prompt: prompt.into(),
            handoff,
            state: "reserved".into(),
        })
    }

    pub fn bind_delivery(
        &self,
        request_id: &str,
        session_id: &str,
        generation_id: &str,
        settings_revision: i64,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (attempt,kind,spent):(String,String,Option<String>)=transaction.query_row("SELECT attempt_id,review_kind,budget_spent_at FROM review_requests WHERE id=?1 AND delivery_state='launching' AND role_generation_id=?2 AND session_id=?3 AND settings_revision=?4",params![request_id,generation_id,session_id,settings_revision],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
        transaction.execute("UPDATE review_requests SET delivery_state='delivered',budget_spent_at=COALESCE(budget_spent_at,?1),updated_at=?1 WHERE id=?2 AND delivery_state='launching'",params![now,request_id])?;
        if final_repair_recheck_request(&transaction, request_id)? {
            spend_final_repair_recheck(&transaction, request_id, &now)?;
        } else if spent.is_none() {
            transaction.execute(
                "UPDATE review_budgets SET spent=spent+1 WHERE attempt_id=?1 AND review_kind=?2",
                params![attempt, kind],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn bind_launch_intent(
        &self,
        request_id: &str,
        session_id: &str,
        generation_id: &str,
        settings_revision: i64,
    ) -> Result<()> {
        let connection = self.store.lock()?;
        let retained: Option<i64> = connection
            .query_row(
                "SELECT reviewer_settings_revision FROM final_repair_rechecks
                 WHERE review_request_id=?1",
                params![request_id],
                |row| row.get(0),
            )
            .optional()?;
        if retained.is_some_and(|revision| revision != settings_revision) {
            bail!("the final-repair recheck must launch under the retained code reviewer's settings revision")
        }
        // Lineage guard: the launch must use the reviewer settings the attempt
        // is bound to now, not a revision replaced after the request was made.
        let changed = connection.execute(
            "UPDATE review_requests SET role_generation_id=?1,session_id=?2,settings_revision=?3,delivery_state='launching',updated_at=?4
             WHERE id=?5 AND delivery_state IN ('reserved','nondelivered')
               AND NOT EXISTS(SELECT 1 FROM trip_attempt_profiles ap
                 WHERE ap.attempt_id=review_requests.attempt_id
                   AND ap.role=CASE review_requests.review_kind WHEN 'plan' THEN 'plan_reviewer'
                     WHEN 'code' THEN 'code_reviewer' WHEN 'final' THEN 'final_verifier' END
                   AND ap.settings_revision!=?3)",
            params![generation_id,session_id,settings_revision,Utc::now().to_rfc3339(),request_id],
        )?;
        if changed != 1 {
            bail!("review request is not available for this launch intent")
        }
        Ok(())
    }

    pub fn mark_delivery_failure(
        &self,
        request_id: &str,
        session_id: Option<&str>,
        ambiguous: bool,
        reason: &str,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (attempt, kind, candidate, spent_at, initial, extension, spent_before): (
            String,
            String,
            String,
            Option<String>,
            i64,
            i64,
            i64,
        ) = transaction.query_row(
            "SELECT r.attempt_id,r.review_kind,r.candidate_hash,r.budget_spent_at,
                    b.initial_allowance,b.extension_allowance,b.spent
             FROM review_requests r JOIN review_budgets b
               ON b.attempt_id=r.attempt_id AND b.review_kind=r.review_kind
             WHERE r.id=?1",
            params![request_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )?;
        let recheck = final_repair_recheck_request(&transaction, request_id)?;
        if ambiguous && recheck {
            spend_final_repair_recheck(&transaction, request_id, &now)?;
        } else if ambiguous && spent_at.is_none() {
            transaction.execute(
                "UPDATE review_budgets SET spent=spent+1 WHERE attempt_id=?1 AND review_kind=?2",
                params![attempt, kind],
            )?;
        }
        transaction.execute("UPDATE review_requests SET session_id=COALESCE(?1,session_id),delivery_state=?2,ambiguity_state=?3,budget_spent_at=CASE WHEN ?4 THEN COALESCE(budget_spent_at,?5) ELSE budget_spent_at END,feedback=?6,updated_at=?5 WHERE id=?7",
            params![session_id,if ambiguous{"ambiguous"}else{"nondelivered"},if ambiguous{"delivery_unknown"}else{"proven_nondelivery"},ambiguous,now,reason,request_id])?;
        if ambiguous {
            let (spent_after, budget_spent_at): (i64, Option<String>) = transaction.query_row(
                "SELECT b.spent,r.budget_spent_at FROM review_budgets b JOIN review_requests r
                   ON r.attempt_id=b.attempt_id AND r.review_kind=b.review_kind
                 WHERE r.id=?1",
                params![request_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let allowance = initial + extension;
            transaction.execute(
                "UPDATE attempts SET status='needs_recovery',updated_at=?1 WHERE id=?2",
                params![now, attempt],
            )?;
            transaction.execute("UPDATE tasks SET version=version+CASE WHEN attention='needs_recovery' THEN 0 ELSE 1 END,attention='needs_recovery',updated_at=?1 WHERE id=(SELECT task_id FROM attempts WHERE id=?2)",params![now,attempt])?;
            transaction.execute(
                "UPDATE claims SET state='unknown',updated_at=?1 WHERE attempt_id=?2",
                params![now, attempt],
            )?;
            let existing: Option<(String, String)> = session_id.map(|session| {
                transaction.query_row(
                    "SELECT id,detail_json FROM recovery_records WHERE session_id=?1 AND state='attention_required' ORDER BY created_at LIMIT 1",
                    params![session],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                ).optional()
            }).transpose()?.flatten();
            let mut review_detail = serde_json::json!({
                "kind":"provider_delivery_unknown",
                "review_request_id":request_id,
                "candidate_hash":candidate,
                "review_kind":kind,
                "review_delivery":"ambiguous",
                "spent_round_retained":true,
                "budget_spent_at":budget_spent_at,
                "budget_spent_at_before":spent_at,
                "budget_spent_at_after":budget_spent_at,
                "budget_before":{
                    "initial_allowance":initial,
                    "extension_allowance":extension,
                    "allowance_total":allowance,
                    "spent":spent_before,
                    "remaining":allowance-spent_before
                },
                "budget_after":{
                    "initial_allowance":initial,
                    "extension_allowance":extension,
                    "allowance_total":allowance,
                    "spent":spent_after,
                    "remaining":allowance-spent_after
                },
                "replacement_allowed_after_quiescence":!recheck,
                "review_delivery_reason":reason,
            });
            if recheck {
                review_detail["accounting_lane"] = serde_json::json!("final_repair_recheck");
            }
            if let Some((record, detail)) = existing {
                let mut merged: serde_json::Value =
                    serde_json::from_str(&detail).unwrap_or_default();
                if let (Some(target), Some(source)) =
                    (merged.as_object_mut(), review_detail.as_object())
                {
                    target.extend(source.clone());
                } else {
                    merged = review_detail;
                }
                transaction.execute(
                    "UPDATE recovery_records SET detail_json=?1,updated_at=?2 WHERE id=?3",
                    params![merged.to_string(), now, record],
                )?;
            } else {
                let mut incident_detail = review_detail;
                if let Some(target) = incident_detail.as_object_mut() {
                    target.insert("reason".to_owned(), serde_json::json!(reason));
                }
                transaction.execute(
                    "INSERT INTO recovery_records(id,session_id,attempt_id,state,detail_json,created_at,updated_at) VALUES(?1,?2,?3,'attention_required',?4,?5,?5)",
                    params![uuid::Uuid::new_v4().to_string(),session_id,attempt,incident_detail.to_string(),now],
                )?;
            }
        }
        transaction.commit()?;
        Ok(())
    }
}

fn insert_review_request(
    connection: &Connection,
    attempt_id: &str,
    kind: &str,
    candidate: &str,
    prompt: &str,
    handoff: &serde_json::Value,
    now: &str,
) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let operation_key = crate::store::json_hash(
        &serde_json::json!({"attempt_id":attempt_id,"kind":kind,"candidate":candidate,"prompt":prompt,"handoff":handoff}),
    )?;
    connection.execute("INSERT INTO review_requests(id,attempt_id,review_kind,candidate_hash,prompt_hash,handoff_hash,delivery_state,created_at,updated_at,operation_key,prompt_text,handoff_json) VALUES(?1,?2,?3,?4,?5,?6,'reserved',?7,?7,?8,?9,?10)",
        params![id,attempt_id,kind,candidate,crate::store::json_hash(&prompt)?,crate::store::json_hash(handoff)?,now,operation_key,prompt,handoff.to_string()])?;
    Ok(id)
}

enum RecheckLane {
    Ordinary,
    Dispatch(String, String),
    Closed,
}

/// A receipt replaces the ordinary allowance for the attempt's code reviews:
/// it binds one candidate and request, and once spent or settled it never
/// admits another code review.
fn final_repair_recheck_lane(
    connection: &Connection,
    attempt_id: &str,
    candidate: &str,
    prompt: &str,
    handoff: &serde_json::Value,
    now: &str,
) -> Result<RecheckLane> {
    let receipt: Option<(String, Option<String>, Option<String>)> = connection
        .query_row(
            "SELECT state,candidate_hash,review_request_id
             FROM final_repair_rechecks WHERE attempt_id=?1",
            params![attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    // `final_repair_code_review_binding` already held a final-repair attempt
    // without a valid receipt, so no receipt here means an ordinary attempt.
    let Some((state, bound, request)) = receipt else {
        return Ok(RecheckLane::Ordinary);
    };
    match state.as_str() {
        "authorized" => {
            let snapshot: String = connection
                .query_row(
                    "SELECT id FROM snapshots WHERE attempt_id=?1 AND kind='candidate'
                       AND manifest_hash=?2 AND complete=1",
                    params![attempt_id, candidate],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or_else(|| anyhow!("the repaired candidate has no complete frozen snapshot"))?;
            let id = insert_review_request(
                connection, attempt_id, "code", candidate, prompt, handoff, now,
            )?;
            if connection.execute(
                "UPDATE final_repair_rechecks SET state='reserved',candidate_hash=?1,
                   candidate_snapshot_id=?2,review_request_id=?3,updated_at=?4
                 WHERE attempt_id=?5 AND state='authorized'",
                params![candidate, snapshot, id, now, attempt_id],
            )? != 1
            {
                bail!("the final-repair recheck receipt changed during reservation")
            }
            Ok(RecheckLane::Dispatch(id, "reserved".into()))
        }
        "reserved" => {
            if bound.as_deref() != Some(candidate) {
                bail!("the final-repair recheck is bound to a different candidate")
            }
            let request = request
                .ok_or_else(|| anyhow!("the reserved final-repair recheck lost its request"))?;
            let (delivery, prompt_hash, handoff_hash): (String, String, String) = connection
                .query_row(
                    "SELECT delivery_state,prompt_hash,handoff_hash FROM review_requests
                     WHERE id=?1",
                    params![request],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
            if delivery != "nondelivered" {
                bail!("the final-repair recheck request is {delivery}")
            }
            if prompt_hash != crate::store::json_hash(&prompt)?
                || handoff_hash != crate::store::json_hash(handoff)?
            {
                bail!("the final-repair recheck must retry with identical prompt and handoff")
            }
            Ok(RecheckLane::Dispatch(request, delivery))
        }
        "spent" => {
            // Delivered and ambiguous requests resume before this point, so the
            // spent request left delivery without any verdict.
            close_final_repair_recheck(connection, attempt_id, "delivery_failed", now)?;
            Ok(RecheckLane::Closed)
        }
        _ => bail!("the final-repair recheck is settled; the attempt has no further code review"),
    }
}

/// Closes a spent receipt without a verdict and ends the attempt incomplete.
/// The request and its spend stay recorded.
fn close_final_repair_recheck(
    connection: &Connection,
    attempt_id: &str,
    reason: &str,
    now: &str,
) -> Result<()> {
    if connection.execute(
        "UPDATE final_repair_rechecks SET state='closed',closed_reason=?1,updated_at=?2
         WHERE attempt_id=?3 AND state='spent'",
        params![reason, now, attempt_id],
    )? != 1
    {
        bail!("the final-repair recheck receipt is not spent")
    }
    connection.execute(
        "UPDATE attempts SET phase='needs_input',updated_at=?1 WHERE id=?2",
        params![now, attempt_id],
    )?;
    connection.execute(
        "UPDATE tasks SET version=version+1,
         attention=CASE WHEN attention IN ('paused','pause_requested','needs_recovery')
           THEN attention ELSE 'needs_input' END,updated_at=?1
         WHERE id=(SELECT task_id FROM attempts WHERE id=?2)",
        params![now, attempt_id],
    )?;
    crate::trip::retire_unmatchable_transition_proposals(
        connection,
        attempt_id,
        "final_repair_recheck_closed",
        now,
    )?;
    connection.execute(
        "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
         VALUES(?1,?2,'service','attempt.final_repair_recheck.closed','attempt',?3,?4,?5)",
        params![
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
            attempt_id,
            serde_json::json!({"closed_reason":reason}).to_string(),
            now
        ],
    )?;
    Ok(())
}

/// The delivered final-repair recheck whose reviewer session exited without
/// an eligible structured result. The exited session can never report, so the
/// delivery consumed the one shot.
pub(crate) fn unanswered_final_repair_recheck(
    connection: &Connection,
    attempt_id: &str,
) -> Result<Option<String>> {
    Ok(connection
        .query_row(
            "SELECT r.id FROM final_repair_rechecks f
             JOIN review_requests r ON r.id=f.review_request_id
             JOIN sessions s ON s.id=r.session_id
             WHERE f.attempt_id=?1 AND f.state='spent' AND r.delivery_state='delivered'
               AND s.status='exited'
               AND NOT EXISTS(SELECT 1 FROM role_results rr
                 WHERE rr.role_generation_id=r.role_generation_id AND rr.session_id=r.session_id
                   AND json_extract(rr.metadata_json,'$.review_request_id')=r.id
                   AND json_extract(rr.metadata_json,'$.review_kind')=r.review_kind
                   AND json_extract(rr.metadata_json,'$.candidate_hash')=r.candidate_hash)",
            params![attempt_id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Retires the unanswered recheck request and closes its receipt; returns the
/// request, or `None` when there is nothing left to close.
pub(crate) fn close_unanswered_final_repair_recheck(
    connection: &Connection,
    attempt_id: &str,
    now: &str,
) -> Result<Option<String>> {
    let Some(request) = unanswered_final_repair_recheck(connection, attempt_id)? else {
        return Ok(None);
    };
    connection.execute(
        "UPDATE review_requests SET delivery_state='abandoned',feedback=?1,updated_at=?2
         WHERE id=?3 AND delivery_state='delivered'",
        params![
            "the final-repair recheck reviewer exited without an eligible structured result",
            now,
            request
        ],
    )?;
    close_final_repair_recheck(
        connection,
        attempt_id,
        "reviewer_exited_without_result",
        now,
    )?;
    Ok(Some(request))
}

fn final_repair_recheck_request(connection: &Connection, request_id: &str) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM final_repair_rechecks WHERE review_request_id=?1)",
        params![request_id],
        |row| row.get(0),
    )?)
}

fn spend_final_repair_recheck(connection: &Connection, request_id: &str, now: &str) -> Result<()> {
    connection.execute(
        "UPDATE final_repair_rechecks SET state='spent',spent_at=?1,updated_at=?1
         WHERE review_request_id=?2 AND state='reserved'",
        params![now, request_id],
    )?;
    Ok(())
}

/// Records the verdict when `request_id` is the final-repair recheck and
/// returns whether it was. Only an explicit approval continues; any other
/// verdict closes the lane and the caller ends the attempt incomplete.
pub(crate) fn settle_final_repair_recheck(
    connection: &Connection,
    request_id: &str,
    verdict: &str,
    now: &str,
) -> Result<bool> {
    if !final_repair_recheck_request(connection, request_id)? {
        return Ok(false);
    }
    if connection.execute(
        "UPDATE final_repair_rechecks SET
           state=CASE WHEN ?1='approved' THEN 'approved' ELSE 'closed' END,verdict=?1,
           closed_reason=CASE WHEN ?1='approved' THEN NULL ELSE 'nonapproval' END,updated_at=?2
         WHERE review_request_id=?3 AND state='spent'",
        params![verdict, now, request_id],
    )? != 1
    {
        bail!("the final-repair recheck verdict does not match its spent receipt")
    }
    Ok(true)
}

/// Opens the one dedicated final repair when the first final review asks for
/// changes. Both result consumers call this in the transaction that then
/// consumes `final_result_id` and applies the verdict, so the receipt, the
/// repair round and the verdict commit together or not at all. Every binding
/// is derived from the ledger: the delivered final request and its unconsumed
/// authenticated result, the single finished code approval of the same
/// candidate, and that approval's reviewer, who is retained for the recheck.
pub(crate) fn begin_normal_final_repair(
    connection: &Connection,
    attempt_id: &str,
    final_request_id: &str,
    final_result_id: &str,
    now: &str,
) -> Result<()> {
    type AttemptState = (
        String,
        i64,
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let (task_id, round, task_version, plan_hash, configuration_hash, candidate): AttemptState =
        connection.query_row(
            "SELECT a.task_id,a.final_repair_round,t.version,a.plan_hash,a.configuration_hash,
                    a.candidate_hash
             FROM attempts a JOIN tasks t ON t.id=a.task_id WHERE a.id=?1",
            params![attempt_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )?;
    if round != 0 {
        bail!("final request_changes exceeds the one dedicated repair cycle")
    }
    let (Some(plan_hash), Some(candidate)) = (plan_hash, candidate) else {
        bail!("final request_changes requires the approved plan and the frozen candidate")
    };
    let receipt_exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM final_repair_rechecks WHERE attempt_id=?1)",
        params![attempt_id],
        |row| row.get(0),
    )?;
    if receipt_exists {
        bail!("a final-repair recheck receipt already exists for this attempt")
    }
    let final_bound: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM review_requests r
           JOIN role_generations g ON g.id=r.role_generation_id AND g.attempt_id=r.attempt_id
             AND g.role='final_verifier'
           JOIN sessions s ON s.id=r.session_id AND s.role_generation_id=g.id
           JOIN role_results rr ON rr.id=?3 AND rr.role_generation_id=g.id AND rr.session_id=s.id
           WHERE r.id=?2 AND r.attempt_id=?1 AND r.review_kind='final'
             AND r.delivery_state='delivered' AND r.candidate_hash=?4
             AND rr.outcome='request_changes' AND rr.consumed_at IS NULL
             AND json_extract(rr.metadata_json,'$.review_request_id')=r.id
             AND json_extract(rr.metadata_json,'$.review_kind')='final'
             AND json_extract(rr.metadata_json,'$.candidate_hash')=r.candidate_hash)",
        params![attempt_id, final_request_id, final_result_id, candidate],
        |row| row.get(0),
    )?;
    if !final_bound {
        bail!("final request_changes is not the delivered final request's unconsumed authenticated result for the frozen candidate")
    }
    let approvals = {
        let mut statement = connection.prepare(
            "SELECT id,role_generation_id,session_id,settings_revision FROM review_requests
             WHERE attempt_id=?1 AND review_kind='code' AND delivery_state='finished'
               AND verdict='approved' AND candidate_hash=?2",
        )?;
        let rows = statement
            .query_map(params![attempt_id, candidate], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let (approved, generation, session, revision) = match approvals.as_slice() {
        [(approved, Some(generation), Some(session), Some(revision))] => {
            (approved.clone(), generation.clone(), session.clone(), *revision)
        }
        [] => bail!("final request_changes lacks a finished code approval of the frozen candidate"),
        [_] => bail!("the code approval of the frozen candidate has no recorded reviewer binding"),
        _ => bail!("the frozen candidate has more than one code approval, so the retained reviewer is ambiguous"),
    };
    let reviewer_bound: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM role_generations g JOIN sessions s ON s.role_generation_id=g.id
           WHERE g.id=?2 AND s.id=?3 AND g.attempt_id=?1 AND g.role='code_reviewer')",
        params![attempt_id, generation, session],
        |row| row.get(0),
    )?;
    if !reviewer_bound {
        bail!("the approving code reviewer's generation and session do not belong to this attempt")
    }
    let profile_hash: String = connection
        .query_row(
            "SELECT profile_hash FROM trip_attempt_profiles
             WHERE attempt_id=?1 AND role='code_reviewer' AND settings_revision=?2",
            params![attempt_id, revision],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| {
            anyhow!("the attempt's code reviewer profile changed after the approving review")
        })?;
    let budget: Option<(i64, i64)> = connection
        .query_row(
            "SELECT initial_allowance+extension_allowance,spent FROM review_budgets
             WHERE attempt_id=?1 AND review_kind='code'",
            params![attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let detail = serde_json::json!({
        "provenance_kind":"normal_final_request_changes",
        "attempt_id":attempt_id,"task_version":task_version,"plan_hash":plan_hash,
        "configuration_hash":configuration_hash,
        "prior_code_approval":{"review_request_id":approved,"candidate_hash":candidate},
        "final_request_changes":{"review_request_id":final_request_id,
            "role_result_id":final_result_id,"candidate_hash":candidate},
        "retained_code_reviewer":{"role_generation_id":generation,"session_id":session,
            "settings_revision":revision,"profile_hash":profile_hash},
        "ordinary_code_budget":budget.map(|(allowance, spent)| serde_json::json!({"allowance":allowance,"spent":spent})),
        "final_repair_recheck":{"allowance":1,"spent":0},
    });
    connection.execute(
        "INSERT INTO final_repair_rechecks(attempt_id,task_id,provenance_kind,operation_id,
           request_hash,authorized_task_version,plan_hash,configuration_hash,
           approved_code_request_id,prior_candidate_hash,final_request_id,final_result_id,
           reviewer_generation_id,reviewer_session_id,reviewer_settings_revision,
           reviewer_profile_hash,detail_json,state,created_at,updated_at)
         VALUES(?1,?2,'normal_final_request_changes',?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,
           ?15,?16,'authorized',?17,?17)",
        params![
            attempt_id,
            task_id,
            format!("final-repair:{final_result_id}"),
            crate::store::json_hash(&detail)?,
            task_version,
            plan_hash,
            configuration_hash,
            approved,
            candidate,
            final_request_id,
            final_result_id,
            generation,
            session,
            revision,
            profile_hash,
            detail.to_string(),
            now
        ],
    )?;
    if connection.execute(
        "UPDATE attempts SET final_repair_round=1 WHERE id=?1 AND final_repair_round=0",
        params![attempt_id],
    )? != 1
    {
        bail!("the attempt's final repair round changed before the receipt was created")
    }
    connection.execute(
        "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
         VALUES(?1,?2,'service','attempt.final_repair_recheck.authorized','attempt',?3,?4,?5)",
        params![
            uuid::Uuid::new_v4().to_string(),
            format!("final-repair:{final_result_id}"),
            attempt_id,
            detail.to_string(),
            now
        ],
    )?;
    Ok(())
}

/// How a code review of this attempt must be reserved.
pub(crate) enum RecheckBinding {
    /// Outside the final repair round: the ordinary allowance applies.
    Ordinary,
    /// The receipt governs the review, under its retained reviewer only.
    Retained,
    /// Nothing may be reserved or spent until a person resolves this.
    Held(String),
}

/// Decides whether a code review may be reserved, launched or have its
/// result consumed, before any reviewer binding could be refreshed. `active`
/// is an existing request and whether it already left `reserved`. A
/// final-repair attempt without a valid receipt never falls back to the
/// ordinary allowance, and in that round any existing request must be the
/// receipt's own recheck of the repaired candidate. A receipt that is still
/// open keeps its retained reviewer: a newer activated profile, changed
/// binding or stale capability evidence holds the attempt rather than
/// substituting. A request already launched keeps its retained reviewer as it
/// was checked at reservation.
pub(crate) fn final_repair_code_review_binding(
    connection: &Connection,
    attempt_id: &str,
    active: Option<(&str, bool)>,
) -> Result<RecheckBinding> {
    let (task_id, round, plan_hash, configuration_hash): (
        String,
        i64,
        Option<String>,
        Option<String>,
    ) = connection.query_row(
        "SELECT task_id,final_repair_round,plan_hash,configuration_hash FROM attempts WHERE id=?1",
        params![attempt_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    type Receipt = (
        String,
        String,
        Option<String>,
        String,
        String,
        String,
        i64,
        String,
        Option<String>,
        Option<String>,
    );
    let receipt: Option<Receipt> = connection
        .query_row(
            "SELECT task_id,plan_hash,configuration_hash,state,reviewer_generation_id,
                    reviewer_session_id,reviewer_settings_revision,reviewer_profile_hash,
                    review_request_id,candidate_hash
             FROM final_repair_rechecks WHERE attempt_id=?1",
            params![attempt_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            },
        )
        .optional()?;
    let held = |reason: &str| Ok(RecheckBinding::Held(reason.to_owned()));
    let receipt = match (round, receipt) {
        (0, None) => return Ok(RecheckBinding::Ordinary),
        (1, Some(receipt)) => receipt,
        (0, Some(_)) => {
            return held("a final-repair recheck receipt exists outside the final repair round")
        }
        (1, None) => return held("the final repair round has no dedicated recheck receipt, and an ordinary code review cannot replace it"),
        (_, _) => return held("the final repair round exceeds the one dedicated repair"),
    };
    let (
        receipt_task,
        receipt_plan,
        receipt_configuration,
        state,
        generation,
        session,
        revision,
        profile,
        receipt_request,
        receipt_candidate,
    ) = receipt;
    if receipt_task != task_id
        || plan_hash.as_deref() != Some(receipt_plan.as_str())
        || receipt_configuration != configuration_hash
    {
        return held("the recheck receipt no longer binds this attempt's task, approved plan and configuration");
    }
    if let Some((request_id, launched)) = active {
        let dedicated: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM review_requests r JOIN attempts a ON a.id=r.attempt_id
               WHERE r.id=?1 AND r.attempt_id=?2 AND r.review_kind='code'
                 AND r.candidate_hash=a.candidate_hash AND r.candidate_hash IS ?3)",
            params![request_id, attempt_id, receipt_candidate],
            |row| row.get(0),
        )?;
        if !dedicated
            || receipt_request.as_deref() != Some(request_id)
            || !matches!(state.as_str(), "reserved" | "spent")
        {
            return held("the attempt's active code review is not its receipt's dedicated recheck of the repaired candidate");
        }
        if launched {
            return Ok(RecheckBinding::Retained);
        }
    }
    if !matches!(state.as_str(), "authorized" | "reserved") {
        return Ok(RecheckBinding::Retained);
    }
    // A later code-reviewer generation before reservation means the retained
    // reviewer was replaced; afterwards it is the recheck's own launch.
    let lineage: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM role_generations g JOIN sessions s ON s.role_generation_id=g.id
           WHERE g.id=?2 AND s.id=?3 AND g.attempt_id=?1 AND g.role='code_reviewer'
             AND (?4!='authorized' OR NOT EXISTS(SELECT 1 FROM role_generations newer
               WHERE newer.attempt_id=g.attempt_id AND newer.role=g.role
                 AND newer.lane_id=g.lane_id AND newer.generation>g.generation)))",
        params![attempt_id, generation, session, state],
        |row| row.get(0),
    )?;
    if !lineage {
        return held("the retained code reviewer's generation, session or lineage changed");
    }
    let bound: Option<(i64, String, String, String)> = connection
        .query_row(
            "SELECT settings_revision,profile_hash,project_config_revision_id,capability_id
             FROM trip_attempt_profiles WHERE attempt_id=?1 AND role='code_reviewer'",
            params![attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((bound_revision, bound_profile, configuration, capability)) = bound else {
        return held("the attempt has no code reviewer profile binding");
    };
    if bound_revision != revision || bound_profile != profile {
        return held("the attempt's code reviewer profile is no longer the retained reviewer's");
    }
    let replacement_pending: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM trip_task_profile_activations
           WHERE task_id=?1 AND role='code_reviewer' AND project_config_revision_id=?2
             AND settings_revision>?3)",
        params![task_id, configuration, revision],
        |row| row.get(0),
    )?;
    if replacement_pending {
        return held("a newer activated code reviewer profile is waiting; the dedicated recheck keeps the retained reviewer, so a person must decide how to continue");
    }
    let current_capability: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM capabilities c WHERE c.id=?1 AND c.status='supported'
           AND c.rowid=(SELECT latest.rowid FROM capabilities latest
             WHERE latest.provider=c.provider AND latest.executable_version=c.executable_version
               AND latest.role=c.role AND latest.mode=c.mode
             ORDER BY latest.checked_at DESC,latest.rowid DESC LIMIT 1))",
        params![capability],
        |row| row.get(0),
    )?;
    if !current_capability {
        return held("the retained code reviewer's capability evidence is no longer current");
    }
    Ok(RecheckBinding::Retained)
}

/// Makes a held recheck visible and stops dispatch. The receipt, requests and
/// both counters are left untouched.
pub(crate) fn hold_final_repair_recheck(
    connection: &Connection,
    attempt_id: &str,
    reason: &str,
    now: &str,
) -> Result<()> {
    connection.execute(
        "UPDATE tasks SET version=version+1,
         attention=CASE WHEN attention IN ('paused','pause_requested','needs_recovery')
           THEN attention ELSE 'needs_input' END,updated_at=?1
         WHERE id=(SELECT task_id FROM attempts WHERE id=?2)",
        params![now, attempt_id],
    )?;
    connection.execute(
        "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
         VALUES(?1,?2,'service','attempt.final_repair_recheck.held','attempt',?3,?4,?5)",
        params![
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
            attempt_id,
            serde_json::json!({"reason":reason}).to_string(),
            now
        ],
    )?;
    Ok(())
}
