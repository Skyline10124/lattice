#!/usr/bin/env python3
"""Tests for scripts/rewrite-links.py."""

import importlib.util
import sys
import tempfile
from pathlib import Path

# Load the module from the script path
SCRIPT_PATH = Path(__file__).resolve().parent.parent / "scripts" / "rewrite-links.py"


def load_module():
    spec = importlib.util.spec_from_file_location("rewrite_links", SCRIPT_PATH)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


# -- Tests for is_outside_wiki --

def test_is_outside_wiki_returns_false_for_internal_link():
    mod = load_module()
    wiki = Path("/tmp/wiki")
    current = wiki / "development" / "code-review.md"
    assert mod.is_outside_wiki("../refactoring/overview.md", current, wiki) is False


def test_is_outside_wiki_returns_true_for_external_link():
    mod = load_module()
    wiki = Path("/tmp/wiki")
    current = wiki / "development" / "code-review.md"
    assert mod.is_outside_wiki("../../docs/review/cli-audit.md", current, wiki) is True


def test_is_outside_wiki_returns_true_for_root_file():
    mod = load_module()
    wiki = Path("/tmp/wiki")
    current = wiki / "development" / "roadmap.md"
    assert mod.is_outside_wiki("../../ROADMAP.md", current, wiki) is True


# -- Tests for rewrite_link --

def test_rewrite_link_converts_docs_path():
    mod = load_module()
    wiki = Path("/tmp/wiki")
    current = wiki / "development" / "code-review.md"
    result = mod.rewrite_link("../../docs/review/cli-audit.md", current, wiki)
    assert "github.com/Skyline10124/lattice/blob/main" in result
    assert "docs/review/cli-audit.md" in result


def test_rewrite_link_converts_roadmap():
    mod = load_module()
    wiki = Path("/tmp/wiki")
    current = wiki / "development" / "roadmap.md"
    result = mod.rewrite_link("../../ROADMAP.md", current, wiki)
    assert result == "https://github.com/Skyline10124/lattice/blob/main/ROADMAP.md"


def test_rewrite_link_converts_image():
    mod = load_module()
    wiki = Path("/tmp/wiki")
    current = wiki / "README.md"
    result = mod.rewrite_link("../logo-banner.svg", current, wiki)
    assert result == "https://github.com/Skyline10124/lattice/blob/main/logo-banner.svg"


def test_rewrite_link_leaves_internal_unchanged():
    mod = load_module()
    wiki = Path("/tmp/wiki")
    current = wiki / "development" / "code-review.md"
    link = "../refactoring/overview.md"
    assert mod.rewrite_link(link, current, wiki) == link


# -- Tests for process_file --

def test_process_file_rewrites_external_links():
    mod = load_module()
    with tempfile.TemporaryDirectory() as tmpdir:
        wiki = Path(tmpdir) / "wiki"
        wiki.mkdir()
        dev = wiki / "development"
        dev.mkdir()
        test_file = dev / "test.md"
        test_file.write_text(
            "# Test\n\n"
            "- [CLI audit](../../docs/review/cli-audit.md)\n"
            "- [Internal](../refactoring/overview.md)\n",
            encoding="utf-8",
        )
        count = mod.process_file(test_file, wiki, dry_run=False)
        content = test_file.read_text(encoding="utf-8")
        assert count == 1
        assert content == (
            "# Test\n\n"
            "- [CLI audit](https://github.com/Skyline10124/lattice/blob/main/docs/review/cli-audit.md)\n"
            "- [Internal](../refactoring/overview.md)\n"
        )


def test_process_file_dry_run_does_not_modify():
    mod = load_module()
    with tempfile.TemporaryDirectory() as tmpdir:
        wiki = Path(tmpdir) / "wiki"
        wiki.mkdir()
        dev = wiki / "development"
        dev.mkdir()
        test_file = dev / "test.md"
        original = "# Test\n\n- [CLI audit](../../docs/review/cli-audit.md)\n"
        test_file.write_text(original, encoding="utf-8")
        count = mod.process_file(test_file, wiki, dry_run=True)
        assert count == 1
        assert test_file.read_text(encoding="utf-8") == original


# -- Tests for check_broken_links --

def test_check_broken_links_finds_missing_target():
    mod = load_module()
    with tempfile.TemporaryDirectory() as tmpdir:
        wiki = Path(tmpdir) / "wiki"
        wiki.mkdir()
        dev = wiki / "development"
        dev.mkdir()
        test_file = dev / "test.md"
        test_file.write_text(
            "# Test\n\n- [Missing](../nonexistent/file.md)\n",
            encoding="utf-8",
        )
        broken = mod.check_broken_links(test_file, wiki)
        assert len(broken) == 1
        assert "nonexistent" in broken[0][1]


def test_check_broken_links_ignores_external():
    mod = load_module()
    with tempfile.TemporaryDirectory() as tmpdir:
        wiki = Path(tmpdir) / "wiki"
        wiki.mkdir()
        dev = wiki / "development"
        dev.mkdir()
        test_file = dev / "test.md"
        test_file.write_text(
            "# Test\n\n- [External](../../docs/review/cli-audit.md)\n",
            encoding="utf-8",
        )
        broken = mod.check_broken_links(test_file, wiki)
        assert len(broken) == 0


# -- Entry point --

if __name__ == "__main__":
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    failures = 0
    for test_fn in tests:
        try:
            test_fn()
            print(f"  PASS: {test_fn.__name__}")
        except Exception as e:
            print(f"  FAIL: {test_fn.__name__}: {e}")
            failures += 1
    if failures:
        print(f"\n{failures} test(s) failed")
        sys.exit(1)
    else:
        print(f"\nAll {len(tests)} tests passed")
