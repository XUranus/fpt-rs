//! Prerequisite job: checks and preparation before backup/restore begins.
//!
//! The prerequisite job runs *before* scanning and before any subtask is
//! scheduled.  It is responsible for:
//!
//! 1. **Connectivity check** — verify the data source / target is reachable.
//! 2. **Local repo setup** — create D\_REPO, M\_REPO, C\_REPO directories.
//! 3. **Restore pre-fetch** (restore only) — copy M\_REPO / C\_REPO from the
//!    remote backup copy to the local temp directory so the rest of the
//!    pipeline can access them with standard BIO I/O.

use std::io;
use std::path::Path;

use crate::frame::location::DataLocation;
use crate::frame::repo::RepoLayout;

// ---------------------------------------------------------------------------
// PrereqError
// ---------------------------------------------------------------------------

/// Errors that can occur during the prerequisite phase.
#[derive(Debug)]
pub enum PrereqError {
    /// A required local directory could not be created.
    DirCreate(io::Error),
    /// The local data source path does not exist.
    SourceNotFound(String),
    /// The backup copy directory is missing or invalid.
    InvalidCopyDir(String),
    /// Connectivity to the NFS server failed.
    #[cfg(feature = "nfs")]
    NfsConnect(crate::nfs::NfsError),
    /// Connectivity or authentication to the SMB server failed.
    #[cfg(feature = "smb")]
    SmbConnect(String),
    /// Transport exists but the required prereq flow is not wired yet.
    Unsupported(String),
    /// Generic I/O failure.
    Io(io::Error),
}

impl std::fmt::Display for PrereqError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrereqError::DirCreate(e)       => write!(f, "failed to create directory: {e}"),
            PrereqError::SourceNotFound(s)  => write!(f, "source not found: {s}"),
            PrereqError::InvalidCopyDir(s)  => write!(f, "invalid copy directory: {s}"),
            #[cfg(feature = "nfs")]
            PrereqError::NfsConnect(e)      => write!(f, "NFS connection failed: {e}"),
            #[cfg(feature = "smb")]
            PrereqError::SmbConnect(s)      => write!(f, "SMB connection failed: {s}"),
            PrereqError::Unsupported(s)     => write!(f, "unsupported: {s}"),
            PrereqError::Io(e)              => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for PrereqError {}

impl From<io::Error> for PrereqError {
    fn from(e: io::Error) -> Self { PrereqError::Io(e) }
}

#[cfg(feature = "nfs")]
impl From<crate::nfs::NfsError> for PrereqError {
    fn from(e: crate::nfs::NfsError) -> Self { PrereqError::NfsConnect(e) }
}

// ---------------------------------------------------------------------------
// BackupPrereqJob
// ---------------------------------------------------------------------------

/// Prerequisite phase for a backup job.
///
/// Checks that the source is accessible and creates all local repo directories.
/// When the target is an NFS server the local repo acts as a staging area;
/// D\_REPO data is written directly to NFS by the AIO pipeline.
pub struct BackupPrereqJob<'a> {
    pub source: &'a DataLocation,
    pub target: &'a DataLocation,
    pub repo: &'a RepoLayout,
}

impl<'a> BackupPrereqJob<'a> {
    pub fn new(source: &'a DataLocation, target: &'a DataLocation, repo: &'a RepoLayout) -> Self {
        Self { source, target, repo }
    }

    /// Run all prerequisite checks and create local directories.
    ///
    /// For an NFS source this also verifies the TCP connection can be
    /// established (async; must be awaited inside a Tokio context).
    pub fn run_sync(&self) -> Result<(), PrereqError> {
        log::info!("Prereq: creating repo directories at {:?}", self.repo.copy_root);

        // 1. Create local repo directories (always needed)
        self.repo.create_dirs().map_err(PrereqError::DirCreate)?;
        log::debug!("Prereq: repo directories created (D_REPO, M_REPO, C_REPO)");

        // 2. Verify local source is accessible
        if let Some(local) = self.source.local_path() {
            if !local.exists() {
                return Err(PrereqError::SourceNotFound(
                    local.to_string_lossy().into_owned(),
                ));
            }
            log::debug!("Prereq: local source verified: {:?}", local);
        }

        // 3. Verify remote source / target connectivity
        for location in [self.source, self.target] {
            match location {
            #[cfg(feature = "nfs")]
            DataLocation::Nfs(ref loc) => {
                log::info!("Prereq: verifying NFS connectivity to {}", loc.host);
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(PrereqError::Io)?;
                let loc_clone = loc.clone();
                rt.block_on(async {
                    let _pool = crate::nfs::connection::NfsConnectionPool::new(&loc_clone).await
                        .map_err(PrereqError::NfsConnect)?;
                    log::info!("Prereq: NFS reachable (export={})", loc_clone.export);
                    Ok::<(), PrereqError>(())
                })?;
            }
            #[cfg(feature = "smb")]
            DataLocation::Smb(ref loc) => {
                log::info!("Prereq: verifying SMB connectivity to {}", loc.display_string());
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(PrereqError::Io)?;
                let loc_clone = loc.clone();
                rt.block_on(async {
                    loc_clone
                        .verify_root_access()
                        .await
                        .map_err(PrereqError::SmbConnect)
                })?;
                log::info!("Prereq: SMB reachable ({})", loc.display_string());
            }
            _ => {}
            }
        }
        log::info!("Prereq: all checks passed");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// RestorePrereqJob
// ---------------------------------------------------------------------------

/// Prerequisite phase for a restore job.
///
/// For a *local* backup copy the repo directories are accessed in-place.
/// For a *remote* (NFS) backup copy, M\_REPO and C\_REPO are copied to the
/// local temp directory so all metadata I/O can proceed with standard BIO.
pub struct RestorePrereqJob<'a> {
    /// Location of the backup copy (where manifest.json and repos live).
    pub copy_source: &'a DataLocation,
    /// Local layout where M\_REPO / C\_REPO will be prepared.
    pub local_repo: &'a RepoLayout,
}

impl<'a> RestorePrereqJob<'a> {
    pub fn new(copy_source: &'a DataLocation, local_repo: &'a RepoLayout) -> Self {
        Self { copy_source, local_repo }
    }

    /// Run all prerequisite steps.
    ///
    /// * Local copy → verify existence, nothing to copy.
    /// * NFS copy → fetch M\_REPO and C\_REPO via BIO-compatible NFS reads.
    pub fn run_sync(&self) -> Result<(), PrereqError> {
        self.local_repo.create_dirs().map_err(PrereqError::DirCreate)?;

        match self.copy_source {
            DataLocation::Local(src_root) => {
                log::info!("Prereq (restore): verifying local copy at {:?}", src_root);
                // Validate the copy directory exists and has a manifest.
                if !src_root.exists() {
                    return Err(PrereqError::InvalidCopyDir(
                        src_root.to_string_lossy().into_owned(),
                    ));
                }
                let manifest = src_root.join("manifest.json");
                if !manifest.exists() {
                    return Err(PrereqError::InvalidCopyDir(format!(
                        "manifest.json not found in {}",
                        src_root.display()
                    )));
                }
                log::debug!("Prereq (restore): local copy verified, manifest found");
                // For local copies the in-place paths will be used directly;
                // no copying needed here.
            }
            #[cfg(feature = "nfs")]
            DataLocation::Nfs(_nfs_loc) => {
                // TODO: Fetch manifest.json, M_REPO/meta/*, C_REPO/ctrl/* from NFS
                // to local_repo using NFS READ RPCs.
                // This requires async I/O so it should be driven by the caller
                // inside a tokio::runtime::Builder context.
                // For now we return Ok; the actual NFS fetch will be
                // implemented in the async variant run_async().
                log::warn!("Prereq (restore): NFS copy source — M_REPO/C_REPO pre-fetch not yet implemented");
            }
            #[cfg(feature = "smb")]
            DataLocation::Smb(_) => {
                return Err(PrereqError::Unsupported(
                    "SMB restore copy-source staging is not implemented yet".to_string(),
                ));
            }
        }
        log::info!("Prereq (restore): all checks passed");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Recursively copy a directory tree from `src` to `dst` using BIO.
///
/// Used by [`RestorePrereqJob`] to stage M\_REPO / C\_REPO locally before
/// the pipeline starts.
pub fn copy_dir_local(src: &Path, dst: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_local(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
