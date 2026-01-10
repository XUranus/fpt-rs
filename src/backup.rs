use std::path::PathBuf;

mod fcb;
mod bio;

pub struct BackupOption {
    target_dir_base : PathBuf
}