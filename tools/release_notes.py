#!/usr/bin/env python3
"""Extract one version section from CHANGELOG.md.

Usage: release_notes.py v1.3.6 notes.md
The heading must be exactly `## 1.3.6` (optional square brackets).
Issue numbers stay as written, for example `#12`.
"""
import pathlib
import sys


def section(text: str, version: str) -> str:
    version = version[1:] if version.startswith("v") else version
    lines = text.splitlines()
    start = None
    for i, line in enumerate(lines):
        if not line.startswith("## "):
            continue
        heading = line[3:].strip().strip("[]")
        if heading == version:
            start = i + 1
            break
    if start is None:
        raise SystemExit(f"CHANGELOG.md has no section '## {version}'")
    body = []
    for line in lines[start:]:
        if line.startswith("## "):
            break
        body.append(line)
    notes = "\n".join(body).strip()
    if not notes:
        raise SystemExit(f"CHANGELOG.md section '## {version}' is empty")
    return notes + "\n"


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit("usage: release_notes.py <version-or-tag> <output>")
    notes = section(pathlib.Path("CHANGELOG.md").read_text(encoding="utf-8"), sys.argv[1])
    pathlib.Path(sys.argv[2]).write_text(notes, encoding="utf-8")


if __name__ == "__main__":
    main()
