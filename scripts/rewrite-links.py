#!/usr/bin/env python3
"""Rewrite out-of-wiki links in markdown files to GitHub absolute URLs.

Walks all .md files in wiki/ and converts links that point outside the wiki
directory to absolute GitHub URLs. Supports --check mode for broken link
detection without modifying files.

Usage:
    python3 scripts/rewrite-links.py          # Rewrite links in-place
    python3 scripts/rewrite-links.py --check  # Report broken links only

Uses only Python 3 stdlib (re, pathlib, os, sys, argparse).
"""

import argparse
import re
import sys
from pathlib import Path

GITHUB_BASE = "https://github.com/Skyline10124/lattice/blob/main"

# Regex to match markdown links: [text](target) and ![alt](target)
LINK_PATTERN = re.compile(r'(!?\[[^\]]*\]\()([^)]+)\)')


def is_outside_wiki(link_target: str, current_file: Path, wiki_dir: Path) -> bool:
    """Check if a link target resolves outside the wiki directory.

    Args:
        link_target: The raw link target string (e.g., '../../docs/foo.md')
        current_file: Absolute path to the file containing the link
        wiki_dir: Absolute path to the wiki/ directory

    Returns:
        True if the resolved path is outside wiki_dir
    """
    try:
        resolved = (current_file.parent / link_target).resolve()
        wiki_resolved = wiki_dir.resolve()
        return not str(resolved).startswith(str(wiki_resolved) + "/") and resolved != wiki_resolved
    except (ValueError, OSError):
        return False


def rewrite_link(link_target: str, current_file: Path, wiki_dir: Path) -> str:
    """Rewrite a link target to a GitHub absolute URL if it points outside wiki/.

    Args:
        link_target: The raw link target string
        current_file: Absolute path to the file containing the link
        wiki_dir: Absolute path to the wiki/ directory

    Returns:
        The original link_target if inside wiki/, or a GitHub absolute URL
    """
    if not is_outside_wiki(link_target, current_file, wiki_dir):
        return link_target

    try:
        resolved = (current_file.parent / link_target).resolve()
        repo_root = wiki_dir.parent
        rel_to_repo = resolved.relative_to(repo_root)
        return f"{GITHUB_BASE}/{rel_to_repo}"
    except (ValueError, OSError):
        return link_target


def process_file(filepath: Path, wiki_dir: Path, dry_run: bool = False) -> int:
    """Process a single markdown file, rewriting out-of-wiki links.

    Args:
        filepath: Path to the markdown file
        wiki_dir: Path to the wiki/ directory
        dry_run: If True, count links but don't modify the file

    Returns:
        Number of links rewritten
    """
    content = filepath.read_text(encoding="utf-8")
    count = 0

    def replace_link(match: re.Match) -> str:
        nonlocal count
        prefix = match.group(1)  # e.g., "[text](" or "![alt]("
        target = match.group(2)  # the link target

        # Skip anchors-only and absolute URLs
        if target.startswith("#") or target.startswith("http://") or target.startswith("https://"):
            return match.group(0)

        new_target = rewrite_link(target, filepath, wiki_dir)
        if new_target != target:
            count += 1
            return f"{prefix}{new_target})"
        return match.group(0)

    new_content = LINK_PATTERN.sub(replace_link, content)

    if not dry_run and count > 0:
        filepath.write_text(new_content, encoding="utf-8")

    return count


def check_broken_links(filepath: Path, wiki_dir: Path) -> list:
    """Check for broken internal links in a markdown file.

    Args:
        filepath: Path to the markdown file
        wiki_dir: Path to the wiki/ directory

    Returns:
        List of (line_number, link_target) tuples for broken links
    """
    broken = []
    content = filepath.read_text(encoding="utf-8")

    for line_num, line in enumerate(content.splitlines(), start=1):
        for match in LINK_PATTERN.finditer(line):
            target = match.group(2)

            # Skip anchors, absolute URLs, and out-of-wiki links
            if target.startswith("#") or target.startswith("http://") or target.startswith("https://"):
                continue
            if is_outside_wiki(target, filepath, wiki_dir):
                continue

            # Check if the internal link target exists
            try:
                resolved = (filepath.parent / target).resolve()
                if not resolved.exists():
                    broken.append((line_num, target))
            except (ValueError, OSError):
                broken.append((line_num, target))

    return broken


def find_md_files(wiki_dir: Path) -> list:
    """Find all .md files in the wiki directory, sorted alphabetically."""
    return sorted(wiki_dir.rglob("*.md"))


def main():
    parser = argparse.ArgumentParser(
        description="Rewrite out-of-wiki links to GitHub absolute URLs"
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="Report broken links without modifying files",
    )
    parser.add_argument(
        "--wiki-dir",
        type=Path,
        default=None,
        help="Path to wiki/ directory (default: auto-detect from script location)",
    )
    args = parser.parse_args()

    # Determine paths
    if args.wiki_dir:
        wiki_dir = args.wiki_dir.resolve()
    else:
        script_dir = Path(__file__).parent
        project_root = script_dir.parent
        wiki_dir = project_root / "wiki"

    if not wiki_dir.exists():
        print(f"Error: wiki/ directory not found at {wiki_dir}", file=sys.stderr)
        sys.exit(1)

    md_files = find_md_files(wiki_dir)

    if args.check:
        # Check mode: report broken links
        total_broken = 0
        for filepath in md_files:
            broken = check_broken_links(filepath, wiki_dir)
            for line_num, target in broken:
                rel = filepath.relative_to(wiki_dir)
                print(f"  BROKEN: {rel}:{line_num}: {target}", file=sys.stderr)
                total_broken += 1
        print(f"Checked {len(md_files)} files, found {total_broken} broken links")
        if total_broken > 0:
            sys.exit(1)
    else:
        # Rewrite mode: fix links in-place
        total_rewrites = 0
        files_modified = 0
        for filepath in md_files:
            count = process_file(filepath, wiki_dir, dry_run=False)
            if count > 0:
                rel = filepath.relative_to(wiki_dir)
                print(f"  Rewrote {count} link(s) in {rel}")
                total_rewrites += count
                files_modified += 1
        print(f"Rewrote {total_rewrites} links in {files_modified} files")


if __name__ == "__main__":
    main()
