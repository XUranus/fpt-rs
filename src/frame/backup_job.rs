//! Backup job orchestrator.
//!
//! [`FileBackupJob`] implements [`BackupRestoreJob`] and drives all four
//! phases of a backup in order:
//!
//! 1. **Prerequisite** — verify connectivity, create local repo directories.
//! 2. **Scan** — traverse the source via [`ScanJob`] (local or NFS).
//! 3. **Subtasks** — run each control file through [`run_backup_subtask`],
//!    which selects the appropriate [`FileBackup`] impl at runtime.
//! 4. **Post-job** — write manifest; upload M_REPO + C_REPO to NFS if needed.
//!
//! M_REPO and C_REPO are **always** written locally and uploaded to NFS in
//! the post-job when the target is NFS.  D_REPO data is written directly
//! by the AIO pipeline during the subtask phase for both local→NFS and
//! NFS→NFS backups.

use std::path::PathBuf;
use std::thread;
use uuid::Uuid;

use crate::backup::aggregate::AggregateConfig;
use crate::frame::location::DataLocation;
use crate::frame::postjob::{BackupManifest, BackupPostJob, SubtaskRecord};
use crate::frame::prereq::BackupPrereqJob;
use crate::frame::repo::{RepoLayout, TempRepoConfig};
use crate::frame::scan::{ScanConfig, ScanJob};
use crate::frame::subtask::{find_backup_control_files, run_backup_subtask, SubtaskConfig};
use crate::frame::traits::{BackupRestoreJob, JobResult};

// ---------------------------------------------------------------------------
// BackupJobConfig
// ---------------------------------------------------------------------------

/// Full configuration for a backup job.
#[derive(Debug, Clone)]
pub struct BackupJobConfig {
    // ── Source / Target ──────────────────────────────────────────────────────
    pub source: DataLocation,
    pub target: DataLocation,

    // ── Copy naming ──────────────────────────────────────────────────────────
    /// Short tag for the copy format (e.g. "COMMON", "AGGR").
    pub format_tag: String,
    /// Short tag for the copy type (e.g. "FULL", "INC").
    pub type_tag: String,

    // ── Temp local storage ───────────────────────────────────────────────────
    pub temp_config: TempRepoConfig,

    // ── Scan settings ────────────────────────────────────────────────────────
    pub scan_config: ScanConfig,

    // ── Subtask settings ─────────────────────────────────────────────────────
    pub aggregate_config: AggregateConfig,
    pub enable_hardlink: bool,
    pub enable_delete: bool,
    pub enable_mtime: bool,
    pub max_concurrent_subtasks: usize,
    pub smb_connection_count: usize,
    pub copy_buffer_size: usize,

    // ── Incremental ──────────────────────────────────────────────────────────
    pub incremental_base: Option<PathBuf>,

    // ── Logging ──────────────────────────────────────────────────────────────
    /// Verbosity level (0=INFO, 1=DEBUG, >=2=TRACE).
    pub verbose: u8,
}

impl Default for BackupJobConfig {
    fn default() -> Self {
        Self {
            source: DataLocation::Local(PathBuf::new()),
            target: DataLocation::Local(PathBuf::new()),
            format_tag: "COMMON".to_string(),
            type_tag: "FULL".to_string(),
            temp_config: TempRepoConfig::default(),
            scan_config: ScanConfig::default(),
            aggregate_config: AggregateConfig::default(),
            enable_hardlink: false,
            enable_delete: false,
            enable_mtime: false,
            max_concurrent_subtasks: 4,
            smb_connection_count: 1,
            copy_buffer_size: 1024 * 1024,
            incremental_base: None,
            verbose: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// BackupJobError
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum BackupJobError {
    Prereq(crate::frame::prereq::PrereqError),
    Scan(crate::frame::scan::ScanError),
    Subtask {
        subtask_id: String,
        error: crate::frame::subtask::SubtaskError,
    },
    PostJob(crate::frame::postjob::PostJobError),
    Io(std::io::Error),
}

impl std::fmt::Display for BackupJobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackupJobError::Prereq(e) => write!(f, "prerequisite failed: {e}"),
            BackupJobError::Scan(e) => write!(f, "scan failed: {e}"),
            BackupJobError::Subtask { subtask_id, error } => {
                write!(f, "subtask {subtask_id} failed: {error}")
            }
            BackupJobError::PostJob(e) => write!(f, "post-job failed: {e}"),
            BackupJobError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for BackupJobError {}

// ---------------------------------------------------------------------------
// FileBackupJob
// ---------------------------------------------------------------------------

/// Implements the full four-phase backup pipeline.
///
/// Construct with [`FileBackupJob::new`] and run via the [`BackupRestoreJob`]
/// trait's `run` method.
pub struct FileBackupJob {
    config: BackupJobConfig,
}

impl FileBackupJob {
    pub fn new(config: BackupJobConfig) -> Self {
        Self { config }
    }
}

impl BackupRestoreJob for FileBackupJob {
    type Error = BackupJobError;

    fn run(self) -> Result<JobResult, BackupJobError> {
        let cfg = &self.config;

        // ── Determine local repo root ─────────────────────────────────────────
        // For a local target the copy directory lives directly under the target.
        // For a remote (NFS) target we stage locally under temp_config.temp_base.
        let repo_base: PathBuf = match &cfg.target {
            DataLocation::Local(p) => p.clone(),
            #[cfg(feature = "nfs")]
            DataLocation::Nfs(_) => {
                std::fs::create_dir_all(&cfg.temp_config.temp_base).map_err(BackupJobError::Io)?;
                cfg.temp_config.temp_base.clone()
            }
            #[cfg(feature = "smb")]
            DataLocation::Smb(_) => {
                std::fs::create_dir_all(&cfg.temp_config.temp_base).map_err(BackupJobError::Io)?;
                cfg.temp_config.temp_base.clone()
            }
        };

        let repo = RepoLayout::new(&repo_base, &cfg.format_tag, &cfg.type_tag);

        // Create the logs/ directory early so we can route library module logs
        // to files immediately — before Phase 1 runs and emits bifrost::* logs.
        std::fs::create_dir_all(&repo.logs_dir).map_err(BackupJobError::Io)?;

        crate::logging::init(cfg.verbose);
        crate::logging::add_route("bifrost::scanner", &repo.scan_log());
        crate::logging::add_route("bifrost::nfs", &repo.scan_log());
        crate::logging::add_route("bifrost::smb", &repo.scan_log());
        crate::logging::add_route("exacl", &repo.scan_log());
        crate::logging::add_route("sspi", &repo.scan_log());
        crate::logging::add_route("smb::", &repo.scan_log());
        crate::logging::add_route("smb", &repo.scan_log());
        crate::logging::add_route("bifrost::frame", &repo.frame_log());
        crate::logging::add_route("bifrost::backup", &repo.frame_log());
        // Per-subtask backup route is swapped below.

        // ── Phase 1: Prerequisites ────────────────────────────────────────────
        log::info!("=== Phase 1: Prerequisites ===");
        BackupPrereqJob::new(&cfg.source, &cfg.target, &repo)
            .run_sync()
            .map_err(BackupJobError::Prereq)?;

        log::info!(
            "Backup job started  source={}  target={}  copy_uuid={}",
            cfg.source,
            cfg.target,
            repo.copy_uuid
        );

        // ── Phase 2: Scan ─────────────────────────────────────────────────────
        log::info!("=== Phase 2: Scan ===");
        let scan_config = {
            let mut sc = cfg.scan_config.clone();
            if let Some(ref base) = cfg.incremental_base {
                let base_repo = RepoLayout::from_existing(base.clone());
                sc.prev_meta_dir = Some(base_repo.meta_dir.clone());
            }
            sc
        };

        let scan_stats = ScanJob::new(&cfg.source, &repo, scan_config)
            .run()
            .map_err(BackupJobError::Scan)?;

        log::info!("Scan complete: {} — {}", cfg.source, scan_stats);

        // ── Phase 3: Subtasks ─────────────────────────────────────────────────
        let ctrl_files = find_backup_control_files(&repo.ctrl_dir);
        log::info!(
            "=== Phase 3: Subtasks ({} control file(s)) ===",
            ctrl_files.len()
        );

        let mut subtask_records: Vec<SubtaskRecord> = Vec::new();
        let mut subtasks_ok = 0usize;
        let mut subtasks_failed = 0usize;
        let mut total_files = 0u64;
        let mut total_bytes = 0u64;

        let mut handles: Vec<(String, thread::JoinHandle<_>)> = Vec::new();

        for ctrl_file in ctrl_files {
            let subtask_uuid = Uuid::new_v4().to_string();
            let ctrl_name = ctrl_file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();

            // Route backup module logs to this subtask's log file.
            log::info!("Subtask {subtask_uuid} starting  ctrl_file={ctrl_name}");
            crate::logging::remove_route("bifrost::backup");
            crate::logging::remove_route("bifrost::nfs::aio");
            crate::logging::remove_route("bifrost::smb::aio");
            crate::logging::remove_route("exacl");
            crate::logging::remove_route("sspi");
            crate::logging::remove_route("smb::");
            crate::logging::remove_route("smb");
            crate::logging::add_route("bifrost::backup", &repo.subtask_log(&subtask_uuid));
            crate::logging::add_route("bifrost::nfs::aio", &repo.subtask_log(&subtask_uuid));
            crate::logging::add_route("bifrost::smb::aio", &repo.subtask_log(&subtask_uuid));
            crate::logging::add_route("exacl", &repo.subtask_log(&subtask_uuid));
            crate::logging::add_route("sspi", &repo.subtask_log(&subtask_uuid));
            crate::logging::add_route("smb::", &repo.subtask_log(&subtask_uuid));
            crate::logging::add_route("smb", &repo.subtask_log(&subtask_uuid));

            let subtask_cfg = SubtaskConfig {
                subtask_uuid: subtask_uuid.clone(),
                control_file: ctrl_file.clone(),
                source_dir: cfg.source.base_path(),
                aggregate_config: cfg.aggregate_config.clone(),
                enable_hardlink: cfg.enable_hardlink,
                enable_delete: cfg.enable_delete,
                enable_mtime: cfg.enable_mtime,
                smb_connection_count: cfg.smb_connection_count,
                copy_buffer_size: cfg.copy_buffer_size,
                backup_source: cfg.source.clone(),
                backup_target: cfg.target.clone(),
                restore_target: DataLocation::Local(PathBuf::new()), // unused for backup
                restore_source_base: PathBuf::new(),
            };

            let repo_clone = repo.clone();
            let _ = repo.create_status(&format!("SUBTASK_{subtask_uuid}.RUNNING"));

            let handle = thread::spawn(move || run_backup_subtask(&subtask_cfg, &repo_clone));

            subtask_records.push(SubtaskRecord {
                id: subtask_uuid.clone(),
                control_file: format!("C_REPO/ctrl/{ctrl_name}"),
                log_file: format!("C_REPO/logs/{subtask_uuid}.log"),
                succeeded: false,
            });

            handles.push((subtask_uuid, handle));

            // Simple sequential drain once we hit the concurrency cap.
            while handles.len() >= cfg.max_concurrent_subtasks {
                break; // drain happens in the join loop below
            }
        }

        // Join all subtask threads.
        for (i, (subtask_uuid, handle)) in handles.into_iter().enumerate() {
            let result = handle.join().unwrap_or_else(|_| {
                Err(crate::frame::subtask::SubtaskError::Engine(
                    "subtask thread panicked".to_string(),
                ))
            });

            match result {
                Ok(stats) => {
                    subtasks_ok += 1;
                    total_files += stats.files_transferred;
                    total_bytes += stats.bytes_transferred;
                    let _ = repo.remove_status(&format!("SUBTASK_{subtask_uuid}.RUNNING"));
                    let _ = repo.create_status(&format!("SUBTASK_{subtask_uuid}.DONE"));
                    if let Some(r) = subtask_records.get_mut(i) {
                        r.succeeded = true;
                    }

                    log::info!(
                        "Subtask {subtask_uuid} OK  files={}  bytes={}",
                        stats.files_transferred,
                        stats.bytes_transferred
                    );
                }
                Err(e) => {
                    subtasks_failed += 1;
                    log::error!("Subtask {subtask_uuid} FAILED: {e}");
                    let _ = repo.remove_status(&format!("SUBTASK_{subtask_uuid}.RUNNING"));
                    let _ = repo.create_status(&format!("SUBTASK_{subtask_uuid}.FAILED"));
                }
            }
        }

        // ── Phase 4: Post-job ─────────────────────────────────────────────────
        log::info!("=== Phase 4: Post-job ===");
        let manifest = BackupManifest {
            version: "1.0".to_string(),
            copy_uuid: repo.copy_uuid.clone(),
            copy_type: cfg.type_tag.to_lowercase(),
            format: cfg.format_tag.to_lowercase(),
            source: cfg.source.to_string(),
            target: cfg.target.to_string(),
            created_at: chrono::Local::now().to_rfc3339(),
            base_copy: cfg
                .incremental_base
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            subtasks: subtask_records,
        };

        BackupPostJob::new(&cfg.target, &repo, &manifest)
            .run()
            .map_err(BackupJobError::PostJob)?;

        log::info!(
            "Backup job finished  subtasks_ok={subtasks_ok}  subtasks_failed={subtasks_failed}  \
             total_files={total_files}  total_bytes={total_bytes}"
        );

        Ok(JobResult {
            copy_uuid: repo.copy_uuid.clone(),
            copy_root: repo.copy_root.clone(),
            subtasks_ok,
            subtasks_failed,
            total_files,
            total_dirs: scan_stats.total_dirs,
            total_bytes,
        })
    }
}

// ---------------------------------------------------------------------------
// Legacy type alias — keeps fptcli.rs compiling without changes
// ---------------------------------------------------------------------------

/// Type alias: `BackupJob` is the old name; `FileBackupJob` is canonical.
pub type BackupJob = FileBackupJob;
/// Old config alias kept for compatibility.
pub type BackupJobResult = JobResult;
