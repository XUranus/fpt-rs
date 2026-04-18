#!/usr/bin/env python3

from __future__ import annotations

import shutil
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent
DEFAULT_OUT = ROOT / "_wiki"


def title_from_path(path: Path) -> str:
    if path.name.lower() == "readme.md":
        return path.parent.name if path.parent != ROOT else "Docs"
    stem = path.stem.replace("-", " ").replace("_", " ")
    return " ".join(word.capitalize() for word in stem.split())


def iter_markdown_files() -> list[Path]:
    files = []
    for path in ROOT.rglob("*.md"):
        if path.name in {"Home.md", "_Sidebar.md"}:
            continue
        if "_wiki" in path.parts:
            continue
        files.append(path)
    return sorted(files)


def write_sidebar(out_dir: Path, files: list[Path]) -> None:
    lines = ["# Bifrost Docs", "", "* [Home](Home)"]
    for path in files:
        rel = path.relative_to(ROOT)
        if rel.name == "README.md":
            continue
        link = rel.with_suffix("")
        title = title_from_path(rel)
        lines.append(f"* [{title}]({link.as_posix()})")
    (out_dir / "_Sidebar.md").write_text("\n".join(lines) + "\n", encoding="utf-8")


def write_home(out_dir: Path) -> None:
    readme = ROOT / "README.md"
    if readme.exists():
        text = readme.read_text(encoding="utf-8")
    else:
        text = "# Bifrost Docs\n"
    (out_dir / "Home.md").write_text(text, encoding="utf-8")


def copy_docs(out_dir: Path, files: list[Path]) -> None:
    for src in files:
        rel = src.relative_to(ROOT)
        dst = out_dir / rel
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(src, dst)


def main() -> int:
    out_dir = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else DEFAULT_OUT
    if out_dir.exists():
        shutil.rmtree(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    files = iter_markdown_files()
    copy_docs(out_dir, files)
    write_home(out_dir)
    write_sidebar(out_dir, files)

    print(f"Wiki built at {out_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
