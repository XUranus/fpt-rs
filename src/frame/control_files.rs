use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const CONTROL_FILE_EXTENSION: &str = ".control.bin";

pub fn control_file_name(phrase: &str, discriminator: &str) -> String {
    format!("{phrase}_{}.control.bin", short_hash(&format!("{phrase}:{discriminator}")))
}

pub fn primary_control_file_name(phrase: &str) -> String {
    control_file_name(phrase, "main")
}

pub fn sharded_copy_control_file_name(shard_id: usize, file_index: u32) -> String {
    control_file_name("copy", &format!("shard:{shard_id}:file:{file_index}"))
}

pub fn primary_control_file_path(ctrl_dir: &Path, phrase: &str) -> PathBuf {
    ctrl_dir.join(primary_control_file_name(phrase))
}

pub fn classify_control_file_name(file_name: &str) -> Option<&'static str> {
    if !file_name.ends_with(CONTROL_FILE_EXTENSION) {
        return None;
    }
    if file_name.starts_with("copy_") {
        return Some("copy");
    }
    if file_name.starts_with("hardlink_") {
        return Some("hardlink");
    }
    if file_name.starts_with("delete_") {
        return Some("delete");
    }
    if file_name.starts_with("mtime_") {
        return Some("mtime");
    }
    None
}

pub fn find_primary_control_file(ctrl_dir: &Path, phrase: &str) -> Option<PathBuf> {
    let expected = primary_control_file_name(phrase);
    let path = ctrl_dir.join(expected);
    if path.exists() {
        return Some(path);
    }

    std::fs::read_dir(ctrl_dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .and_then(classify_control_file_name)
                == Some(phrase)
        })
}

pub fn discover_control_files(ctrl_dir: &Path, phrase: &str) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(ctrl_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .and_then(classify_control_file_name)
                == Some(phrase)
        })
        .collect();
    files.sort();
    files
}

fn short_hash(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

