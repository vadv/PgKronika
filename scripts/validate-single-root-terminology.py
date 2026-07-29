#!/usr/bin/env python3
"""Reject retired global-namespace terminology in the current tree."""

from __future__ import annotations

import os
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Iterator


MAX_MATCHES = 10_000
READ_CHUNK_SIZE = 64 * 1024
SKIPPED_DIRECTORIES = frozenset({".git", "target"})


@dataclass(frozen=True, order=True)
class Match:
    path: str
    line: int
    term: str


def forbidden_terms() -> tuple[str, ...]:
    return (
        "source_" + "id",
        "KRONIKA_SOURCE_" + "ID",
        "source_" + "identity",
        "source " + "identity",
        "source " + "ID",
    )


def is_git_repository(root: Path) -> bool:
    result = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "--is-inside-work-tree"],
        check=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    return result.returncode == 0


def git_tracked_files(root: Path) -> Iterator[Path]:
    process = subprocess.Popen(
        ["git", "-C", str(root), "ls-files", "-z"],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    if process.stdout is None:
        raise OSError("git ls-files did not provide stdout")

    pending = b""
    try:
        while chunk := process.stdout.read(READ_CHUNK_SIZE):
            fields = (pending + chunk).split(b"\0")
            pending = fields.pop()
            for field in fields:
                if field:
                    yield root / os.fsdecode(field)
    finally:
        process.stdout.close()
        return_code = process.wait()
    if return_code != 0:
        raise OSError(f"git ls-files exited with status {return_code}")


def filesystem_files(root: Path) -> Iterator[Path]:
    for directory, names, filenames in os.walk(root):
        names[:] = sorted(name for name in names if name not in SKIPPED_DIRECTORIES)
        base = Path(directory)
        for filename in sorted(filenames):
            path = base / filename
            if path.is_file() and not path.is_symlink():
                yield path


def text_files(root: Path) -> Iterator[Path]:
    if is_git_repository(root):
        yield from git_tracked_files(root)
    else:
        yield from filesystem_files(root)


def matches_in_file(
    path: Path,
    relative: str,
    terms: tuple[str, ...],
    limit: int,
) -> list[Match]:
    matches: list[Match] = []
    overlap = max(map(len, terms)) - 1
    tail = ""
    first_line = 1

    try:
        with path.open("r", encoding="utf-8") as handle:
            while chunk := handle.read(READ_CHUNK_SIZE):
                combined = tail + chunk
                searchable_length = max(0, len(combined) - overlap)
                if len(matches) < limit:
                    for term in terms:
                        start = 0
                        while len(matches) < limit:
                            index = combined.find(term, start)
                            if index < 0 or index >= searchable_length:
                                break
                            line = first_line + combined.count("\n", 0, index)
                            matches.append(Match(relative, line, term))
                            start = index + len(term)
                first_line += combined.count("\n", 0, searchable_length)
                tail = combined[searchable_length:]

            if len(matches) < limit:
                for term in terms:
                    start = 0
                    while len(matches) < limit:
                        index = tail.find(term, start)
                        if index < 0:
                            break
                        line = first_line + tail.count("\n", 0, index)
                        matches.append(Match(relative, line, term))
                        start = index + len(term)
    except (OSError, UnicodeDecodeError):
        return []

    return matches


def find_matches(root: Path) -> list[Match]:
    matches: list[Match] = []
    terms = forbidden_terms()
    for path in text_files(root):
        if len(matches) >= MAX_MATCHES or not path.is_file() or path.is_symlink():
            continue
        relative = path.relative_to(root).as_posix()
        matches.extend(
            matches_in_file(
                path,
                relative,
                terms,
                MAX_MATCHES - len(matches),
            )
        )
    return sorted(matches)


def main(argv: list[str]) -> int:
    if len(argv) > 2:
        print("usage: validate-single-root-terminology.py [ROOT]", file=sys.stderr)
        return 2
    root = Path(argv[1] if len(argv) == 2 else Path(__file__).resolve().parent.parent)
    matches = find_matches(root.resolve())
    for match in matches:
        print(f"{match.path}:{match.line}:{match.term}")
    return int(bool(matches))


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
