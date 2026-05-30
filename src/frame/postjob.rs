//! Post-job phase: commit locally-staged repos to the final target location.
//!
//! After all subtasks have completed, the post-job is responsible for:
//!
//! **Backup post-job**
//! - D_REPO data is always written directly by the AIO pipeline during the
//!   subtask phase (both for local→NFS and NFS→NFS). No D_REPO upload here.
//! - M_REPO and C_REPO are always written locally and uploaded to a remote
//!   target (currently NFS or SMB). These repos contain only a few small files.
//! - The `manifest.json` is written to the copy root.
//!
//! **Restore post-job**
//! - When the restore target is **local**: data files are already at the
//!   destination (written by BIO or AIO subtasks).
//! - When the restore target is **NFS**: data files were written directly by
//!   the AIO pipeline; no extra copy required.
//!
//! # Remote upload
//!
//! Uploading M_REPO / C_REPO to a remote target is done from the local staging
//! repo inside a one-off Tokio runtime so we can re-use the async transport
//! clients without making the whole post-job async from the caller's
//! perspective.

use std::io;
#[cfg(feature = "nfs")]
use std::path::Path;

use crate::backup::aggregate::AggregateLayout;
use crate::frame::location::DataLocation;
use crate::frame::repo::RepoLayout;

// ---------------------------------------------------------------------------
// PostJobError
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum PostJobError {
    Io(io::Error),
    /// Manifest serialisation failed.
    ManifestWrite(String),
    /// NFS upload of repo files failed.
    #[cfg(feature = "nfs")]
    NfsUpload(String),
    /// SMB upload of repo files failed.
    #[cfg(feature = "smb")]
    SmbUpload(String),
    /// Transport exists but the post-job uploader is not wired yet.
    Unsupported(String),
}

impl std::fmt::Display for PostJobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PostJobError::Io(e) => write!(f, "I/O error: {e}"),
            PostJobError::ManifestWrite(s) => write!(f, "manifest write error: {s}"),
            #[cfg(feature = "nfs")]
            PostJobError::NfsUpload(s) => write!(f, "NFS upload error: {s}"),
            #[cfg(feature = "smb")]
            PostJobError::SmbUpload(s) => write!(f, "SMB upload error: {s}"),
            PostJobError::Unsupported(s) => write!(f, "unsupported: {s}"),
        }
    }
}

impl std::error::Error for PostJobError {}

impl From<io::Error> for PostJobError {
    fn from(e: io::Error) -> Self {
        PostJobError::Io(e)
    }
}

// ---------------------------------------------------------------------------
// BackupPostJob
// ---------------------------------------------------------------------------

/// Post-job phase for backup.
///
/// Writes the manifest and, when the target is remote, copies the staged
/// repos to that target. D_REPO is written directly by the AIO pipeline during
/// the subtask phase and is not uploaded here.
pub struct BackupPostJob<'a> {
    /// The target location for the *copy* (not the data source).
    pub target: &'a DataLocation,
    /// Local staging repo layout.
    pub local_repo: &'a RepoLayout,
    /// The manifest to serialise and write.
    pub manifest: &'a BackupManifest,
}

/// Minimal manifest written at the copy root so the copy can be re-opened
/// for restore or inspection.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct BackupManifest {
    pub version: String,
    pub copy_uuid: String,
    pub copy_type: String, // "full" | "incremental"
    pub format: String,    // "common" | "aggregated"
    pub source: String,    // DataLocation::display_string()
    pub target: String,    // DataLocation::display_string()
    pub created_at: String,
    pub base_copy: Option<String>,
    pub aggregation: Option<AggregationManifest>,
    pub subtasks: Vec<SubtaskRecord>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AggregationManifest {
    pub layout: AggregateLayout,
    pub max_blob_size: u64,
    pub file_threshold: u64,
    pub shard_count: u16,
}

/// One entry in the manifest's subtask list.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SubtaskRecord {
    pub id: String,
    pub control_file: String, // relative path inside the copy
    pub log_file: String,     // relative path inside the copy
    pub succeeded: bool,
}

impl<'a> BackupPostJob<'a> {
    pub fn new(
        target: &'a DataLocation,
        local_repo: &'a RepoLayout,
        manifest: &'a BackupManifest,
    ) -> Self {
        Self {
            target,
            local_repo,
            manifest,
        }
    }

    /// Run the post-job.
    pub fn run(&self) -> Result<(), PostJobError> {
        // 1. Write manifest.json locally.
        let manifest_path = self.local_repo.manifest_path();
        log::info!("Post-job: writing manifest to {:?}", manifest_path);
        let manifest_json = serde_json::to_string_pretty(self.manifest)
            .map_err(|e| PostJobError::ManifestWrite(e.to_string()))?;
        std::fs::write(&manifest_path, &manifest_json)?;
        log::debug!("Post-job: manifest written ({} bytes)", manifest_json.len());

        // 2. If target is remote, upload manifest, M_REPO, and C_REPO.
        match self.target {
            DataLocation::Local(target_root) => {
                // For a local target the copy root IS the local staging area,
                // so data, metadata, and control files are already in place.
                // Verify the copy root is under the target.
                let _ = target_root; // already used during repo layout setup
                log::info!("Post-job: local target — no repo transfer needed");
            }
            #[cfg(feature = "nfs")]
            DataLocation::Nfs(nfs_loc) => {
                // Upload M_REPO and C_REPO to NFS.
                // D_REPO is written directly by the AIO pipeline during the
                // subtask phase (both local→NFS and NFS→NFS), so we don't
                // upload it here.
                log::info!("Post-job: uploading M_REPO and C_REPO to NFS target");
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .thread_name("fpt-nfs-post")
                    .build()
                    .map_err(|e| PostJobError::Io(e))?;

                let copy_folder = self
                    .local_repo
                    .copy_root
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("COPY_UNKNOWN")
                    .to_string();

                let nfs_loc_clone = nfs_loc.clone();
                let m_repo = self.local_repo.copy_root.join("M_REPO");
                let c_repo = self.local_repo.copy_root.join("C_REPO");
                let manifest = self.local_repo.manifest_path();

                rt.block_on(async move {
                    log::info!(
                        "Post-job: uploading M_REPO ({}) → NFS:{}/{}/M_REPO",
                        m_repo.display(),
                        nfs_loc_clone.export,
                        copy_folder
                    );
                    upload_local_dir_to_nfs(
                        &m_repo,
                        &nfs_loc_clone,
                        &format!("{copy_folder}/M_REPO"),
                    )
                    .await?;
                    log::info!(
                        "Post-job: uploading C_REPO ({}) → NFS:{}/{}/C_REPO",
                        c_repo.display(),
                        nfs_loc_clone.export,
                        copy_folder
                    );
                    upload_local_dir_to_nfs(
                        &c_repo,
                        &nfs_loc_clone,
                        &format!("{copy_folder}/C_REPO"),
                    )
                    .await?;
                    log::info!("Post-job: uploading manifest.json");
                    upload_file_to_nfs(
                        &manifest,
                        &nfs_loc_clone,
                        &format!("{copy_folder}/manifest.json"),
                    )
                    .await
                })
                .map_err(PostJobError::NfsUpload)?;
                log::info!("Post-job: NFS upload complete");
            }
            #[cfg(feature = "smb")]
            DataLocation::Smb(smb_loc) => {
                log::info!("Post-job: uploading M_REPO and C_REPO to SMB target");
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .thread_name("fpt-smb-post")
                    .build()
                    .map_err(PostJobError::Io)?;

                let copy_folder = self
                    .local_repo
                    .copy_root
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("COPY_UNKNOWN")
                    .to_string();

                let smb_loc_clone = smb_loc.clone();
                let m_repo = self.local_repo.copy_root.join("M_REPO");
                let c_repo = self.local_repo.copy_root.join("C_REPO");
                let manifest = self.local_repo.manifest_path();

                rt.block_on(async move {
                    crate::smb::aio::upload_local_dir_to_smb(
                        &m_repo,
                        &smb_loc_clone,
                        &format!("{copy_folder}/M_REPO"),
                    )
                    .await?;
                    crate::smb::aio::upload_local_dir_to_smb(
                        &c_repo,
                        &smb_loc_clone,
                        &format!("{copy_folder}/C_REPO"),
                    )
                    .await?;
                    crate::smb::aio::upload_local_file_to_smb(
                        &manifest,
                        &smb_loc_clone,
                        &format!("{copy_folder}/manifest.json"),
                    )
                    .await
                })
                .map_err(PostJobError::SmbUpload)?;
                log::info!("Post-job: SMB upload complete");
            }
        }
        log::info!("Post-job: done");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// RestorePostJob
// ---------------------------------------------------------------------------

/// Post-job phase for restore.
///
/// For local targets data is already in place.  For NFS targets the AIO
/// pipeline has already written data files; no additional transfer is needed.
pub struct RestorePostJob;

impl RestorePostJob {
    pub fn run() -> Result<(), PostJobError> {
        // Nothing to do: BIO subtasks wrote to the local target, AIO subtasks
        // wrote directly to NFS.
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// NFS upload helpers (feature-gated)
// ---------------------------------------------------------------------------

/// Recursively upload a local directory tree to an NFS sub-path.
#[cfg(feature = "nfs")]
async fn upload_local_dir_to_nfs(
    local_dir: &Path,
    nfs_loc: &crate::nfs::NfsLocation,
    nfs_sub_path: &str,
) -> Result<(), String> {
    if !local_dir.exists() {
        return Ok(());
    }
    log::debug!("Uploading directory: {:?} -> {}", local_dir, nfs_sub_path);
    let entries = std::fs::read_dir(local_dir)
        .map_err(|e| format!("read_dir {}: {e}", local_dir.display()))?;
    for entry in entries.flatten() {
        let child_name = entry.file_name().to_string_lossy().into_owned();
        let child_nfs = format!("{nfs_sub_path}/{child_name}");
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            let child_path = entry.path();
            let f = upload_local_dir_to_nfs(&child_path, nfs_loc, &child_nfs);
            Box::pin(f).await?;
        } else {
            let child_path = entry.path();
            upload_file_to_nfs(&child_path, nfs_loc, &child_nfs).await?;
        }
    }
    Ok(())
}

/// Upload one local file to an NFS path using CREATE + WRITE RPCs.
#[cfg(feature = "nfs")]
async fn upload_file_to_nfs(
    local_file: &Path,
    nfs_loc: &crate::nfs::NfsLocation,
    nfs_path: &str,
) -> Result<(), String> {
    use crate::nfs::NfsConnectionPool;

    let data =
        std::fs::read(local_file).map_err(|e| format!("read {}: {e}", local_file.display()))?;

    log::debug!(
        "Uploading file: {:?} -> {} ({} bytes)",
        local_file,
        nfs_path,
        data.len()
    );

    let pool = NfsConnectionPool::new(nfs_loc)
        .await
        .map_err(|e| format!("NFS connect: {e}"))?;

    crate::nfs::aio::writer::nfs_create_and_write(pool, std::path::PathBuf::from(nfs_path), data)
        .await
        .map_err(|e| format!("NFS write {nfs_path}: {e}"))
}
