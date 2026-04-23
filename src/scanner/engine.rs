use crate::scanner::metadata::{DirDiff, FileDiff};
use log::{error, info};
use std::{
    fs, io,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
};

use crate::scanner::{
    metadata::{
        generate_incremental_control_files, ControlFileHeader, ControlFileWriter, DirCacheEntry,
        DirCacheIterator, DirCacheRandomReader, DirCacheWriter, DirControlEntry, FileCacheEntry,
        FileCacheIterator, FileCacheRandomReader, FileCacheWriter, FileControlEntry, FixedSize,
        HardlinkIndex, MetaRepoReader, MetaRepoWriter, MtimeControlFileWriter, MtimeDirEntry,
    },
    models::DirBatchScanResult,
    options::ScanOption,
    ScanWorkerContext,
};
use crate::frame::control_files::primary_control_file_path;

pub mod bio;
// mod aio;

// generate meta data to files
pub fn start_meta_writers(
    context: &ScanWorkerContext,
    writer_count: usize,
    hardlink_index: Option<Arc<Mutex<HardlinkIndex>>>,
) -> Vec<thread::JoinHandle<()>> {
    let mut writer_handles = Vec::with_capacity(writer_count);
    let target_dir = &context.scan_option.target_dir;
    let scan_hardlinks = context.scan_option.meta_option.scan_hardlinks;

    for i in 0..writer_count {
        let output_queue = Arc::clone(&context.output_queue);
        let meta_dir = target_dir.meta_dir.clone();
        let dcache_dir = target_dir.meta_dir.clone();
        let fcache_dir = target_dir.meta_dir.clone();
        let hardlink_index = hardlink_index.clone();

        let handle = std::thread::spawn(move || {
            // writer thread logic here
            let writer_shard = i as u32;
            let mut meta_writer = MetaRepoWriter::new(meta_dir, writer_shard as u16).unwrap();
            let mut dcache_writer: DirCacheWriter =
                DirCacheWriter::new(dcache_dir, writer_shard).unwrap();
            let mut fcache_writer: FileCacheWriter =
                FileCacheWriter::new(fcache_dir, writer_shard).unwrap();
            info!("Writer thread {} started", i);
            loop {
                // pop path from output meta queue and process
                if let Some(dir_scan_result) = output_queue.pop() {
                    // process the path, open the directory, read entries, etc.
                    process_scan_result(
                        dir_scan_result,
                        &mut meta_writer,
                        &mut dcache_writer,
                        &mut fcache_writer,
                        writer_shard,
                        hardlink_index.as_ref(),
                        scan_hardlinks,
                    );
                } else {
                    break;
                }
            }
            info!("Writer thread {} exit", i);
        });
        writer_handles.push(handle);
    }
    writer_handles
}

pub fn start_stats_consumers(
    context: &ScanWorkerContext,
    consumer_count: usize,
) -> Vec<thread::JoinHandle<()>> {
    let mut consumer_handles = Vec::with_capacity(consumer_count);

    for _ in 0..consumer_count {
        let output_queue = Arc::clone(&context.output_queue);
        let handle = std::thread::spawn(move || while output_queue.pop().is_some() {});
        consumer_handles.push(handle);
    }

    consumer_handles
}

fn process_scan_result(
    dir_scan_result: DirBatchScanResult,
    meta_writer: &mut MetaRepoWriter,
    dcache_writer: &mut DirCacheWriter,
    fcache_writer: &mut FileCacheWriter,
    writer_shard: u32,
    hardlink_index: Option<&Arc<Mutex<HardlinkIndex>>>,
    scan_hardlinks: bool,
) {
    // write the dir_scan_result into meta files
    //debug!("Writing dir scan result: {:#?}", dir_scan_result);

    let dmeta_loc = meta_writer.write_dirmeta(&dir_scan_result.dir).unwrap();
    //info!("store dir {:#?} => {:#?}", dir_scan_result.dir.common.name, dmeta_loc);

    let mut sorted_fcaches = vec![];
    let files_count = dir_scan_result.files.len();
    let (_, fcache_offset) = fcache_writer.current();
    let fcache_fid = writer_shard;

    for fmeta in dir_scan_result.files {
        let fmeta_loc = meta_writer.write_filemeta(&fmeta).unwrap();

        // Track hardlinks if enabled
        if scan_hardlinks && fmeta.links > 1 {
            if let Some(index) = hardlink_index {
                if let Ok(mut idx) = index.lock() {
                    // Build full path from directory path and file name
                    let full_path = format!("{}/{}", dir_scan_result.dir.path, fmeta.common.name);
                    idx.add_file(
                        fmeta.common.id,    // inode
                        fmeta.common.devno, // device
                        fmeta.links as u32, // link count
                        fmeta_loc.0,        // meta_fid
                        fmeta_loc.1,        // meta_offset
                        full_path,
                    );
                }
            }
        }

        let mut fcache: FileCacheEntry = fmeta.into();
        fcache.meta_loc = fmeta_loc;
        sorted_fcaches.push(fcache);
    }
    sorted_fcaches.sort_by_key(|v| v.id);

    for fcache in sorted_fcaches {
        _ = fcache_writer.write(&fcache).unwrap();
        //debug!("write fcache {:#?}", fcache)
    }
    let mut dcache: DirCacheEntry = dir_scan_result.dir.into();
    dcache.meta_loc = dmeta_loc;
    dcache.files_count = files_count as u32;
    (dcache.fcache_fid, dcache.fcache_offset) = (fcache_fid, fcache_offset);
    _ = dcache_writer.write(&dcache).unwrap();

    // TODO:: sort dcache later
    // TODO:: merge fcache later
}

pub fn generate_control_files(scan_option: &ScanOption) -> Result<(), io::Error> {
    let target_option = &scan_option.target_dir;
    let meta_dir = target_option.meta_dir.clone();
    let ctrl_dir = target_option.ctrl_dir.clone();
    let dcache_dir = target_option.meta_dir.clone();
    let fcache_dir = target_option.meta_dir.clone();
    let ctrl_header = ControlFileHeader {
        source_kind: scan_option.control_path.source_kind.clone(),
        source_root: scan_option.control_path.source_root.clone(),
        ..ControlFileHeader::default()
    };

    // Ensure ctrl_dir exists
    fs::create_dir_all(&ctrl_dir)?;

    let mtime_file_path = primary_control_file_path(&ctrl_dir, "mtime");

    // Check if incremental backup is requested
    if let Some(ref prev_meta_dir) = target_option.prev_meta_dir {
        info!("Generating incremental control files...");
        info!("  Previous metadata: {}", prev_meta_dir.display());
        info!("  Current metadata: {}", meta_dir.display());

        match generate_incremental_control_files(
            Some(prev_meta_dir.as_path()),
            meta_dir.as_path(),
            ctrl_dir.as_path(),
            &scan_option.control_path.source_kind,
            &scan_option.control_path.source_root,
        ) {
            Ok(stats) => {
                info!("Incremental control files generated:");
                info!(
                    "  New dirs: {}, Modified dirs: {}, Deleted dirs: {}",
                    stats.new_dirs, stats.modified_dirs, stats.deleted_dirs
                );
                info!(
                    "  New files: {}, Modified files: {}, Deleted files: {}",
                    stats.new_files, stats.modified_files, stats.deleted_files
                );
            }
            Err(e) => {
                error!("Failed to generate incremental control files: {}", e);
                return Err(e);
            }
        }

        // Still generate the mtime control file for all directories (needed for mtime phase)
        let meta_reader = MetaRepoReader::new(meta_dir).unwrap();
        let mut mtime_writer = MtimeControlFileWriter::new_with_source(
            mtime_file_path,
            &scan_option.control_path.source_kind,
            &scan_option.control_path.source_root,
        )
        .unwrap();

        let dcaches: Vec<PathBuf> = fs::read_dir(dcache_dir.clone())
            .unwrap()
            .filter_map(|f| f.ok())
            .filter(|f| f.file_name().to_string_lossy().starts_with("dcache_"))
            .map(|f| f.path())
            .collect();

        for dcache in dcaches {
            let dcache_iter: DirCacheIterator =
                DirCacheIterator::from(DirCacheRandomReader::open(dcache).unwrap());

            for dcache_entry in dcache_iter {
                let dmeta = meta_reader.get_dmeta(dcache_entry.meta_loc).unwrap();
                let mtime_entry = MtimeDirEntry {
                    path: dmeta.path,
                    mode: dmeta.common.mode,
                    uid: 0,
                    gid: 0,
                    atime: dmeta.common.atime as u64,
                    mtime: dmeta.common.mtime as u64,
                };
                mtime_writer.write_dir(&mtime_entry).unwrap();
            }
        }

        mtime_writer.finish().unwrap();
        if scan_option.shard_option.enabled {
            split_copy_control_file(scan_option, &ctrl_header)?;
        }
        return Ok(());
    }

    // Full backup mode - generate copy control files with all entries marked as NN
    let meta_reader = MetaRepoReader::new(meta_dir).unwrap();
    let mut copy_writer = if scan_option.shard_option.enabled {
        None
    } else {
        Some(
            ControlFileWriter::new_with_header(primary_control_file_path(&ctrl_dir, "copy"), &ctrl_header)
                .unwrap(),
        )
    };
    let mut sharded_copy = if scan_option.shard_option.enabled {
        Some(
            crate::scanner::metadata::ShardedControlFileManager::with_policy(
                ctrl_dir.clone(),
                "copy".to_string(),
                scan_option.shard_option.num_shards.max(1),
                crate::scanner::metadata::ShardSplitPolicy::MaxSize {
                    max_size: scan_option.shard_option.max_size,
                    max_entries: scan_option.shard_option.max_entries_copy,
                },
            )?
            .max_files_per_batch(u32::MAX),
        )
    } else {
        None
    };
    let mut mtime_writer = MtimeControlFileWriter::new_with_source(
        mtime_file_path,
        &scan_option.control_path.source_kind,
        &scan_option.control_path.source_root,
    )
    .unwrap();

    let dcaches: Vec<PathBuf> = fs::read_dir(dcache_dir.clone())
        .unwrap()
        .filter_map(|f| f.ok())
        .filter(|f| f.file_name().to_string_lossy().starts_with("dcache_"))
        .map(|f| f.path())
        .collect();

    for dcache in dcaches {
        let dcache_iter: DirCacheIterator =
            DirCacheIterator::from(DirCacheRandomReader::open(dcache).unwrap());

        for dcache_entry in dcache_iter {
            let (fcache_fid, fcache_offset) = (dcache_entry.fcache_fid, dcache_entry.fcache_offset);
            let files_count = dcache_entry.files_count;
            let dmeta = meta_reader.get_dmeta(dcache_entry.meta_loc).unwrap();
            let dctrl_entry = DirControlEntry {
                path: dmeta.path.clone(),
                diff: DirDiff::New,
                meta_fid: dcache_entry.meta_loc.0,
                meta_offset: dcache_entry.meta_loc.1,
                files_count: files_count,
            };
            if let Some(copy_writer) = copy_writer.as_mut() {
                copy_writer.write_dir(&dctrl_entry).unwrap();
            } else if let Some(sharded_copy) = sharded_copy.as_mut() {
                sharded_copy.write_directory(&dctrl_entry, None)?;
            }

            // Write mtime entry for directory
            let mtime_entry = MtimeDirEntry {
                path: dmeta.path,
                mode: dmeta.common.mode,
                uid: 0, // TODO: extract from metadata if available
                gid: 0, // TODO: extract from metadata if available
                atime: dmeta.common.atime as u64,
                mtime: dmeta.common.mtime as u64,
            };
            mtime_writer.write_dir(&mtime_entry).unwrap();

            if files_count == 0 {
                continue;
            }

            // read file cache
            let fcache_path = crate::scanner::metadata::file_cache_path(&fcache_dir, fcache_fid);
            let fcache_iter: FileCacheIterator = FileCacheIterator::from(
                FileCacheRandomReader::open(fcache_path).unwrap(),
                files_count,
                fcache_offset / FileCacheEntry::SIZE as u32,
            );

            for fcache_entry in fcache_iter {
                let fmeta = meta_reader.get_fmeta(fcache_entry.meta_loc).unwrap();
                let fctrl_entry = FileControlEntry {
                    name: fmeta.common.name,
                    diff: FileDiff::New,
                    meta_fid: fcache_entry.meta_loc.0,
                    meta_offset: fcache_entry.meta_loc.1,
                };
                if let Some(copy_writer) = copy_writer.as_mut() {
                    copy_writer.write_file(&fctrl_entry).unwrap();
                } else if let Some(sharded_copy) = sharded_copy.as_mut() {
                    sharded_copy.write_file(&dctrl_entry.path, &fctrl_entry)?;
                }
            }
        }
    }

    if let Some(copy_writer) = copy_writer {
        copy_writer.finish().unwrap();
    }
    if let Some(sharded_copy) = sharded_copy {
        sharded_copy.finish()?;
    }
    mtime_writer.finish().unwrap();
    Ok(())
}

fn split_copy_control_file(
    scan_option: &ScanOption,
    ctrl_header: &ControlFileHeader,
) -> Result<(), io::Error> {
    use crate::scanner::metadata::{
        ControlEntry, ControlFileReader, ShardSplitPolicy, ShardedControlFileManager,
    };

    let copy_path = primary_control_file_path(&scan_option.target_dir.ctrl_dir, "copy");
    if !copy_path.exists() {
        return Ok(());
    }

    let mut sharded_copy = ShardedControlFileManager::with_policy(
        scan_option.target_dir.ctrl_dir.clone(),
        "copy".to_string(),
        scan_option.shard_option.num_shards.max(1),
        ShardSplitPolicy::MaxSize {
            max_size: scan_option.shard_option.max_size,
            max_entries: scan_option.shard_option.max_entries_copy,
        },
    )?
    .max_files_per_batch(u32::MAX);

    let reader = ControlFileReader::open(&copy_path)?;
    let mut current_dir_path = String::new();
    for entry in reader {
        match entry? {
            ControlEntry::Dir(dir) => {
                current_dir_path = dir.path.clone();
                sharded_copy.write_directory(&dir, None)?;
            }
            ControlEntry::File(file) => {
                sharded_copy.write_file(&current_dir_path, &file)?;
            }
        }
    }
    sharded_copy.finish()?;
    std::fs::remove_file(copy_path)?;

    // Recreate an empty headerless marker is unnecessary; sharded copies are now authoritative.
    let _ = ctrl_header;
    Ok(())
}
