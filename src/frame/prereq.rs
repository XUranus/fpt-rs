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
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
    /// A local or remote source path exists but is not usable as a directory.
    SourceNotReadable(String),
    /// A target path could not be verified for write access.
    TargetNotWritable(String),
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
            PrereqError::DirCreate(e) => write!(f, "failed to create directory: {e}"),
            PrereqError::SourceNotFound(s) => write!(f, "source not found: {s}"),
            PrereqError::SourceNotReadable(s) => write!(f, "source not readable: {s}"),
            PrereqError::TargetNotWritable(s) => write!(f, "target not writable: {s}"),
            PrereqError::InvalidCopyDir(s) => write!(f, "invalid copy directory: {s}"),
            #[cfg(feature = "nfs")]
            PrereqError::NfsConnect(e) => write!(f, "NFS connection failed: {e}"),
            #[cfg(feature = "smb")]
            PrereqError::SmbConnect(s) => write!(f, "SMB connection failed: {s}"),
            PrereqError::Unsupported(s) => write!(f, "unsupported: {s}"),
            PrereqError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for PrereqError {}

impl From<io::Error> for PrereqError {
    fn from(e: io::Error) -> Self {
        PrereqError::Io(e)
    }
}

#[cfg(feature = "nfs")]
impl From<crate::nfs::NfsError> for PrereqError {
    fn from(e: crate::nfs::NfsError) -> Self {
        PrereqError::NfsConnect(e)
    }
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
        Self {
            source,
            target,
            repo,
        }
    }

    /// Run all prerequisite checks and create local directories.
    ///
    /// Remote sources are checked by resolving the configured root. Remote
    /// targets are checked by writing and removing a small magic file.
    pub fn run_sync(&self) -> Result<(), PrereqError> {
        log::info!(
            "Prereq: creating repo directories at {:?}",
            self.repo.copy_root
        );

        // 1. Create local repo directories (always needed)
        self.repo.create_dirs().map_err(PrereqError::DirCreate)?;
        log::debug!("Prereq: repo directories created (D_REPO, M_REPO, C_REPO)");

        // 2. Verify source and target before scan/backup starts.
        validate_backup_source(self.source)?;
        validate_target_writable(self.target)?;

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
    /// Restore destination that must be writable before subtasks start.
    pub restore_target: &'a DataLocation,
    /// Local layout where M\_REPO / C\_REPO will be prepared.
    pub local_repo: &'a RepoLayout,
}

impl<'a> RestorePrereqJob<'a> {
    pub fn new(
        copy_source: &'a DataLocation,
        restore_target: &'a DataLocation,
        local_repo: &'a RepoLayout,
    ) -> Self {
        Self {
            copy_source,
            restore_target,
            local_repo,
        }
    }

    /// Run all prerequisite steps.
    ///
    /// * Local copy → verify existence, nothing to copy.
    /// * NFS copy → fetch M\_REPO and C\_REPO via BIO-compatible NFS reads.
    pub fn run_sync(&self) -> Result<(), PrereqError> {
        if !matches!(self.copy_source, DataLocation::Local(_)) {
            self.local_repo
                .create_dirs()
                .map_err(PrereqError::DirCreate)?;
        }

        match self.copy_source {
            DataLocation::Local(src_root) => {
                log::info!("Prereq (restore): verifying local copy at {:?}", src_root);
                validate_copy_structure_local(src_root)?;
                log::debug!("Prereq (restore): local copy structure verified");
                // For local copies the in-place paths will be used directly;
                // no copying needed here.
            }
            #[cfg(feature = "nfs")]
            DataLocation::Nfs(nfs_loc) => {
                log::info!(
                    "Prereq (restore): verifying NFS copy structure at {}",
                    nfs_display(nfs_loc)
                );
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(PrereqError::Io)?;
                let loc_clone = nfs_loc.clone();
                rt.block_on(async {
                    validate_copy_structure_nfs(&loc_clone)
                        .await
                        .map_err(PrereqError::NfsConnect)
                })?;
                return Err(PrereqError::Unsupported(
                    "NFS restore copy-source staging is not implemented yet".to_string(),
                ));
            }
            #[cfg(feature = "smb")]
            DataLocation::Smb(smb_loc) => {
                log::info!(
                    "Prereq (restore): verifying SMB copy structure at {}",
                    smb_loc.display_string()
                );
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(PrereqError::Io)?;
                let loc_clone = smb_loc.clone();
                rt.block_on(async {
                    validate_copy_structure_smb(&loc_clone)
                        .await
                        .map_err(PrereqError::SmbConnect)
                })?;
                return Err(PrereqError::Unsupported(
                    "SMB restore copy-source staging is not implemented yet".to_string(),
                ));
            }
        }
        validate_target_writable(self.restore_target)?;
        log::info!("Prereq (restore): all checks passed");
        Ok(())
    }
}

fn validate_backup_source(source: &DataLocation) -> Result<(), PrereqError> {
    match source {
        DataLocation::Local(path) => validate_local_source_dir(path),
        #[cfg(feature = "nfs")]
        DataLocation::Nfs(loc) => {
            log::info!("Prereq: verifying NFS source root {}", nfs_display(loc));
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(PrereqError::Io)?;
            let loc_clone = loc.clone();
            rt.block_on(async {
                let _pool = crate::nfs::connection::NfsConnectionPool::new(&loc_clone).await?;
                Ok::<(), PrereqError>(())
            })
        }
        #[cfg(feature = "smb")]
        DataLocation::Smb(loc) => {
            log::info!("Prereq: verifying SMB source root {}", loc.display_string());
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
            })
        }
    }
}

fn validate_target_writable(target: &DataLocation) -> Result<(), PrereqError> {
    match target {
        DataLocation::Local(path) => validate_local_target_writable(path),
        #[cfg(feature = "nfs")]
        DataLocation::Nfs(loc) => {
            log::info!(
                "Prereq: verifying NFS target write access {}",
                nfs_display(loc)
            );
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(PrereqError::Io)?;
            let loc_clone = loc.clone();
            rt.block_on(async {
                validate_nfs_target_writable(&loc_clone)
                    .await
                    .map_err(PrereqError::NfsConnect)
            })
        }
        #[cfg(feature = "smb")]
        DataLocation::Smb(loc) => {
            log::info!(
                "Prereq: verifying SMB target write access {}",
                loc.display_string()
            );
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(PrereqError::Io)?;
            let loc_clone = loc.clone();
            rt.block_on(async {
                validate_smb_target_writable(&loc_clone)
                    .await
                    .map_err(PrereqError::SmbConnect)
            })
        }
    }
}

fn validate_local_source_dir(path: &Path) -> Result<(), PrereqError> {
    if !path.exists() {
        return Err(PrereqError::SourceNotFound(
            path.to_string_lossy().into_owned(),
        ));
    }
    if !path.is_dir() {
        return Err(PrereqError::SourceNotReadable(format!(
            "{} is not a directory",
            path.display()
        )));
    }
    std::fs::read_dir(path)
        .map_err(|e| PrereqError::SourceNotReadable(format!("read_dir {}: {e}", path.display())))?;
    log::debug!("Prereq: local source verified: {:?}", path);
    Ok(())
}

fn validate_local_target_writable(path: &Path) -> Result<(), PrereqError> {
    std::fs::create_dir_all(path).map_err(PrereqError::DirCreate)?;
    let magic = path.join(prereq_magic_file_name());
    std::fs::write(&magic, b"bifrost-prereq")
        .map_err(|e| PrereqError::TargetNotWritable(format!("write {}: {e}", magic.display())))?;
    std::fs::remove_file(&magic)
        .map_err(|e| PrereqError::TargetNotWritable(format!("remove {}: {e}", magic.display())))?;
    log::debug!("Prereq: local target write verified: {:?}", path);
    Ok(())
}

fn validate_copy_structure_local(copy_root: &Path) -> Result<(), PrereqError> {
    if !copy_root.exists() {
        return Err(PrereqError::InvalidCopyDir(
            copy_root.to_string_lossy().into_owned(),
        ));
    }
    let repo = RepoLayout::from_existing(copy_root.to_path_buf());
    for (path, kind) in [
        (repo.manifest_path(), "file"),
        (repo.d_repo, "dir"),
        (repo.meta_dir, "dir"),
        (repo.ctrl_dir, "dir"),
    ] {
        validate_local_copy_component(&path, kind)?;
    }
    Ok(())
}

fn validate_local_copy_component(path: &Path, kind: &str) -> Result<(), PrereqError> {
    let ok = match kind {
        "file" => path.is_file(),
        "dir" => path.is_dir(),
        _ => path.exists(),
    };
    if ok {
        Ok(())
    } else {
        Err(PrereqError::InvalidCopyDir(format!(
            "required {kind} missing: {}",
            path.display()
        )))
    }
}

fn prereq_magic_file_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!(".bifrost_prereq_{}_{}.tmp", std::process::id(), nanos)
}

#[cfg(feature = "nfs")]
async fn validate_nfs_target_writable(
    loc: &crate::nfs::NfsLocation,
) -> Result<(), crate::nfs::NfsError> {
    use nfs3_client::nfs3_types::nfs3::{diropargs3, filename3, Nfs3Result, REMOVE3args};

    let pool = crate::nfs::connection::NfsConnectionPool::new(loc).await?;
    let magic = prereq_magic_file_name();
    crate::nfs::aio::writer::nfs_create_and_write(
        pool.clone(),
        PathBuf::from(&magic),
        b"bifrost-prereq".to_vec(),
    )
    .await?;

    let mut conn = pool.acquire().await;
    let res = conn
        .remove(&REMOVE3args {
            object: diropargs3 {
                dir: pool.root_fh(),
                name: filename3::from(magic.as_bytes()),
            },
        })
        .await?;
    match res {
        Nfs3Result::Ok(_) => Ok(()),
        Nfs3Result::Err((stat, _)) => {
            Err(crate::nfs::NfsError::Nfs(stat, format!("remove {magic}")))
        }
    }
}

#[cfg(feature = "nfs")]
fn nfs_display(loc: &crate::nfs::NfsLocation) -> String {
    if loc.sub_path.is_empty() {
        format!("nfs://{}{}", loc.host, loc.export)
    } else {
        format!(
            "nfs://{}{}?sub={}",
            loc.host,
            loc.export,
            loc.sub_path.trim_start_matches('/')
        )
    }
}

#[cfg(feature = "nfs")]
async fn validate_copy_structure_nfs(
    loc: &crate::nfs::NfsLocation,
) -> Result<(), crate::nfs::NfsError> {
    let pool = crate::nfs::connection::NfsConnectionPool::new(loc).await?;
    let cache = crate::nfs::aio::reader::new_file_handle_cache();
    let root_fh = pool.root_fh();
    for rel in ["manifest.json", "D_REPO", "M_REPO/meta", "C_REPO/ctrl"] {
        crate::nfs::aio::reader::resolve_path(&pool, &cache, rel, &root_fh)
            .await
            .map_err(|e| match e {
                crate::nfs::NfsError::Nfs(stat, _) => {
                    crate::nfs::NfsError::Nfs(stat, format!("restore copy lookup {rel}"))
                }
                other => other,
            })?;
    }
    Ok(())
}

#[cfg(feature = "smb")]
async fn validate_smb_target_writable(loc: &crate::smb::SmbLocation) -> Result<(), String> {
    let client = crate::smb::aio::connect_client(loc).await?;
    let dir_cache = crate::smb::aio::new_dir_cache();
    let magic_dir = prereq_magic_file_name();
    let result = async {
        crate::smb::aio::ensure_relative_directory(&client, loc, &dir_cache, &magic_dir).await?;
        crate::smb::aio::delete::mark_delete_pending(&client, loc, &magic_dir, true)
            .await
            .and_then(|deleted| {
                if deleted {
                    Ok(())
                } else {
                    Err(format!(
                        "delete temp magic directory {magic_dir}: not found"
                    ))
                }
            })
    }
    .await;
    let close_result = client.close().await.map_err(|e| e.to_string());
    result.and(close_result)
}

#[cfg(feature = "smb")]
async fn validate_copy_structure_smb(loc: &crate::smb::SmbLocation) -> Result<(), String> {
    let client = crate::smb::aio::connect_client(loc).await?;
    let result = async {
        open_smb_existing(&client, loc, "manifest.json", false).await?;
        for rel in ["D_REPO", "M_REPO/meta", "C_REPO/ctrl"] {
            open_smb_existing(&client, loc, rel, true).await?;
        }
        Ok::<(), String>(())
    }
    .await;
    let close_result = client.close().await.map_err(|e| e.to_string());
    result.and(close_result)
}

#[cfg(feature = "smb")]
async fn open_smb_existing(
    client: &smb_client::Client,
    loc: &crate::smb::SmbLocation,
    relative_path: &str,
    expect_dir: bool,
) -> Result<(), String> {
    let unc = crate::smb::aio::relative_unc_path(loc, relative_path)?;
    let args = if expect_dir {
        smb_client::FileCreateArgs {
            disposition: smb_client::CreateDisposition::Open,
            attributes: smb_client::FileAttributes::new().with_directory(true),
            options: smb_client::CreateOptions::new().with_directory_file(true),
            desired_access: smb_client::DirAccessMask::new()
                .with_list_directory(true)
                .into(),
        }
    } else {
        smb_client::FileCreateArgs::make_open_existing(
            smb_client::FileAccessMask::new().with_generic_read(true),
        )
    };
    let resource = client
        .create_file(&unc, &args)
        .await
        .map_err(|e| format!("open {unc}: {e}"))?;
    crate::smb::aio::close_resource(resource).await
}
