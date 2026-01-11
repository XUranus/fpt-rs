use std::{fs::{self, OpenOptions}, io, path::PathBuf, sync::Arc, thread};
use log::{debug, info, warn, error};

use crate::scanner::{
    ScanWorkerContext,
    metadata::{
        ControlFileWriter, DirCacheEntry, DirCacheIterator, DirCacheRandomReader, DirCacheWriter, DirControlEntry, DirDiff, FileCacheEntry, FileCacheIterator, FileCacheRandomReader, FileCacheWriter, FileControlEntry, FileDiff, FixedSize, MetaRepoReader, MetaRepoWriter
    },
    models::DirBatchScanResult, options::TargetDirOption
};

pub mod bio;
// mod aio;


// generate meta data to files
pub fn start_meta_writers(context : &ScanWorkerContext, writer_count : usize) -> Vec<thread::JoinHandle<()>> {
    let mut writer_handles = Vec::with_capacity(writer_count);
    let target_dir = &context.scan_option.target_dir;

    for i in 0..writer_count {
        let output_queue = Arc::clone(&context.output_queue);
        let meta_dir = target_dir.meta_dir.clone();
        let dcache_dir = target_dir.meta_dir.clone();
        let fcache_dir = target_dir.meta_dir.clone();

        let handle = std::thread::spawn(move || {
            // writer thread logic here
            let mut meta_writer = MetaRepoWriter::new(meta_dir).unwrap();
            let mut dcache_writer : DirCacheWriter = DirCacheWriter::new(dcache_dir, i as u32).unwrap();
            let mut fcache_writer : FileCacheWriter = FileCacheWriter::new(fcache_dir, i as u32).unwrap();
            print!("Writer thread {} started\n", i);
            loop {
                // pop path from output meta queue and process
                if let Some(dir_scan_result) = output_queue.pop() {
                    // process the path, open the directory, read entries, etc.
                    process_scan_result(dir_scan_result, &mut meta_writer, &mut dcache_writer, &mut fcache_writer);
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


fn process_scan_result(dir_scan_result : DirBatchScanResult,
    meta_writer : &mut MetaRepoWriter,
    dcache_writer : &mut DirCacheWriter,
    fcache_writer : &mut FileCacheWriter
)
{
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
        let mut fcache : FileCacheEntry =  fmeta.into();
        fcache.meta_loc = fmeta_loc;
        sorted_fcaches.push(fcache);
    }
    sorted_fcaches.sort_by_key(|v| v.id);
    
    for fcache in sorted_fcaches {
        _ = fcache_writer.write(&fcache).unwrap();
        //debug!("write fcache {:#?}", fcache)
    }
    let mut dcache : DirCacheEntry = dir_scan_result.dir.into();
    dcache.meta_loc = dmeta_loc;
    dcache.files_count = files_count as u32;
    (dcache.fcache_fid, dcache.fcache_offset) = (fcache_fid, fcache_offset);
    _ = dcache_writer.write(&dcache).unwrap();

    // TODO:: sort dcache later
    // TODO:: merge fcache later

}



pub fn generate_control_files(target_option : &TargetDirOption) -> Result<(), io::Error> {
    let meta_dir = target_option.meta_dir.clone();
    let dcache_dir = target_option.meta_dir.clone();
    let fcache_dir = target_option.meta_dir.clone();

    let ctrl_file_path = meta_dir.join("ctrl.txt");
    
    let meta_reader = MetaRepoReader::new(meta_dir).unwrap();
    let mut ctrl_writer = ControlFileWriter::new(ctrl_file_path).unwrap();    

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
                path: dmeta.path,
                diff: DirDiff::New,
                meta_fid: dcache_entry.meta_loc.0,
                meta_offset: dcache_entry.meta_loc.1,
                files_count: files_count,
            };
            ctrl_writer.write_dir(&dctrl_entry).unwrap();
            
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
                ctrl_writer.write_file(&fctrl_entry).unwrap();

            }
        }
    }

    ctrl_writer.finish().unwrap();
    Ok(())
}