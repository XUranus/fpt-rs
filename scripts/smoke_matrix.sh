#!/usr/bin/env bash
set -euo pipefail

# End-to-end smoke matrix for fptcli backup/restore across local, NFS, and SMB.
#
# Required assumption:
#   TEST_ROOT_DIR is visible as:
#     - local filesystem path
#     - NFS export TEST_NFS_EXPORT
#     - SMB share TEST_SMB_SHARE
#
# Default layout matches the local setup used during SMB/NFS development:
#   TEST_ROOT_DIR=/opt/dataset
#   NFS: nfs://127.0.0.1/opt/dataset?sub=/...
#   SMB: smb://127.0.0.1/dataset/...?...credentials...

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"

TEST_ROOT_DIR="${TEST_ROOT_DIR:-/opt/dataset}"
TEST_TEMP_DIR="${TEST_TEMP_DIR:-/opt/target/work}"
TEST_BIN_DIR="${TEST_BIN_DIR:-${REPO_ROOT}/target/release}"
TEST_TIMEOUT_SEC="${TEST_TIMEOUT_SEC:-60}"
TEST_BUILD="${TEST_BUILD:-1}"
TEST_CLEAN="${TEST_CLEAN:-1}"

TEST_NFS_HOST="${TEST_NFS_HOST:-127.0.0.1}"
TEST_NFS_EXPORT="${TEST_NFS_EXPORT:-${TEST_ROOT_DIR}}"
TEST_NFS_UID="${TEST_NFS_UID:-$(id -u)}"
TEST_NFS_GID="${TEST_NFS_GID:-$(id -g)}"

TEST_SMB_HOST="${TEST_SMB_HOST:-127.0.0.1}"
TEST_SMB_SHARE="${TEST_SMB_SHARE:-dataset}"
TEST_SMB_USER="${TEST_SMB_USER:-xuranus}"
TEST_SMB_PASSWORD="${TEST_SMB_PASSWORD:-123456789}"

# Space-separated aggregate layouts to test. Use "shard" for a shorter run.
TEST_AGGREGATE_LAYOUTS="${TEST_AGGREGATE_LAYOUTS:-shard dir-level}"
# Space-separated transports to include. Default covers the full matrix.
TEST_TRANSPORTS="${TEST_TRANSPORTS:-local nfs smb}"

DATASET_DIR="${TEST_ROOT_DIR}/test_smoke"
OUT_DIR="${TEST_ROOT_DIR}/out"
RESTORE_ROOT="${TEST_ROOT_DIR}/restore"
LOG_DIR="${TEST_ROOT_DIR}/smoke_logs/$(date +%Y%m%d_%H%M%S)"

FPTCLI="${TEST_BIN_DIR}/fptcli"
FSDIFF="${TEST_BIN_DIR}/fsdiff"
VDBENCH="${TEST_BIN_DIR}/vdbench"

COMMON_TIMEOUT_STATUS=124

require_safe_path() {
  local path="$1"
  if [[ -z "${path}" || "${path}" == "/" ]]; then
    echo "Refusing unsafe path: '${path}'" >&2
    exit 2
  fi
}

nfs_url() {
  local sub="$1"
  printf 'nfs://%s%s?sub=%s' "${TEST_NFS_HOST}" "${TEST_NFS_EXPORT}" "${sub}"
}

smb_url() {
  local sub="$1"
  printf 'smb://%s/%s/%s?username=%s&password=%s' \
    "${TEST_SMB_HOST}" "${TEST_SMB_SHARE}" "${sub#/}" \
    "${TEST_SMB_USER}" "${TEST_SMB_PASSWORD}"
}

run_step() {
  local label="$1"
  local log_file="$2"
  shift 2

  echo "[RUN] ${label}"
  set +e
  timeout --foreground -k 5s "${TEST_TIMEOUT_SEC}s" "$@" >"${log_file}" 2>&1
  local rc=$?
  set -e

  if [[ ${rc} -eq ${COMMON_TIMEOUT_STATUS} ]]; then
    echo "[HANG?] ${label} exceeded ${TEST_TIMEOUT_SEC}s and was killed. Log: ${log_file}" >&2
    return "${rc}"
  fi
  if [[ ${rc} -ne 0 ]]; then
    echo "[FAIL] ${label} exited ${rc}. Log: ${log_file}" >&2
    tail -n 80 "${log_file}" >&2 || true
    return "${rc}"
  fi

  echo "[OK] ${label}"
}

latest_copy_root() {
  local format_tag="$1"
  local copy
  copy="$(find "${OUT_DIR}" -maxdepth 1 -type d -name "COPY_${format_tag}_FULL_*" \
    -printf '%T@ %p\n' | sort -nr | awk 'NR == 1 {print $2}')"
  if [[ -z "${copy}" ]]; then
    echo "No COPY_${format_tag}_FULL_* found under ${OUT_DIR}" >&2
    return 1
  fi
  printf '%s\n' "${copy}"
}

copy_uuid_from_root() {
  basename "$1" | sed 's/^.*_//'
}

generate_dataset() {
  echo "[INFO] Generating smoke dataset at ${DATASET_DIR}"
  mkdir -p "${DATASET_DIR}"

  # 15 x 1K, 15 x 128K, 15 x 1M, 10 x 4M, 2 x 100M.
  # Total: 57 files, ~257 MiB plus directory metadata.
  run_step "vdbench 1K files" "${LOG_DIR}/vdbench_1k.log" \
    "${VDBENCH}" --output "${DATASET_DIR}/sz_1k" \
    --depth 2 --dirs 2 --files 5 --size 1K --threads 4 \
    --dir-prefix vdb.1k.dir. --file-prefix file. --level-names --index-base 1 -y

  run_step "vdbench 128K files" "${LOG_DIR}/vdbench_128k.log" \
    "${VDBENCH}" --output "${DATASET_DIR}/sz_128k" \
    --depth 2 --dirs 2 --files 5 --size 128K --threads 4 \
    --dir-prefix vdb.128k.dir. --file-prefix file. --level-names --index-base 1 -y

  run_step "vdbench 1M files" "${LOG_DIR}/vdbench_1m.log" \
    "${VDBENCH}" --output "${DATASET_DIR}/sz_1m" \
    --depth 2 --dirs 2 --files 5 --size 1M --threads 4 \
    --dir-prefix vdb.1m.dir. --file-prefix file. --level-names --index-base 1 -y

  run_step "vdbench 4M files" "${LOG_DIR}/vdbench_4m.log" \
    "${VDBENCH}" --output "${DATASET_DIR}/sz_4m" \
    --depth 1 --dirs 0 --files 10 --size 4M --threads 4 \
    --dir-prefix vdb.4m.dir. --file-prefix file. --level-names --index-base 1 -y

  run_step "vdbench 100M files" "${LOG_DIR}/vdbench_100m.log" \
    "${VDBENCH}" --output "${DATASET_DIR}/sz_100m" \
    --depth 1 --dirs 0 --files 2 --size 100M --threads 2 \
    --dir-prefix vdb.100m.dir. --file-prefix file. --level-names --index-base 1 -y

  local files bytes
  files="$(find "${DATASET_DIR}" -type f | wc -l)"
  bytes="$(find "${DATASET_DIR}" -type f -printf '%s\n' | awk '{s += $1} END {print s + 0}')"
  echo "[INFO] Dataset ready: ${files} files, ${bytes} bytes"

  if (( files >= 100 )); then
    echo "Dataset file count ${files} exceeds requested <100 limit" >&2
    exit 1
  fi
  if (( bytes >= 1073741824 )); then
    echo "Dataset size ${bytes} exceeds requested <1GiB limit" >&2
    exit 1
  fi
}

backup_source_spec() {
  case "$1" in
    local) printf '%s\n' "${DATASET_DIR}" ;;
    nfs) nfs_url "/test_smoke" ;;
    smb) smb_url "/test_smoke" ;;
    *) echo "unknown source type: $1" >&2; return 1 ;;
  esac
}

backup_target_spec() {
  case "$1" in
    local) printf '%s\n' "${OUT_DIR}" ;;
    nfs) nfs_url "/out" ;;
    smb) smb_url "/out" ;;
    *) echo "unknown target type: $1" >&2; return 1 ;;
  esac
}

run_case() {
  local source_type="$1"
  local target_type="$2"
  local format="$3"
  local layout="${4:-}"

  local format_tag="COMMON"
  local case_name="${source_type}_to_${target_type}_${format}"
  local -a format_args=(--format "${format}")

  if [[ "${format}" == "aggregated" ]]; then
    format_tag="AGGR"
    case_name="${case_name}_${layout}"
    format_args+=(--aggregate-layout "${layout}" --blob-size 4 --threshold 1024)
  fi

  local source target copy_root uuid restore_dir
  source="$(backup_source_spec "${source_type}")"
  target="$(backup_target_spec "${target_type}")"

  echo
  echo "======================================================================"
  echo "[CASE] ${case_name}"
  echo "Source: ${source}"
  echo "Target: ${target}"
  echo "======================================================================"

  run_step "backup ${case_name}" "${LOG_DIR}/${case_name}.backup.log" \
    "${FPTCLI}" backup \
      --data "${source}" \
      --target "${target}" \
      --temp-dir "${TEST_TEMP_DIR}" \
      "${format_args[@]}" \
      --nfs-uid "${TEST_NFS_UID}" \
      --nfs-gid "${TEST_NFS_GID}"

  copy_root="$(latest_copy_root "${format_tag}")"
  uuid="$(copy_uuid_from_root "${copy_root}")"
  restore_dir="${RESTORE_ROOT}/${uuid}"
  mkdir -p "${restore_dir}"

  run_step "restore ${case_name}" "${LOG_DIR}/${case_name}.restore.log" \
    "${FPTCLI}" restore \
      --copy "${copy_root}" \
      --target "${restore_dir}" \
      --policy replace \
      --nfs-uid "${TEST_NFS_UID}" \
      --nfs-gid "${TEST_NFS_GID}"

  run_step "diff ${case_name}" "${LOG_DIR}/${case_name}.diff.log" \
    "${FSDIFF}" \
      --source "${DATASET_DIR}" \
      --target "${restore_dir}"

  echo "[PASS] ${case_name} copy=${copy_root} restore=${restore_dir}"
}

main() {
  require_safe_path "${TEST_ROOT_DIR}"
  require_safe_path "${DATASET_DIR}"
  require_safe_path "${OUT_DIR}"
  require_safe_path "${RESTORE_ROOT}"
  require_safe_path "${TEST_TEMP_DIR}"

  cd "${REPO_ROOT}"

  if [[ "${TEST_BUILD}" == "1" ]]; then
    echo "[INFO] Building release binaries with nfs+smb features"
    cargo build --release --features nfs --features smb --bin fptcli --bin fsdiff --bin vdbench
  fi

  for bin in "${FPTCLI}" "${FSDIFF}" "${VDBENCH}"; do
    if [[ ! -x "${bin}" ]]; then
      echo "Required binary not found or not executable: ${bin}" >&2
      exit 1
    fi
  done

  mkdir -p "${LOG_DIR}" "${TEST_TEMP_DIR}"

  if [[ "${TEST_CLEAN}" == "1" ]]; then
    echo "[INFO] Cleaning ${DATASET_DIR}, ${OUT_DIR}, ${RESTORE_ROOT}"
    rm -rf "${DATASET_DIR}" "${OUT_DIR}" "${RESTORE_ROOT}"
    mkdir -p "${DATASET_DIR}" "${OUT_DIR}" "${RESTORE_ROOT}"
  else
    mkdir -p "${DATASET_DIR}" "${OUT_DIR}" "${RESTORE_ROOT}"
  fi

  generate_dataset

  local source_type target_type layout

  for source_type in ${TEST_TRANSPORTS}; do
    for target_type in ${TEST_TRANSPORTS}; do
      run_case "${source_type}" "${target_type}" common
      for layout in ${TEST_AGGREGATE_LAYOUTS}; do
        run_case "${source_type}" "${target_type}" aggregated "${layout}"
      done
    done
  done

  echo
  echo "======================================================================"
  echo "SMOKE MATRIX PASSED"
  echo "Dataset : ${DATASET_DIR}"
  echo "Copies  : ${OUT_DIR}"
  echo "Restore : ${RESTORE_ROOT}"
  echo "Logs    : ${LOG_DIR}"
  echo "======================================================================"
}

main "$@"
