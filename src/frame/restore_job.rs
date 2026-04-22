//! Restore job orchestrator.
//!
//! [`FileRestoreJob`] implements [`BackupRestoreJob`] and drives all phases
//! of a restore in order:
//!
//! 1. **Prerequisite** — verify the backup copy is accessible.
//! 2. **Subtasks** — run each control file through [`run_restore_subtask`],
//!    which selects [`LocalFileRestore`] or [`NfsFileRestore`] at runtime.
//! 3. **Post-job** — no-op (data is already at the target).
//!
//! M_REPO / C_REPO are always accessed locally (pre-fetched in the prereq
//! phase for remote copies).

use std::path::PathBuf;
use std::thread;
use uuid::Uuid;

use crate::backup::aggregate::AggregateConfig;
use crate::backup::RestorePolicy;
use crate::failure::RetryPolicy;
use crate::frame::location::DataLocation;
use crate::frame::postjob::BackupManifest;
use crate::frame::postjob::RestorePostJob;
use crate::frame::prereq::RestorePrereqJob;
use crate::frame::repo::{RepoLayout, TempRepoConfig};
use crate::frame::subtask::{
    find_restore_control_files, run_restore_subtask, SubtaskConfig, SubtaskError,
};
use crate::frame::traits::{BackupRestoreJob, JobResult};

// ---------------------------------------------------------------------------
// RestoreJobConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RestoreJobConfig {
    /// Location of the backup copy (local directory or NFS path).
    pub copy_source: DataLocation,
    /// Where to write restored data (local or NFS).
    pub restore_target: DataLocation,
    /// Restore conflict policy.
    pub policy: RestorePolicy,
    /// Temp local storage used when the copy source is remote.
    pub temp_config: TempRepoConfig,
    /// Maximum concurrent restore subtasks.
    pub max_concurrent_subtasks: usize,
}

impl Default for RestoreJobConfig {
    fn default() -> Self {
        Self {
            copy_source: DataLocation::Local(PathBuf::new()),
            restore_target: DataLocation::Local(PathBuf::new()),
            policy: RestorePolicy::Replace,
            temp_config: TempRepoConfig::default(),
            max_concurrent_subtasks: 4,
        }
    }
}

// ---------------------------------------------------------------------------
// RestoreJobError
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum RestoreJobError {
    Prereq(crate::frame::prereq::PrereqError),
    Subtask {
        subtask_id: String,
        error: SubtaskError,
    },
    PostJob(crate::frame::postjob::PostJobError),
    MissingManifest(PathBuf),
    Io(std::io::Error),
}

impl std::fmt::Display for RestoreJobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RestoreJobError::Prereq(e) => write!(f, "prerequisite failed: {e}"),
            RestoreJobError::Subtask { subtask_id, error } => {
                write!(f, "subtask {subtask_id} failed: {error}")
            }
            RestoreJobError::PostJob(e) => write!(f, "post-job failed: {e}"),
            RestoreJobError::MissingManifest(p) => {
                write!(f, "manifest.json not found in {}", p.display())
            }
            RestoreJobError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for RestoreJobError {}

// ---------------------------------------------------------------------------
// FileRestoreJob
// ---------------------------------------------------------------------------

/// Implements the full restore pipeline.
///
/// Construct with [`FileRestoreJob::new`] and run via the [`BackupRestoreJob`]
/// trait's `run` method.
pub struct FileRestoreJob {
    config: RestoreJobConfig,
}

impl FileRestoreJob {
    pub fn new(config: RestoreJobConfig) -> Self {
        Self { config }
    }
}

impl BackupRestoreJob for FileRestoreJob {
    type Error = RestoreJobError;

    fn run(self) -> Result<JobResult, RestoreJobError> {
        let cfg = &self.config;

        // ── Determine the local repo layout ──────────────────────────────────
        let local_copy_root: PathBuf = match &cfg.copy_source {
            DataLocation::Local(p) => p.clone(),
            #[cfg(feature = "nfs")]
            DataLocation::Nfs(_) => {
                std::fs::create_dir_all(&cfg.temp_config.temp_base).map_err(RestoreJobError::Io)?;
                let staging = cfg
                    .temp_config
                    .temp_base
                    .join(format!("RESTORE_{}", Uuid::new_v4()));
                std::fs::create_dir_all(&staging).map_err(RestoreJobError::Io)?;
                staging
            }
            #[cfg(feature = "smb")]
            DataLocation::Smb(_) => {
                std::fs::create_dir_all(&cfg.temp_config.temp_base).map_err(RestoreJobError::Io)?;
                let staging = cfg
                    .temp_config
                    .temp_base
                    .join(format!("RESTORE_{}", Uuid::new_v4()));
                std::fs::create_dir_all(&staging).map_err(RestoreJobError::Io)?;
                staging
            }
        };

        let repo = RepoLayout::from_existing(local_copy_root.clone());

        std::fs::create_dir_all(&repo.logs_dir).map_err(RestoreJobError::Io)?;

        crate::logging::add_route("bifrost::nfs", &repo.frame_log());
        crate::logging::add_route("bifrost::smb", &repo.frame_log());
        crate::logging::add_route("sspi", &repo.frame_log());
        crate::logging::add_route("smb::", &repo.frame_log());
        crate::logging::add_route("smb", &repo.frame_log());
        crate::logging::add_route("bifrost::frame", &repo.frame_log());

        // ── Phase 1: Prerequisites ────────────────────────────────────────────
        RestorePrereqJob::new(&cfg.copy_source, &repo)
            .run_sync()
            .map_err(RestoreJobError::Prereq)?;

        if let DataLocation::Local(_) = &cfg.copy_source {
            if !repo.manifest_path().exists() {
                return Err(RestoreJobError::MissingManifest(repo.manifest_path()));
            }
        }

        // ── Phase 2: Subtasks ─────────────────────────────────────────────────
        let manifest: BackupManifest = serde_json::from_str(
            &std::fs::read_to_string(repo.manifest_path()).map_err(RestoreJobError::Io)?,
        )
        .map_err(|e| {
            RestoreJobError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        })?;
        let restore_source_base = parse_manifest_source_base(&manifest.source);
        let aggregate_config = manifest
            .aggregation
            .as_ref()
            .map(|agg| {
                AggregateConfig::enabled()
                    .layout(agg.layout)
                    .max_blob_size(agg.max_blob_size)
                    .file_threshold(agg.file_threshold)
                    .shard_count(agg.shard_count)
            })
            .unwrap_or_default();

        let ctrl_files = find_restore_control_files(&repo.ctrl_dir);
        log::info!("{} restore control file(s) found", ctrl_files.len());

        let local_restore_target: PathBuf = cfg
            .restore_target
            .local_path()
            .cloned()
            .unwrap_or_else(|| cfg.temp_config.temp_base.join("_restore_placeholder"));

        if let DataLocation::Local(_) = &cfg.restore_target {
            std::fs::create_dir_all(&local_restore_target).map_err(RestoreJobError::Io)?;
        }

        let mut subtasks_ok = 0usize;
        let mut subtasks_failed = 0usize;
        let mut total_files = 0u64;
        let phase_order = ["copy", "hardlink", "delete", "mtime"];

        for phase in phase_order {
            let phase_ctrls: Vec<PathBuf> = ctrl_files
                .iter()
                .filter(|(_, tag)| *tag == phase)
                .map(|(path, _)| path.clone())
                .collect();

            if phase_ctrls.is_empty() {
                continue;
            }

            let mut handles: Vec<(String, thread::JoinHandle<_>)> = Vec::new();

            for ctrl_file in phase_ctrls {
                let subtask_uuid = Uuid::new_v4().to_string();

                let subtask_cfg = SubtaskConfig {
                    subtask_uuid: subtask_uuid.clone(),
                    control_file: ctrl_file,
                    source_dir: repo.d_repo.clone(),
                    aggregate_config,
                    enable_hardlink: false,
                    enable_delete: false,
                    enable_mtime: false,
                    smb_connection_count: 4,
                    smb_copy_task_count: 0,
                    copy_buffer_size: 1024 * 1024,
                    failure_log: None,
                    retry_policy: RetryPolicy::default(),
                    backup_source: DataLocation::Local(PathBuf::new()), // unused for restore
                    backup_target: DataLocation::Local(PathBuf::new()), // unused for restore
                    restore_target: cfg.restore_target.clone(),
                    restore_source_base: restore_source_base.clone(),
                };

                let repo_clone = repo.clone();
                let local_target_clone = local_restore_target.clone();
                let handle = thread::spawn(move || {
                    run_restore_subtask(&subtask_cfg, &repo_clone, &local_target_clone)
                });
                handles.push((subtask_uuid, handle));
            }

            for (subtask_uuid, handle) in handles {
                let result = handle.join().unwrap_or_else(|_| {
                    Err(SubtaskError::Engine("subtask thread panicked".to_string()))
                });

                match result {
                    Ok(stats) => {
                        subtasks_ok += 1;
                        total_files += stats.files_transferred;
                    }
                    Err(e) => {
                        subtasks_failed += 1;
                        log::error!("Restore subtask {subtask_uuid} failed: {e}");
                    }
                }
            }
        }

        // ── Phase 3: Post-job ─────────────────────────────────────────────────
        RestorePostJob::run().map_err(RestoreJobError::PostJob)?;

        Ok(JobResult {
            copy_uuid: String::new(), // restore does not produce a new copy UUID
            copy_root: local_restore_target,
            subtasks_ok,
            subtasks_failed,
            total_files,
            total_dirs: 0,
            total_bytes: 0,
        })
    }
}

// ---------------------------------------------------------------------------
// Legacy type aliases
// ---------------------------------------------------------------------------

/// `RestoreJob` is the old name; `FileRestoreJob` is canonical.
pub type RestoreJob = FileRestoreJob;
pub type RestoreJobResult = JobResult;

fn parse_manifest_source_base(spec: &str) -> PathBuf {
    if spec.starts_with("nfs://") {
        return DataLocation::from_nfs_url(spec)
            .map(|loc| loc.base_path())
            .unwrap_or_else(|_| PathBuf::from(spec));
    }
    if spec.starts_with("smb://") || spec.starts_with("smb:\\\\") {
        return DataLocation::from_smb_url(spec)
            .map(|loc| loc.base_path())
            .unwrap_or_else(|_| PathBuf::from(spec));
    }
    PathBuf::from(spec)
}
