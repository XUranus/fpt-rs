use std::{path::PathBuf, time::Duration};

use fpt::{
    backup::{BackupOption, BackupTask},
    localfs::Scanner,
    scanner::options::ScanOption,
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
    setup_logger().expect("failed to initialize logger");

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
        .expect("failed to enqueue scan path");
    let scan = scanner.start().expect("failed to start scanner");
    while !scan.complete() {
        println!("{:#?}", scan.stats());
        std::thread::sleep(Duration::from_secs(1));
    }
    println!("Scan complete");

    let source = fpt::frame::location::DataLocation::local("/");
    let target = fpt::frame::location::DataLocation::local("/tmp/fpt/target");
    let meta_dir = PathBuf::from("/tmp/fpt/meta");
    let ctrl_dir = PathBuf::from("/tmp/fpt/ctrl");
    let control_file = PathBuf::from("/tmp/fpt/meta/ctrl.txt");

    let fsbackup: BackupTask = BackupOption::new(
        source,
        target,
        meta_dir,
        ctrl_dir,
        control_file,
    )
    .into();
    let task = fsbackup.start().expect("failed to start backup task");
    while !task.complete() {
        let stats = task.stats();
        print!("{:#?}", stats);
        std::thread::sleep(Duration::from_secs(1));
    }
    task.wait().expect("backup task failed");
}
