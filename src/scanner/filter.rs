use std::path::{Path, PathBuf};

use super::options::ControlPathOption;

#[derive(Debug, Clone, Default)]
pub struct ScanPathFilterSet {
    include_dirs: Vec<PathPattern>,
    include_files: Vec<PathPattern>,
    exclude_dirs: Vec<PathPattern>,
    exclude_files: Vec<PathPattern>,
    has_includes: bool,
}

#[derive(Debug, Clone)]
struct PathPattern {
    segments: Vec<SegmentPattern>,
}

#[derive(Debug, Clone)]
struct SegmentPattern {
    parts: Vec<String>,
    leading_wildcard: bool,
    trailing_wildcard: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanEntryKind {
    Directory,
    File,
}

impl ScanPathFilterSet {
    pub fn is_empty(&self) -> bool {
        !self.has_includes && self.exclude_dirs.is_empty() && self.exclude_files.is_empty()
    }

    pub fn compile(
        include_dirs: Vec<String>,
        include_files: Vec<String>,
        exclude_dirs: Vec<String>,
        exclude_files: Vec<String>,
    ) -> Result<Option<Self>, String> {
        let has_includes = !(include_dirs.is_empty() && include_files.is_empty());
        let filter = Self {
            include_dirs: compile_patterns(include_dirs)?,
            include_files: compile_patterns(include_files)?,
            exclude_dirs: compile_patterns(exclude_dirs)?,
            exclude_files: compile_patterns(exclude_files)?,
            has_includes,
        };
        if filter.is_empty() {
            Ok(None)
        } else {
            Ok(Some(filter))
        }
    }

    pub fn should_emit_dir(&self, logical_path: &str) -> bool {
        if self.matches_excluded_dir(logical_path) {
            return false;
        }
        if !self.has_includes {
            return true;
        }
        self.dir_is_included_subtree(logical_path) || self.dir_can_reach_include(logical_path)
    }

    pub fn should_descend_dir(&self, logical_path: &str) -> bool {
        if self.matches_excluded_dir(logical_path) {
            return false;
        }
        if !self.has_includes {
            return true;
        }
        self.dir_is_included_subtree(logical_path) || self.dir_can_reach_include(logical_path)
    }

    pub fn should_emit_file(&self, logical_path: &str) -> bool {
        if self.matches_excluded_dir(logical_path) || self.matches_excluded_file(logical_path) {
            return false;
        }
        if !self.has_includes {
            return true;
        }
        self.file_in_included_dir_subtree(logical_path) || self.file_matches_include(logical_path)
    }

    fn matches_excluded_dir(&self, logical_path: &str) -> bool {
        let segs = split_logical_path(logical_path);
        self.exclude_dirs
            .iter()
            .any(|pattern| pattern.matches_dir_subtree(&segs))
    }

    fn matches_excluded_file(&self, logical_path: &str) -> bool {
        let segs = split_logical_path(logical_path);
        self.exclude_files
            .iter()
            .any(|pattern| pattern.matches_exact(&segs))
    }

    fn dir_is_included_subtree(&self, logical_path: &str) -> bool {
        let segs = split_logical_path(logical_path);
        self.include_dirs
            .iter()
            .any(|pattern| pattern.matches_dir_subtree(&segs))
    }

    fn file_in_included_dir_subtree(&self, logical_path: &str) -> bool {
        let segs = split_logical_path(logical_path);
        self.include_dirs
            .iter()
            .any(|pattern| pattern.matches_file_under_dir_subtree(&segs))
    }

    fn file_matches_include(&self, logical_path: &str) -> bool {
        let segs = split_logical_path(logical_path);
        self.include_files
            .iter()
            .any(|pattern| pattern.matches_exact(&segs))
    }

    fn dir_can_reach_include(&self, logical_path: &str) -> bool {
        let segs = split_logical_path(logical_path);
        self.include_dirs
            .iter()
            .any(|pattern| pattern.can_reach_dir_match(&segs))
            || self
                .include_files
                .iter()
                .any(|pattern| pattern.can_reach_file_match(&segs))
    }
}

impl PathPattern {
    fn matches_exact(&self, path_segments: &[&str]) -> bool {
        self.segments.len() == path_segments.len()
            && self
                .segments
                .iter()
                .zip(path_segments.iter())
                .all(|(pattern, segment)| pattern.matches(segment))
    }

    fn matches_dir_subtree(&self, path_segments: &[&str]) -> bool {
        path_segments.len() >= self.segments.len()
            && self
                .segments
                .iter()
                .zip(path_segments.iter())
                .all(|(pattern, segment)| pattern.matches(segment))
    }

    fn matches_file_under_dir_subtree(&self, path_segments: &[&str]) -> bool {
        self.matches_dir_subtree(path_segments)
    }

    fn can_reach_dir_match(&self, dir_segments: &[&str]) -> bool {
        if dir_segments.len() <= self.segments.len() {
            return self
                .segments
                .iter()
                .take(dir_segments.len())
                .zip(dir_segments.iter())
                .all(|(pattern, segment)| pattern.matches(segment));
        }
        self.matches_dir_subtree(dir_segments)
    }

    fn can_reach_file_match(&self, dir_segments: &[&str]) -> bool {
        if self.segments.is_empty() {
            return false;
        }
        let parent_len = self.segments.len() - 1;
        if dir_segments.len() > parent_len {
            return false;
        }
        self.segments
            .iter()
            .take(dir_segments.len())
            .zip(dir_segments.iter())
            .all(|(pattern, segment)| pattern.matches(segment))
    }
}

impl SegmentPattern {
    fn new(raw: &str) -> Self {
        let leading_wildcard = raw.starts_with('*');
        let trailing_wildcard = raw.ends_with('*');
        let parts = raw
            .split('*')
            .filter(|part| !part.is_empty())
            .map(|part| part.to_string())
            .collect();
        Self {
            parts,
            leading_wildcard,
            trailing_wildcard,
        }
    }

    fn matches(&self, candidate: &str) -> bool {
        if self.parts.is_empty() {
            return true;
        }

        let mut pos = 0usize;
        for (idx, part) in self.parts.iter().enumerate() {
            if idx == 0 && !self.leading_wildcard {
                if !candidate[pos..].starts_with(part) {
                    return false;
                }
                pos += part.len();
                continue;
            }

            match candidate[pos..].find(part) {
                Some(found) => pos += found + part.len(),
                None => return false,
            }
        }

        if !self.trailing_wildcard {
            if let Some(last) = self.parts.last() {
                return candidate.ends_with(last);
            }
        }
        true
    }
}

fn compile_patterns(patterns: Vec<String>) -> Result<Vec<PathPattern>, String> {
    patterns
        .into_iter()
        .map(|pattern| compile_pattern(&pattern))
        .collect()
}

fn compile_pattern(pattern: &str) -> Result<PathPattern, String> {
    let normalized = crate::path_util::normalize_logical(pattern);
    let segments = split_logical_path(&normalized)
        .into_iter()
        .map(SegmentPattern::new)
        .collect::<Vec<_>>();
    if normalized != "/" && segments.is_empty() {
        return Err(format!("invalid empty filter pattern: {pattern}"));
    }
    Ok(PathPattern { segments })
}

pub fn logical_path_from_physical(control: &ControlPathOption, physical_path: &Path) -> String {
    let logical_root = PathBuf::from(&control.source_root);
    if !physical_path.starts_with(&control.physical_base)
        && physical_path.starts_with(&logical_root)
    {
        return crate::path_util::normalize_logical(&physical_path.to_string_lossy());
    }
    let rel = physical_path
        .strip_prefix(&control.physical_base)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| physical_path.to_path_buf());
    crate::path_util::normalize_logical(&format!(
        "/{}",
        rel.to_string_lossy().trim_start_matches('/')
    ))
}

fn split_logical_path(path: &str) -> Vec<&str> {
    path.trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_wildcards_match_within_one_path_component() {
        let pattern = SegmentPattern::new("*.txt");
        assert!(pattern.matches("a.txt"));
        assert!(!pattern.matches("a.txt.bak"));
    }

    #[test]
    fn dir_include_selects_subtree_and_ancestors() {
        let filters =
            ScanPathFilterSet::compile(vec!["/dir/*/*/dir1".to_string()], vec![], vec![], vec![])
                .unwrap()
                .unwrap();
        assert!(filters.should_descend_dir("/"));
        assert!(filters.should_descend_dir("/dir"));
        assert!(filters.should_descend_dir("/dir/a"));
        assert!(filters.should_descend_dir("/dir/a/b"));
        assert!(filters.should_emit_dir("/dir/a/b/dir1"));
        assert!(filters.should_descend_dir("/dir/a/b/dir1/x"));
    }

    #[test]
    fn file_filters_apply_exactly() {
        let filters = ScanPathFilterSet::compile(
            vec![],
            vec!["/dir/*/dir1/*.txt".to_string()],
            vec![],
            vec!["/dir/*/dir1/1.txt".to_string()],
        )
        .unwrap()
        .unwrap();
        assert!(filters.should_emit_file("/dir/a/dir1/2.txt"));
        assert!(!filters.should_emit_file("/dir/a/dir1/1.txt"));
        assert!(!filters.should_emit_file("/dir/a/dir1/2.log"));
    }

    #[test]
    fn exclude_dir_prunes_subtree() {
        let filters = ScanPathFilterSet::compile(
            vec!["/dir/*/dir1".to_string()],
            vec![],
            vec!["/dir/*/dir1/dir1".to_string()],
            vec![],
        )
        .unwrap()
        .unwrap();
        assert!(filters.should_descend_dir("/dir/a/dir1"));
        assert!(!filters.should_descend_dir("/dir/a/dir1/dir1"));
        assert!(!filters.should_emit_dir("/dir/a/dir1/dir1/x"));
    }
}
