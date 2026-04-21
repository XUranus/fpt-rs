use crate::backup::aggregate::{AggregateConfig, PendingFile};
use crate::backup::aggregate_engine::{AggregateBackupEngine, AggregateBackupState};
use crate::backup::bio::{delete, hardlink, mtime};
use crate::backup::stats::BackupStats;
use crate::scanner::metadata::FileMeta;
use crate::scanner::metadata::{ControlEntry, ControlFileReader, MetaRepoReader};
use log::{error, info};
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

enum LocalCopyJob {
    Copy {
        meta: crate::scanner::metadata::FileMeta,
        src_path: PathBuf,
        dst_path: PathBuf,
    },
    Aggregate {
        meta: crate::scanner::metadata::FileMeta,
        src_path: PathBuf,
    },
}

pub(crate) fn spawn_local_common_copy_pipeline(
    control_file: PathBuf,
    source_dir_base: PathBuf,
    target_dir_base: PathBuf,
    meta_dir: PathBuf,
    ctrl_dir: PathBuf,
    worker_count: usize,
    copy_buffer_size: usize,
    aggregate_config: AggregateConfig,
    enable_hardlink_phase: bool,
    enable_delete_phase: bool,
    enable_mtime_phase: bool,
    stats: Arc<BackupStats>,
    terminate_indicator: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let aggregate_state = if aggregate_config.enabled {
            match AggregateBackupEngine::new(
                aggregate_config.clone(),
                source_dir_base.clone(),
                target_dir_base.clone(),
            ) {
                Ok(engine) => Some(Arc::new(AggregateBackupState::new(Arc::new(engine)))),
                Err(e) => {
                    error!("Failed to create aggregate engine: {}", e);
                    None
                }
            }
        } else {
            None
        };

        let queue_capacity = worker_count.max(1) * 2;
        let (job_tx, job_rx) = mpsc::sync_channel::<LocalCopyJob>(queue_capacity);
        let worker_rx = Arc::new(std::sync::Mutex::new(job_rx));

        let mut workers = Vec::with_capacity(worker_count.max(1));
        for i in 0..worker_count.max(1) {
            let rx = Arc::clone(&worker_rx);
            let stats = Arc::clone(&stats);
            let aggregate_state = aggregate_state.clone();
            workers.push(thread::spawn(move || {
                let mut buffer = vec![0_u8; copy_buffer_size.clamp(256 * 1024, 4 * 1024 * 1024)];
                loop {
                    let recv_result = {
                        let rx = rx.lock().unwrap();
                        rx.recv()
                    };
                    let job = match recv_result {
                        Ok(job) => job,
                        Err(_) => break,
                    };
                    match job {
                        LocalCopyJob::Copy {
                            meta,
                            src_path,
                            dst_path,
                        } => {
                            if let Err(e) = copy_one_local_file(
                                &meta,
                                &src_path,
                                &dst_path,
                                &stats,
                                &mut buffer,
                            ) {
                                error!(
                                    "Local copy worker {} failed: {:?} -> {:?}: {}",
                                    i, src_path, dst_path, e
                                );
                                stats.inc_files_failed();
                            }
                        }
                        LocalCopyJob::Aggregate { meta, src_path } => {
                            let Some(agg_state) = aggregate_state.as_ref() else {
                                error!(
                                    "Local copy worker {} missing aggregate state for {:?}",
                                    i, src_path
                                );
                                stats.inc_files_failed();
                                continue;
                            };
                            if let Err(e) =
                                aggregate_one_local_file(agg_state, &stats, &meta, &src_path)
                            {
                                error!(
                                    "Local aggregate worker {} failed: {:?}: {}",
                                    i, src_path, e
                                );
                                stats.inc_files_failed();
                            }
                        }
                    }
                }
            }));
        }

        if let Err(e) = produce_local_copy_jobs(
            &control_file,
            &meta_dir,
            &source_dir_base,
            &target_dir_base,
            &job_tx,
            &stats,
            aggregate_state.as_ref(),
        ) {
            error!("Local copy producer failed: {}", e);
        }
        drop(job_tx);

        for handle in workers {
            if let Err(e) = handle.join() {
                error!("Local copy worker join failed: {:?}", e);
            }
        }

        if let Some(agg_state) = aggregate_state {
            flush_aggregate_state(&agg_state, &stats);
            let agg_stats = agg_state.engine.stats();
            info!(
                "Aggregate stats: {} blobs created, {} files aggregated",
                agg_stats.blobs_created, agg_stats.files_aggregated
            );
        }

        run_followup_phases(
            enable_hardlink_phase,
            enable_delete_phase,
            enable_mtime_phase,
            &ctrl_dir,
            &meta_dir,
            &source_dir_base,
            &target_dir_base,
        );

        terminate_indicator.store(true, Ordering::Relaxed);
    })
}

fn produce_local_copy_jobs(
    control_file: &Path,
    meta_dir: &Path,
    source_dir_base: &Path,
    target_dir_base: &Path,
    job_tx: &mpsc::SyncSender<LocalCopyJob>,
    stats: &Arc<BackupStats>,
    aggregate_state: Option<&Arc<AggregateBackupState>>,
) -> io::Result<()> {
    let meta_repo_reader = MetaRepoReader::new(meta_dir.to_path_buf())?;
    let control_reader = ControlFileReader::open(control_file)?;
    let logical_source_root = PathBuf::from(control_reader.header().source_root.clone());
    let mut dirpath = PathBuf::new();

    for entry in control_reader {
        let entry = entry?;
        match entry {
            ControlEntry::Dir(dentry) => {
                let dmeta = meta_repo_reader.get_dmeta((dentry.meta_fid, dentry.meta_offset))?;
                let dst_path = logical_target_path(target_dir_base, &dentry.path);
                if let Err(e) = std::fs::create_dir_all(&dst_path) {
                    error!("Failed to create target directory {:?}: {}", dst_path, e);
                    stats.inc_dirs_failed();
                } else {
                    #[cfg(target_os = "linux")]
                    {
                        restore_xattrs(&dst_path, &dmeta.common.xattributes);
                        restore_acl(
                            &dst_path,
                            &dmeta.common.posix_access_acl,
                            &dmeta.common.posix_default_acl,
                        );
                    }
                    stats.inc_dirs_created();
                }
                dirpath = dentry.path.into();
            }
            ControlEntry::File(fentry) => {
                let fmeta = meta_repo_reader.get_fmeta((fentry.meta_fid, fentry.meta_offset))?;
                let src_path = resolve_local_source_path(
                    source_dir_base,
                    &logical_source_root,
                    &dirpath.to_string_lossy(),
                )
                .join(&fentry.name);
                let dst_path = logical_target_path(target_dir_base, &dirpath.to_string_lossy())
                    .join(&fentry.name);
                if let Some(agg_state) = aggregate_state {
                    if fmeta.common.symlink_target_path.is_none()
                        && agg_state.engine.should_aggregate(fmeta.size)
                    {
                        job_tx
                            .send(LocalCopyJob::Aggregate {
                                meta: fmeta,
                                src_path,
                            })
                            .map_err(|_| {
                                io::Error::new(
                                    io::ErrorKind::BrokenPipe,
                                    "local copy workers disconnected",
                                )
                            })?;
                        continue;
                    }
                }
                job_tx
                    .send(LocalCopyJob::Copy {
                        meta: fmeta,
                        src_path,
                        dst_path,
                    })
                    .map_err(|_| {
                        io::Error::new(io::ErrorKind::BrokenPipe, "local copy workers disconnected")
                    })?;
            }
        }
    }

    Ok(())
}

fn aggregate_one_local_file(
    agg_state: &AggregateBackupState,
    stats: &BackupStats,
    meta: &FileMeta,
    src_path: &Path,
) -> io::Result<()> {
    let data = std::fs::read(src_path)?;
    let pending = PendingFile {
        file_name: meta.common.name.clone(),
        data,
        ctime: meta.common.ctime as u64,
        mtime: meta.common.mtime as u64,
        mode: meta.common.mode,
        xattrs: meta.common.xattributes.clone(),
        acl: meta.common.posix_access_acl.clone(),
    };
    let dir_path = src_path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    if let Some((dir, files)) = agg_state.add_file(&dir_path, pending) {
        write_aggregate_blob(agg_state, stats, &dir, files);
    }
    Ok(())
}

fn flush_aggregate_state(agg_state: &AggregateBackupState, stats: &BackupStats) {
    for (dir, files) in agg_state.flush_all() {
        write_aggregate_blob(agg_state, stats, &dir, files);
    }
}

fn write_aggregate_blob(
    agg_state: &AggregateBackupState,
    stats: &BackupStats,
    dir: &str,
    files: Vec<PendingFile>,
) {
    let file_count = files.len() as u64;
    let bytes_in_blob: u64 = files.iter().map(|f| f.data.len() as u64).sum();
    match agg_state.engine.create_blob(dir, files) {
        Ok(blob_meta) => {
            info!(
                "Created blob {} for dir {} with {} files",
                blob_meta.blob_name, dir, blob_meta.file_count
            );
            stats.files_copied.fetch_add(file_count, Ordering::Relaxed);
            stats
                .bytes_copied
                .fetch_add(bytes_in_blob, Ordering::Relaxed);
        }
        Err(e) => {
            error!("Failed to create aggregate blob for dir {}: {}", dir, e);
            stats
                .files_failed
                .fetch_add(file_count.max(1), Ordering::Relaxed);
        }
    }
}

fn copy_one_local_file(
    meta: &FileMeta,
    src_path: &Path,
    dst_path: &Path,
    stats: &BackupStats,
    buffer: &mut [u8],
) -> io::Result<()> {
    if let Some(ref symlink_target) = meta.common.symlink_target_path {
        if let Some(parent) = dst_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        create_symlink(dst_path, symlink_target)?;
        #[cfg(target_os = "linux")]
        {
            restore_xattrs(dst_path, &meta.common.xattributes);
            restore_acl(
                dst_path,
                &meta.common.posix_access_acl,
                &meta.common.posix_default_acl,
            );
        }
        stats.inc_files_copied();
        return Ok(());
    }

    if let Some(parent) = dst_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut src = File::open(src_path)?;
    stats.inc_src_opened();
    let mut dst = File::create(dst_path)?;
    stats.inc_dst_opened();

    loop {
        let read_n = src.read(buffer)?;
        if read_n == 0 {
            break;
        }
        dst.write_all(&buffer[..read_n])?;
        stats.add_bytes_copied(read_n as u64);
    }
    dst.flush()?;
    drop(dst);
    stats.inc_dst_closed();

    #[cfg(target_os = "linux")]
    {
        restore_xattrs(dst_path, &meta.common.xattributes);
        restore_acl(
            dst_path,
            &meta.common.posix_access_acl,
            &meta.common.posix_default_acl,
        );
    }

    drop(src);
    stats.inc_src_closed();
    stats.inc_files_copied();
    Ok(())
}

fn resolve_local_source_path(
    source_root: &Path,
    logical_source_root: &Path,
    control_path: &str,
) -> PathBuf {
    let control_path = PathBuf::from(control_path);
    if control_path.starts_with(source_root) {
        return control_path;
    }
    let rel = control_path
        .strip_prefix(logical_source_root)
        .or_else(|_| control_path.strip_prefix("/"))
        .map(|p| p.to_path_buf())
        .unwrap_or(control_path);
    source_root.join(rel)
}

fn logical_target_path(target_root: &Path, control_path: &str) -> PathBuf {
    target_root.join(
        PathBuf::from(control_path)
            .strip_prefix("/")
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| PathBuf::from(control_path)),
    )
}

#[cfg(target_os = "linux")]
fn restore_xattrs(path: &Path, xattrs: &Option<String>) {
    use base64::Engine as _;

    if let Some(xattr_str) = xattrs {
        for line in xattr_str.lines() {
            if let Some((name, b64_value)) = line.split_once('=') {
                if let Ok(value) = base64::engine::general_purpose::STANDARD.decode(b64_value) {
                    if let Err(e) = xattr::set(path, name, &value) {
                        error!("Failed to set xattr {} on {:?}: {}", name, path, e);
                    }
                }
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn restore_xattrs(_path: &Path, _xattrs: &Option<String>) {}

#[cfg(target_os = "linux")]
fn restore_acl(path: &Path, access_acl: &Option<String>, default_acl: &Option<String>) {
    use exacl::{setfacl, AclEntry};

    let mut acl_entries = Vec::new();

    if let Some(acl_str) = access_acl {
        for line in acl_str.lines() {
            if let Ok(entry) = line.parse::<AclEntry>() {
                acl_entries.push(entry);
            }
        }
    }

    if let Some(acl_str) = default_acl {
        for line in acl_str.lines() {
            if let Ok(entry) = line.parse::<AclEntry>() {
                acl_entries.push(entry);
            }
        }
    }

    if !acl_entries.is_empty() {
        if let Err(e) = setfacl(&[path], &acl_entries, None) {
            error!("Failed to set ACL on {:?}: {}", path, e);
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn restore_acl(_path: &Path, _access_acl: &Option<String>, _default_acl: &Option<String>) {}

fn create_symlink(dst_path: &Path, target: &str) -> io::Result<()> {
    if dst_path.exists() {
        std::fs::remove_file(dst_path)?;
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, dst_path)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, dst_path)
    }
}

fn run_followup_phases(
    enable_hardlink_phase: bool,
    enable_delete_phase: bool,
    enable_mtime_phase: bool,
    ctrl_dir: &Path,
    meta_dir: &Path,
    source_dir_base: &Path,
    target_dir_base: &Path,
) {
    if enable_hardlink_phase {
        info!("Starting hardlink phase...");
        match hardlink::run_hardlink_phase(ctrl_dir, meta_dir, source_dir_base, target_dir_base) {
            Ok(hl_stats) => {
                info!(
                    "Hardlink phase completed: {} created, {} failed",
                    hl_stats.hardlinks_created, hl_stats.hardlinks_failed
                );
            }
            Err(e) => {
                error!("Hardlink phase failed: {}", e);
            }
        }
    }

    if enable_delete_phase {
        info!("Starting delete phase...");
        match delete::run_delete_phase(ctrl_dir, source_dir_base, target_dir_base) {
            Ok(del_stats) => {
                info!(
                    "Delete phase completed: {} files deleted, {} dirs deleted",
                    del_stats.files_deleted, del_stats.dirs_deleted
                );
            }
            Err(e) => {
                error!("Delete phase failed: {}", e);
            }
        }
    }

    if enable_mtime_phase {
        info!("Starting mtime phase...");
        match mtime::run_mtime_phase(ctrl_dir, source_dir_base, target_dir_base) {
            Ok(mt_stats) => {
                info!(
                    "Mtime phase completed: {} restored, {} failed",
                    mt_stats.dirs_restored, mt_stats.dirs_failed
                );
            }
            Err(e) => {
                error!("Mtime phase failed: {}", e);
            }
        }
    }
}
