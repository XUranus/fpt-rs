#!/bin/bash
#
# Test script for hardlink backup functionality
# Creates files with hardlinks to test the hardlink backup and restore process
#

set -e

# Default output directory
OUTPUT_DIR="${1:-/tmp/bifrost_hardlink_test}"

echo "Creating hardlink test files in: $OUTPUT_DIR"

# Clean up previous test data if exists
if [ -d "$OUTPUT_DIR" ]; then
    echo "Cleaning up previous test data..."
    rm -rf "$OUTPUT_DIR"
fi

# Create directory structure
mkdir -p "$OUTPUT_DIR/regular_files"
mkdir -p "$OUTPUT_DIR/hardlinks/simple"
mkdir -p "$OUTPUT_DIR/hardlinks/cross_dir/subdir1"
mkdir -p "$OUTPUT_DIR/hardlinks/cross_dir/subdir2"
mkdir -p "$OUTPUT_DIR/hardlinks/multiple"
mkdir -p "$OUTPUT_DIR/hardlinks/large"

echo "Creating regular files (no hardlinks)..."
echo "This is a regular file with no hardlinks" > "$OUTPUT_DIR/regular_files/file1.txt"
echo "Another regular file" > "$OUTPUT_DIR/regular_files/file2.txt"
dd if=/dev/urandom of="$OUTPUT_DIR/regular_files/random.bin" bs=1K count=10 2>/dev/null

echo "Creating simple hardlink pair..."
echo "Content shared between hardlinks" > "$OUTPUT_DIR/hardlinks/simple/original.txt"
ln "$OUTPUT_DIR/hardlinks/simple/original.txt" "$OUTPUT_DIR/hardlinks/simple/link.txt"

echo "Creating cross-directory hardlinks..."
echo "Cross-directory shared content" > "$OUTPUT_DIR/hardlinks/cross_dir/subdir1/file_a.txt"
ln "$OUTPUT_DIR/hardlinks/cross_dir/subdir1/file_a.txt" "$OUTPUT_DIR/hardlinks/cross_dir/subdir2/file_b.txt"
ln "$OUTPUT_DIR/hardlinks/cross_dir/subdir1/file_a.txt" "$OUTPUT_DIR/hardlinks/cross_dir/file_c.txt"

echo "Creating multiple hardlinks (5 links to same inode)..."
echo "Content shared by 5 hardlinks" > "$OUTPUT_DIR/hardlinks/multiple/base.txt"
for i in 1 2 3 4; do
    ln "$OUTPUT_DIR/hardlinks/multiple/base.txt" "$OUTPUT_DIR/hardlinks/multiple/link_$i.txt"
done

echo "Creating large file with hardlinks..."
dd if=/dev/urandom of="$OUTPUT_DIR/hardlinks/large/large_file.bin" bs=1M count=5 2>/dev/null
ln "$OUTPUT_DIR/hardlinks/large/large_file.bin" "$OUTPUT_DIR/hardlinks/large/large_link.bin"

echo "Creating nested directory structure with hardlinks..."
mkdir -p "$OUTPUT_DIR/hardlinks/nested/level1/level2"
echo "Nested hardlink content" > "$OUTPUT_DIR/hardlinks/nested/root.txt"
ln "$OUTPUT_DIR/hardlinks/nested/root.txt" "$OUTPUT_DIR/hardlinks/nested/level1/level1_link.txt"
ln "$OUTPUT_DIR/hardlinks/nested/root.txt" "$OUTPUT_DIR/hardlinks/nested/level1/level2/deep_link.txt"

echo ""
echo "=== Hardlink Test Files Created ==="
echo ""

# Display statistics
echo "Directory structure:"
find "$OUTPUT_DIR" -type f -exec ls -li {} \; | sort -n | while read -r line; do
    echo "  $line"
done

echo ""
echo "Hardlink groups (same inode):"
find "$OUTPUT_DIR" -type f -exec ls -i {} \; | sort -n | awk '
{
    inode = $1
    path = $2
    for (i=3; i<=NF; i++) path = path " " $i
    if (inode in groups) {
        groups[inode] = groups[inode] "\n  " path
        counts[inode]++
    } else {
        groups[inode] = path
        counts[inode] = 1
    }
}
END {
    for (inode in groups) {
        if (counts[inode] > 1) {
            print "Inode " inode " (" counts[inode] " links):"
            print "  " groups[inode]
            print ""
        }
    }
}
'

echo ""
echo "Summary:"
echo "  Total files: $(find "$OUTPUT_DIR" -type f | wc -l)"
echo "  Total directories: $(find "$OUTPUT_DIR" -type d | wc -l)"
echo "  Hardlink groups: $(find "$OUTPUT_DIR" -type f -links +1 | wc -l)"
echo ""
echo "Test files ready in: $OUTPUT_DIR"
