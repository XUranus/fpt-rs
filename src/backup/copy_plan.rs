use std::path::PathBuf;

use crate::backup::aio::entry::{produce_entries_for_each, EntryMapping};
use crate::backup::fcb::FileControlBlock;
use crate::scanner::metadata::{DirMeta, FileMeta};

pub(crate) enum CopyPlanEntry {
    Directory { meta: DirMeta, dst_path: PathBuf },
    File(FileCopyPlan),
}

pub(crate) enum FileCopyPlan {
    Direct {
        meta: FileMeta,
        src_path: PathBuf,
        dst_path: PathBuf,
    },
    Aggregate {
        meta: FileMeta,
        src_path: PathBuf,
    },
}

impl FileCopyPlan {
    pub fn from_fcb(
        fcb: FileControlBlock,
        should_aggregate: impl FnOnce(&FileMeta) -> bool,
    ) -> Self {
        let meta = *fcb.meta;
        if should_aggregate(&meta) {
            Self::Aggregate {
                meta,
                src_path: fcb.src_path,
            }
        } else {
            Self::Direct {
                meta,
                src_path: fcb.src_path,
                dst_path: fcb.dst_path,
            }
        }
    }
}

pub(crate) fn produce_local_copy_plan<F, A>(
    control_file: PathBuf,
    meta_dir: PathBuf,
    source_dir_base: PathBuf,
    target_dir_base: PathBuf,
    should_aggregate: A,
    on_entry: F,
) -> usize
where
    F: FnMut(CopyPlanEntry) -> bool,
    A: FnMut(&FileMeta) -> bool,
{
    let mapping = EntryMapping::local_to_prefixed_target(source_dir_base, target_dir_base);
    produce_copy_plan(
        control_file,
        meta_dir,
        mapping,
        "local->local",
        should_aggregate,
        on_entry,
    )
}

pub(crate) fn produce_copy_plan<F, A>(
    control_file: PathBuf,
    meta_dir: PathBuf,
    mapping: EntryMapping,
    log_prefix: &str,
    mut should_aggregate: A,
    mut on_entry: F,
) -> usize
where
    F: FnMut(CopyPlanEntry) -> bool,
    A: FnMut(&FileMeta) -> bool,
{
    produce_entries_for_each(
        control_file,
        meta_dir,
        mapping,
        log_prefix,
        |entry| match entry {
            crate::backup::fcb::ControlBlockVarient::DirControlBlock(dcb) => {
                on_entry(CopyPlanEntry::Directory {
                    meta: *dcb.meta,
                    dst_path: dcb.dst_path,
                })
            }
            crate::backup::fcb::ControlBlockVarient::FileControlBlock(fcb) => {
                let plan = FileCopyPlan::from_fcb(fcb, |meta| should_aggregate(meta));
                on_entry(CopyPlanEntry::File(plan))
            }
        },
    )
}
