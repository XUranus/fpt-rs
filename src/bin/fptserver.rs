use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use fpt::backup::{
    aggregate::{AggregateConfig, AggregateLayout},
    RestorePolicy,
};
use fpt::failure::{failure_file_path, FailureLogConfig, FailureLogFormat, RetryPolicy};
use fpt::frame::{
    BackupJobConfig, BackupRestoreJob, DataLocation, FileBackupJob, FileRestoreJob,
    RestoreJobConfig, ScanConfig, TempRepoConfig,
};
use fpt::scanner::{ScanOption, ScanPathFilterSet, Scanner};
use chrono::Utc;
use clap::{Parser, ValueEnum};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path as FsPath, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::RwLock;
use uuid::Uuid;

#[path = "fptserver/api.rs"]
mod fptserver_api;
#[path = "fptserver/cli.rs"]
mod fptserver_cli;

use fptserver_api::ApiError;
use fptserver_cli::{Cli, CommandMode, WorkerArgs};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TaskKind {
    Scan,
    Backup,
    Restore,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TaskState {
    Created,
    Starting,
    Running,
    Stopping,
    Stopped,
    Completed,
    Failed,
    Killed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum FailureLogFormatArg {
    Csv,
    Json,
    Xml,
}

impl From<FailureLogFormatArg> for FailureLogFormat {
    fn from(value: FailureLogFormatArg) -> Self {
        match value {
            FailureLogFormatArg::Csv => FailureLogFormat::Csv,
            FailureLogFormatArg::Json => FailureLogFormat::Json,
            FailureLogFormatArg::Xml => FailureLogFormat::Xml,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum AggregateLayoutArg {
    DirLevel,
    Shard,
}

impl From<AggregateLayoutArg> for AggregateLayout {
    fn from(value: AggregateLayoutArg) -> Self {
        match value {
            AggregateLayoutArg::DirLevel => AggregateLayout::DirLevel,
            AggregateLayoutArg::Shard => AggregateLayout::Shard,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum BackupFormatArg {
    Common,
    Aggregated,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum RestorePolicyArg {
    Skip,
    Replace,
}

impl From<RestorePolicyArg> for RestorePolicy {
    fn from(value: RestorePolicyArg) -> Self {
        match value {
            RestorePolicyArg::Skip => RestorePolicy::Skip,
            RestorePolicyArg::Replace => RestorePolicy::Replace,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RetryPolicySpec {
    #[serde(default = "default_operation_retries")]
    operation_retries: u32,
    #[serde(default = "default_retry_delay_ms")]
    retry_delay_ms: u64,
    #[serde(default = "default_retry_backoff")]
    retry_backoff: f64,
    #[serde(default = "default_retry_max_delay_ms")]
    retry_max_delay_ms: u64,
    #[serde(default)]
    retry_jitter: f64,
}

impl Default for RetryPolicySpec {
    fn default() -> Self {
        Self {
            operation_retries: default_operation_retries(),
            retry_delay_ms: default_retry_delay_ms(),
            retry_backoff: default_retry_backoff(),
            retry_max_delay_ms: default_retry_max_delay_ms(),
            retry_jitter: 0.0,
        }
    }
}

impl RetryPolicySpec {
    fn build(&self) -> RetryPolicy {
        RetryPolicy::new(
            self.operation_retries,
            Duration::from_millis(self.retry_delay_ms),
        )
        .with_backoff(
            self.retry_backoff,
            Duration::from_millis(self.retry_max_delay_ms),
        )
        .with_jitter(self.retry_jitter)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScanTaskSpec {
    source: String,
    ctrl_dir: PathBuf,
    meta_dir: PathBuf,
    #[serde(default = "default_scan_temp_dir")]
    temp_dir: PathBuf,
    #[serde(default = "default_scan_workers")]
    workers: usize,
    #[serde(default = "default_scan_writers")]
    writers: usize,
    #[serde(default)]
    follow_symlinks: bool,
    #[serde(default)]
    scan_hidden: bool,
    #[serde(default)]
    max_depth: Option<usize>,
    #[serde(default)]
    scan_acl: bool,
    #[serde(default)]
    scan_xattrs: bool,
    #[serde(default)]
    scan_hardlinks: bool,
    #[serde(default = "default_skip_block_devices")]
    skip_block_devices: bool,
    #[serde(default)]
    skip: Vec<String>,
    #[serde(default)]
    filters: ScanPathFilterSpec,
    #[serde(default)]
    prev_meta_dir: Option<PathBuf>,
    #[serde(default)]
    shard: bool,
    #[serde(default = "default_shard_num")]
    shard_num: usize,
    #[serde(default)]
    shard_max_entries_copy: Option<usize>,
    #[serde(default)]
    shard_max_entries_other: Option<usize>,
    #[serde(default)]
    shard_max_size: Option<u64>,
    #[serde(default = "default_smb_query_buffer_mb")]
    smb_query_buffer_mb: u32,
    #[serde(default = "default_nfs_connections")]
    nfs_connections: usize,
    #[serde(default)]
    nfs_uid: Option<u32>,
    #[serde(default)]
    nfs_gid: Option<u32>,
    #[serde(default)]
    stats_only: bool,
    #[serde(default)]
    failure_log_format: Option<FailureLogFormatArg>,
    #[serde(default)]
    retry: RetryPolicySpec,
    #[serde(default)]
    verbose: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AggregateOptionsSpec {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_aggregate_layout")]
    layout: AggregateLayoutArg,
    #[serde(default = "default_blob_size_mb")]
    blob_size_mb: u64,
    #[serde(default = "default_threshold_kb")]
    threshold_kb: u64,
    #[serde(default = "default_aggregate_shards")]
    shard_count: u16,
}

impl Default for AggregateOptionsSpec {
    fn default() -> Self {
        Self {
            enabled: false,
            layout: default_aggregate_layout(),
            blob_size_mb: default_blob_size_mb(),
            threshold_kb: default_threshold_kb(),
            shard_count: default_aggregate_shards(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BackupTaskSpec {
    source: String,
    target: String,
    #[serde(default = "default_backup_format")]
    format: BackupFormatArg,
    #[serde(default)]
    incremental_base: Option<PathBuf>,
    #[serde(default = "default_backup_temp_dir")]
    temp_dir: PathBuf,
    #[serde(default = "default_scan_workers")]
    scan_workers: usize,
    #[serde(default = "default_scan_writers")]
    scan_writers: usize,
    #[serde(default = "default_job_count")]
    jobs: usize,
    #[serde(default)]
    aggregate: AggregateOptionsSpec,
    #[serde(default)]
    hardlink: bool,
    #[serde(default)]
    delete: bool,
    #[serde(default)]
    mtime: bool,
    #[serde(default)]
    scan_filters: ScanPathFilterSpec,
    #[serde(default = "default_nfs_connections")]
    nfs_connections: usize,
    #[serde(default)]
    nfs_uid: Option<u32>,
    #[serde(default)]
    nfs_gid: Option<u32>,
    #[serde(default = "default_smb_connections")]
    smb_connections: usize,
    #[serde(default)]
    smb_copy_tasks: usize,
    #[serde(default = "default_buffer_size_kb")]
    buffer_size_kb: usize,
    #[serde(default)]
    failure_log_format: Option<FailureLogFormatArg>,
    #[serde(default)]
    retry: RetryPolicySpec,
    #[serde(default)]
    verbose: u8,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ScanPathFilterSpec {
    #[serde(default)]
    include_dir_patterns: Vec<String>,
    #[serde(default)]
    include_file_patterns: Vec<String>,
    #[serde(default)]
    exclude_dir_patterns: Vec<String>,
    #[serde(default)]
    exclude_file_patterns: Vec<String>,
}

impl ScanPathFilterSpec {
    fn compile(&self) -> Result<Option<ScanPathFilterSet>, io::Error> {
        ScanPathFilterSet::compile(
            self.include_dir_patterns.clone(),
            self.include_file_patterns.clone(),
            self.exclude_dir_patterns.clone(),
            self.exclude_file_patterns.clone(),
        )
        .map_err(io::Error::other)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RestoreTaskSpec {
    copy_source: String,
    target: String,
    #[serde(default = "default_restore_policy")]
    policy: RestorePolicyArg,
    #[serde(default = "default_backup_temp_dir")]
    temp_dir: PathBuf,
    #[serde(default = "default_job_count")]
    jobs: usize,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default = "default_nfs_connections")]
    nfs_connections: usize,
    #[serde(default)]
    nfs_uid: Option<u32>,
    #[serde(default)]
    nfs_gid: Option<u32>,
    #[serde(default)]
    verbose: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum TaskRequest {
    Scan(ScanTaskSpec),
    Backup(BackupTaskSpec),
    Restore(RestoreTaskSpec),
}

impl TaskRequest {
    fn kind(&self) -> TaskKind {
        match self {
            Self::Scan(_) => TaskKind::Scan,
            Self::Backup(_) => TaskKind::Backup,
            Self::Restore(_) => TaskKind::Restore,
        }
    }

    fn verbose(&self) -> u8 {
        match self {
            Self::Scan(spec) => spec.verbose,
            Self::Backup(spec) => spec.verbose,
            Self::Restore(spec) => spec.verbose,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TaskEnvelope {
    uuid: String,
    request: TaskRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TaskStats {
    None,
    Scan {
        total_files: u64,
        total_dirs: u64,
        total_size_bytes: u64,
        failed_files: u64,
        failed_dirs: u64,
    },
    Transfer {
        total_files: u64,
        total_dirs: u64,
        total_bytes: u64,
        failed_files: u64,
        failed_dirs: u64,
        subtasks_ok: usize,
        subtasks_failed: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TaskStatusSnapshot {
    uuid: String,
    kind: TaskKind,
    state: TaskState,
    pid: Option<u32>,
    created_at: String,
    started_at: Option<String>,
    finished_at: Option<String>,
    message: Option<String>,
    exit_code: Option<i32>,
    stats: TaskStats,
    request: TaskRequest,
}

#[derive(Debug, Clone)]
struct ManagedTask {
    snapshot: TaskStatusSnapshot,
    status_file: PathBuf,
    log_file: PathBuf,
}

#[derive(Debug, Clone)]
struct ServerConfig {
    runtime_dir: PathBuf,
    max_scanners_count: usize,
    max_subtasks_count: usize,
}

#[derive(Clone)]
struct AppState {
    config: Arc<ServerConfig>,
    tasks: Arc<RwLock<HashMap<String, ManagedTask>>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RpcRequest {
    #[serde(default)]
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct RpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
struct RpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Serialize)]
struct CreateTaskResponse {
    uuid: String,
    pid: Option<u32>,
    kind: TaskKind,
    state: TaskState,
    status_url: String,
    logs_url: String,
}

#[derive(Debug, Deserialize)]
struct TaskIdParams {
    uuid: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    if let Some(CommandMode::Worker(args)) = cli.command {
        return run_worker(args);
    }

    fs::create_dir_all(&cli.runtime_dir)?;
    fpt::logging::init(0);

    let state = AppState {
        config: Arc::new(ServerConfig {
            runtime_dir: cli.runtime_dir.clone(),
            max_scanners_count: cli.max_scanners_count.max(1),
            max_subtasks_count: cli.max_subtasks_count.max(1),
        }),
        tasks: Arc::new(RwLock::new(HashMap::new())),
    };

    let app = Router::new()
        .route("/rpc", post(handle_rpc))
        .route("/health", get(get_health))
        .route("/tasks", get(get_tasks))
        .route("/tasks/:uuid", get(get_task))
        .route("/tasks/:uuid/status", get(get_task_status))
        .route("/tasks/:uuid/logs", get(get_task_logs))
        .with_state(state.clone());

    let bind_addr = format!("{}:{}", cli.host, cli.port);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    println!("fptserver listening on http://{bind_addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

fn run_worker(args: WorkerArgs) -> Result<(), Box<dyn std::error::Error>> {
    let envelope: TaskEnvelope = read_json_file(&args.task_file)?;
    let pid = std::process::id();
    let created_at = Utc::now().to_rfc3339();

    let mut snapshot = TaskStatusSnapshot {
        uuid: envelope.uuid.clone(),
        kind: envelope.request.kind(),
        state: TaskState::Starting,
        pid: Some(pid),
        created_at: created_at.clone(),
        started_at: Some(created_at),
        finished_at: None,
        message: Some("worker starting".to_string()),
        exit_code: None,
        stats: TaskStats::None,
        request: envelope.request.clone(),
    };
    write_status_file(&args.status_file, &snapshot)?;

    fpt::logging::init(envelope.request.verbose());

    snapshot.state = TaskState::Running;
    snapshot.message = Some("task running".to_string());
    write_status_file(&args.status_file, &snapshot)?;

    let result = match &envelope.request {
        TaskRequest::Scan(spec) => run_scan_task(spec),
        TaskRequest::Backup(spec) => run_backup_task(spec),
        TaskRequest::Restore(spec) => run_restore_task(spec),
    };

    match result {
        Ok(stats) => {
            snapshot.state = TaskState::Completed;
            snapshot.finished_at = Some(Utc::now().to_rfc3339());
            snapshot.message = Some("task completed".to_string());
            snapshot.stats = stats;
            snapshot.exit_code = Some(0);
            write_status_file(&args.status_file, &snapshot)?;
            Ok(())
        }
        Err(err) => {
            snapshot.state = TaskState::Failed;
            snapshot.finished_at = Some(Utc::now().to_rfc3339());
            snapshot.message = Some(err.to_string());
            snapshot.exit_code = Some(1);
            write_status_file(&args.status_file, &snapshot)?;
            Err(err)
        }
    }
}

async fn handle_rpc(
    State(state): State<AppState>,
    Json(request): Json<RpcRequest>,
) -> Json<RpcResponse> {
    let response = match dispatch_rpc(state, &request).await {
        Ok(result) => RpcResponse {
            jsonrpc: "2.0",
            id: request.id.clone(),
            result: Some(result),
            error: None,
        },
        Err((code, message)) => RpcResponse {
            jsonrpc: "2.0",
            id: request.id.clone(),
            result: None,
            error: Some(RpcError { code, message }),
        },
    };
    Json(response)
}

async fn dispatch_rpc(state: AppState, request: &RpcRequest) -> Result<Value, (i64, String)> {
    match request.method.as_str() {
        "task.create_scan" => {
            let params: ScanTaskSpec = parse_rpc_params(&request.params)?;
            let snapshot = spawn_task(state, TaskRequest::Scan(params)).await?;
            Ok(serde_json::to_value(create_task_response(&snapshot)).map_err(json_to_rpc)?)
        }
        "task.create_backup" => {
            let params: BackupTaskSpec = parse_rpc_params(&request.params)?;
            let snapshot = spawn_task(state, TaskRequest::Backup(params)).await?;
            Ok(serde_json::to_value(create_task_response(&snapshot)).map_err(json_to_rpc)?)
        }
        "task.create_restore" => {
            let params: RestoreTaskSpec = parse_rpc_params(&request.params)?;
            let snapshot = spawn_task(state, TaskRequest::Restore(params)).await?;
            Ok(serde_json::to_value(create_task_response(&snapshot)).map_err(json_to_rpc)?)
        }
        "task.stop" => {
            let params: TaskIdParams = parse_rpc_params(&request.params)?;
            let snapshot = stop_task(state, &params.uuid).await?;
            Ok(serde_json::to_value(snapshot).map_err(json_to_rpc)?)
        }
        "task.kill" => {
            let params: TaskIdParams = parse_rpc_params(&request.params)?;
            let snapshot = kill_task(state, &params.uuid).await?;
            Ok(serde_json::to_value(snapshot).map_err(json_to_rpc)?)
        }
        "task.get" => {
            let params: TaskIdParams = parse_rpc_params(&request.params)?;
            let snapshot = get_task_snapshot(state, &params.uuid)
                .await
                .map_err(io_to_rpc)?;
            Ok(serde_json::to_value(snapshot).map_err(json_to_rpc)?)
        }
        "task.list" | "task.all" => {
            let snapshots = list_task_snapshots(state).await.map_err(io_to_rpc)?;
            Ok(serde_json::to_value(snapshots).map_err(json_to_rpc)?)
        }
        other => Err((-32601, format!("unknown method: {other}"))),
    }
}

async fn get_health(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "runtime_dir": state.config.runtime_dir,
        "max_scanners_count": state.config.max_scanners_count,
        "max_subtasks_count": state.config.max_subtasks_count,
    }))
}

async fn get_tasks(
    State(state): State<AppState>,
) -> Result<Json<Vec<TaskStatusSnapshot>>, ApiError> {
    Ok(Json(
        list_task_snapshots(state).await.map_err(ApiError::server)?,
    ))
}

async fn get_task(
    State(state): State<AppState>,
    Path(uuid): Path<String>,
) -> Result<Json<TaskStatusSnapshot>, ApiError> {
    Ok(Json(
        get_task_snapshot(state, &uuid)
            .await
            .map_err(ApiError::server)?,
    ))
}

async fn get_task_status(
    State(state): State<AppState>,
    Path(uuid): Path<String>,
) -> Result<Json<TaskStatusSnapshot>, ApiError> {
    Ok(Json(
        get_task_snapshot(state, &uuid)
            .await
            .map_err(ApiError::server)?,
    ))
}

async fn get_task_logs(
    State(state): State<AppState>,
    Path(uuid): Path<String>,
) -> Result<String, ApiError> {
    let tasks = state.tasks.read().await;
    let task = tasks
        .get(&uuid)
        .ok_or_else(|| ApiError::server(format!("task not found: {uuid}")))?;
    fs::read_to_string(&task.log_file).map_err(ApiError::server)
}

async fn spawn_task(
    state: AppState,
    request: TaskRequest,
) -> Result<TaskStatusSnapshot, (i64, String)> {
    enforce_limits(&state, request.kind())
        .await
        .map_err(|e| (-32001, e))?;

    let task_id = Uuid::new_v4().to_string();
    let task_dir = state.config.runtime_dir.join(&task_id);
    fs::create_dir_all(&task_dir).map_err(io_to_rpc)?;

    let request_file = task_dir.join("request.json");
    let status_file = task_dir.join("status.json");
    let log_file = task_dir.join("worker.log");

    let envelope = TaskEnvelope {
        uuid: task_id.clone(),
        request: request.clone(),
    };
    write_json_file(&request_file, &envelope).map_err(io_to_rpc)?;

    let mut snapshot = TaskStatusSnapshot {
        uuid: task_id.clone(),
        kind: request.kind(),
        state: TaskState::Created,
        pid: None,
        created_at: Utc::now().to_rfc3339(),
        started_at: None,
        finished_at: None,
        message: Some("task created".to_string()),
        exit_code: None,
        stats: TaskStats::None,
        request,
    };
    write_status_file(&status_file, &snapshot).map_err(io_to_rpc)?;

    let exe = std::env::current_exe().map_err(io_to_rpc)?;
    let stdout = open_log_file(&log_file).map_err(io_to_rpc)?;
    let stderr = stdout.try_clone().map_err(io_to_rpc)?;

    let mut child = Command::new(exe)
        .arg("worker")
        .arg("--task-file")
        .arg(&request_file)
        .arg("--status-file")
        .arg(&status_file)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(io_to_rpc)?;

    snapshot.pid = child.id();
    snapshot.state = TaskState::Starting;
    snapshot.started_at = Some(Utc::now().to_rfc3339());
    snapshot.message = Some("worker spawned".to_string());
    write_status_file(&status_file, &snapshot).map_err(io_to_rpc)?;

    let managed = ManagedTask {
        snapshot: snapshot.clone(),
        status_file: status_file.clone(),
        log_file,
    };

    {
        let mut tasks = state.tasks.write().await;
        tasks.insert(task_id.clone(), managed);
    }

    let state_for_wait = state.clone();
    tokio::spawn(async move {
        let exit = child.wait().await;
        if let Err(err) = reconcile_task_exit(state_for_wait, &task_id, exit).await {
            log::error!("failed to reconcile task {task_id} exit: {err}");
        }
    });

    Ok(snapshot)
}

async fn enforce_limits(state: &AppState, kind: TaskKind) -> Result<(), String> {
    let snapshots = list_task_snapshots(state.clone())
        .await
        .map_err(|e| e.to_string())?;
    let running_scanners = snapshots
        .iter()
        .filter(|task| task.kind == TaskKind::Scan && is_active_state(task.state))
        .count();
    let running_subtasks = snapshots
        .iter()
        .filter(|task| {
            matches!(task.kind, TaskKind::Backup | TaskKind::Restore) && is_active_state(task.state)
        })
        .count();

    match kind {
        TaskKind::Scan if running_scanners >= state.config.max_scanners_count => Err(format!(
            "scanner limit reached: {running_scanners}/{}",
            state.config.max_scanners_count
        )),
        TaskKind::Backup | TaskKind::Restore
            if running_subtasks >= state.config.max_subtasks_count =>
        {
            Err(format!(
                "subtask limit reached: {running_subtasks}/{}",
                state.config.max_subtasks_count
            ))
        }
        _ => Ok(()),
    }
}

async fn reconcile_task_exit(
    state: AppState,
    task_id: &str,
    exit: io::Result<std::process::ExitStatus>,
) -> io::Result<()> {
    let mut tasks = state.tasks.write().await;
    let Some(task) = tasks.get_mut(task_id) else {
        return Ok(());
    };
    if let Ok(latest) = read_json_file::<TaskStatusSnapshot>(&task.status_file) {
        task.snapshot = latest;
    }
    if matches!(
        task.snapshot.state,
        TaskState::Completed | TaskState::Failed | TaskState::Killed | TaskState::Stopped
    ) {
        return Ok(());
    }

    match exit {
        Ok(status) => {
            task.snapshot.finished_at = Some(Utc::now().to_rfc3339());
            task.snapshot.exit_code = status.code();
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                if let Some(signal) = status.signal() {
                    task.snapshot.state = if signal == libc::SIGKILL {
                        TaskState::Killed
                    } else {
                        TaskState::Stopped
                    };
                    task.snapshot.message = Some(format!("worker exited by signal {signal}"));
                    write_status_file(&task.status_file, &task.snapshot)?;
                    return Ok(());
                }
            }
            if status.success() {
                task.snapshot.state = TaskState::Completed;
                task.snapshot.message = Some("worker exited successfully".to_string());
            } else {
                task.snapshot.state = TaskState::Failed;
                task.snapshot.message = Some(format!("worker exited with status {status}"));
            }
        }
        Err(err) => {
            task.snapshot.finished_at = Some(Utc::now().to_rfc3339());
            task.snapshot.state = TaskState::Failed;
            task.snapshot.message = Some(format!("failed waiting for worker: {err}"));
        }
    }
    write_status_file(&task.status_file, &task.snapshot)?;
    Ok(())
}

async fn stop_task(state: AppState, task_id: &str) -> Result<TaskStatusSnapshot, (i64, String)> {
    signal_task(
        state,
        task_id,
        libc::SIGTERM,
        TaskState::Stopping,
        "stop requested",
    )
    .await
}

async fn kill_task(state: AppState, task_id: &str) -> Result<TaskStatusSnapshot, (i64, String)> {
    signal_task(
        state,
        task_id,
        libc::SIGKILL,
        TaskState::Killed,
        "kill requested",
    )
    .await
}

async fn signal_task(
    state: AppState,
    task_id: &str,
    signal: i32,
    next_state: TaskState,
    message: &str,
) -> Result<TaskStatusSnapshot, (i64, String)> {
    let mut tasks = state.tasks.write().await;
    let task = tasks
        .get_mut(task_id)
        .ok_or_else(|| (-32004, format!("task not found: {task_id}")))?;
    if let Ok(latest) = read_json_file::<TaskStatusSnapshot>(&task.status_file) {
        task.snapshot = latest;
    }
    let pid = task
        .snapshot
        .pid
        .ok_or_else(|| (-32005, format!("task {task_id} does not have a live pid")))?;

    #[cfg(unix)]
    {
        let rc = unsafe { libc::kill(pid as i32, signal) };
        if rc != 0 {
            return Err((
                -32007,
                format!(
                    "failed to signal task {task_id}: {}",
                    io::Error::last_os_error()
                ),
            ));
        }
    }

    task.snapshot.state = next_state;
    task.snapshot.message = Some(message.to_string());
    if next_state == TaskState::Killed {
        task.snapshot.finished_at = Some(Utc::now().to_rfc3339());
    }
    write_status_file(&task.status_file, &task.snapshot).map_err(io_to_rpc)?;
    Ok(task.snapshot.clone())
}

async fn get_task_snapshot(
    state: AppState,
    task_id: &str,
) -> Result<TaskStatusSnapshot, io::Error> {
    refresh_task_snapshot(&state, task_id).await?;
    let tasks = state.tasks.read().await;
    tasks
        .get(task_id)
        .map(|task| task.snapshot.clone())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("task not found: {task_id}"),
            )
        })
}

async fn list_task_snapshots(state: AppState) -> Result<Vec<TaskStatusSnapshot>, io::Error> {
    let ids: Vec<String> = {
        let tasks = state.tasks.read().await;
        tasks.keys().cloned().collect()
    };
    for id in ids {
        refresh_task_snapshot(&state, &id).await?;
    }
    let tasks = state.tasks.read().await;
    let mut list: Vec<TaskStatusSnapshot> =
        tasks.values().map(|task| task.snapshot.clone()).collect();
    list.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    Ok(list)
}

async fn refresh_task_snapshot(state: &AppState, task_id: &str) -> Result<(), io::Error> {
    let mut tasks = state.tasks.write().await;
    let Some(task) = tasks.get_mut(task_id) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("task not found: {task_id}"),
        ));
    };
    if task.status_file.exists() {
        task.snapshot = read_json_file(&task.status_file)?;
    }
    Ok(())
}

fn create_task_response(snapshot: &TaskStatusSnapshot) -> CreateTaskResponse {
    CreateTaskResponse {
        uuid: snapshot.uuid.clone(),
        pid: snapshot.pid,
        kind: snapshot.kind,
        state: snapshot.state,
        status_url: format!("/tasks/{}/status", snapshot.uuid),
        logs_url: format!("/tasks/{}/logs", snapshot.uuid),
    }
}

fn parse_rpc_params<T: DeserializeOwned>(value: &Value) -> Result<T, (i64, String)> {
    serde_json::from_value(value.clone()).map_err(|e| (-32602, format!("invalid params: {e}")))
}

fn io_to_rpc(err: io::Error) -> (i64, String) {
    (-32000, err.to_string())
}

fn json_to_rpc(err: serde_json::Error) -> (i64, String) {
    (-32603, err.to_string())
}

fn run_scan_task(spec: &ScanTaskSpec) -> Result<TaskStats, Box<dyn std::error::Error>> {
    let location = parse_data_location(
        &spec.source,
        spec.nfs_connections,
        spec.nfs_uid,
        spec.nfs_gid,
    )?;
    let retry_policy = spec.retry.build();
    let mut scan_option = ScanOption::new(spec.ctrl_dir.clone(), spec.meta_dir.clone())
        .worker_count(spec.workers)
        .writer_count(spec.writers)
        .temp_dir(spec.temp_dir.clone())
        .follow_symlinks(spec.follow_symlinks)
        .scan_hidden(spec.scan_hidden)
        .max_depth(spec.max_depth)
        .scan_acl(spec.scan_acl)
        .scan_xattrs(spec.scan_xattrs)
        .scan_hardlinks(spec.scan_hardlinks)
        .skip_block_devices(spec.skip_block_devices)
        .skip_entries(spec.skip.clone())
        .prev_meta_dir(spec.prev_meta_dir.clone())
        .enable_sharding(spec.shard)
        .shard_num(spec.shard_num)
        .smb_query_buffer_size(spec.smb_query_buffer_mb.saturating_mul(1024 * 1024))
        .control_path(
            location.control_path_base(),
            location.logical_source_root(),
            location.kind_name().to_string(),
        )
        .path_filters(spec.filters.compile()?)
        .retry_policy(retry_policy)
        .stats_only(spec.stats_only);
    if let Some(max_entries) = spec.shard_max_entries_copy {
        scan_option = scan_option.shard_max_entries_copy(max_entries);
    }
    if let Some(max_entries) = spec.shard_max_entries_other {
        scan_option = scan_option.shard_max_entries_other(max_entries);
    }
    if let Some(max_size) = spec.shard_max_size {
        scan_option = scan_option.shard_max_size(max_size);
    }
    if let Some(format) = spec.failure_log_format {
        let format: FailureLogFormat = format.into();
        let path = failure_file_path(&spec.ctrl_dir, "SCAN_FAILURE", format);
        scan_option = scan_option.failure_log(Some(FailureLogConfig::new(path, format)));
    }

    match location {
        DataLocation::Local(path) => {
            if !path.exists() {
                return Err(format!("source path does not exist: {}", path.display()).into());
            }
            if !path.is_dir() {
                return Err(format!("source path is not a directory: {}", path.display()).into());
            }
            let mut scanner = Scanner::new(scan_option);
            scanner.enqueue_path(path)?;
            let running = scanner.start()?;
            while !running.complete() {
                std::thread::sleep(Duration::from_millis(200));
            }
            let snap = running.stats();
            running.wait();
            Ok(TaskStats::Scan {
                total_files: snap.tot_files,
                total_dirs: snap.tot_dirs,
                total_size_bytes: snap.tot_size,
                failed_files: snap.failed_files,
                failed_dirs: snap.failed_dirs,
            })
        }
        #[cfg(feature = "nfs")]
        DataLocation::Nfs(loc) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("fpt-fptserver-scan-nfs")
                .build()?;
            let (total_files, total_dirs, total_size_bytes, failed_files, failed_dirs) =
                rt.block_on(fpt::scanner::run_nfs_scan(&loc, scan_option))?;
            Ok(TaskStats::Scan {
                total_files,
                total_dirs,
                total_size_bytes,
                failed_files,
                failed_dirs,
            })
        }
        #[cfg(feature = "smb")]
        DataLocation::Smb(loc) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("fpt-fptserver-scan-smb")
                .build()?;
            let (total_files, total_dirs, total_size_bytes, failed_files, failed_dirs) =
                rt.block_on(fpt::scanner::run_smb_scan(&loc, scan_option))?;
            Ok(TaskStats::Scan {
                total_files,
                total_dirs,
                total_size_bytes,
                failed_files,
                failed_dirs,
            })
        }
    }
}

fn run_backup_task(spec: &BackupTaskSpec) -> Result<TaskStats, Box<dyn std::error::Error>> {
    let source = parse_data_location(
        &spec.source,
        spec.nfs_connections,
        spec.nfs_uid,
        spec.nfs_gid,
    )?;
    let target = parse_data_location(
        &spec.target,
        spec.nfs_connections,
        spec.nfs_uid,
        spec.nfs_gid,
    )?;
    let retry_policy = spec.retry.build();
    let aggregate_enabled =
        matches!(spec.format, BackupFormatArg::Aggregated) || spec.aggregate.enabled;
    let aggregate_config = if aggregate_enabled {
        AggregateConfig::enabled()
            .layout(spec.aggregate.layout.into())
            .max_blob_size(spec.aggregate.blob_size_mb * 1024 * 1024)
            .file_threshold(spec.aggregate.threshold_kb * 1024)
            .shard_count(spec.aggregate.shard_count)
    } else {
        AggregateConfig::default()
    };
    let scan_config = ScanConfig {
        worker_count: spec.scan_workers,
        writer_count: spec.scan_writers,
        prev_meta_dir: None,
        enable_aggregation: aggregate_enabled,
        max_aggregate_blob_size: spec.aggregate.blob_size_mb * 1024 * 1024,
        aggregate_file_threshold: spec.aggregate.threshold_kb * 1024,
        failure_log: None,
        retry_policy,
        path_filters: spec.scan_filters.compile()?,
    };
    let config = BackupJobConfig {
        source,
        target,
        format_tag: match spec.format {
            BackupFormatArg::Common => "COMMON".to_string(),
            BackupFormatArg::Aggregated => "AGGR".to_string(),
        },
        type_tag: if spec.incremental_base.is_some() {
            "INC".to_string()
        } else {
            "FULL".to_string()
        },
        temp_config: TempRepoConfig::new(spec.temp_dir.clone()),
        scan_config,
        aggregate_config,
        enable_hardlink: spec.hardlink && !aggregate_enabled,
        enable_delete: spec.delete && !aggregate_enabled,
        enable_mtime: spec.mtime && !aggregate_enabled,
        max_concurrent_subtasks: spec.jobs,
        smb_connection_count: spec.smb_connections.max(1),
        smb_copy_task_count: spec.smb_copy_tasks,
        copy_buffer_size: (spec.buffer_size_kb * 1024).clamp(256 * 1024, 4 * 1024 * 1024),
        failure_log_format: spec.failure_log_format.map(Into::into),
        retry_policy,
        incremental_base: spec.incremental_base.clone(),
        verbose: spec.verbose,
    };
    let result = FileBackupJob::new(config).run()?;
    Ok(TaskStats::Transfer {
        total_files: result.total_files,
        total_dirs: result.total_dirs,
        total_bytes: result.total_bytes,
        failed_files: 0,
        failed_dirs: 0,
        subtasks_ok: result.subtasks_ok,
        subtasks_failed: result.subtasks_failed,
    })
}

fn run_restore_task(spec: &RestoreTaskSpec) -> Result<TaskStats, Box<dyn std::error::Error>> {
    let copy_source = parse_data_location(
        &spec.copy_source,
        spec.nfs_connections,
        spec.nfs_uid,
        spec.nfs_gid,
    )?;
    let restore_target = parse_data_location(
        &spec.target,
        spec.nfs_connections,
        spec.nfs_uid,
        spec.nfs_gid,
    )?;
    let config = RestoreJobConfig {
        copy_source,
        restore_target,
        policy: spec.policy.into(),
        temp_config: TempRepoConfig::new(spec.temp_dir.clone()),
        max_concurrent_subtasks: spec.jobs,
        fine_grain_paths: spec.paths.clone(),
    };
    let result = FileRestoreJob::new(config).run()?;
    Ok(TaskStats::Transfer {
        total_files: result.total_files,
        total_dirs: result.total_dirs,
        total_bytes: result.total_bytes,
        failed_files: 0,
        failed_dirs: 0,
        subtasks_ok: result.subtasks_ok,
        subtasks_failed: result.subtasks_failed,
    })
}

fn parse_data_location(
    spec: &str,
    connections: usize,
    uid: Option<u32>,
    gid: Option<u32>,
) -> Result<DataLocation, Box<dyn std::error::Error>> {
    if spec.starts_with("nfs://") {
        #[cfg(feature = "nfs")]
        {
            let mut loc = fpt::nfs::NfsLocation::from_url(spec)?.connection_count(connections);
            let default_uid = if loc.uid == 0 {
                unsafe { libc::geteuid() as u32 }
            } else {
                loc.uid
            };
            let default_gid = if loc.gid == 0 {
                unsafe { libc::getegid() as u32 }
            } else {
                loc.gid
            };
            loc = loc.credentials(uid.unwrap_or(default_uid), gid.unwrap_or(default_gid));
            Ok(DataLocation::nfs(loc))
        }
        #[cfg(not(feature = "nfs"))]
        {
            let _ = (connections, uid, gid);
            Err("NFS support not compiled in. Rebuild with --features nfs".into())
        }
    } else if spec.starts_with("smb://") || spec.starts_with(r"smb:\\") {
        #[cfg(feature = "smb")]
        {
            let _ = (connections, uid, gid);
            Ok(DataLocation::smb(fpt::smb::SmbLocation::from_url(
                spec,
            )?))
        }
        #[cfg(not(feature = "smb"))]
        {
            let _ = (connections, uid, gid);
            Err("SMB support not compiled in. Rebuild with --features smb".into())
        }
    } else {
        let _ = (connections, uid, gid);
        Ok(DataLocation::local(PathBuf::from(spec)))
    }
}

fn open_log_file(path: &FsPath) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn write_status_file(path: &FsPath, snapshot: &TaskStatusSnapshot) -> io::Result<()> {
    write_json_file(path, snapshot)
}

fn write_json_file<T: Serialize>(path: &FsPath, value: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = File::create(path)?;
    serde_json::to_writer_pretty(&mut file, value)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    file.write_all(b"\n")?;
    Ok(())
}

fn read_json_file<T: DeserializeOwned>(path: &FsPath) -> io::Result<T> {
    let file = File::open(path)?;
    serde_json::from_reader(file).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn is_active_state(state: TaskState) -> bool {
    matches!(
        state,
        TaskState::Created | TaskState::Starting | TaskState::Running | TaskState::Stopping
    )
}

fn default_operation_retries() -> u32 {
    3
}

fn default_retry_delay_ms() -> u64 {
    1000
}

fn default_retry_backoff() -> f64 {
    1.0
}

fn default_retry_max_delay_ms() -> u64 {
    1000
}

fn default_scan_temp_dir() -> PathBuf {
    PathBuf::from("/tmp/fpt/cache")
}

fn default_backup_temp_dir() -> PathBuf {
    PathBuf::from("/tmp/fpt")
}

fn default_scan_workers() -> usize {
    8
}

fn default_scan_writers() -> usize {
    1
}

fn default_skip_block_devices() -> bool {
    true
}

fn default_shard_num() -> usize {
    16
}

fn default_smb_query_buffer_mb() -> u32 {
    8
}

fn default_nfs_connections() -> usize {
    32
}

fn default_smb_connections() -> usize {
    4
}

fn default_job_count() -> usize {
    4
}

fn default_buffer_size_kb() -> usize {
    1024
}

fn default_blob_size_mb() -> u64 {
    64
}

fn default_threshold_kb() -> u64 {
    1024
}

fn default_aggregate_shards() -> u16 {
    16
}

fn default_aggregate_layout() -> AggregateLayoutArg {
    AggregateLayoutArg::Shard
}

fn default_backup_format() -> BackupFormatArg {
    BackupFormatArg::Common
}

fn default_restore_policy() -> RestorePolicyArg {
    RestorePolicyArg::Replace
}
