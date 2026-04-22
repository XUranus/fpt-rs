use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

pub fn copy_stream<R: Read, W: Write>(
    src: &mut R,
    dst: &mut W,
    buffer: &mut [u8],
) -> io::Result<u64> {
    let mut copied = 0u64;
    loop {
        let read_n = src.read(buffer)?;
        if read_n == 0 {
            break;
        }
        dst.write_all(&buffer[..read_n])?;
        copied += read_n as u64;
    }
    Ok(copied)
}

pub fn copy_exact_file_to_writer(
    src_path: &Path,
    expected_size: u64,
    dst: &mut impl Write,
    buffer: &mut [u8],
) -> io::Result<u64> {
    let mut src = File::open(src_path)?;
    let mut remaining = expected_size;
    let mut copied = 0u64;
    while remaining > 0 {
        let max_read = buffer.len().min(remaining as usize);
        let read_n = src.read(&mut buffer[..max_read])?;
        if read_n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("short read while copying {}", src_path.display()),
            ));
        }
        dst.write_all(&buffer[..read_n])?;
        remaining -= read_n as u64;
        copied += read_n as u64;
    }
    Ok(copied)
}
