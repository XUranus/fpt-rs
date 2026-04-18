//! Shared control-file entry production for async backup pipelines.
//!
//! The async transport pipelines all consume the same control-file / metadata
//! repository pair but differ in how they map recorded source paths into:
//! - `fcb.src_path` for the read side
//! - `fcb.dst_path` for the write side
//!
//! This module centralizes that path-mapping logic so transport-specific
//! pipelines only need to provide a declarative mapping instead of duplicating
//! the control-file reader loop.

use std::path::PathBuf;

use log::{error, info};
use tokio::sync::mpsc;

use crate::backup::fcb::{ControlBlockVarient, DirControlBlock, FileControlBlock};
use crate::scanner::metadata::{ControlEntry, ControlFileReader, MetaRepoReader};

#[derive(Debug, Clone)]
pub enum PathLayout {
    AbsoluteFromControl,
    RelativeToBase {
        base: PathBuf,
    },
    PrefixedRelativeToBase {
        base: PathBuf,
        prefix: PathBuf,
    },
}

#[derive(Debug, Clone)]
pub struct EntryMapping {
    pub dir_source: PathLayout,
    pub dir_target: PathLayout,
    pub file_source: PathLayout,
    pub file_target: PathLayout,
}

impl EntryMapping {
    pub fn local_to_prefixed_target(source_base: PathBuf, target_prefix: PathBuf) -> Self {
        Self {
            dir_source: PathLayout::AbsoluteFromControl,
            dir_target: PathLayout::PrefixedRelativeToBase {
                base: source_base.clone(),
                prefix: target_prefix.clone(),
            },
            file_source: PathLayout::AbsoluteFromControl,
            file_target: PathLayout::PrefixedRelativeToBase {
                base: source_base,
                prefix: target_prefix,
            },
        }
    }

    pub fn remote_to_local(source_base: PathBuf) -> Self {
        Self {
            dir_source: PathLayout::AbsoluteFromControl,
            dir_target: PathLayout::RelativeToBase {
                base: source_base.clone(),
            },
            file_source: PathLayout::RelativeToBase {
                base: source_base.clone(),
            },
            file_target: PathLayout::RelativeToBase { base: source_base },
        }
    }

    pub fn remote_to_prefixed_target(source_base: PathBuf, target_prefix: PathBuf) -> Self {
        Self {
            dir_source: PathLayout::RelativeToBase {
                base: source_base.clone(),
            },
            dir_target: PathLayout::PrefixedRelativeToBase {
                base: source_base.clone(),
                prefix: target_prefix.clone(),
            },
            file_source: PathLayout::RelativeToBase {
                base: source_base.clone(),
            },
            file_target: PathLayout::PrefixedRelativeToBase {
                base: source_base,
                prefix: target_prefix,
            },
        }
    }
}

pub fn produce_entries(
    control_file: PathBuf,
    meta_dir: PathBuf,
    mapping: EntryMapping,
    tx: mpsc::Sender<ControlBlockVarient>,
    log_prefix: &str,
) {
    let meta_repo = match MetaRepoReader::new(meta_dir) {
        Ok(r) => r,
        Err(e) => {
            error!("{log_prefix}: cannot open meta repo: {e}");
            return;
        }
    };

    let reader = match ControlFileReader::open(control_file) {
        Ok(r) => r,
        Err(e) => {
            error!("{log_prefix}: cannot open control file: {e}");
            return;
        }
    };

    let mut current_dir = PathBuf::new();
    let mut entry_count: usize = 0;

    for entry_result in reader {
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                error!("{log_prefix}: read error: {e}");
                continue;
            }
        };

        let item = match entry {
            ControlEntry::Dir(dentry) => {
                let dmeta = match meta_repo.get_dmeta((dentry.meta_fid, dentry.meta_offset)) {
                    Ok(m) => m,
                    Err(e) => {
                        error!("{log_prefix}: get_dmeta error: {e}");
                        continue;
                    }
                };

                current_dir = PathBuf::from(&dentry.path);
                let mut dcb = DirControlBlock::from(dmeta);
                dcb.src_path = map_control_path(&mapping.dir_source, &current_dir);
                dcb.dst_path = map_control_path(&mapping.dir_target, &current_dir);
                ControlBlockVarient::DirControlBlock(dcb)
            }
            ControlEntry::File(fentry) => {
                let fmeta = match meta_repo.get_fmeta((fentry.meta_fid, fentry.meta_offset)) {
                    Ok(m) => m,
                    Err(e) => {
                        error!("{log_prefix}: get_fmeta error: {e}");
                        continue;
                    }
                };

                let mut fcb = FileControlBlock::from(fmeta);
                fcb.src_path = map_child_path(&mapping.file_source, &current_dir, &fentry.name);
                fcb.dst_path = map_child_path(&mapping.file_target, &current_dir, &fentry.name);
                ControlBlockVarient::FileControlBlock(fcb)
            }
        };

        if tx.blocking_send(item).is_err() {
            break;
        }
        entry_count += 1;
    }

    info!("{log_prefix}: done, {entry_count} entries produced");
}

fn map_control_path(layout: &PathLayout, control_path: &PathBuf) -> PathBuf {
    match layout {
        PathLayout::AbsoluteFromControl => control_path.clone(),
        PathLayout::RelativeToBase { base } => make_relative(base, control_path),
        PathLayout::PrefixedRelativeToBase { base, prefix } => {
            prefix.join(make_relative(base, control_path))
        }
    }
}

fn map_child_path(layout: &PathLayout, control_dir: &PathBuf, file_name: &str) -> PathBuf {
    match layout {
        PathLayout::AbsoluteFromControl => control_dir.join(file_name),
        PathLayout::RelativeToBase { base } => make_relative(base, control_dir).join(file_name),
        PathLayout::PrefixedRelativeToBase { base, prefix } => {
            prefix.join(make_relative(base, control_dir)).join(file_name)
        }
    }
}

pub fn make_relative(base: &PathBuf, path: &PathBuf) -> PathBuf {
    if let Ok(rel) = path.strip_prefix(base) {
        rel.to_path_buf()
    } else if path.is_absolute() {
        path.file_name().map(PathBuf::from).unwrap_or_else(|| path.clone())
    } else {
        path.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{EntryMapping, PathLayout, map_child_path, map_control_path};
    use std::path::PathBuf;

    #[test]
    fn remote_to_prefixed_target_maps_relative_paths() {
        let mapping = EntryMapping::remote_to_prefixed_target(
            PathBuf::from("/export/sub"),
            PathBuf::from("COPY_COMMON_FULL_x/D_REPO"),
        );

        let dir = PathBuf::from("/export/sub/a/b");
        assert_eq!(
            map_control_path(&mapping.dir_source, &dir),
            PathBuf::from("a/b")
        );
        assert_eq!(
            map_control_path(&mapping.dir_target, &dir),
            PathBuf::from("COPY_COMMON_FULL_x/D_REPO/a/b")
        );
        assert_eq!(
            map_child_path(&mapping.file_source, &dir, "f.txt"),
            PathBuf::from("a/b/f.txt")
        );
        assert_eq!(
            map_child_path(&mapping.file_target, &dir, "f.txt"),
            PathBuf::from("COPY_COMMON_FULL_x/D_REPO/a/b/f.txt")
        );
    }

    #[test]
    fn absolute_layout_keeps_control_paths() {
        let dir = PathBuf::from("/data/src/dir");
        assert_eq!(
            map_control_path(&PathLayout::AbsoluteFromControl, &dir),
            dir
        );
    }
}
