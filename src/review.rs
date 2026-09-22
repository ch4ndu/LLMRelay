use crate::snapshot::SnapshotManifest;
use crate::store::Store;
use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
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
            "candidate" => transaction.query_row("SELECT rr.role_generation_id FROM role_results rr JOIN role_generations rg ON rg.id=rr.role_generation_id JOIN role_settings rs ON rs.effective_generation_id=rg.id AND rs.role='implementer' WHERE rg.attempt_id=?1 AND rr.outcome='candidate_ready' AND rr.consumed_at IS NULL ORDER BY rr.created_at DESC LIMIT 1",params![attempt_id],|row|row.get(0)).optional()?,
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
                connection.query_row("SELECT EXISTS(SELECT 1 FROM role_results rr JOIN role_generations rg ON rg.id=rr.role_generation_id JOIN sessions s ON s.id=rr.session_id JOIN attempts a ON a.id=rg.attempt_id WHERE rg.attempt_id=?1 AND rg.role='implementer' AND rr.outcome='candidate_ready' AND rr.consumed_at IS NULL AND rr.created_at>=a.updated_at AND s.status='exited')",params![attempt_id],|row|row.get(0))?
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
            transaction.execute(
                "UPDATE role_results SET consumed_at=?1 WHERE id=(SELECT rr.id FROM role_results rr JOIN role_generations rg ON rg.id=rr.role_generation_id JOIN attempts a ON a.id=rg.attempt_id WHERE rg.attempt_id=?2 AND rg.role='implementer' AND rr.outcome='candidate_ready' AND rr.consumed_at IS NULL AND rr.created_at>=a.updated_at ORDER BY rr.created_at DESC LIMIT 1)",
                params![now, attempt_id],
            )?;
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
        if let Some((id,stored_prompt,stored_handoff,state,prompt_text,handoff_json))=transaction.query_row(
            "SELECT id,prompt_hash,handoff_hash,delivery_state,prompt_text,handoff_json FROM review_requests WHERE attempt_id=?1 AND review_kind=?2 AND delivery_state IN ('reserved','launching','delivered','ambiguous')",
            params![attempt_id,kind],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,Option<String>>(4)?,row.get::<_,Option<String>>(5)?))).optional()?{
            if stored_prompt!=crate::store::json_hash(&prompt)?||stored_handoff!=crate::store::json_hash(&handoff)?{bail!("active review request must resume with identical prompt and handoff")}
            return Ok(ReviewDispatch{request_id:id,attempt_id:attempt_id.into(),role,candidate_hash:candidate,prompt:prompt_text.unwrap_or_else(||prompt.into()),handoff:handoff_json.map(|value|serde_json::from_str(&value)).transpose()?.unwrap_or(handoff),state})
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
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let operation_key = crate::store::json_hash(
            &serde_json::json!({"attempt_id":attempt_id,"kind":kind,"candidate":candidate.clone(),"prompt":prompt,"handoff":handoff.clone()}),
        )?;
        transaction.execute("INSERT INTO review_requests(id,attempt_id,review_kind,candidate_hash,prompt_hash,handoff_hash,delivery_state,created_at,updated_at,operation_key,prompt_text,handoff_json) VALUES(?1,?2,?3,?4,?5,?6,'reserved',?7,?7,?8,?9,?10)",
            params![id,attempt_id,kind,candidate,crate::store::json_hash(&prompt)?,crate::store::json_hash(&handoff)?,now,operation_key,prompt,handoff.to_string()])?;
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
        if spent.is_none() {
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
        let changed = connection.execute(
            "UPDATE review_requests SET role_generation_id=?1,session_id=?2,settings_revision=?3,delivery_state='launching',updated_at=?4 WHERE id=?5 AND delivery_state IN ('reserved','nondelivered')",
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
        if ambiguous && spent_at.is_none() {
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
            let review_detail = serde_json::json!({
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
                "replacement_allowed_after_quiescence":true,
                "review_delivery_reason":reason,
            });
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
