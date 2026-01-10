use bifrost::scanner::metadata::{DirCacheEntry, FileCacheEntry};
use clap::Parser;
use csv::WriterBuilder;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the cache file (e.g., fcache_0.dat or dcache_0.dat)
    #[arg(short, long)]
    input: PathBuf,

    /// Type of cache file: 'fcache' or 'dcache'
    #[arg(short, long, value_parser = ["fcache", "dcache"])]
    r#type: String,

    /// Output format: 'csv' or 'json'
    #[arg(short, long, value_parser = ["csv", "json"], default_value = "csv")]
    format: String,
}

const FILE_CACHE_ENTRY_SIZE: usize = 20;//std::mem::size_of::<FileCacheEntry>();
const DIR_CACHE_ENTRY_SIZE: usize = 32;//std::mem::size_of::<DirCacheEntry>();

fn read_fcache_entries(file: &mut File) -> io::Result<Vec<FileCacheEntry>> {
    let mut entries = Vec::new();
    let mut buffer = vec![0u8; FILE_CACHE_ENTRY_SIZE];

    loop {
        match file.read_exact(&mut buffer) {
            Ok(()) => {
                // Safe because #[repr(C)] and we control layout
                let entry = FileCacheEntry {
                    id: u64::from_le_bytes(buffer[0..8].try_into().unwrap()),
                    hash: u32::from_le_bytes(buffer[8..12].try_into().unwrap()),
                    meta_loc: (u32::from_le_bytes(buffer[12..16].try_into().unwrap()), u32::from_le_bytes(buffer[16..20].try_into().unwrap())),
                };
                entries.push(entry);
            }
            Err(ref e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
    }
    Ok(entries)
}

fn read_dcache_entries(file: &mut File) -> io::Result<Vec<DirCacheEntry>> {
    let mut entries = Vec::new();
    let mut buffer = vec![0u8; DIR_CACHE_ENTRY_SIZE];

    loop {
        match file.read_exact(&mut buffer) {
            Ok(()) => {
                // Safe because #[repr(C)] and we control layout
                let entry: DirCacheEntry = DirCacheEntry {
                    id: u64::from_le_bytes(buffer[0..8].try_into().unwrap()),
                    hash: u32::from_le_bytes(buffer[8..12].try_into().unwrap()),
                    meta_loc: (u32::from_le_bytes(buffer[12..16].try_into().unwrap()), u32::from_le_bytes(buffer[16..20].try_into().unwrap())),
                    files_count: u32::from_le_bytes(buffer[20..24].try_into().unwrap()),
                    fcache_fid: u32::from_le_bytes(buffer[24..28].try_into().unwrap()),
                    fcache_offset: u32::from_le_bytes(buffer[28..32].try_into().unwrap()),
                };
                entries.push(entry);
            }
            Err(ref e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
    }
    Ok(entries)
}

fn main() -> io::Result<()> {
    let args = Args::parse();

    let mut file = File::open(&args.input)?;
    file.seek(SeekFrom::Start(0))?;

    match args.r#type.as_str() {
        "fcache" => {
            let entries = read_fcache_entries(&mut file)?;
            match args.format.as_str() {
                "json" => {
                    println!("{}", serde_json::to_string_pretty(&entries).unwrap());
                }
                "csv" | _ => {
                    let mut wtr = WriterBuilder::new().has_headers(true).from_writer(io::stdout());
                    for entry in entries {
                        wtr.serialize(entry)?;
                    }
                    wtr.flush()?;
                }
            }
        }
        "dcache" => {
            let entries = read_dcache_entries(&mut file)?;
            match args.format.as_str() {
                "json" => {
                    println!("{}", serde_json::to_string_pretty(&entries).unwrap());
                }
                "csv" | _ => {
                    let mut wtr = WriterBuilder::new().has_headers(true).from_writer(io::stdout());
                    for entry in entries {
                        wtr.serialize(entry)?;
                    }
                    wtr.flush()?;
                }
            }
        }
        _ => {
            eprintln!("Error: type must be 'fcache' or 'dcache'");
            std::process::exit(1);
        }
    }

    Ok(())
}