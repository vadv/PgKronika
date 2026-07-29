#!/usr/bin/env python3
"""Regression tests for the repository terminology guard."""

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent
VALIDATOR = Path(__file__).with_name("validate-single-root-terminology.py")
RETIRED_FIELD = "source_" + "id"
RETIRED_ENV = "KRONIKA_SOURCE_" + "ID"
RETIRED_IDENTITY = "source_" + "identity"
RETIRED_IDENTITY_PHRASE = "source " + "identity"
RETIRED_ID_PHRASE = "source " + "ID"
RETIRED_NODE_FIELD = "node_self_" + "id"
RETIRED_NODE_ENV = "KRONIKA_NODE_SELF_" + "ID"
RETIRED_TERMS = (
    RETIRED_FIELD,
    RETIRED_ENV,
    RETIRED_IDENTITY,
    RETIRED_IDENTITY_PHRASE,
    RETIRED_ID_PHRASE,
    RETIRED_NODE_FIELD,
    RETIRED_NODE_ENV,
)


def run_validator(root: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-B", str(VALIDATOR), str(root)],
        cwd=REPO_ROOT,
        check=False,
        capture_output=True,
        text=True,
    )


class TerminologyGuardTests(unittest.TestCase):
    def test_clean_tree_passes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "clean.txt").write_text("single root\n", encoding="utf-8")

            result = run_validator(root)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")

    def test_each_retired_term_reports_exact_path_and_line(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "nested").mkdir()
            (root / "nested" / "legacy.txt").write_text(
                "first\n" + "\n".join(RETIRED_TERMS) + "\n",
                encoding="utf-8",
            )

            result = run_validator(root)

        self.assertEqual(result.returncode, 1)
        for line, term in enumerate(RETIRED_TERMS, start=2):
            self.assertIn(f"nested/legacy.txt:{line}:{term}", result.stdout)

    def test_metadata_build_outputs_and_binary_files_are_ignored(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for ignored in (".git", "target"):
                path = root / ignored
                path.mkdir()
                (path / "legacy.txt").write_text(RETIRED_FIELD, encoding="utf-8")
            (root / "binary.dat").write_bytes(b"\xff" + RETIRED_ENV.encode())

            result = run_validator(root)

        self.assertEqual(result.returncode, 0, result.stdout)

    def test_git_repository_scans_only_tracked_files(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(
                ["git", "init", "--quiet", str(root)],
                check=True,
                capture_output=True,
                text=True,
            )
            (root / "clean.txt").write_text("single root\n", encoding="utf-8")
            (root / "untracked.txt").write_text(RETIRED_FIELD, encoding="utf-8")
            subprocess.run(
                ["git", "-C", str(root), "add", "clean.txt"],
                check=True,
                capture_output=True,
                text=True,
            )

            result = run_validator(root)

        self.assertEqual(result.returncode, 0, result.stdout)

    def test_term_crossing_a_read_boundary_is_reported(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            prefix = "x" * (64 * 1024 - 3)
            (root / "boundary.txt").write_text(
                prefix + RETIRED_FIELD + "\n",
                encoding="utf-8",
            )

            result = run_validator(root)

        self.assertEqual(result.returncode, 1)
        self.assertIn(f"boundary.txt:1:{RETIRED_FIELD}", result.stdout)

    def test_guard_source_does_not_embed_the_terms_it_rejects(self) -> None:
        self.assertTrue(VALIDATOR.exists(), "terminology validator must exist")
        source = VALIDATOR.read_text(encoding="utf-8")

        for term in RETIRED_TERMS:
            self.assertNotIn(term, source)


if __name__ == "__main__":
    unittest.main()
