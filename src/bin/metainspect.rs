use anyhow::{bail, Context, Result};
use fpt::scanner::metadata::{
    ControlEntry, ControlFileReader, DeleteControlFileReader, DeleteEntryType, DirCacheEntry,
    DirMeta, FileCacheEntry, FileMeta, FixedSize, HardlinkControlFileReader, HardlinkEntry,
    MtimeControlFileReader,
};
use clap::{ArgGroup, Parser, ValueEnum};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

const TAG_DIR: u8 = 1;
const TAG_FILE: u8 = 2;

#[derive(Parser, Debug)]
#[command(version, about)]
#[command(group(
    ArgGroup::new("typed_input")
        .args(["meta", "dcache", "fcache", "control"])
        .multiple(false),
))]
#[command(group(
    ArgGroup::new("format_flags")
        .args(["json", "csv", "tab"])
        .multiple(false),
))]
struct Cli {
    /// Input file path with automatic type detection.
    #[arg(value_name = "FILE")]
    input: Option<PathBuf>,

    /// Inspect a metadata file like meta_0_0.dat
    #[arg(long, value_name = "FILE")]
    meta: Option<PathBuf>,

    /// Inspect a directory cache file like dcache_0.dat
    #[arg(long, value_name = "FILE")]
    dcache: Option<PathBuf>,

    /// Inspect a file cache file like fcache_0.dat
    #[arg(long, value_name = "FILE")]
    fcache: Option<PathBuf>,

    /// Inspect a control file like `copy_<hash>.control.bin` / `hardlink_<hash>.control.bin`
    #[arg(long, value_name = "FILE")]
    control: Option<PathBuf>,

    /// Output format
    #[arg(long, value_enum, default_value = "tab")]
    format: OutputFormat,

    /// Shortcut for --format json
    #[arg(long, action = clap::ArgAction::SetTrue)]
    json: bool,

    /// Shortcut for --format csv
    #[arg(long, action = clap::ArgAction::SetTrue)]
    csv: bool,

    /// Shortcut for --format tab
    #[arg(long, action = clap::ArgAction::SetTrue)]
    tab: bool,

    /// Optional output file, stdout if omitted
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum OutputFormat {
    Json,
    Csv,
    Tab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InspectInputKind {
    Meta,
    Dcache,
    Fcache,
    Control,
}

#[derive(Debug, Clone, Serialize)]
struct InspectRecord {
    kind: String,
    record_type: String,
    #[serde(flatten)]
    fields: BTreeMap<String, Value>,
}

impl InspectRecord {
    fn new(kind: impl Into<String>, record_type: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            record_type: record_type.into(),
            fields: BTreeMap::new(),
        }
    }

    fn with_string(mut self, key: &str, value: impl Into<String>) -> Self {
        self.fields
            .insert(key.to_string(), Value::String(value.into()));
        self
    }

    fn with_u32(mut self, key: &str, value: u32) -> Self {
        self.fields.insert(key.to_string(), json!(value));
        self
    }

    fn with_u64(mut self, key: &str, value: u64) -> Self {
        self.fields.insert(key.to_string(), json!(value));
        self
    }

    fn with_hex_u32(mut self, key: &str, value: u32) -> Self {
        self.fields
            .insert(key.to_string(), Value::String(format!("{value:08X}")));
        self
    }

    fn with_oct_u32(mut self, key: &str, value: u32) -> Self {
        self.fields
            .insert(key.to_string(), Value::String(format!("{value:o}")));
        self
    }

    fn value(&self, key: &str) -> String {
        match key {
            "kind" => self.kind.clone(),
            "record_type" => self.record_type.clone(),
            _ => value_to_string(self.fields.get(key)),
        }
    }
}

struct InspectResult {
    input_path: PathBuf,
    detected_kind: InspectInputKind,
    records: Vec<InspectRecord>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let format = resolve_format(&cli);
    let (kind, path) = resolve_input(&cli)?;
    let records = match kind {
        InspectInputKind::Meta => inspect_meta_file(&path)?,
        InspectInputKind::Dcache => inspect_dcache_file(&path)?,
        InspectInputKind::Fcache => inspect_fcache_file(&path)?,
        InspectInputKind::Control => inspect_control_file(&path)?,
    };
    let result = InspectResult {
        input_path: path,
        detected_kind: kind,
        records,
    };

    let mut output: Box<dyn Write> = match &cli.output {
        Some(path) => Box::new(File::create(path).with_context(|| format!("create {}", path.display()))?),
        None => Box::new(io::stdout()),
    };

    match format {
        OutputFormat::Json => write_json(&mut output, &result)?,
        OutputFormat::Csv => write_csv(&mut output, &result)?,
        OutputFormat::Tab => write_tab(&mut output, &result)?,
    }

    Ok(())
}

fn resolve_format(cli: &Cli) -> OutputFormat {
    if cli.json {
        OutputFormat::Json
    } else if cli.csv {
        OutputFormat::Csv
    } else if cli.tab {
        OutputFormat::Tab
    } else {
        cli.format
    }
}

fn resolve_input(cli: &Cli) -> Result<(InspectInputKind, PathBuf)> {
    if let Some(path) = &cli.meta {
        return Ok((InspectInputKind::Meta, path.clone()));
    }
    if let Some(path) = &cli.dcache {
        return Ok((InspectInputKind::Dcache, path.clone()));
    }
    if let Some(path) = &cli.fcache {
        return Ok((InspectInputKind::Fcache, path.clone()));
    }
    if let Some(path) = &cli.control {
        return Ok((InspectInputKind::Control, path.clone()));
    }

    let path = cli
        .input
        .clone()
        .context("missing input file; pass FILE or one of --meta/--dcache/--fcache/--control")?;
    let kind = detect_input_kind(&path)?;
    Ok((kind, path))
}

fn detect_input_kind(path: &Path) -> Result<InspectInputKind> {
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if file_name.starts_with("meta_") {
        return Ok(InspectInputKind::Meta);
    }
    if file_name.starts_with("dcache_") {
        return Ok(InspectInputKind::Dcache);
    }
    if file_name.starts_with("fcache_") {
        return Ok(InspectInputKind::Fcache);
    }
    if file_name.ends_with(".control.bin") {
        return Ok(InspectInputKind::Control);
    }

    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut prefix = [0u8; 32];
    let n = file.read(&mut prefix)?;
    if n == 0 {
        bail!("cannot detect empty file type for {}", path.display());
    }

    if prefix.starts_with(b"#FPT_") {
        return Ok(InspectInputKind::Control);
    }

    let metadata = file.metadata()?;
    let len = metadata.len() as usize;
    if len % DirCacheEntry::SIZE == 0 && len != 0 && file_name.contains("dcache") {
        return Ok(InspectInputKind::Dcache);
    }
    if len % FileCacheEntry::SIZE == 0 && len != 0 && file_name.contains("fcache") {
        return Ok(InspectInputKind::Fcache);
    }
    if prefix[0] == TAG_DIR || prefix[0] == TAG_FILE {
        return Ok(InspectInputKind::Meta);
    }

    bail!(
        "cannot detect file type for {}; use --meta/--dcache/--fcache/--control",
        path.display()
    )
}

fn inspect_meta_file(path: &Path) -> Result<Vec<InspectRecord>> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut records = Vec::new();

    loop {
        let mut tag = [0u8; 1];
        if file.read(&mut tag)? == 0 {
            break;
        }

        let mut len_bytes = [0u8; 4];
        file.read_exact(&mut len_bytes)?;
        let len = u32::from_le_bytes(len_bytes) as usize;
        let mut payload = vec![0u8; len];
        file.read_exact(&mut payload)?;

        match tag[0] {
            TAG_DIR => {
                let dir: DirMeta = bincode::deserialize(&payload)?;
                records.push(record_from_dir_meta(dir));
            }
            TAG_FILE => {
                let file_meta: FileMeta = bincode::deserialize(&payload)?;
                records.push(record_from_file_meta(file_meta));
            }
            other => bail!("unknown meta tag {other} in {}", path.display()),
        }
    }

    Ok(records)
}

fn inspect_dcache_file(path: &Path) -> Result<Vec<InspectRecord>> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut records = Vec::new();
    let mut buffer = vec![0u8; DirCacheEntry::SIZE];

    loop {
        match file.read_exact(&mut buffer) {
            Ok(()) => {
                let entry = DirCacheEntry {
                    id: u64::from_le_bytes(buffer[0..8].try_into().unwrap()),
                    hash: u32::from_le_bytes(buffer[8..12].try_into().unwrap()),
                    meta_loc: (
                        u32::from_le_bytes(buffer[12..16].try_into().unwrap()),
                        u32::from_le_bytes(buffer[16..20].try_into().unwrap()),
                    ),
                    files_count: u32::from_le_bytes(buffer[20..24].try_into().unwrap()),
                    fcache_fid: u32::from_le_bytes(buffer[24..28].try_into().unwrap()),
                    fcache_offset: u32::from_le_bytes(buffer[28..32].try_into().unwrap()),
                };
                records.push(
                    InspectRecord::new("dcache", "dir_cache")
                        .with_u64("id", entry.id)
                        .with_hex_u32("hash", entry.hash)
                        .with_u32("meta_fid", entry.meta_loc.0)
                        .with_u32("meta_offset", entry.meta_loc.1)
                        .with_u32("files_count", entry.files_count)
                        .with_u32("fcache_fid", entry.fcache_fid)
                        .with_u32("fcache_offset", entry.fcache_offset),
                );
            }
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(err) => return Err(err.into()),
        }
    }

    Ok(records)
}

fn inspect_fcache_file(path: &Path) -> Result<Vec<InspectRecord>> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut records = Vec::new();
    let mut buffer = vec![0u8; FileCacheEntry::SIZE];

    loop {
        match file.read_exact(&mut buffer) {
            Ok(()) => {
                let entry = FileCacheEntry {
                    id: u64::from_le_bytes(buffer[0..8].try_into().unwrap()),
                    hash: u32::from_le_bytes(buffer[8..12].try_into().unwrap()),
                    meta_loc: (
                        u32::from_le_bytes(buffer[12..16].try_into().unwrap()),
                        u32::from_le_bytes(buffer[16..20].try_into().unwrap()),
                    ),
                };
                records.push(
                    InspectRecord::new("fcache", "file_cache")
                        .with_u64("id", entry.id)
                        .with_hex_u32("hash", entry.hash)
                        .with_u32("meta_fid", entry.meta_loc.0)
                        .with_u32("meta_offset", entry.meta_loc.1),
                );
            }
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(err) => return Err(err.into()),
        }
    }

    Ok(records)
}

fn inspect_control_file(path: &Path) -> Result<Vec<InspectRecord>> {
    let magic = read_control_magic(path)?;
    match magic.as_str() {
        "#FPT_BACKUP_CTRL_FILE" => inspect_copy_control(path),
        "#FPT_DELETE_CTRL_FILE" => inspect_delete_control(path),
        "#FPT_MTIME_CTRL_FILE" => inspect_mtime_control(path),
        "#FPT_HARDLINK_CTRL_FILE" => inspect_hardlink_control(path),
        other => bail!("unsupported control file magic {other} in {}", path.display()),
    }
}

fn inspect_copy_control(path: &Path) -> Result<Vec<InspectRecord>> {
    let reader = ControlFileReader::open(path)?;
    let mut records = Vec::new();
    let mut current_dir: Option<String> = None;
    for entry in reader {
        match entry? {
            ControlEntry::Dir(dir) => {
                current_dir = Some(dir.path.clone());
                records.push(
                    InspectRecord::new("control", "copy_dir")
                        .with_string("type", "DIR")
                        .with_string("path", dir.path)
                        .with_string("diff", format!("{:?}", dir.diff))
                        .with_u32("meta_fid", dir.meta_fid)
                        .with_u32("meta_offset", dir.meta_offset)
                        .with_u32("files_count", dir.files_count),
                );
            }
            ControlEntry::File(file) => {
                let full_path = join_logical_path(current_dir.as_deref().unwrap_or("/"), &file.name);
                records.push(
                    InspectRecord::new("control", "copy_file")
                        .with_string("type", "FILE")
                        .with_string("path", full_path)
                        .with_string("diff", format!("{:?}", file.diff))
                        .with_u32("meta_fid", file.meta_fid)
                        .with_u32("meta_offset", file.meta_offset)
                        .with_string("files_count", "N/A"),
                );
            }
        }
    }
    Ok(records)
}

fn inspect_delete_control(path: &Path) -> Result<Vec<InspectRecord>> {
    let reader = DeleteControlFileReader::open(path)?;
    let mut records = Vec::new();
    for entry in reader {
        let entry = entry?;
        records.push(
            InspectRecord::new(
                "control",
                match entry.entry_type {
                    DeleteEntryType::Dir => "delete_dir",
                    DeleteEntryType::File => "delete_file",
                },
            )
            .with_string(
                "type",
                match entry.entry_type {
                    DeleteEntryType::Dir => "DIR",
                    DeleteEntryType::File => "FILE",
                },
            )
            .with_string("path", entry.path),
        );
    }
    Ok(records)
}

fn inspect_mtime_control(path: &Path) -> Result<Vec<InspectRecord>> {
    let reader = MtimeControlFileReader::open(path)?;
    let mut records = Vec::new();
    for entry in reader {
        let entry = entry?;
        records.push(
            InspectRecord::new("control", "mtime_dir")
                .with_string("type", "DIR")
                .with_string("path", entry.path)
                .with_oct_u32("mode", entry.mode)
                .with_u32("uid", entry.uid)
                .with_u32("gid", entry.gid)
                .with_u64("atime", entry.atime)
                .with_u64("mtime", entry.mtime),
        );
    }
    Ok(records)
}

fn inspect_hardlink_control(path: &Path) -> Result<Vec<InspectRecord>> {
    let reader = HardlinkControlFileReader::open(path)?;
    let mut records = Vec::new();
    for entry in reader {
        match entry? {
            HardlinkEntry::Inode(inode) => records.push(
                InspectRecord::new("control", "hardlink_inode")
                    .with_string("type", "INODE")
                    .with_u64("inode", inode.inode)
                    .with_u64("device", inode.device)
                    .with_u32("link_count", inode.link_count),
            ),
            HardlinkEntry::File(file) => records.push(
                InspectRecord::new("control", "hardlink_file")
                    .with_string("type", "FILE")
                    .with_string("path", file.path)
                    .with_u32("meta_fid", file.meta_fid)
                    .with_u32("meta_offset", file.meta_offset),
            ),
        }
    }
    Ok(records)
}

fn read_control_magic(path: &Path) -> Result<String> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut first_line = String::new();
    reader.read_line(&mut first_line)?;
    Ok(first_line
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string())
}

fn record_from_dir_meta(dir: DirMeta) -> InspectRecord {
    InspectRecord::new("meta", "dir_meta")
        .with_string("name", dir.common.name)
        .with_string("path", dir.path)
        .with_u64("id", dir.common.id)
        .with_u64("device", dir.common.devno)
        .with_oct_u32("mode", dir.common.mode)
        .with_u64("atime", dir.common.atime as u64)
        .with_u64("mtime", dir.common.mtime as u64)
        .with_u64("ctime", dir.common.ctime as u64)
}

fn record_from_file_meta(file: FileMeta) -> InspectRecord {
    InspectRecord::new("meta", "file_meta")
        .with_string("name", file.common.name)
        .with_u64("id", file.common.id)
        .with_u64("device", file.common.devno)
        .with_u64("size", file.size)
        .with_u64("link_count", file.links)
        .with_oct_u32("mode", file.common.mode)
        .with_u64("atime", file.common.atime as u64)
        .with_u64("mtime", file.common.mtime as u64)
        .with_u64("ctime", file.common.ctime as u64)
}

fn write_json(output: &mut dyn Write, result: &InspectResult) -> Result<()> {
    let groups = group_by_record_type(&result.records);
    let groups_json: Vec<Value> = groups
        .into_iter()
        .map(|((kind, record_type), records)| {
            let records_json: Vec<Value> = records
                .into_iter()
                .map(|record| {
                    let mut map = Map::new();
                    for (k, v) in record.fields {
                        map.insert(k, v);
                    }
                    Value::Object(map)
                })
                .collect();
            json!({
                "kind": kind,
                "record_type": record_type,
                "records": records_json
            })
        })
        .collect();
    let payload = json!({
        "input": result.input_path,
        "detected_type": inspect_kind_name(result.detected_kind),
        "record_count": result.records.len(),
        "groups": groups_json,
    });
    serde_json::to_writer_pretty(&mut *output, &payload)?;
    writeln!(output)?;
    Ok(())
}

fn write_csv(output: &mut dyn Write, result: &InspectResult) -> Result<()> {
    let columns = union_columns(&result.records);
    let mut writer = csv::Writer::from_writer(output);
    writer.write_record(&columns)?;
    for record in &result.records {
        let row: Vec<String> = columns.iter().map(|column| record.value(column)).collect();
        writer.write_record(row)?;
    }
    writer.flush()?;
    Ok(())
}

fn write_tab(output: &mut dyn Write, result: &InspectResult) -> Result<()> {
    writeln!(output, "Input        : {}", result.input_path.display())?;
    writeln!(output, "Detected type: {}", inspect_kind_name(result.detected_kind))?;
    writeln!(output, "Records      : {}", result.records.len())?;
    writeln!(output)?;

    if result.detected_kind == InspectInputKind::Control {
        let columns = control_tab_columns(&result.records);
        let widths = column_widths(&columns, &result.records);
        write_tab_table(output, &columns, &widths, &result.records)?;
        return Ok(());
    }

    let groups = group_by_record_type(&result.records);
    let total_groups = groups.len();
    for (idx, ((kind, record_type), group_records)) in groups.into_iter().enumerate() {
        writeln!(output, "[{kind}:{record_type}]")?;
        let columns = group_columns(&group_records);
        let widths = column_widths(&columns, &group_records);
        write_tab_table(output, &columns, &widths, &group_records)?;

        if idx + 1 != total_groups {
            writeln!(output)?;
        }
    }
    Ok(())
}

fn group_by_record_type(records: &[InspectRecord]) -> Vec<((String, String), Vec<InspectRecord>)> {
    let mut groups: BTreeMap<(String, String), Vec<InspectRecord>> = BTreeMap::new();
    for record in records {
        groups
            .entry((record.kind.clone(), record.record_type.clone()))
            .or_default()
            .push(record.clone());
    }
    groups.into_iter().collect()
}

fn union_columns(records: &[InspectRecord]) -> Vec<String> {
    let mut cols = vec!["kind".to_string(), "record_type".to_string()];
    let mut seen = BTreeSet::new();
    for record in records {
        for key in record.fields.keys() {
            seen.insert(key.clone());
        }
    }
    cols.extend(seen);
    cols
}

fn group_columns(records: &[InspectRecord]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    for record in records {
        for key in record.fields.keys() {
            seen.insert(key.clone());
        }
    }
    seen.into_iter().collect()
}

fn control_tab_columns(records: &[InspectRecord]) -> Vec<String> {
    let preferred = ["type", "path", "diff", "meta_fid", "meta_offset", "files_count"];
    let mut columns = Vec::new();
    for column in preferred {
        if records.iter().any(|record| record.fields.contains_key(column)) {
            columns.push(column.to_string());
        }
    }
    let mut seen: BTreeSet<String> = columns.iter().cloned().collect();
    for record in records {
        for key in record.fields.keys() {
            if seen.insert(key.clone()) {
                columns.push(key.clone());
            }
        }
    }
    columns
}

fn column_widths(columns: &[String], records: &[InspectRecord]) -> Vec<usize> {
    columns
        .iter()
        .map(|column| {
            let mut width = column.chars().count();
            for record in records {
                width = width.max(sanitize_tab_value(&record.value(column)).chars().count());
            }
            width
        })
        .collect()
}

fn write_tab_table(
    output: &mut dyn Write,
    columns: &[String],
    widths: &[usize],
    records: &[InspectRecord],
) -> Result<()> {
    for (i, column) in columns.iter().enumerate() {
        if i > 0 {
            write!(output, "  ")?;
        }
        write!(output, "{column:<width$}", width = widths[i])?;
    }
    writeln!(output)?;

    for (i, width) in widths.iter().enumerate() {
        if i > 0 {
            write!(output, "  ")?;
        }
        write!(output, "{:-<width$}", "", width = *width)?;
    }
    writeln!(output)?;

    for record in records {
        for (i, column) in columns.iter().enumerate() {
            if i > 0 {
                write!(output, "  ")?;
            }
            let value = sanitize_tab_value(&record.value(column));
            write!(output, "{value:<width$}", width = widths[i])?;
        }
        writeln!(output)?;
    }

    Ok(())
}

fn sanitize_tab_value(value: &str) -> String {
    value.replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

fn inspect_kind_name(kind: InspectInputKind) -> &'static str {
    match kind {
        InspectInputKind::Meta => "meta",
        InspectInputKind::Dcache => "dcache",
        InspectInputKind::Fcache => "fcache",
        InspectInputKind::Control => "control",
    }
}

fn join_logical_path(dir: &str, name: &str) -> String {
    if dir == "/" {
        format!("/{}", name)
    } else {
        format!("{}/{}", dir.trim_end_matches('/'), name)
    }
}

fn value_to_string(value: Option<&Value>) -> String {
    match value {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(v)) => v.to_string(),
        Some(Value::Null) => String::new(),
        Some(other) => other.to_string(),
    }
}
