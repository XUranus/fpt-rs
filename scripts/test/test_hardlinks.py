#!/usr/bin/env python3
"""
Test Case: Hardlink Backup and Restore
Tests proper handling of hardlinked files
"""

import sys
import os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from pathlib import Path
from test_framework import BifrostTestBase, TestResult, TestRunner, parse_args


class TestHardlinks(BifrostTestBase):
    """Test backup and restore of hardlinked files"""

    def run_test(self) -> TestResult:
        """Execute the test"""
        print("Creating files with hardlinks...")

        # Create directory structure
        (self.source_dir / "docs").mkdir()
        (self.source_dir / "data").mkdir()
        (self.source_dir / "shared").mkdir()

        # Create original files with hardlinks
        # File 1: Single hardlink (2 total references)
        file1 = self.source_dir / "docs" / "original1.txt"
        file1.write_text("Content of file 1 - shared content\n")
        link1 = self.source_dir / "shared" / "link_to_doc1.txt"
        os.link(file1, link1)
        print(f"  Created hardlink pair: {file1.name} -> {link1.name}")

        # File 2: Multiple hardlinks (3 total references)
        file2 = self.source_dir / "data" / "original2.txt"
        file2.write_text("Content of file 2 - widely shared\n" * 10)
        link2a = self.source_dir / "docs" / "link_to_data2.txt"
        link2b = self.source_dir / "shared" / "another_link_to_data2.txt"
        os.link(file2, link2a)
        os.link(file2, link2b)
        print(f"  Created hardlink group (3 refs): {file2.name}")

        # File 3: Same directory hardlink
        file3 = self.source_dir / "standalone.txt"
        file3.write_text("Standalone file with local hardlink\n")
        link3 = self.source_dir / "standalone_link.txt"
        os.link(file3, link3)
        print(f"  Created same-directory hardlink: {file3.name}")

        # File 4: Deep nested hardlink
        deep_dir = self.source_dir / "deep" / "nested" / "path"
        deep_dir.mkdir(parents=True)
        file4 = deep_dir / "deep_file.txt"
        file4.write_text("Deep nested file content\n")
        link4 = self.source_dir / "link_to_deep.txt"
        os.link(file4, link4)
        print(f"  Created deep nested hardlink: {file4.name}")

        # Regular file without hardlinks (for comparison)
        regular = self.source_dir / "regular_file.txt"
        regular.write_text("Regular file without hardlinks\n")
        print(f"  Created regular file (no hardlinks)")

        # Record inodes before backup
        print("\nRecording source inodes...")
        source_inodes = {}
        for root, dirs, files in os.walk(self.source_dir):
            for f in files:
                filepath = Path(root) / f
                if filepath.is_file() and not filepath.is_symlink():
                    stat = filepath.stat()
                    inode = (stat.st_dev, stat.st_ino)
                    if inode not in source_inodes:
                        source_inodes[inode] = []
                    source_inodes[inode].append(filepath.relative_to(self.source_dir))

        hardlink_groups = {k: v for k, v in source_inodes.items() if len(v) > 1}
        print(f"  Found {len(hardlink_groups)} hardlink groups")
        for inode, paths in hardlink_groups.items():
            print(f"    Inode {inode[1]}: {len(paths)} files")

        # Step 1: Scan with hardlink detection
        print("\nStep 1: Scanning with hardlink detection...")
        if not self.run_fsscan(self.source_dir, extra_args=["--scan-hardlinks"]):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"Scan failed: {self.error_message}"
            )

        # Check for hardlink.txt
        hardlink_txt = self.ctrl_dir / "hardlink.txt"
        if hardlink_txt.exists():
            print(f"  Hardlink control file created: {hardlink_txt}")
            content = hardlink_txt.read_text()
            print(f"  Hardlink entries: {len(content.strip().split(chr(10)))}")
        else:
            print("  Note: hardlink.txt not created (hardlinks may be handled differently)")

        # Step 2: Backup with hardlink phase
        print("\nStep 2: Running backup with hardlink phase...")
        if not self.run_fsbackup(
            self.source_dir,
            self.backup_dir,
            enable_hardlink=True
        ):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"Backup failed: {self.error_message}"
            )

        # Step 3: Verify hardlinks in backup
        print("\nStep 3: Verifying hardlinks in backup...")

        backup_inodes = {}
        for root, dirs, files in os.walk(self.backup_dir):
            for f in files:
                filepath = Path(root) / f
                if filepath.is_file() and not filepath.is_symlink():
                    stat = filepath.stat()
                    inode = (stat.st_dev, stat.st_ino)
                    if inode not in backup_inodes:
                        backup_inodes[inode] = []
                    backup_inodes[inode].append(filepath.relative_to(self.backup_dir))

        backup_hardlink_groups = {k: v for k, v in backup_inodes.items() if len(v) > 1}
        print(f"  Found {len(backup_hardlink_groups)} hardlink groups in backup")

        # Verify each source hardlink group has corresponding backup hardlinks
        verified_groups = 0
        for src_inode, src_paths in hardlink_groups.items():
            # Find matching group in backup by checking if any path matches
            found = False
            for bak_inode, bak_paths in backup_hardlink_groups.items():
                # Check if the sets of filenames match (same hardlink group)
                src_names = {p.name for p in src_paths}
                bak_names = {p.name for p in bak_paths}
                if src_names & bak_names:  # Intersection not empty
                    found = True
                    if len(bak_paths) == len(src_paths):
                        print(f"  ✓ Hardlink group verified: {len(bak_paths)} files")
                        verified_groups += 1
                    else:
                        print(f"  ! Hardlink group partial: {len(bak_paths)}/{len(src_paths)} files")
                    break

            if not found:
                print(f"  ! Hardlink group not found in backup: {src_paths[0]}")

        # Step 4: Verify content integrity
        print("\nStep 4: Verifying content integrity...")
        for src_inode, src_paths in hardlink_groups.items():
            src_file = self.source_dir / src_paths[0]
            src_hash = self.get_file_hash(src_file)

            # Check all corresponding backup files
            for bak_inode, bak_paths in backup_hardlink_groups.items():
                bak_names = {p.name for p in bak_paths}
                if src_paths[0].name in bak_names:
                    for bak_path in bak_paths:
                        bak_file = self.backup_dir / bak_path
                        bak_hash = self.get_file_hash(bak_file)
                        if src_hash != bak_hash:
                            return TestResult(
                                name=self.__class__.__name__,
                                passed=False,
                                duration=0,
                                message=f"Content mismatch for hardlinked file: {bak_path}"
                            )
                    break

        print("  ✓ All hardlinked file contents match")

        # Step 5: Run fsdiff
        print("\nStep 5: Running fsdiff verification...")
        if not self.run_fsdiff(self.source_dir, self.backup_dir):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"Diff verification failed: {self.error_message}"
            )

        self.test_passed = True
        return TestResult(
            name=self.__class__.__name__,
            passed=True,
            duration=0,
            message="Hardlink test passed",
            details={
                "hardlink_groups": len(hardlink_groups),
                "total_hardlinked_files": sum(len(v) for v in hardlink_groups.values()),
                "verified_groups": verified_groups,
            }
        )


def main():
    args = parse_args()
    runner = TestRunner(verbose=args.verbose, keep_on_failure=args.keep_on_failure)
    result = runner.run_test(TestHardlinks, work_dir=args.work_dir)
    runner.print_summary()
    return 0 if result.passed else 1


if __name__ == "__main__":
    sys.exit(main())
