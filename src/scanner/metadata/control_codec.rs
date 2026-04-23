use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const CONTROL_HEADER_SIZE: usize = 4096;
pub const CONTROL_VERSION_V3: u32 = 3;

#[derive(Debug, Clone, PartialEq)]
pub struct ControlFileHeader {
    pub version: u32,
    pub file_count: u64,
    pub dir_count: u64,
    pub inode_count: u64,
    pub record_count: u64,
    pub time: u64,
    pub source_kind: String,
    pub source_root: String,
    pub header_size: u32,
}

impl Default for ControlFileHeader {
    fn default() -> Self {
        Self {
            version: CONTROL_VERSION_V3,
            file_count: 0,
            dir_count: 0,
            inode_count: 0,
            record_count: 0,
            time: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            source_kind: "local".to_string(),
            source_root: "/".to_string(),
            header_size: CONTROL_HEADER_SIZE as u32,
        }
    }
}

pub(crate) fn encode_text_field(value: &str) -> io::Result<String> {
    serde_json::to_string(value).map_err(io::Error::other)
}

pub(crate) fn decode_text_field(value: &str) -> io::Result<String> {
    serde_json::from_str(value).map_err(io::Error::other)
}

pub(crate) fn create_record_writer<P: AsRef<Path>>(
    path: P,
    magic: &str,
    header: &ControlFileHeader,
) -> io::Result<BufWriter<File>> {
    let mut file = File::create(path)?;
    write_header_block(&mut file, magic, header)?;
    file.seek(SeekFrom::Start(CONTROL_HEADER_SIZE as u64))?;
    Ok(BufWriter::new(file))
}

pub(crate) fn finish_record_writer(
    writer: BufWriter<File>,
    magic: &str,
    header: &ControlFileHeader,
) -> io::Result<()> {
    let mut file = writer.into_inner().map_err(|e| e.into_error())?;
    file.flush()?;
    file.seek(SeekFrom::Start(0))?;
    write_header_block(&mut file, magic, header)?;
    file.flush()
}

pub(crate) fn open_record_reader<P: AsRef<Path>>(
    path: P,
    expected_magic: &str,
) -> io::Result<(BufReader<File>, ControlFileHeader)> {
    let mut file = File::open(path)?;
    let header = read_header_block(&mut file, expected_magic)?;
    file.seek(SeekFrom::Start(header.header_size as u64))?;
    Ok((BufReader::new(file), header))
}

pub(crate) fn write_record(writer: &mut BufWriter<File>, payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "record too large"))?;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(payload)
}

pub(crate) fn read_record(reader: &mut BufReader<File>) -> io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    let mut read = 0usize;
    while read < len_buf.len() {
        let n = reader.read(&mut len_buf[read..])?;
        if n == 0 {
            if read == 0 {
                return Ok(None);
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated record length",
            ));
        }
        read += n;
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;
    Ok(Some(payload))
}

pub(crate) fn put_u8(buf: &mut Vec<u8>, value: u8) {
    buf.push(value);
}

pub(crate) fn put_u16(buf: &mut Vec<u8>, value: u16) {
    buf.extend_from_slice(&value.to_le_bytes());
}

pub(crate) fn put_u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_le_bytes());
}

pub(crate) fn put_u64(buf: &mut Vec<u8>, value: u64) {
    buf.extend_from_slice(&value.to_le_bytes());
}

pub(crate) fn put_bytes(buf: &mut Vec<u8>, value: &[u8]) {
    buf.extend_from_slice(value);
}

pub(crate) fn take_u8(buf: &[u8], cursor: &mut usize) -> io::Result<u8> {
    if *cursor + 1 > buf.len() {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "missing u8"));
    }
    let value = buf[*cursor];
    *cursor += 1;
    Ok(value)
}

pub(crate) fn take_u16(buf: &[u8], cursor: &mut usize) -> io::Result<u16> {
    if *cursor + 2 > buf.len() {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "missing u16"));
    }
    let mut bytes = [0u8; 2];
    bytes.copy_from_slice(&buf[*cursor..*cursor + 2]);
    *cursor += 2;
    Ok(u16::from_le_bytes(bytes))
}

pub(crate) fn take_u32(buf: &[u8], cursor: &mut usize) -> io::Result<u32> {
    if *cursor + 4 > buf.len() {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "missing u32"));
    }
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&buf[*cursor..*cursor + 4]);
    *cursor += 4;
    Ok(u32::from_le_bytes(bytes))
}

pub(crate) fn take_u64(buf: &[u8], cursor: &mut usize) -> io::Result<u64> {
    if *cursor + 8 > buf.len() {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "missing u64"));
    }
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&buf[*cursor..*cursor + 8]);
    *cursor += 8;
    Ok(u64::from_le_bytes(bytes))
}

pub(crate) fn take_bytes<'a>(buf: &'a [u8], cursor: &mut usize, len: usize) -> io::Result<&'a [u8]> {
    if *cursor + len > buf.len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "payload shorter than declared length",
        ));
    }
    let bytes = &buf[*cursor..*cursor + len];
    *cursor += len;
    Ok(bytes)
}

fn write_header_block(file: &mut File, magic: &str, header: &ControlFileHeader) -> io::Result<()> {
    let mut text = String::new();
    text.push_str(magic);
    text.push(' ');
    text.push('V');
    text.push_str(&header.version.to_string());
    text.push('\n');
    text.push_str(&format!("HEADER_SIZE={}\n", header.header_size));
    text.push_str(&format!("FILE_COUNT={}\n", header.file_count));
    text.push_str(&format!("DIR_COUNT={}\n", header.dir_count));
    text.push_str(&format!("INODE_COUNT={}\n", header.inode_count));
    text.push_str(&format!("RECORD_COUNT={}\n", header.record_count));
    text.push_str(&format!("TIME={}\n", header.time));
    text.push_str(&format!(
        "SOURCE_KIND={}\n",
        encode_text_field(&header.source_kind)?
    ));
    text.push_str(&format!(
        "SOURCE_ROOT={}\n",
        encode_text_field(&header.source_root)?
    ));
    text.push_str("END\n");

    let bytes = text.as_bytes();
    if bytes.len() > CONTROL_HEADER_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "header exceeds fixed header size",
        ));
    }

    let mut block = vec![0u8; CONTROL_HEADER_SIZE];
    block[..bytes.len()].copy_from_slice(bytes);
    file.write_all(&block)
}

fn read_header_block(file: &mut File, expected_magic: &str) -> io::Result<ControlFileHeader> {
    let mut block = vec![0u8; CONTROL_HEADER_SIZE];
    file.read_exact(&mut block)?;
    let content = match block.iter().position(|b| *b == 0) {
        Some(end) => &block[..end],
        None => &block[..],
    };
    let text = std::str::from_utf8(content)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let mut lines = text.lines();
    let first = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing header line"))?;
    if !first.starts_with(expected_magic) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid control file magic",
        ));
    }
    let version = first
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.strip_prefix('V'))
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(CONTROL_VERSION_V3);

    let mut kv = HashMap::new();
    for line in lines {
        if line == "END" {
            break;
        }
        if let Some((k, v)) = line.split_once('=') {
            kv.insert(k.trim().to_string(), v.trim().to_string());
        }
    }

    Ok(ControlFileHeader {
        version,
        header_size: kv
            .get("HEADER_SIZE")
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(CONTROL_HEADER_SIZE as u32),
        file_count: kv
            .get("FILE_COUNT")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0),
        dir_count: kv
            .get("DIR_COUNT")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0),
        inode_count: kv
            .get("INODE_COUNT")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0),
        record_count: kv
            .get("RECORD_COUNT")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0),
        time: kv
            .get("TIME")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0),
        source_kind: kv
            .get("SOURCE_KIND")
            .map(|v| decode_text_field(v))
            .transpose()?
            .unwrap_or_else(|| "local".to_string()),
        source_root: kv
            .get("SOURCE_ROOT")
            .map(|v| decode_text_field(v))
            .transpose()?
            .unwrap_or_else(|| "/".to_string()),
    })
}

