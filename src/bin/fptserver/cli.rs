use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(author, version, about = "Bifrost task RPC server", long_about = None)]
pub(crate) struct Cli {
    #[arg(long, default_value = "127.0.0.1")]
    pub(crate) host: String,

    #[arg(long, default_value_t = 3000)]
    pub(crate) port: u16,

    #[arg(long, default_value = "/tmp/fptserver")]
    pub(crate) runtime_dir: PathBuf,

    #[arg(long, default_value_t = 1)]
    pub(crate) max_scanners_count: usize,

    #[arg(long, default_value_t = 4)]
    pub(crate) max_subtasks_count: usize,

    #[command(subcommand)]
    pub(crate) command: Option<CommandMode>,
}

#[derive(Subcommand, Debug)]
pub(crate) enum CommandMode {
    #[command(hide = true)]
    Worker(WorkerArgs),
}

#[derive(Parser, Debug)]
pub(crate) struct WorkerArgs {
    #[arg(long)]
    pub(crate) task_file: PathBuf,

    #[arg(long)]
    pub(crate) status_file: PathBuf,
}
