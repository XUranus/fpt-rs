#!/bin/bash
#
# Bifrost Integrated Test Script
# Runs fsscan -> fsbackup -> fsdiff to verify backup integrity
#

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Default values
SCAN_WORKERS=4
SCAN_WRITERS=1
BACKUP_WORKERS=4
VERBOSE=0
SKIP_DIFF=0
KEEP_WORK_DIR=0
SCAN_ACL=0
SCAN_XATTRS=0
SCAN_HARDLINKS=0
BACKUP_HARDLINKS=0

# Get script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIFROST_ROOT="$(dirname "$SCRIPT_DIR")"

# Binary paths
FSSCAN="${BIFROST_ROOT}/target/release/fsscan"
FSBACKUP="${BIFROST_ROOT}/target/release/fsbackup"
FSDIFF="${BIFROST_ROOT}/target/release/fsdiff"

# Function to print section headers
print_section() {
    echo ""
    echo "============================================================"
    echo "$1"
    echo "============================================================"
}

# Function to print error and exit
error_exit() {
    echo -e "${RED}ERROR: $1${NC}" >&2
    exit 1
}

# Function to print success
print_success() {
    echo -e "${GREEN}$1${NC}"
}

# Function to print warning
print_warning() {
    echo -e "${YELLOW}$1${NC}"
}

# Usage information
usage() {
    cat << EOF
Usage: $0 [OPTIONS] -i <input_path> -o <output_dir>

Integrated test for fsscan, fsbackup, and fsdiff

Required Arguments:
  -i, --input <PATH>       Source directory path (can be specified multiple times)
  -o, --output <DIR>       Output directory for backup

Optional Arguments:
  -w, --work-dir <DIR>     Working directory for metadata (default: temp dir)
  --scan-workers <N>       Number of scan workers (default: 4)
  --scan-writers <N>       Number of scan writers (default: 1)
  --backup-workers <N>     Number of backup workers (default: 4)
  --skip-diff              Skip diff verification
  -v, --verbose            Verbose output
  --keep-work-dir          Keep working directory after test
  --scan-acl               Scan ACLs during backup
  --scan-xattrs            Scan extended attributes during backup
  --scan-hardlinks         Scan and track hardlinks during backup
  --backup-hardlinks       Enable hardlink phase during backup
  -h, --help               Show this help message

Examples:
  $0 -i /data/source -o /backup/target
  $0 -i /data/dir1 -i /data/dir2 -o /backup/target -v
  $0 -i /data/source -o /backup/target --skip-diff

EOF
}

# Parse arguments
INPUT_PATHS=()
while [[ $# -gt 0 ]]; do
    case $1 in
        -i|--input)
            INPUT_PATHS+=("$2")
            shift 2
            ;;
        -o|--output)
            OUTPUT_DIR="$2"
            shift 2
            ;;
        -w|--work-dir)
            WORK_DIR="$2"
            shift 2
            ;;
        --scan-workers)
            SCAN_WORKERS="$2"
            shift 2
            ;;
        --scan-writers)
            SCAN_WRITERS="$2"
            shift 2
            ;;
        --backup-workers)
            BACKUP_WORKERS="$2"
            shift 2
            ;;
        --skip-diff)
            SKIP_DIFF=1
            shift
            ;;
        -v|--verbose)
            VERBOSE=1
            shift
            ;;
        --keep-work-dir)
            KEEP_WORK_DIR=1
            shift
            ;;
        --scan-acl)
            SCAN_ACL=1
            shift
            ;;
        --scan-xattrs)
            SCAN_XATTRS=1
            shift
            ;;
        --scan-hardlinks)
            SCAN_HARDLINKS=1
            shift
            ;;
        --backup-hardlinks)
            BACKUP_HARDLINKS=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            error_exit "Unknown option: $1"
            ;;
    esac
done

# Validate required arguments
if [[ ${#INPUT_PATHS[@]} -eq 0 ]]; then
    error_exit "No input paths specified. Use -i <path>"
fi

if [[ -z "$OUTPUT_DIR" ]]; then
    error_exit "No output directory specified. Use -o <dir>"
fi

# Check binaries exist
for binary in "$FSSCAN" "$FSBACKUP" "$FSDIFF"; do
    if [[ ! -f "$binary" ]]; then
        error_exit "Binary not found: $binary\nPlease build with: cargo build --release"
    fi
done

# Validate input paths
for path in "${INPUT_PATHS[@]}"; do
    if [[ ! -d "$path" ]]; then
        error_exit "Input path does not exist or is not a directory: $path"
    fi
done

# Create working directory
if [[ -z "$WORK_DIR" ]]; then
    WORK_DIR=$(mktemp -d)
    TEMP_WORK_DIR=1
else
    mkdir -p "$WORK_DIR"
    TEMP_WORK_DIR=0
fi

CTRL_DIR="${WORK_DIR}/ctrl"
META_DIR="${WORK_DIR}/meta"
mkdir -p "$CTRL_DIR" "$META_DIR"

# Cleanup function
cleanup() {
    if [[ $KEEP_WORK_DIR -eq 0 && $TEMP_WORK_DIR -eq 1 ]]; then
        echo ""
        echo "Cleaning up working directory..."
        rm -rf "$WORK_DIR"
    fi
}
trap cleanup EXIT

print_section "Bifrost Integrated Test"
echo "Input paths:"
for path in "${INPUT_PATHS[@]}"; do
    echo "  - $path"
done
echo "Output directory: $OUTPUT_DIR"
echo "Working directory: $WORK_DIR"
echo ""

# =============================================================================
# STEP 1: Scan
# =============================================================================
print_section "STEP 1: Scanning Source Directories"

SCAN_ARGS=()
if [[ $VERBOSE -eq 1 ]]; then
    SCAN_ARGS+=("-v")
fi
if [[ $SCAN_ACL -eq 1 ]]; then
    SCAN_ARGS+=("--scan-acl")
fi
if [[ $SCAN_XATTRS -eq 1 ]]; then
    SCAN_ARGS+=("--scan-xattrs")
fi
if [[ $SCAN_HARDLINKS -eq 1 ]]; then
    SCAN_ARGS+=("--scan-hardlinks")
fi

# Run fsscan
echo "Running fsscan..."
"$FSSCAN" \
    -c "$CTRL_DIR" \
    -m "$META_DIR" \
    -w "$SCAN_WORKERS" \
    -W "$SCAN_WRITERS" \
    "${SCAN_ARGS[@]}" \
    "${INPUT_PATHS[@]}"

CTRL_FILE="${META_DIR}/ctrl.txt"
if [[ ! -f "$CTRL_FILE" ]]; then
    error_exit "Control file was not created: $CTRL_FILE"
fi

print_success "Scan completed successfully."
echo "  Control file: $CTRL_FILE"

# =============================================================================
# STEP 2: Backup
# =============================================================================
print_section "STEP 2: Running Backup"

mkdir -p "$OUTPUT_DIR"

# Backup each input directory
for ((i=0; i<${#INPUT_PATHS[@]}; i++)); do
    input_path="${INPUT_PATHS[$i]}"
    dir_name=$(basename "$input_path")
    target_subdir="${OUTPUT_DIR}/${dir_name}"
    
    echo ""
    echo "Backing up:"
    echo "  Source: $input_path"
    echo "  Target: $target_subdir"
    
    mkdir -p "$target_subdir"
    
    BACKUP_ARGS=()
    if [[ $VERBOSE -eq 1 ]]; then
        BACKUP_ARGS+=("-v")
    fi
    if [[ $BACKUP_HARDLINKS -eq 1 ]]; then
        BACKUP_ARGS+=("--hardlink")
    fi
    
    # Add ctrl-dir if hardlink backup is enabled
    if [[ $BACKUP_HARDLINKS -eq 1 ]]; then
        BACKUP_ARGS+=("--ctrl-dir" "$CTRL_DIR")
    fi
    
    "$FSBACKUP" \
        -s "$input_path" \
        -t "$target_subdir" \
        -m "$META_DIR" \
        -c "$CTRL_FILE" \
        "${BACKUP_ARGS[@]}"
    
    print_success "Backup completed for: $dir_name"
done

# =============================================================================
# STEP 3: Diff Verification
# =============================================================================
if [[ $SKIP_DIFF -eq 0 ]]; then
    print_section "STEP 3: Verifying Backup with Diff"
    
    ALL_PASSED=1
    
    for ((i=0; i<${#INPUT_PATHS[@]}; i++)); do
        input_path="${INPUT_PATHS[$i]}"
        dir_name=$(basename "$input_path")
        target_subdir="${OUTPUT_DIR}/${dir_name}"
        
        echo ""
        echo "Verifying:"
        echo "  Source: $input_path"
        echo "  Target: $target_subdir"
        
        DIFF_ARGS=()
        if [[ $VERBOSE -eq 1 ]]; then
            DIFF_ARGS+=("-v")
        fi
        if [[ $SCAN_ACL -eq 1 ]]; then
            DIFF_ARGS+=("--compare-acl")
        fi
        if [[ $SCAN_XATTRS -eq 1 ]]; then
            DIFF_ARGS+=("--compare-xattrs")
        fi
        
        if "$FSDIFF" \
            --source "$input_path" \
            --target "$target_subdir" \
            "${DIFF_ARGS[@]}"; then
            print_success "Diff verification passed for: $dir_name"
        else
            print_warning "Diff verification failed for: $dir_name"
            ALL_PASSED=0
        fi
    done
    
    if [[ $ALL_PASSED -eq 0 ]]; then
        print_section "TEST RESULTS"
        echo "Scan:    PASSED"
        echo "Backup:  PASSED"
        echo "Diff:    FAILED"
        error_exit "Some diff verifications failed."
    fi
else
    print_section "STEP 3: Skipping Diff Verification"
    echo "Diff verification skipped (--skip-diff)"
fi

# =============================================================================
# Test Complete
# =============================================================================
print_section "TEST RESULTS"
print_success "Scan:    PASSED"
print_success "Backup:  PASSED"
if [[ $SKIP_DIFF -eq 0 ]]; then
    print_success "Diff:    PASSED"
fi
print_success "All tests PASSED!"

if [[ $KEEP_WORK_DIR -eq 1 ]]; then
    echo ""
    echo "Working directory preserved: $WORK_DIR"
fi

exit 0
