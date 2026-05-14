#!/usr/bin/env python3
"""Generate SUMMARY.md for mdBook from wiki/ directory structure.

Scans the wiki/ directory, extracts titles from # headings, and produces
a structured SUMMARY.md with alphabetically sorted entries.

Usage:
    python3 scripts/generate-summary.py [--verify]

Options:
    --verify    Count .md references in generated SUMMARY.md and exit 1 if count != 48
"""

import argparse
import re
import sys
from pathlib import Path


def extract_title(filepath: Path) -> str:
    """Extract first # heading from a markdown file.

    Falls back to filename stem if no heading found.
    """
    with open(filepath, encoding="utf-8") as f:
        for line in f:
            match = re.match(r'^#\s+(.+)', line)
            if match:
                return match.group(1).strip()
    return filepath.stem


def generate_summary(wiki_dir: Path) -> str:
    """Generate SUMMARY.md content from wiki/ directory structure.

    Args:
        wiki_dir: Path to the wiki/ directory

    Returns:
        Generated SUMMARY.md content as string
    """
    lines = ["# Summary"]

    # Homepage (wiki/README.md)
    readme = wiki_dir / "README.md"
    if readme.exists():
        title = extract_title(readme)
        lines.append(f"[{title}](README.md)")

    # Subdirectories (alphabetical)
    for subdir in sorted(wiki_dir.iterdir()):
        if not subdir.is_dir() or subdir.name.startswith('.'):
            continue

        # Section header
        subdir_readme = subdir / "README.md"
        if subdir_readme.exists():
            lines.append(f"- [{subdir.name}]({subdir.name}/README.md)")
        else:
            lines.append(f"- {subdir.name}")

        # Files in subdir (alphabetical, excluding README.md)
        for md_file in sorted(subdir.glob("*.md")):
            if md_file.name == "README.md":
                continue
            title = extract_title(md_file)
            lines.append(f"    - [{title}]({subdir.name}/{md_file.name})")

    return "\n".join(lines) + "\n"


def count_md_references(content: str) -> int:
    """Count .md file references in SUMMARY.md content."""
    return len(re.findall(r'\.md[\)\]]', content))


def main():
    parser = argparse.ArgumentParser(description="Generate SUMMARY.md for mdBook")
    parser.add_argument("--verify", action="store_true",
                        help="Count .md references and exit 1 if count != 48")
    args = parser.parse_args()

    # Determine paths
    script_dir = Path(__file__).parent
    project_root = script_dir.parent
    wiki_dir = project_root / "wiki"
    output_file = wiki_dir / "SUMMARY.md"

    if not wiki_dir.exists():
        print(f"Error: wiki/ directory not found at {wiki_dir}", file=sys.stderr)
        sys.exit(1)

    # Generate
    content = generate_summary(wiki_dir)

    # Write
    output_file.write_text(content, encoding="utf-8")
    print(f"Generated {output_file}")

    # Verify mode
    if args.verify:
        count = count_md_references(content)
        print(f"Found {count} .md references")
        if count != 58:
            print(f"Error: Expected 58 .md references, found {count}", file=sys.stderr)
            sys.exit(1)
        print("Verification passed!")


if __name__ == "__main__":
    main()
