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

use crate::backup::fcb::{ControlBlockVarient, DirControlBlock, FileControlBlock};
use crate::scanner::metadata::{ControlEntry, ControlFileReader, MetaRepoReader};
use log::{error, info};

#[derive(Debug, Clone)]
pub enum PathLayout {
    LocalSource { root: PathBuf },
    RemoteSource,
    LogicalTarget,
    PrefixedLogicalTarget { prefix: PathBuf },
}

#[derive(Debug, Clone)]
pub struct EntryMapping {
    pub dir_source: PathLayout,
    pub dir_target: PathLayout,
    pub file_source: PathLayout,
    pub file_target: PathLayout,
}

impl EntryMapping {
    pub fn local_to_prefixed_target(source_root: PathBuf, target_prefix: PathBuf) -> Self {
        Self {
            dir_source: PathLayout::LocalSource {
                root: source_root.clone(),
            },
            dir_target: PathLayout::PrefixedLogicalTarget {
                prefix: target_prefix.clone(),
            },
            file_source: PathLayout::LocalSource { root: source_root },
            file_target: PathLayout::PrefixedLogicalTarget {
                prefix: target_prefix,
            },
        }
    }

    pub fn remote_to_local() -> Self {
        Self {
            dir_source: PathLayout::RemoteSource,
            dir_target: PathLayout::LogicalTarget,
            file_source: PathLayout::RemoteSource,
            file_target: PathLayout::LogicalTarget,
        }
    }

    pub fn remote_to_prefixed_target(target_prefix: PathBuf) -> Self {
        Self {
            dir_source: PathLayout::RemoteSource,
            dir_target: PathLayout::PrefixedLogicalTarget {
                prefix: target_prefix.clone(),
            },
            file_source: PathLayout::RemoteSource,
            file_target: PathLayout::PrefixedLogicalTarget {
                prefix: target_prefix,
            },
        }
    }
}

pub(crate) fn produce_entries_for_each<F>(
    control_file: PathBuf,
    meta_dir: PathBuf,
    mapping: EntryMapping,
    log_prefix: &str,
    mut on_entry: F,
) -> usize
where
    F: FnMut(ControlBlockVarient) -> bool,
{
    let meta_repo = match MetaRepoReader::new(meta_dir) {
        Ok(r) => r,
        Err(e) => {
            error!("{log_prefix}: cannot open meta repo: {e}");
            return 0;
        }
    };

    let reader = match ControlFileReader::open(control_file) {
        Ok(r) => r,
        Err(e) => {
            error!("{log_prefix}: cannot open control file: {e}");
            return 0;
        }
    };
    let logical_source_root = PathBuf::from(reader.header().source_root.clone());

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
                dcb.src_path =
                    map_control_path(&mapping.dir_source, &current_dir, &logical_source_root);
                dcb.dst_path =
                    map_control_path(&mapping.dir_target, &current_dir, &logical_source_root);
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
                fcb.src_path = map_child_path(
                    &mapping.file_source,
                    &current_dir,
                    &fentry.name,
                    &logical_source_root,
                );
                fcb.dst_path = map_child_path(
                    &mapping.file_target,
                    &current_dir,
                    &fentry.name,
                    &logical_source_root,
                );
                ControlBlockVarient::FileControlBlock(fcb)
            }
        };

        if !on_entry(item) {
            break;
        }
        entry_count += 1;
    }

    info!("{log_prefix}: done, {entry_count} entries produced");
    entry_count
}

fn map_control_path(
    layout: &PathLayout,
    control_path: &PathBuf,
    logical_source_root: &PathBuf,
) -> PathBuf {
    match layout {
        PathLayout::LocalSource { root } => {
            join_local_source(root, logical_source_root, control_path)
        }
        PathLayout::RemoteSource => strip_logical_source_root(logical_source_root, control_path),
        PathLayout::LogicalTarget => logical_relative_path(control_path),
        PathLayout::PrefixedLogicalTarget { prefix } => {
            prefix.join(logical_relative_path(control_path))
        }
    }
}

fn map_child_path(
    layout: &PathLayout,
    control_dir: &PathBuf,
    file_name: &str,
    logical_source_root: &PathBuf,
) -> PathBuf {
    match layout {
        PathLayout::LocalSource { root } => {
            join_local_source(root, logical_source_root, control_dir).join(file_name)
        }
        PathLayout::RemoteSource => {
            strip_logical_source_root(logical_source_root, control_dir).join(file_name)
        }
        PathLayout::LogicalTarget => logical_relative_path(control_dir).join(file_name),
        PathLayout::PrefixedLogicalTarget { prefix } => prefix
            .join(logical_relative_path(control_dir))
            .join(file_name),
    }
}

fn join_local_source(
    root: &PathBuf,
    logical_source_root: &PathBuf,
    control_path: &PathBuf,
) -> PathBuf {
    root.join(strip_logical_source_root(logical_source_root, control_path))
}

fn strip_logical_source_root(logical_source_root: &PathBuf, control_path: &PathBuf) -> PathBuf {
    let path = logical_relative_path(control_path);
    let root = logical_relative_path(logical_source_root);
    if root.as_os_str().is_empty() {
        return path;
    }
    path.strip_prefix(&root)
        .map(|p| p.to_path_buf())
        .unwrap_or(path)
}

fn logical_relative_path(control_path: &PathBuf) -> PathBuf {
    control_path
        .strip_prefix("/")
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| control_path.clone())
}

#[cfg(test)]
mod tests {
    use super::{map_child_path, map_control_path, EntryMapping, PathLayout};
    use std::path::PathBuf;

    #[test]
    fn remote_to_prefixed_target_maps_relative_paths() {
        let mapping =
            EntryMapping::remote_to_prefixed_target(PathBuf::from("COPY_COMMON_FULL_x/D_REPO"));
        let logical_root = PathBuf::from("/ds2");

        let dir = PathBuf::from("/ds2/a/b");
        assert_eq!(
            map_control_path(&mapping.dir_source, &dir, &logical_root),
            PathBuf::from("a/b")
        );
        assert_eq!(
            map_control_path(&mapping.dir_target, &dir, &logical_root),
            PathBuf::from("COPY_COMMON_FULL_x/D_REPO/ds2/a/b")
        );
        assert_eq!(
            map_child_path(&mapping.file_source, &dir, "f.txt", &logical_root),
            PathBuf::from("a/b/f.txt")
        );
        assert_eq!(
            map_child_path(&mapping.file_target, &dir, "f.txt", &logical_root),
            PathBuf::from("COPY_COMMON_FULL_x/D_REPO/ds2/a/b/f.txt")
        );
    }

    #[test]
    fn local_source_layout_joins_real_root() {
        let dir = PathBuf::from("/ds2/dir");
        let logical_root = PathBuf::from("/ds2");
        assert_eq!(
            map_control_path(
                &PathLayout::LocalSource {
                    root: PathBuf::from("/opt/dataset/ds2")
                },
                &dir,
                &logical_root
            ),
            PathBuf::from("/opt/dataset/ds2/dir")
        );
    }
}
