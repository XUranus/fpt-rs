#!/bin/bash
#
# Create test filesets with special files for backup testing
# Includes: symlinks, files with ACLs, files with xattrs, sparse files
#

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Default output directory
OUTPUT_DIR="${1:-/tmp/special_files_test}"

echo "Creating special files test set in: $OUTPUT_DIR"

# Clean up if exists
rm -rf "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR"

# =============================================================================
# 1. Create symlinks
# =============================================================================
echo "Creating symlinks..."
mkdir -p "$OUTPUT_DIR/symlinks"

# File symlink
echo "This is a regular file" > "$OUTPUT_DIR/symlinks/target_file.txt"
ln -s "target_file.txt" "$OUTPUT_DIR/symlinks/link_to_file"

# Directory symlink
mkdir -p "$OUTPUT_DIR/symlinks/target_dir"
echo "File in target dir" > "$OUTPUT_DIR/symlinks/target_dir/file_in_dir.txt"
ln -s "target_dir" "$OUTPUT_DIR/symlinks/link_to_dir"

# Absolute path symlink
ln -s "$OUTPUT_DIR/symlinks/target_file.txt" "$OUTPUT_DIR/symlinks/absolute_link"

# Broken symlink
ln -s "nonexistent_file" "$OUTPUT_DIR/symlinks/broken_link"

echo -e "${GREEN}Created symlinks:${NC}"
ls -la "$OUTPUT_DIR/symlinks/"

# =============================================================================
# 2. Create files with extended attributes (xattrs)
# =============================================================================
echo ""
echo "Creating files with xattrs..."
mkdir -p "$OUTPUT_DIR/xattrs"

# Create files
echo "File with xattrs 1" > "$OUTPUT_DIR/xattrs/file1.txt"
echo "File with xattrs 2" > "$OUTPUT_DIR/xattrs/file2.txt"

# Set xattrs (requires user namespace attributes on most systems)
if command -v setfattr &> /dev/null; then
    setfattr -n user.comment -v "Test comment for file1" "$OUTPUT_DIR/xattrs/file1.txt" 2>/dev/null || true
    setfattr -n user.checksum -v "abc123def456" "$OUTPUT_DIR/xattrs/file1.txt" 2>/dev/null || true
    setfattr -n user.custom -v "Custom value here" "$OUTPUT_DIR/xattrs/file2.txt" 2>/dev/null || true
    echo -e "${GREEN}Set xattrs using setfattr${NC}"
else
    echo -e "${YELLOW}setfattr not found, trying python...${NC}"
    python3 -c "
import os
import xattr
for f in ['$OUTPUT_DIR/xattrs/file1.txt', '$OUTPUT_DIR/xattrs/file2.txt']:
    try:
        x = xattr.xattr(f)
        x.set('user.comment', b'Test comment')
        x.set('user.checksum', b'abc123')
    except Exception as e:
        print(f'Warning: Could not set xattr on {f}: {e}')
" 2>/dev/null || echo -e "${YELLOW}xattr module not available, skipping xattrs${NC}"
fi

echo -e "${GREEN}Files with xattrs:${NC}"
getfattr -d "$OUTPUT_DIR/xattrs/"* 2>/dev/null || ls -la "$OUTPUT_DIR/xattrs/"

# =============================================================================
# 3. Create files with ACLs
# =============================================================================
echo ""
echo "Creating files with ACLs..."
mkdir -p "$OUTPUT_DIR/acls"

# Create files
echo "File with ACL 1" > "$OUTPUT_DIR/acls/file1.txt"
echo "File with ACL 2" > "$OUTPUT_DIR/acls/file2.txt"
mkdir -p "$OUTPUT_DIR/acls/dir_with_acl"
echo "File in dir" > "$OUTPUT_DIR/acls/dir_with_acl/file.txt"

# Set ACLs if setfacl is available
if command -v setfacl &> /dev/null; then
    # Set ACL for user
    setfacl -m u:$(id -u):rwx "$OUTPUT_DIR/acls/file1.txt" 2>/dev/null || true
    setfacl -m u:$(id -u):rw- "$OUTPUT_DIR/acls/file2.txt" 2>/dev/null || true
    
    # Set default ACL on directory
    setfacl -m d:u:$(id -u):rwx "$OUTPUT_DIR/acls/dir_with_acl" 2>/dev/null || true
    setfacl -m d:g:$(id -g):rx "$OUTPUT_DIR/acls/dir_with_acl" 2>/dev/null || true
    
    echo -e "${GREEN}Set ACLs using setfacl${NC}"
else
    echo -e "${YELLOW}setfacl not available, skipping ACLs${NC}"
fi

echo -e "${GREEN}Files with ACLs:${NC}"
getfacl "$OUTPUT_DIR/acls/"* 2>/dev/null || ls -la "$OUTPUT_DIR/acls/"

# =============================================================================
# 4. Create sparse files
# =============================================================================
echo ""
echo "Creating sparse files..."
mkdir -p "$OUTPUT_DIR/sparse"

# Create a sparse file using dd with seek
# This creates a 10MB file with only the first 1KB and last 1KB allocated
dd if=/dev/zero of="$OUTPUT_DIR/sparse/sparse_file1.bin" bs=1K count=1 2>/dev/null
dd if=/dev/zero of="$OUTPUT_DIR/sparse/sparse_file1.bin" bs=1K seek=10239 count=1 conv=notrunc 2>/dev/null

# Create another sparse file with holes in the middle
# 100KB data, 900KB hole, 100KB data
dd if=/dev/urandom of="$OUTPUT_DIR/sparse/sparse_file2.bin" bs=1K count=100 2>/dev/null
dd if=/dev/urandom of="$OUTPUT_DIR/sparse/sparse_file2.bin" bs=1K seek=1000 count=100 conv=notrunc 2>/dev/null

echo -e "${GREEN}Sparse files created:${NC}"
ls -lsh "$OUTPUT_DIR/sparse/"
echo "Apparent size vs actual blocks:"
du -h "$OUTPUT_DIR/sparse/"* 2>/dev/null || true

# =============================================================================
# 5. Create combined special file (has multiple attributes)
# =============================================================================
echo ""
echo "Creating combined special file..."
mkdir -p "$OUTPUT_DIR/combined"

echo "File with multiple special attributes" > "$OUTPUT_DIR/combined/special_file.txt"

# Set xattrs
if command -v setfattr &> /dev/null; then
    setfattr -n user.multi -v "value1" "$OUTPUT_DIR/combined/special_file.txt" 2>/dev/null || true
fi

# Set ACL
if command -v setfacl &> /dev/null; then
    setfacl -m u:$(id -u):rw- "$OUTPUT_DIR/combined/special_file.txt" 2>/dev/null || true
fi

# Create a symlink to it
ln -s "special_file.txt" "$OUTPUT_DIR/combined/link_to_special"

echo -e "${GREEN}Combined special file:${NC}"
ls -la "$OUTPUT_DIR/combined/"

# =============================================================================
# Summary
# =============================================================================
echo ""
echo "============================================================"
echo "Special files test set created in: $OUTPUT_DIR"
echo "============================================================"
echo ""
echo "Directory structure:"
find "$OUTPUT_DIR" -type f -o -type l -o -type d | sort
echo ""
echo "Summary:"
echo "  - Symlinks: $(find "$OUTPUT_DIR" -type l | wc -l)"
echo "  - Regular files: $(find "$OUTPUT_DIR" -type f | wc -l)"
echo "  - Directories: $(find "$OUTPUT_DIR" -type d | wc -l)"
echo ""
echo "To test backup:"
echo "  ./scripts/bifrost_test.sh -i $OUTPUT_DIR -o /tmp/backup_test"
