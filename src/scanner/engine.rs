use std::{fs::{self, OpenOptions}, io, path::PathBuf, sync::{Arc, Mutex}, thread};
use log::{debug, info, warn, error};

use crate::scanner::{
    ScanWorkerContext,
    metadata::{
        ControlFileWriter, DirCacheEntry, DirCacheIterator, DirCacheRandomReader, DirCacheWriter, DirControlEntry, DirDiff, FileCacheEntry, FileCacheIterator, FileCacheRandomReader, FileCacheWriter, FileControlEntry, FileDiff, FixedSize, HardlinkIndex, MetaRepoReader, MetaRepoWriter, MtimeControlFileWriter, MtimeDirEntry,
        generate_incremental_control_files,
    },
    models::DirBatchScanResult, options::TargetDirOption
};

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
            let mut meta_writer = MetaRepoWriter::new(meta_dir).unwrap();
            let mut dcache_writer: DirCacheWriter = DirCacheWriter::new(dcache_dir, i as u32).unwrap();
            let mut fcache_writer: FileCacheWriter = FileCacheWriter::new(fcache_dir, i as u32).unwrap();
            print!("Writer thread {} started\n", i);
            loop {
                // pop path from output meta queue and process
                if let Some(dir_scan_result) = output_queue.pop() {
                    // process the path, open the directory, read entries, etc.
                    process_scan_result(
                        dir_scan_result,
                        &mut meta_writer,
                        &mut dcache_writer,
                        &mut fcache_writer,
                        hardlink_index.as_ref(),
                        scan_hardlinks,
                    );
                } else {
                    break;
                }
            }
            print!("Writer thread {} exit\n", i);
        });
        writer_handles.push(handle);
    }
    writer_handles
}


fn process_scan_result(
    dir_scan_result: DirBatchScanResult,
    meta_writer: &mut MetaRepoWriter,
    dcache_writer: &mut DirCacheWriter,
    fcache_writer: &mut FileCacheWriter,
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
    let fcache_fid = 0;

    for fmeta in dir_scan_result.files {
        let fmeta_loc = meta_writer.write_filemeta(&fmeta).unwrap();
        
        // Track hardlinks if enabled
        if scan_hardlinks && fmeta.links > 1 {
            if let Some(index) = hardlink_index {
                if let Ok(mut idx) = index.lock() {
                    // Build full path from directory path and file name
                    let full_path = format!("{}/{}", dir_scan_result.dir.path, fmeta.common.name);
                    idx.add_file(
                        fmeta.common.id,      // inode
                        fmeta.common.devno,   // device
                        fmeta.links as u32,   // link count
                        fmeta_loc.0,          // meta_fid
                        fmeta_loc.1,          // meta_offset
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



pub fn generate_control_files(target_option : &TargetDirOption) -> Result<(), io::Error> {
    let meta_dir = target_option.meta_dir.clone();
    let ctrl_dir = target_option.ctrl_dir.clone();
    let dcache_dir = target_option.meta_dir.clone();
    let fcache_dir = target_option.meta_dir.clone();

    // Ensure ctrl_dir exists
    fs::create_dir_all(&ctrl_dir)?;

    let copy_file_path = ctrl_dir.join("copy.txt");
    let mtime_file_path = ctrl_dir.join("mtime.txt");
    
    // Check if incremental backup is requested
    if let Some(ref prev_meta_dir) = target_option.prev_meta_dir {
        info!("Generating incremental control files...");
        info!("  Previous metadata: {}", prev_meta_dir.display());
        info!("  Current metadata: {}", meta_dir.display());
        
        match generate_incremental_control_files(
            Some(prev_meta_dir.as_path()),
            meta_dir.as_path(),
            ctrl_dir.as_path(),
        ) {
            Ok(stats) => {
                info!("Incremental control files generated:");
                info!("  New dirs: {}, Modified dirs: {}, Deleted dirs: {}", 
                    stats.new_dirs, stats.modified_dirs, stats.deleted_dirs);
                info!("  New files: {}, Modified files: {}, Deleted files: {}",
                    stats.new_files, stats.modified_files, stats.deleted_files);
            }
            Err(e) => {
                error!("Failed to generate incremental control files: {}", e);
                return Err(e);
            }
        }
        
        // Still generate mtime.txt for all directories (needed for mtime phase)
        let meta_reader = MetaRepoReader::new(meta_dir).unwrap();
        let mut mtime_writer = MtimeControlFileWriter::new(mtime_file_path).unwrap();
        
        let dcaches : Vec<PathBuf> = fs::read_dir(dcache_dir.clone()).unwrap()
            .filter_map(|f| f.ok())
            .filter(|f|f.file_name().to_string_lossy().starts_with("dcache_"))
            .map(|f| f.path())
            .collect();

        for dcache in dcaches {
            let dcache_iter : DirCacheIterator = DirCacheIterator::from(
                DirCacheRandomReader::open(dcache).unwrap());
            
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
        return Ok(());
    }
    
    // Full backup mode - generate copy.txt with all entries marked as NN
    let meta_reader = MetaRepoReader::new(meta_dir).unwrap();
    let mut copy_writer = ControlFileWriter::new(copy_file_path).unwrap();    
    let mut mtime_writer = MtimeControlFileWriter::new(mtime_file_path).unwrap();

    let dcaches : Vec<PathBuf> = fs::read_dir(dcache_dir.clone()).unwrap()
        .filter_map(|f| f.ok())
        .filter(|f|f.file_name().to_string_lossy().starts_with("dcache_"))
        .map(|f| f.path())
        .collect();

    for dcache in dcaches {
        let dcache_iter : DirCacheIterator = DirCacheIterator::from(
            DirCacheRandomReader::open(dcache).unwrap());
        
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
            copy_writer.write_dir(&dctrl_entry).unwrap();
            
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
            let fcache_path = fcache_dir.join(format!("{}_{}.dat", "fcache", fcache_fid));
            let fcache_iter : FileCacheIterator = FileCacheIterator::from(
                FileCacheRandomReader::open(fcache_path).unwrap(), 
                files_count, 
                fcache_offset/FileCacheEntry::SIZE as u32);

            for fcache_entry in fcache_iter {
                let fmeta = meta_reader.get_fmeta(fcache_entry.meta_loc).unwrap();
                let fctrl_entry = FileControlEntry {
                    name: fmeta.common.name,
                    diff: FileDiff::New,
                    meta_fid: fcache_entry.meta_loc.0,
                    meta_offset: fcache_entry.meta_loc.1,
                };
                copy_writer.write_file(&fctrl_entry).unwrap();

            }
        }
    }

    copy_writer.finish().unwrap();
    mtime_writer.finish().unwrap();
    Ok(())
}