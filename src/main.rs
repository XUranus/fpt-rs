use std::{path::PathBuf, time::Duration};

use fpt::{
    backup::{BackupOption, BackupTask},
    scanner::{options::ScanOption, Scanner},
};

fn setup_logger() -> Result<(), fern::InitError> {
    fern::Dispatch::new()
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
        .chain(fern::log_file("output.log")?)
        .apply()?;
    Ok(())
}

fn main() {
    setup_logger().unwrap();

    let mut scanner: Scanner = ScanOption::new(
        PathBuf::from("/tmp/fpt/ctrl"),
        PathBuf::from("/tmp/fpt/meta"),
    )
    .follow_symlinks(false)
    .scan_hidden(false)
    .max_depth(None)
    .worker_count(4)
    .writer_count(1)
    .into();

    scanner
        .enqueue_path(PathBuf::from("/tmp/fpt/source"))
        .unwrap();
    let scan = scanner.start().unwrap();
    while !scan.complete() {
        println!("{:#?}", scan.stats());
        std::thread::sleep(Duration::from_secs(1));
    }
    println!("Scan complete");

    let source_dir_base = PathBuf::from("/");
    let target_dir_base = PathBuf::from("/tmp/fpt/target");
    let meta_dir = PathBuf::from("/tmp/fpt/meta");
    let ctrl_dir = PathBuf::from("/tmp/fpt/ctrl");
    let control_file = PathBuf::from("/tmp/fpt/meta/ctrl.txt");

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
