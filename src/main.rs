extern crate libc;

use std::{path::PathBuf, time::Duration};

use bifrost::{
    backup::{BackupOption, BackupTask},
    scanner::{options::ScanOption, Scanner},
};

fn setup_logger() -> Result<(), fern::InitError> {
    fern::Dispatch::new()
        // Perform allocation-free log formatting
        .format(|out, message, record| {
            out.finish(format_args!(
                "[{}] [{}] [{}] [{:?}:{:?}] {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                record.level(),
                record.target(),
                record.file(),
                record.line(),
                message
            ))
        })
        // Log to stdout (optional)
        //
        //.chain(std::io::stdout())
        // Log to a file
        .chain(fern::log_file("output.log")?)
        // Set the global log level
        .apply()?;
    Ok(())
}

fn main() {
    // let path = "/home";

    // match get_inode(path) {
    //     Ok(inode) => ls("Inode of {}: {}", path, inode),
    //     Err(e) => eprintln!("Error: {}", e),
    // }
    //---------------------------------------------------

    // let my_data = (42u32, "Hello, world!".to_string());

    // let queue = SpillQueue::new(
    //     PathBuf::from("/tmp/myqueue"),
    //     1000,    // memory_upper_bound
    //     500,     // memory_lower_bound
    //     200      // spill_load_batch_size (must be ≤ 500)
    // ).unwrap();

    // for i in 0..1500 {
    //     let item = (i, format!("Item {}", i));
    //     queue.push(item).unwrap();
    // }
    // while !queue.is_empty() {
    //     let item = queue.pop().unwrap();
    //     print!("{:?}\n", item);
    // }

    //---------------------------------------------------
    setup_logger().unwrap();

    let mut scanner: Scanner = ScanOption::new(
        PathBuf::from("/tmp/bifrost/ctrl"),
        PathBuf::from("/tmp/bifrost/meta"),
    )
    .follow_symlinks(false)
    .scan_hidden(false)
    .max_depth(None)
    .worker_count(4)
    .writer_count(1)
    .into();

    scanner
        .enqueue_path(PathBuf::from("/home/xuranus/workspace/bifrost/mnt/source"))
        .unwrap();
    let scan = scanner.start().unwrap();
    while !scan.complete() {
        println!("{:#?}", scan.stats());
        std::thread::sleep(Duration::from_secs(1));
    }
    println!("Scan complete");

    //    let mut w = bifrost::scanner::fsidx_storage::FileCacheWriter::new("/tmp/bifrost", "wxx").unwrap();
    //    let res = w.write(&bifrost::scanner::fsidx_storage::FileCacheEntry::default()).unwrap();
    //    println!("{:#?}", res);

    //    let res = w.write(&bifrost::scanner::fsidx_storage::FileCacheEntry::default()).unwrap();
    //    println!("{:#?}", res);

    // let mut r = bifrost::scanner::fsidx_storage::FileCacheReader::new("/tmp/bifrost", "wxx").unwrap();
    // let res = r.read(0, 5).unwrap();
    // print!("{:#?}", res);

    // let a = PathBuf::from("/tmp/target");
    // let b = PathBuf::from("/home/xuranus/dataset/dir1/dir11");
    // let c = String::from("11.txt");
    // let d = a.join(b.clone()).join(c.clone());
    // println!("{:?} {:?} {} {:?}", a, b, c, d);

    // why the output is not /tmp/target/home/xuranus/dataset/dir1/dir11/11.txt

    let source_dir_base = PathBuf::from("/");
    let target_dir_base = PathBuf::from("/home/xuranus/workspace/bifrost/mnt/target");
    let meta_dir = PathBuf::from("/tmp/bifrost/meta");
    let ctrl_dir = PathBuf::from("/tmp/bifrost/ctrl");
    let control_file = PathBuf::from("/tmp/bifrost/meta/ctrl.txt");

    let fsbackup: BackupTask = BackupOption::new(
        source_dir_base,
        target_dir_base,
        meta_dir,
        ctrl_dir,
        control_file,
    )
    .into();
    let task = fsbackup.start().unwrap();
    while !task.complete() {
        let stats = task.stats();
        print!("{:#?}", stats);
        std::thread::sleep(Duration::from_secs(1));
    }
    task.wait().unwrap();
}
