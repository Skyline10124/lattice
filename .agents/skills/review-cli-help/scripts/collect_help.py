#!/usr/bin/env python3
# Copyright 2026 Ast Rin
# SPDX-License-Identifier: Apache-2.0
"""动态遍历并预检 `lattice` 全命令树与 `lattice-daemon` 的 `--help` / `-h` 输出。"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

FORBIDDEN_SUBSTRINGS = (
    "Usage:",
    "Commands:",
    "Arguments:",
    "Options:",
    "Print help",
    "Print version",
    "For more information, try",
    "[default:",
    "[possible values:",
    "[aliases:",
    "[env:",
    "说明：",
    "[OPTIONS]",
    "<COMMAND>",
)

GLOBAL_FLAGS = (
    "-j, --json",
    "-v, --verbose",
    "-h, --help",
    "-V, --version",
)

ALL_CAPS_PLACEHOLDER_RE = re.compile(r"<[A-Z][A-Z0-9_]+>")
SPACE_PLACEHOLDER_RE = re.compile(r"<[^>\n]*\s[^>\n]*>")
PLACEHOLDER_RE = re.compile(r"<[^>\n]+>")
OPTION_LINE_RE = re.compile(r"^\s{2,}(-[A-Za-z],\s+--[a-z0-9-]+|--[a-z0-9-]+|-[A-Za-z])(.*)$")
USAGE_LINE_RE = re.compile(r"^用法：(.+)$")


@dataclass
class CommandReport:
    label: str
    help_text: str
    json_desc: str | None = None
    placeholders: set[str] = field(default_factory=set)
    errors: list[str] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)


def repo_root_from_script() -> Path:
    # .agents/skills/review-cli-help/scripts/collect_help.py -> repo root
    return Path(__file__).resolve().parents[4]


def run_help(binary: Path, args: list[str]) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env["NO_COLOR"] = "1"
    env["CLICOLOR"] = "0"
    return subprocess.run(
        [str(binary), *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        env=env,
        check=False,
    )


def parse_sections(text: str) -> dict[str, list[str]]:
    sections: dict[str, list[str]] = {}
    current: str | None = None
    for line in text.splitlines():
        if line in ("参数：", "命令：", "选项：", "Arguments:", "Commands:", "Options:"):
            current = line
            sections.setdefault(current, [])
            continue
        if current is not None:
            if not line.strip():
                current = None
            else:
                sections[current].append(line)
    return sections


def extract_subcommands(text: str) -> tuple[list[str], bool]:
    sections = parse_sections(text)
    lines = sections.get("命令：", []) + sections.get("Commands:", [])
    subcommands: list[str] = []
    leaked_help_subcommand = False
    for line in lines:
        # 子命令首行按 2 空格缩进；更深缩进属于折行描述。
        if not line.startswith("  ") or line.startswith("   "):
            continue
        token = line.strip().split(None, 1)[0]
        if token == "help":
            leaked_help_subcommand = True
            continue
        if token and token not in subcommands:
            subcommands.append(token)
    return subcommands, leaked_help_subcommand


def split_signature_and_desc(line: str) -> tuple[str, str]:
    stripped = line.strip()
    parts = re.split(r"\s{2,}", stripped, maxsplit=1)
    if len(parts) == 2:
        return parts[0], parts[1]
    return stripped, ""


def inspect_command(
    binary: Path,
    bin_name: str,
    path: list[str],
    check_global_order: bool,
) -> tuple[CommandReport, list[str]]:
    label = " ".join([bin_name, *path])
    long_proc = run_help(binary, [*path, "--help"])
    short_proc = run_help(binary, [*path, "-h"])

    report = CommandReport(label=label, help_text=long_proc.stdout.rstrip("\n"))

    if long_proc.returncode != 0:
        report.errors.append(f"`--help` 退出码为 {long_proc.returncode}（期望 0）")
    if short_proc.returncode != 0:
        report.errors.append(f"`-h` 退出码为 {short_proc.returncode}（期望 0）")
    if long_proc.stderr:
        report.errors.append(f"`--help` 写入了非空 stderr：{long_proc.stderr.strip()!r}")
    if short_proc.stderr:
        report.errors.append(f"`-h` 写入了非空 stderr：{short_proc.stderr.strip()!r}")
    if long_proc.stdout != short_proc.stdout:
        report.errors.append("`--help` 与 `-h` 输出不一致")

    text = long_proc.stdout
    lines = text.splitlines()
    if not lines or not lines[0].strip():
        report.errors.append("缺少首行中文摘要")
    else:
        summary_line = lines[0].rstrip()
        if summary_line.endswith("。"):
            report.errors.append(f"首行摘要末尾带句号：{summary_line.strip()!r}")
        if "（" in summary_line or "(" in summary_line:
            report.warnings.append(f"首行摘要含括号说明，请确认必要性：{summary_line.strip()}")

    for forbidden in FORBIDDEN_SUBSTRINGS:
        if forbidden in text:
            report.errors.append(f"包含禁止片段：{forbidden!r}")

    usage_lines = [m.group(1) for line in lines if (m := USAGE_LINE_RE.match(line))]
    if not usage_lines:
        report.errors.append("缺少 `用法：` 行")
    else:
        usage = usage_lines[0].strip()
        expected_prefix = label
        if usage != expected_prefix and not usage.startswith(f"{expected_prefix} "):
            report.errors.append(f"用法行前缀不匹配：实际 `{usage}`，期望以 `{expected_prefix}` 开头")
        report.placeholders.update(PLACEHOLDER_RE.findall(usage))
        for match in ALL_CAPS_PLACEHOLDER_RE.findall(usage):
            report.errors.append(f"用法行包含全大写英文占位符：{match}")
        for match in SPACE_PLACEHOLDER_RE.findall(usage):
            report.errors.append(f"用法行包含带空格占位符：{match}")

    subcommands, leaked_help = extract_subcommands(text)
    if leaked_help:
        report.errors.append("暴露了默认 `help` 子命令")

    sections = parse_sections(text)
    if "选项：" not in sections:
        report.errors.append("缺少 `选项：` 段落")

    for section_name in ("参数：", "命令：", "选项："):
        for line in sections.get(section_name, []):
            trimmed = line.rstrip()
            if trimmed.endswith("。"):
                report.errors.append(f"{section_name} 条目末尾带句号：{trimmed.strip()!r}")
            sig, desc = split_signature_and_desc(line)
            if section_name in ("参数：", "选项：") and not line.startswith("          "):
                report.placeholders.update(PLACEHOLDER_RE.findall(sig))
                for match in ALL_CAPS_PLACEHOLDER_RE.findall(sig):
                    report.errors.append(f"{section_name} 签名包含全大写英文占位符：{match}")
                for match in SPACE_PLACEHOLDER_RE.findall(sig):
                    report.errors.append(f"{section_name} 签名包含带空格占位符：{match}")
            target_text = desc if desc else (trimmed.strip() if line.startswith("          ") else "")
            if "（" in target_text or "(" in target_text:
                report.warnings.append(f"{section_name} 含括号说明，请确认必要性：{trimmed.strip()}")

    if check_global_order and "选项：" in sections:
        option_flags: list[tuple[str, str]] = []
        opt_lines = sections["选项："]
        for idx, line in enumerate(opt_lines):
            m = OPTION_LINE_RE.match(line)
            if not m:
                continue
            flag = m.group(1)
            _, desc = split_signature_and_desc(line)
            if not desc and idx + 1 < len(opt_lines) and opt_lines[idx + 1].startswith("          "):
                desc = opt_lines[idx + 1].strip()
            option_flags.append((flag, desc))

        for flag, desc in option_flags:
            if flag == "-j, --json":
                report.json_desc = desc

        global_indices = [
            (i, GLOBAL_FLAGS.index(flag), flag)
            for i, (flag, _) in enumerate(option_flags)
            if flag in GLOBAL_FLAGS
        ]
        if global_indices:
            first_global = global_indices[0][0]
            expected_positions = list(range(first_global, len(option_flags)))
            actual_positions = [i for i, _, _ in global_indices]
            if actual_positions != expected_positions:
                report.errors.append("全局选项未连续沉底排列")
            ranks = [rank for _, rank, _ in global_indices]
            if ranks != sorted(ranks):
                flags_order = ", ".join(flag for _, _, flag in global_indices)
                report.errors.append(f"全局选项顺序不符合 -j/-v/-h/-V：{flags_order}")

    return report, subcommands


def walk_lattice(binary: Path) -> list[CommandReport]:
    reports: list[CommandReport] = []
    visited: set[tuple[str, ...]] = set()

    def visit(path: list[str]) -> None:
        key = tuple(path)
        if key in visited or len(path) > 6:
            return
        visited.add(key)
        report, subcommands = inspect_command(binary, "lattice", path, check_global_order=True)
        reports.append(report)
        for sub in subcommands:
            visit([*path, sub])

    visit([])
    return reports


def main() -> int:
    root = repo_root_from_script()
    default_lattice = os.environ.get("LATTICE_BIN") or str(root / "target/debug/lattice")
    default_daemon = os.environ.get("LATTICE_DAEMON_BIN") or str(root / "target/debug/lattice-daemon")

    parser = argparse.ArgumentParser(description="遍历并预检 lattice 与 lattice-daemon 全部帮助信息")
    parser.add_argument("--lattice-bin", default=default_lattice, help="lattice 二进制路径")
    parser.add_argument("--daemon-bin", default=default_daemon, help="lattice-daemon 二进制路径")
    parser.add_argument("--no-daemon", action="store_true", help="跳过 lattice-daemon 检查")
    parser.add_argument("--check-only", action="store_true", help="只输出预检汇总，不打印帮助全文")
    args = parser.parse_args()

    lattice_bin = Path(args.lattice_bin)
    if not lattice_bin.is_file():
        print(f"错误：找不到 lattice 二进制：{lattice_bin}", file=sys.stderr)
        print("请先构建：`cargo build -p lattice-cli -p lattice-daemon --bins`", file=sys.stderr)
        return 2

    reports = walk_lattice(lattice_bin)

    if not args.no_daemon:
        daemon_bin = Path(args.daemon_bin)
        if not daemon_bin.is_file():
            print(f"错误：找不到 lattice-daemon 二进制：{daemon_bin}", file=sys.stderr)
            print("请先构建：`cargo build -p lattice-cli -p lattice-daemon --bins`", file=sys.stderr)
            return 2
        daemon_report, _ = inspect_command(daemon_bin, "lattice-daemon", [], check_global_order=False)
        reports.append(daemon_report)

    if not args.check_only:
        for report in reports:
            print(f"=== {report.label} --help ===")
            print(report.help_text)
            print()

    print("=== 帮助信息预检汇总 ===")
    print(f"共检查 {len(reports)} 条命令：")
    print()
    print("--json 暴露矩阵：")
    for report in reports:
        if report.label == "lattice-daemon":
            continue
        status = report.json_desc if report.json_desc is not None else "（隐藏）"
        print(f"  {report.label:<28} {status}")

    all_placeholders = sorted({ph for r in reports for ph in r.placeholders})
    print()
    print(f"占位符汇总（{len(all_placeholders)} 种）：")
    print(f"  {', '.join(all_placeholders)}")

    total_errors = sum(len(r.errors) for r in reports)
    total_warnings = sum(len(r.warnings) for r in reports)

    if total_errors:
        print()
        print(f"错误（{total_errors} 处）：")
        for report in reports:
            for err in report.errors:
                print(f"  [ERROR] {report.label}: {err}")

    if total_warnings:
        print()
        print(f"人工复核提示（{total_warnings} 处）：")
        for report in reports:
            for warn in report.warnings:
                print(f"  [WARN]  {report.label}: {warn}")

    print()
    if total_errors == 0:
        print(f"自动化预检通过：{len(reports)} 条命令，0 处错误，{total_warnings} 处人工复核提示。")
        return 0
    print(f"自动化预检未通过：{total_errors} 处错误，{total_warnings} 处人工复核提示。")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
