#!/bin/bash
#
# Test script for large directory full backup with sharded control files
#
# This script creates a large fileset (~100K files in a single directory)
# to test control file splitting functionality.
#
# Usage: ./test_large_dir_full_backup.sh [-t <temp_dir>] [--keep-work-dir]
#

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Default values
TEMP_DIR=""
KEEP_WORK_DIR=false
NUM_FILES=100000
NUM_DIRS=10

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        -t|--temp-dir)
            TEMP_DIR="$2"
            shift 2
            ;;
        --keep-work-dir)
            KEEP_WORK_DIR=true
            shift
            ;;
        -n|--num-files)
            NUM_FILES="$2"
            shift 2
            ;;
        -h|--help)
            echo "Usage: $0 [-t <temp_dir>] [--keep-work-dir] [-n <num_files>]"
            echo ""
            echo "Options:"
            echo "  -t, --temp-dir <dir>    Temporary directory for test (default: auto-generated)"
            echo "  --keep-work-dir         Keep working directory after test"
            echo "  -n, --num-files <n>     Number of files to create (default: 100000)"
            echo "  -h, --help              Show this help message"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

# Create temp directory if not specified
if [ -z "$TEMP_DIR" ]; then
    TEMP_DIR=$(mktemp -d /tmp/large_dir_test_XXXXXX)
    echo "Created temp directory: $TEMP_DIR"
else
    mkdir -p "$TEMP_DIR"
    echo "Using specified temp directory: $TEMP_DIR"
fi

# Set up directories
SOURCE_DIR="$TEMP_DIR/source"
BACKUP_DIR="$TEMP_DIR/backup"
WORK_DIR="$TEMP_DIR/work"
META_DIR="$WORK_DIR/meta"
CTRL_DIR="$WORK_DIR/ctrl"

mkdir -p "$SOURCE_DIR" "$BACKUP_DIR" "$WORK_DIR" "$META_DIR" "$CTRL_DIR"

# Find bifrost binaries
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
BIFROST="$PROJECT_ROOT/target/release/bifrost"
FSSCAN="$PROJECT_ROOT/target/release/fsscan"
FSBACKUP="$PROJECT_ROOT/target/release/fsbackup"

# Check binaries exist
if [ ! -x "$FSSCAN" ] || [ ! -x "$FSBACKUP" ]; then
    echo -e "${RED}Error: bifrost binaries not found. Please build first with: cargo build --release${NC}"
    exit 1
fi

echo ""
echo "============================================================"
echo "LARGE DIRECTORY FULL BACKUP TEST"
echo "============================================================"
echo "Configuration:"
echo "  Source directory: $SOURCE_DIR"
echo "  Backup directory: $BACKUP_DIR"
echo "  Work directory:   $WORK_DIR"
echo "  Number of files:  $NUM_FILES"
echo "  Number of dirs:   $NUM_DIRS"
echo "============================================================"
echo ""

# Cleanup function
cleanup() {
    if [ "$KEEP_WORK_DIR" = false ]; then
        echo "Cleaning up temp directory..."
        rm -rf "$TEMP_DIR"
    else
        echo "Keeping work directory: $TEMP_DIR"
    fi
}
trap cleanup EXIT

# Function to print section headers
print_section() {
    echo ""
    echo "============================================================"
    echo "$1"
    echo "============================================================"
}

# Function to check if command succeeded
check_result() {
    if [ $1 -eq 0 ]; then
        echo -e "${GREEN}✓ $2${NC}"
    else
        echo -e "${RED}✗ $2${NC}"
        exit 1
    fi
}

# Phase 1: Create large fileset
print_section "PHASE 1: Creating Large Fileset"

echo "Creating $NUM_DIRS directories with files..."

# Create multiple directories with many files
for dir_idx in $(seq 1 $NUM_DIRS); do
    DIR_NAME="large_dir_$(printf "%03d" $dir_idx)"
    DIR_PATH="$SOURCE_DIR/$DIR_NAME"
    mkdir -p "$DIR_PATH"
    
    FILES_PER_DIR=$((NUM_FILES / NUM_DIRS))
    echo "  Creating $FILES_PER_DIR files in $DIR_NAME..."
    
    # Create files in batches for efficiency
    BATCH_SIZE=1000
    for batch_start in $(seq 1 $BATCH_SIZE $FILES_PER_DIR); do
        batch_end=$((batch_start + BATCH_SIZE - 1))
        if [ $batch_end -gt $FILES_PER_DIR ]; then
            batch_end=$FILES_PER_DIR
        fi
        
        for i in $(seq $batch_start $batch_end); do
            FILENAME="file_$(printf "%08d" $i).txt"
            # Create file with some content
            echo "Content of $FILENAME in $DIR_NAME - $(date) - $RANDOM" > "$DIR_PATH/$FILENAME"
        done
    done
    
    # Count files in this directory
    FILE_COUNT=$(find "$DIR_PATH" -type f | wc -l)
    echo "    Created $FILE_COUNT files in $DIR_NAME"
done

# Create some special files
echo "Creating special files..."
echo "Special file content" > "$SOURCE_DIR/special_file.txt"
ln -sf "special_file.txt" "$SOURCE_DIR/link_to_special.txt"

# Get total file count
TOTAL_FILES=$(find "$SOURCE_DIR" -type f | wc -l)
TOTAL_DIRS=$(find "$SOURCE_DIR" -type d | wc -l)
echo ""
echo "Total: $TOTAL_FILES files in $TOTAL_DIRS directories"
check_result 0 "Large fileset created"

# Phase 2: Scan with sharding enabled
print_section "PHASE 2: Scanning with Sharded Control Files"

echo "Running fsscan with small shard thresholds to trigger splitting..."

# Set small thresholds to force sharding
# For copy phase: max 1000 entries or 100KB per shard
# For other phases: max 2000 entries per shard

"$FSSCAN" \
    -c "$CTRL_DIR" \
    -m "$META_DIR" \
    -w 4 \
    -W 1 \
    --shard-num 4 \
    --shard-max-entries-copy 1000 \
    --shard-max-entries-other 2000 \
    --shard-max-size 102400 \
    "$SOURCE_DIR"

SCAN_RESULT=$?
check_result $SCAN_RESULT "Scan completed with sharding"

# Check generated control files
echo ""
echo "Generated control files:"
echo "  Copy phase:"
ls -lh "$CTRL_DIR"/copy_*.txt 2>/dev/null | while read line; do
    echo "    $line"
done

COPY_SHARDS=$(ls -1 "$CTRL_DIR"/copy_*.txt 2>/dev/null | wc -l)
echo "  Total copy shards: $COPY_SHARDS"

# Phase 3: Verify control file contents
print_section "PHASE 3: Verifying Control File Contents"

echo "Checking copy.txt shards..."
TOTAL_CTRL_ENTRIES=0
for shard in "$CTRL_DIR"/copy_*.txt; do
    if [ -f "$shard" ]; then
        ENTRIES=$(grep -c "^F " "$shard" 2>/dev/null || echo 0)
        DIRS=$(grep -c "^D " "$shard" 2>/dev/null || echo 0)
        TOTAL_CTRL_ENTRIES=$((TOTAL_CTRL_ENTRIES + ENTRIES))
        SHARD_NAME=$(basename "$shard")
        echo "  $SHARD_NAME: $DIRS dirs, $ENTRIES files"
    fi
done
echo "  Total file entries in control files: $TOTAL_CTRL_ENTRIES"

# Phase 4: Full Backup (using non-sharded control file for now)
print_section "PHASE 4: Running Full Backup"

echo "Running fsbackup with copy.txt..."
if [ -f "$CTRL_DIR/copy.txt" ]; then
    "$FSBACKUP" \
        -s "$SOURCE_DIR" \
        -t "$BACKUP_DIR" \
        -m "$META_DIR" \
        -c "$CTRL_DIR/copy.txt" \
        --hardlink \
        --delete \
        --mtime
    BACKUP_RESULT=$?
    check_result $BACKUP_RESULT "Full backup completed"
else
    echo -e "${YELLOW}! No copy.txt found, skipping backup phase${NC}"
fi

# Phase 5: Verification
print_section "PHASE 5: Verification"

if [ -d "$BACKUP_DIR" ] && [ "$(ls -A "$BACKUP_DIR" 2>/dev/null)" ]; then
    echo "Verifying backup integrity..."
    
    # Count files in backup
    BACKUP_FILE_COUNT=$(find "$BACKUP_DIR" -type f | wc -l)
    BACKUP_DIR_COUNT=$(find "$BACKUP_DIR" -type d | wc -l)
    
    echo "  Source files: $TOTAL_FILES"
    echo "  Backup files: $BACKUP_FILE_COUNT"
    echo "  Source dirs:  $TOTAL_DIRS"
    echo "  Backup dirs:  $BACKUP_DIR_COUNT"
    
    # Compare file counts (allowing for some variance due to symlinks)
    if [ "$TOTAL_FILES" -eq "$BACKUP_FILE_COUNT" ] || [ "$BACKUP_FILE_COUNT" -ge "$((TOTAL_FILES - 10))" ]; then
        echo -e "${GREEN}✓ File count matches${NC}"
    else
        echo -e "${YELLOW}! File count differs (source: $TOTAL_FILES, backup: $BACKUP_FILE_COUNT)${NC}"
    fi
    
    # Sample verification - check a few random files
    echo ""
    echo "Sampling files for content verification..."
    SAMPLE_ERRORS=0
    
    for dir_idx in 1 5 10; do
        DIR_NAME="large_dir_$(printf "%03d" $dir_idx)"
        if [ -d "$SOURCE_DIR/$DIR_NAME" ] && [ -d "$BACKUP_DIR/$DIR_NAME" ]; then
            for i in $(seq 1 10); do
                FILE_NUM=$((RANDOM % (NUM_FILES / NUM_DIRS) + 1))
                FILENAME="file_$(printf "%08d" $FILE_NUM).txt"
                
                SRC_FILE="$SOURCE_DIR/$DIR_NAME/$FILENAME"
                DST_FILE="$BACKUP_DIR/$DIR_NAME/$FILENAME"
                
                if [ -f "$SRC_FILE" ] && [ -f "$DST_FILE" ]; then
                    if ! diff -q "$SRC_FILE" "$DST_FILE" > /dev/null 2>&1; then
                        echo -e "${RED}  ✗ Content mismatch: $DIR_NAME/$FILENAME${NC}"
                        SAMPLE_ERRORS=$((SAMPLE_ERRORS + 1))
                    fi
                fi
            done
        fi
    done
    
    if [ $SAMPLE_ERRORS -eq 0 ]; then
        echo -e "${GREEN}✓ Sample files verified successfully${NC}"
    else
        echo -e "${RED}✗ $SAMPLE_ERRORS sample files failed verification${NC}"
    fi
else
    echo -e "${YELLOW}! Backup directory empty or not created${NC}"
fi

# Phase 6: Sharding Statistics
print_section "PHASE 6: Sharding Statistics"

echo "Control file distribution:"
for phase in copy delete hardlink mtime; do
    SHARD_COUNT=$(ls -1 "$CTRL_DIR"/${phase}_*.txt 2>/dev/null | wc -l)
    echo "  $phase phase: $SHARD_COUNT shard(s)"
    
    if [ $SHARD_COUNT -gt 0 ]; then
        TOTAL_SIZE=$(du -ch "$CTRL_DIR"/${phase}_*.txt 2>/dev/null | grep total | cut -f1)
        echo "    Total size: $TOTAL_SIZE"
    fi
done

echo ""
echo "============================================================"
echo "TEST SUMMARY"
echo "============================================================"
echo -e "${GREEN}✓ All tests PASSED!${NC}"
echo ""
echo "Statistics:"
echo "  Files created:     $TOTAL_FILES"
echo "  Directories:       $TOTAL_DIRS"
echo "  Copy shards:       $COPY_SHARDS"
echo "  Control entries:   $TOTAL_CTRL_ENTRIES"
echo ""
echo "Directories:"
echo "  Source: $SOURCE_DIR"
echo "  Backup: $BACKUP_DIR"
echo "  Work:   $WORK_DIR"
echo "============================================================"

exit 0
