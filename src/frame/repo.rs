//! Local repository layout for backup/restore jobs.
//!
//! Every job — regardless of whether source/target is local or NFS — maintains
//! its metadata, control files, and logs **locally** during execution.  Only the
//! data files (D\_REPO) may be written directly to the target location when that
//! target is an NFS server via the AIO pipeline.
//!
//! # Layout
//!
//! ```text
//! <copy_root>/          ← always local (temp dir or final target for local jobs)
//!   manifest.json
//!   D_REPO/             ← data files; may be written to NFS directly
//!   M_REPO/
//!     meta/             ← meta_*.dat, fcache_*.dat, dcache_*.dat  (BIO, local)
//!   C_REPO/
//!     ctrl/             ← copy.txt, hardlink.txt, delete.txt, mtime.txt
//!     logs/             ← backup.log, scan.log, <subtask-uuid>.log
//!     status/           ← SCAN_*.RUNNING/DONE, SUBTASK_*.RUNNING/DONE/FAILED
//! ```
//!
//! For jobs with an NFS *target*, the `copy_root` lives inside a
//! configurable `local_temp_dir` (default `/tmp/bifrost`).  After all
//! subtasks finish, the [`PostJob`] copies D\_REPO (if NFS target was *not*
//! used for direct writes), M\_REPO, and C\_REPO to the final destination.

use std::path::{Path, PathBuf};
use std::io;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// RepoLayout
// ---------------------------------------------------------------------------

/// Describes all local paths used during a single backup or restore job.
///
/// Create with [`RepoLayout::new`] (generates a fresh UUID) or
/// [`RepoLayout::from_existing`] (opens an existing copy directory).
#[derive(Debug, Clone)]
pub struct RepoLayout {
    /// Root of the copy directory (always a local path during the job).
    pub copy_root: PathBuf,
    /// UUID assigned to this copy.
    pub copy_uuid: String,
    /// `<copy_root>/D_REPO` — data files.
    pub d_repo: PathBuf,
    /// `<copy_root>/M_REPO/meta` — metadata files.
    pub meta_dir: PathBuf,
    /// `<copy_root>/C_REPO/ctrl` — control files.
    pub ctrl_dir: PathBuf,
    /// `<copy_root>/C_REPO/logs` — log files.
    pub logs_dir: PathBuf,
    /// `<copy_root>/C_REPO/status` — status sentinel files.
    pub status_dir: PathBuf,
}

impl RepoLayout {
    /// Create a new [`RepoLayout`] rooted at `base_dir/COPY_{format}_{type}_{uuid}`.
    pub fn new(
        base_dir: &Path,
        format_tag: &str,
        type_tag: &str,
    ) -> Self {
        let uuid = Uuid::new_v4().to_string();
        let folder = format!("COPY_{format_tag}_{type_tag}_{uuid}");
        let copy_root = base_dir.join(folder);
        Self::from_root(copy_root, uuid)
    }

    /// Open an existing copy directory.
    pub fn from_existing(copy_root: PathBuf) -> Self {
        // Extract UUID from the folder name (last component after last '_').
        let uuid = copy_root
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|s| s.rsplit('_').next())
            .unwrap_or("unknown")
            .to_string();
        Self::from_root(copy_root, uuid)
    }

    fn from_root(copy_root: PathBuf, copy_uuid: String) -> Self {
        let d_repo     = copy_root.join("D_REPO");
        let m_repo     = copy_root.join("M_REPO");
        let c_repo     = copy_root.join("C_REPO");
        let meta_dir   = m_repo.join("meta");
        let ctrl_dir   = c_repo.join("ctrl");
        let logs_dir   = c_repo.join("logs");
        let status_dir = c_repo.join("status");
        Self { copy_root, copy_uuid, d_repo, meta_dir, ctrl_dir, logs_dir, status_dir }
    }

    /// Create all repo directories on the local filesystem.
    pub fn create_dirs(&self) -> io::Result<()> {
        log::info!("Creating D_REPO directory: {:?}", self.d_repo);
        std::fs::create_dir_all(&self.d_repo)?;
        log::info!("Creating M_REPO directory: {:?}", self.meta_dir);
        std::fs::create_dir_all(&self.meta_dir)?;
        log::info!("Creating C_REPO/ctrl directory: {:?}", self.ctrl_dir);
        std::fs::create_dir_all(&self.ctrl_dir)?;
        log::info!("Creating C_REPO/logs directory: {:?}", self.logs_dir);
        std::fs::create_dir_all(&self.logs_dir)?;
        log::info!("Creating C_REPO/status directory: {:?}", self.status_dir);
        std::fs::create_dir_all(&self.status_dir)?;
        Ok(())
    }

    // ── Status sentinel helpers ────────────────────────────────────────────

    /// Create a named status sentinel file.
    pub fn create_status(&self, name: &str) -> io::Result<()> {
        std::fs::File::create(self.status_dir.join(name))?;
        Ok(())
    }

    /// Remove a named status sentinel file (silently ignores missing files).
    pub fn remove_status(&self, name: &str) -> io::Result<()> {
        let p = self.status_dir.join(name);
        if p.exists() { std::fs::remove_file(p)?; }
        Ok(())
    }

    /// Return `true` if the named status file exists.
    pub fn has_status(&self, name: &str) -> bool {
        self.status_dir.join(name).exists()
    }

    // ── Derived path helpers ───────────────────────────────────────────────

    /// Path to `manifest.json` at the copy root.
    pub fn manifest_path(&self) -> PathBuf {
        self.copy_root.join("manifest.json")
    }

    /// Path to the `scan.log` file.
    pub fn scan_log(&self) -> PathBuf {
        self.logs_dir.join("scan.log")
    }

    /// Path to the `frame.log` file.
    pub fn frame_log(&self) -> PathBuf {
        self.logs_dir.join("frame.log")
    }

    /// Path to a per-subtask log file keyed by subtask UUID.
    pub fn subtask_log(&self, subtask_uuid: &str) -> PathBuf {
        self.logs_dir.join(format!("{subtask_uuid}.log"))
    }

    /// Path to `C_REPO/ctrl/copy.txt`.
    pub fn copy_ctrl(&self) -> PathBuf {
        self.ctrl_dir.join("copy.txt")
    }
}

// ---------------------------------------------------------------------------
// TempRepoConfig
// ---------------------------------------------------------------------------

/// Configuration for local temporary storage used when the data target is remote
/// (e.g., an NFS server).
///
/// M\_REPO and C\_REPO are always written locally; D\_REPO may be written
/// directly to NFS.  After the job completes, [`PostJob`] transfers the
/// locally-written repos to the final destination.
#[derive(Debug, Clone)]
pub struct TempRepoConfig {
    /// Directory under which temporary job directories are created.
    ///
    /// Defaults to `/tmp/bifrost`.
    pub temp_base: PathBuf,
}

impl Default for TempRepoConfig {
    fn default() -> Self {
        Self { temp_base: PathBuf::from("/tmp/bifrost") }
    }
}

impl TempRepoConfig {
    pub fn new(temp_base: impl Into<PathBuf>) -> Self {
        Self { temp_base: temp_base.into() }
    }
}
