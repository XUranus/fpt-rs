# Optimization Plan

## Deferred: Reuse SMB Scanner Metadata During Backup

Do not implement this now. This records the idea for later work.

SMB scan and backup are currently mostly independent. The scanner already pays
the cost to traverse directories and collect entry metadata, then backup later
replays control files and performs target-side directory/file operations
independently. For SMB, `CREATE`/open operations are expensive, so reusing scan
knowledge can reduce backup overhead.

Potential optimizations:

- Build a target directory-create plan from scan/control metadata and execute it
  once before file copy tasks.
- Avoid per-file parent directory checks for SMB target writes when the precreate
  plan has already succeeded.
- Use scanner-known file sizes to schedule large files earlier and group small
  files more efficiently.
- For aggregate mode, use scanner directory/file grouping to choose blob
  placement before copy starts.
- For restore/diff, trust copy manifest/control metadata where possible to avoid
  redundant remote stats.

Most practical first step:

- Add an SMB-target directory planning phase before file copies.
- Log directory precreate time separately from file copy time.
- Keep fallback parent creation in the writer for robustness, but make it a cold
  path.
