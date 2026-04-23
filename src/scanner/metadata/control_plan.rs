use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use crate::scanner::metadata::{
    dir_cache_path, file_cache_path, ControlFileHeader, ControlFileWriter, DirCacheEntry,
    DirCacheRandomReader, DirControlEntry, DirDiff, FileCacheEntry, FileCacheRandomReader,
    FileControlEntry, FileDiff, FixedSize, HardlinkControlFileWriter, HardlinkFileEntry,
    HardlinkInodeEntry, MetaRepoReader, MtimeControlFileWriter, MtimeDirEntry,
    ShardedControlFileManager,
};

#[derive(Debug, Clone)]
pub enum ControlPlanMode {
    Full,
    Diff { prev_meta_dir: Option<PathBuf> },
    FineGrain { paths: Vec<String> },
}

#[derive(Debug, Clone)]
pub struct GeneratedControlPlan {
    pub ctrl_dir: PathBuf,
    pub copy_files: Vec<PathBuf>,
    pub hardlink_file: Option<PathBuf>,
    pub mtime_file: Option<PathBuf>,
    pub delete_file: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct SourceLayout {
    source_kind: String,
    physical_base: PathBuf,
    logical_root: String,
}

#[derive(Debug, Clone)]
struct DirRecord {
    logical_path: String,
    dcache: DirCacheEntry,
    atime: u64,
    mtime: u64,
    mode: u32,
    uid: u32,
    gid: u32,
}

#[derive(Debug, Clone)]
struct FileSelection {
    meta_fid: u32,
    meta_offset: u32,
    name: String,
    logical_path: String,
    inode: u64,
    devno: u64,
    links: u64,
}

#[derive(Debug, Clone)]
struct FineGrainSelection {
    requested_dirs: HashSet<String>,
    requested_files: HashSet<String>,
}

impl FineGrainSelection {
    fn include_dir(&self, dir_path: &str) -> bool {
        self.requested_dirs
            .iter()
            .any(|prefix| path_prefix_matches(prefix, dir_path))
            || self
                .requested_files
                .iter()
                .any(|file| path_is_ancestor(dir_path, file))
    }

    fn include_file(&self, file_path: &str) -> bool {
        self.requested_files.contains(file_path)
            || self
                .requested_dirs
                .iter()
                .any(|prefix| path_prefix_matches(prefix, file_path))
    }
}

pub fn generate_control_plan(
    curr_meta_dir: &Path,
    ctrl_dir: &Path,
    source_spec: &str,
    mode: ControlPlanMode,
    copy_shards: usize,
) -> io::Result<GeneratedControlPlan> {
    match mode {
        ControlPlanMode::Full => {
            generate_restore_plan(curr_meta_dir, ctrl_dir, source_spec, None, copy_shards)
        }
        ControlPlanMode::FineGrain { paths } => {
            let selection = classify_fine_grain_selection(curr_meta_dir, source_spec, &paths)?;
            generate_restore_plan(
                curr_meta_dir,
                ctrl_dir,
                source_spec,
                Some(selection),
                copy_shards,
            )
        }
        ControlPlanMode::Diff { prev_meta_dir } => {
            std::fs::create_dir_all(ctrl_dir)?;
            let layout = source_layout_from_spec(source_spec);
            super::generate_incremental_control_files(
                prev_meta_dir.as_deref(),
                curr_meta_dir,
                ctrl_dir,
                &layout.source_kind,
                &layout.logical_root,
            )?;
            Ok(GeneratedControlPlan {
                ctrl_dir: ctrl_dir.to_path_buf(),
                copy_files: vec![ctrl_dir.join("copy.txt")],
                hardlink_file: None,
                mtime_file: None,
                delete_file: Some(ctrl_dir.join("delete.txt")),
            })
        }
    }
}

fn generate_restore_plan(
    meta_dir: &Path,
    ctrl_dir: &Path,
    source_spec: &str,
    selection: Option<FineGrainSelection>,
    copy_shards: usize,
) -> io::Result<GeneratedControlPlan> {
    std::fs::create_dir_all(ctrl_dir)?;
    let layout = source_layout_from_spec(source_spec);
    let meta_repo = MetaRepoReader::new(meta_dir)?;
    let dirs = load_sorted_dirs(meta_dir, &meta_repo, &layout)?;

    let copy_header = ControlFileHeader {
        source_kind: layout.source_kind.clone(),
        source_root: layout.logical_root.clone(),
        ..ControlFileHeader::default()
    };

    let copy_shard_count = copy_shards.max(1);
    let mut sharded_copy = ShardedControlFileManager::new_with_header(
        ctrl_dir.to_path_buf(),
        "copy".to_string(),
        copy_shard_count,
        copy_header.clone(),
    )?;
    let hardlink_path = ctrl_dir.join("hardlink.txt");
    let mut hardlink_writer = HardlinkControlFileWriter::new_with_source(
        &hardlink_path,
        &layout.source_kind,
        &layout.logical_root,
    )?;
    let mtime_path = ctrl_dir.join("mtime.txt");
    let mut mtime_writer = MtimeControlFileWriter::new_with_source(
        &mtime_path,
        &layout.source_kind,
        &layout.logical_root,
    )?;

    let mut hardlink_groups: BTreeMap<(u64, u64), Vec<HardlinkFileEntry>> = BTreeMap::new();
    let mut emitted_any_copy = false;
    let mut emitted_any_mtime = false;
    let mut emitted_any_hardlink = false;

    for dir in &dirs {
        let include_dir = selection
            .as_ref()
            .map(|selected| selected.include_dir(&dir.logical_path))
            .unwrap_or(true);
        let file_entries = collect_selected_files(meta_dir, &meta_repo, dir, selection.as_ref())?;
        let needs_dir = include_dir || !file_entries.is_empty();
        if !needs_dir {
            continue;
        }

        let dir_entry = DirControlEntry {
            path: dir.logical_path.clone(),
            diff: DirDiff::New,
            meta_fid: dir.dcache.meta_loc.0,
            meta_offset: dir.dcache.meta_loc.1,
            files_count: file_entries.len() as u32,
        };
        sharded_copy.write_directory(&dir_entry, None)?;
        emitted_any_copy = true;

        let mtime_entry = MtimeDirEntry {
            path: dir.logical_path.clone(),
            mode: dir.mode,
            uid: dir.uid,
            gid: dir.gid,
            atime: dir.atime,
            mtime: dir.mtime,
        };
        mtime_writer.write_dir(&mtime_entry)?;
        emitted_any_mtime = true;

        for file in file_entries {
            sharded_copy.write_file(
                &dir.logical_path,
                &FileControlEntry {
                    name: file.name.clone(),
                    diff: FileDiff::New,
                    meta_fid: file.meta_fid,
                    meta_offset: file.meta_offset,
                },
            )?;
            emitted_any_copy = true;

            if file.links > 1 {
                hardlink_groups
                    .entry((file.inode, file.devno))
                    .or_default()
                    .push(HardlinkFileEntry {
                        meta_fid: file.meta_fid,
                        meta_offset: file.meta_offset,
                        path: file.logical_path.clone(),
                    });
            }
        }
    }

    for ((inode, device), files) in hardlink_groups {
        if files.len() < 2 {
            continue;
        }
        hardlink_writer.write_inode(&HardlinkInodeEntry {
            inode,
            device,
            link_count: files.len() as u32,
        })?;
        for file in files {
            hardlink_writer.write_file(&file)?;
        }
        emitted_any_hardlink = true;
    }

    let copy_files = if emitted_any_copy {
        let mut files = sharded_copy.finish()?;
        if files.is_empty() {
            let copy_path = ctrl_dir.join("copy.txt");
            ControlFileWriter::new_with_header(&copy_path, &copy_header)?.finish()?;
            files.push(copy_path);
        }
        files.sort();
        files
    } else {
        sharded_copy.finish()?;
        Vec::new()
    };

    let hardlink_file = if emitted_any_hardlink {
        hardlink_writer.finish()?;
        Some(hardlink_path)
    } else {
        drop(hardlink_writer);
        let _ = std::fs::remove_file(&hardlink_path);
        None
    };

    let mtime_file = if emitted_any_mtime {
        mtime_writer.finish()?;
        Some(mtime_path)
    } else {
        drop(mtime_writer);
        let _ = std::fs::remove_file(&mtime_path);
        None
    };

    Ok(GeneratedControlPlan {
        ctrl_dir: ctrl_dir.to_path_buf(),
        copy_files,
        hardlink_file,
        mtime_file,
        delete_file: None,
    })
}

fn classify_fine_grain_selection(
    meta_dir: &Path,
    source_spec: &str,
    requested_paths: &[String],
) -> io::Result<FineGrainSelection> {
    let layout = source_layout_from_spec(source_spec);
    let meta_repo = MetaRepoReader::new(meta_dir)?;
    let dirs = load_sorted_dirs(meta_dir, &meta_repo, &layout)?;

    let normalized_requests: Vec<(String, String)> = requested_paths
        .iter()
        .map(|path| (path.clone(), normalize_requested_path(&layout, path)))
        .collect();

    let mut requested_dirs = HashSet::new();
    let mut requested_files = HashSet::new();
    let mut unresolved: HashSet<String> = normalized_requests
        .iter()
        .map(|(_, norm)| norm.clone())
        .collect();

    for dir in &dirs {
        if unresolved.remove(&dir.logical_path) {
            requested_dirs.insert(dir.logical_path.clone());
        }
        if unresolved.is_empty() {
            break;
        }
        for file in collect_selected_files(meta_dir, &meta_repo, dir, None)? {
            if unresolved.remove(&file.logical_path) {
                requested_files.insert(file.logical_path.clone());
            }
            if unresolved.is_empty() {
                break;
            }
        }
        if unresolved.is_empty() {
            break;
        }
    }

    if !unresolved.is_empty() {
        let mut missing = Vec::new();
        for (raw, normalized) in &normalized_requests {
            if unresolved.contains(normalized) {
                missing.push(raw.clone());
            }
        }
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "fine-grain restore path(s) not found in metadata: {}",
                missing.join(", ")
            ),
        ));
    }

    Ok(FineGrainSelection {
        requested_dirs,
        requested_files,
    })
}

fn collect_selected_files(
    meta_dir: &Path,
    meta_repo: &MetaRepoReader,
    dir: &DirRecord,
    selection: Option<&FineGrainSelection>,
) -> io::Result<Vec<FileSelection>> {
    let fcache_path = file_cache_path(meta_dir, dir.dcache.fcache_fid);
    let mut reader = FileCacheRandomReader::open(&fcache_path)?;
    let start_idx = dir.dcache.fcache_offset / FileCacheEntry::SIZE as u32;
    let mut selected = Vec::new();
    for i in 0..dir.dcache.files_count {
        let entry = reader.read_object(start_idx + i)?;
        let fmeta = meta_repo.get_fmeta(entry.meta_loc)?;
        let logical_path = join_logical_child(&dir.logical_path, &fmeta.common.name);
        let include = selection
            .map(|selected_paths| selected_paths.include_file(&logical_path))
            .unwrap_or(true);
        if !include {
            continue;
        }
        selected.push(FileSelection {
            meta_fid: entry.meta_loc.0,
            meta_offset: entry.meta_loc.1,
            name: fmeta.common.name.clone(),
            logical_path,
            inode: fmeta.common.id,
            devno: fmeta.common.devno,
            links: fmeta.links,
        });
    }
    Ok(selected)
}

fn load_sorted_dirs(
    meta_dir: &Path,
    meta_repo: &MetaRepoReader,
    layout: &SourceLayout,
) -> io::Result<Vec<DirRecord>> {
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(meta_dir)? {
        let path = entry?.path();
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !file_name.starts_with("dcache_") || file_name.ends_with(".tmp") {
            continue;
        }
        let shard_id = parse_cache_fid(file_name, "dcache_")?;
        let dcache_path = dir_cache_path(meta_dir, shard_id);
        if !dcache_path.exists() {
            continue;
        }
        let mut reader = DirCacheRandomReader::open(&dcache_path)?;
        for idx in 0..reader.total_count() {
            let dcache = reader.read_object(idx)?;
            let dmeta = meta_repo.get_dmeta(dcache.meta_loc)?;
            let logical_path = normalize_metadata_path(layout, &dmeta.path);
            dirs.push(DirRecord {
                logical_path,
                dcache,
                atime: dmeta.common.atime as u64,
                mtime: dmeta.common.mtime as u64,
                mode: dmeta.common.mode,
                uid: 0,
                gid: 0,
            });
        }
    }
    dirs.sort_by(|a, b| a.logical_path.cmp(&b.logical_path));
    Ok(dirs)
}

fn parse_cache_fid(file_name: &str, prefix: &str) -> io::Result<u32> {
    let raw = file_name
        .strip_prefix(prefix)
        .and_then(|s| s.strip_suffix(".dat"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid cache file name"))?;
    raw.parse::<u32>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn source_layout_from_spec(spec: &str) -> SourceLayout {
    let location = if spec.starts_with("nfs://") {
        crate::frame::DataLocation::from_nfs_url(spec)
            .unwrap_or_else(|_| crate::frame::DataLocation::local(PathBuf::from(spec)))
    } else if spec.starts_with("smb://") || spec.starts_with(r"smb:\\") {
        crate::frame::DataLocation::from_smb_url(spec)
            .unwrap_or_else(|_| crate::frame::DataLocation::local(PathBuf::from(spec)))
    } else {
        crate::frame::DataLocation::local(PathBuf::from(spec))
    };

    SourceLayout {
        source_kind: location.kind_name().to_string(),
        physical_base: location.control_path_base(),
        logical_root: location.logical_source_root(),
    }
}

fn normalize_metadata_path(layout: &SourceLayout, path: &str) -> String {
    let physical = PathBuf::from(path);
    let logical_root = PathBuf::from(&layout.logical_root);
    if !physical.starts_with(&layout.physical_base) && physical.starts_with(&logical_root) {
        return normalize_logical_string(path);
    }
    let rel = physical
        .strip_prefix(&layout.physical_base)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| physical.clone());
    normalize_logical_string(&format!(
        "/{}",
        rel.to_string_lossy().trim_start_matches('/')
    ))
}

fn normalize_requested_path(layout: &SourceLayout, raw: &str) -> String {
    let raw_path = PathBuf::from(raw);
    if raw_path.is_absolute() && raw_path.starts_with(&layout.physical_base) {
        return normalize_metadata_path(layout, raw);
    }

    let normalized = normalize_logical_string(raw);
    if layout.logical_root == "/" {
        return normalized;
    }
    if normalized == layout.logical_root || path_prefix_matches(&layout.logical_root, &normalized) {
        return normalized;
    }
    let suffix = normalized.trim_start_matches('/');
    normalize_logical_string(&format!(
        "{}/{}",
        layout.logical_root.trim_end_matches('/'),
        suffix
    ))
}

fn normalize_logical_string(raw: &str) -> String {
    let mut parts = Vec::new();
    let normalized = raw.replace('\\', "/");
    for part in normalized.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            let _ = parts.pop();
            continue;
        }
        parts.push(part);
    }
    if parts.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", parts.join("/"))
    }
}

fn join_logical_child(dir_path: &str, name: &str) -> String {
    if dir_path == "/" {
        normalize_logical_string(&format!("/{}", name))
    } else {
        normalize_logical_string(&format!("{}/{}", dir_path.trim_end_matches('/'), name))
    }
}

fn path_prefix_matches(prefix: &str, candidate: &str) -> bool {
    let prefix = normalize_logical_string(prefix);
    let candidate = normalize_logical_string(candidate);
    prefix == "/"
        || candidate == prefix
        || candidate.strip_prefix(&(prefix.clone() + "/")).is_some()
}

fn path_is_ancestor(dir_path: &str, file_path: &str) -> bool {
    let dir_path = normalize_logical_string(dir_path);
    let file_path = normalize_logical_string(file_path);
    dir_path == "/" || file_path.strip_prefix(&(dir_path.clone() + "/")).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_logical_paths() {
        assert_eq!(normalize_logical_string("/d1//d2/"), "/d1/d2");
        assert_eq!(normalize_logical_string("d1/d2"), "/d1/d2");
        assert_eq!(normalize_logical_string("/d1/./d2/../f1"), "/d1/f1");
    }

    #[test]
    fn prefix_matching_respects_boundaries() {
        assert!(path_prefix_matches("/d1", "/d1"));
        assert!(path_prefix_matches("/d1", "/d1/f1"));
        assert!(!path_prefix_matches("/d1", "/d10/f1"));
    }

    #[test]
    fn requested_path_maps_under_non_root_logical_prefix() {
        let layout = SourceLayout {
            source_kind: "nfs".to_string(),
            physical_base: PathBuf::from("/opt/dataset"),
            logical_root: "/ds3".to_string(),
        };
        assert_eq!(normalize_requested_path(&layout, "/d1/f1"), "/ds3/d1/f1");
        assert_eq!(
            normalize_requested_path(&layout, "/opt/dataset/ds3/d1"),
            "/ds3/d1"
        );
    }
}
