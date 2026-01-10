use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::path::PathBuf;
use bifrost::scanner::metadata::{DirMeta, FileMeta};
use clap::Parser;
use serde::{Serialize};
use anyhow::{Context, Result};

const TAG_DIR: u8 = 1;
const TAG_FILE: u8 = 2;


#[derive(Serialize, Debug)]
#[serde(tag = "type", rename_all = "lowercase")]
enum OutputRecord {
    Dir(DirMeta),
    File(FileMeta),
}

fn read_record<R: Read>(reader: &mut R) -> Result<Option<OutputRecord>> {
    let mut tag = [0u8; 1];
    if reader.read(&mut tag)? == 0 {
        return Ok(None); // EOF
    }
    let tag = tag[0];

    let mut len_bytes = [0u8; 4];
    reader.read_exact(&mut len_bytes)?;
    let len = u32::from_le_bytes(len_bytes) as usize;

    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;

    match tag {
        TAG_DIR => {
            let dir: DirMeta = bincode::deserialize(&payload)?;
            Ok(Some(OutputRecord::Dir(dir)))
        }
        TAG_FILE => {
            let file: FileMeta = bincode::deserialize(&payload)?;
            Ok(Some(OutputRecord::File(file)))
        }
        _ => {
            eprintln!("Warning: skipping unknown tag {}", tag);
            Ok(None)
        }
    }
}

#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// Input meta .dat file path
    #[arg(short, long)]
    input: PathBuf,

    /// Output format: 'json' or 'csv'
    #[arg(short, long, default_value = "json")]
    format: String,

    /// Output file (stdout if not specified)
    #[arg(short, long)]
    output: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let file = File::open(&cli.input)
        .with_context(|| format!("Cannot open {}", cli.input.display()))?;
    let mut reader = BufReader::new(file);

    let mut output: Box<dyn Write> = match &cli.output {
        Some(p) => Box::new(File::create(p)?),
        None => Box::new(io::stdout()),
    };

    match cli.format.as_str() {
        "json" => {
            let mut first = true;
            writeln!(output, "[")?;
            loop {
                match read_record(&mut reader) {
                    Ok(Some(record)) => {
                        if !first {
                            writeln!(output, ",")?;
                        }
                        let json = serde_json::to_string_pretty(&record)?;
                        write!(output, "{}", json)?;
                        first = false;
                    }
                    Ok(None) => break,
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        break;
                    }
                }
            }
            writeln!(output, "\n]")?;
        }

        "csv" => {
            let mut wtr = csv::Writer::from_writer(output);
            wtr.write_record(&[
                "type", "name", "id", "size_bytes", "atime", "mtime", "ctime",
            ])?;
            loop {
                match read_record(&mut reader) {
                    Ok(Some(OutputRecord::Dir(dir))) => {
                        wtr.write_record(&[
                            "dir",
                            &dir.common.name,
                            &dir.common.id.to_string(),
                            "0",
                            &dir.common.atime.to_string(),
                            &dir.common.mtime.to_string(),
                            &dir.common.ctime.to_string(),
                        ])?;
                    }
                    Ok(Some(OutputRecord::File(file))) => {
                        wtr.write_record(&[
                            "file",
                            &file.common.name,
                            &file.common.id.to_string(),
                            &file.size.to_string(),
                            &file.common.atime.to_string(),
                            &file.common.mtime.to_string(),
                            &file.common.ctime.to_string(),
                        ])?;
                    }
                    Ok(None) => break,
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        break;
                    }
                }
            }
            wtr.flush()?;
        }

        _ => {
            eprintln!("Error: format must be 'json' or 'csv'");
            std::process::exit(1);
        }
    }

    Ok(())
}