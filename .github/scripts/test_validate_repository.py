"""Unit tests for the dependency-free repository validator."""

from pathlib import Path
import tempfile
import unittest

from validate_repository import validate_markdown


class ValidateMarkdownTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = Path(self.temp_dir.name)
        (self.root / "docs").mkdir()
        (self.root / "docs" / "target.md").write_text("# Target\n", encoding="utf-8")

    def tearDown(self) -> None:
        self.temp_dir.cleanup()

    def write_markdown(self, content: bytes) -> Path:
        path = self.root / "docs" / "source.md"
        path.write_bytes(content)
        return path

    def test_valid_markdown_and_relative_link(self) -> None:
        path = self.write_markdown(b"# Source\n\n[Target](target.md)\n")
        self.assertEqual(validate_markdown(path, self.root), [])

    def test_broken_local_link_is_reported(self) -> None:
        path = self.write_markdown(b"[Missing](missing.md)\n")
        errors = validate_markdown(path, self.root)
        self.assertTrue(any("broken local link" in error for error in errors))

    def test_crlf_and_missing_final_newline_are_reported(self) -> None:
        path = self.write_markdown(b"# Title\r")
        errors = validate_markdown(path, self.root, enforce_lf=True)
        self.assertTrue(any("CR characters" in error for error in errors))
        self.assertTrue(any("final newline" in error for error in errors))

    def test_links_inside_fenced_and_inline_code_are_ignored(self) -> None:
        content = (
            b"# Examples\n\n"
            b"\x60\x60\x60markdown\n[not a link](missing-fenced.md)\n\x60\x60\x60\n\n"
            b"Inline code: \x60[not a link](missing-inline.md)\x60.\n\n"
            b"[A real link](target.md)\n"
        )
        path = self.write_markdown(content)
        self.assertEqual(validate_markdown(path, self.root), [])

    def test_trailing_whitespace_and_unclosed_fence_are_reported(self) -> None:
        path = self.write_markdown(b"# Title  \n\n```text\nunfinished\n")
        errors = validate_markdown(path, self.root)
        self.assertTrue(any("trailing whitespace" in error for error in errors))
        self.assertTrue(any("unclosed fenced code block" in error for error in errors))


if __name__ == "__main__":
    unittest.main()
