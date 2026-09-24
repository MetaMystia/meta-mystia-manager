#!/usr/bin/env python3
"""生成发行日志素材：把两个 tag 之间的提交按类型分组，附链接与正文要点。

只做"收集"，不做"润色"：输出会写进 draft release 作为素材，
由维护者改写成面向用户的说明后再发布。

用法：python gen_release_material.py <tag> [previous tag] [--output <file>]

不给 --output 时打印到标准输出；CI 里用 --output 直接写文件，避免经过 PowerShell 管道重新编码。
"""

import re
import subprocess
import sys

# Windows 的 CI 控制台默认是 cp1252，直接打印中文会 UnicodeEncodeError
if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
if hasattr(sys.stderr, "reconfigure"):
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")

REPO_URL = "https://github.com/MetaMystia/meta-mystia-manager"

# 分组顺序即输出顺序；skip 的分组不出现在素材里
GROUPS = [
    ("feat", "新功能"),
    ("fix", "修复"),
    ("perf", "性能"),
    ("build", "构建与依赖"),
    ("deps", "构建与依赖"),
    ("refactor", "内部调整"),
    ("chore", "内部调整"),
    ("docs", "文档"),
]

SKIP_SUBJECT_PATTERNS = [
    r"^chore: update package version",
    r"dependabot",
    r"^chore\(deps\)",
]


def git(*args):
    return subprocess.run(
        ["git", *args], check=True, capture_output=True, text=True
    ).stdout


def previous_tag(tag):
    try:
        return git("describe", "--tags", "--abbrev=0", f"{tag}^").strip()
    except subprocess.CalledProcessError:
        return None


def read_commits(range_spec):
    raw = git("log", "--no-merges", "--pretty=format:%H%x1f%s%x1f%b%x1e", range_spec)
    commits = []

    for record in raw.split("\x1e"):
        record = record.strip("\n")
        if not record:
            continue

        fields = record.split("\x1f")
        if len(fields) < 3:
            continue

        commit_hash, subject, body = fields[0], fields[1], fields[2]

        if any(re.search(pattern, subject, re.I) for pattern in SKIP_SUBJECT_PATTERNS):
            continue

        commits.append((commit_hash, subject.strip(), body.strip()))

    return commits


def group_of(subject):
    match = re.match(r"^([a-z]+)(\([^)]*\))?:", subject)
    kind = match.group(1) if match else ""

    for key, label in GROUPS:
        if kind == key:
            return label

    return "其他"


def body_items(body):
    """把正文拆成条目：带 `-`/`*` 或顶格的行是新条目，缩进的续行并回上一条"""
    items = []

    for line in body.splitlines():
        stripped = line.strip()
        if not stripped:
            continue

        is_bullet = stripped.startswith(("-", "*"))
        is_continuation = line[:1] in (" ", "\t") and not is_bullet

        if is_continuation and items:
            items[-1] = f"{items[-1]} {stripped}"
            continue

        items.append(stripped.lstrip("-*").strip())

    return items


def format_entry(commit_hash, subject, body):
    title = re.sub(r"^[a-z]+(\([^)]*\))?:\s*", "", subject)
    lines = [f"- {title} ([{commit_hash[:7]}]({REPO_URL}/commit/{commit_hash}))"]

    for item in body_items(body):
        lines.append(f"  - {item}")

    return "\n".join(lines)


def main(tag, previous=None, output=None):
    previous = previous or previous_tag(tag)
    range_spec = f"{previous}..{tag}" if previous else tag
    commits = read_commits(range_spec)

    buckets = {}
    order = []

    for commit in commits:
        label = group_of(commit[1])
        if label not in buckets:
            buckets[label] = []
            order.append(label)

        buckets[label].append(commit)

    out = [
        f"<!-- 自动收集的提交素材（{range_spec}），共 {len(commits)} 条。 -->",
        "<!-- 发布前请改写为面向用户的说明，参考 v3.0.0 的写法：中文卖点 + 合并同类项。 -->",
        "",
    ]

    for label in [item[1] for item in GROUPS] + ["其他"]:
        if label not in buckets or label in ("内部调整", "构建与依赖", "文档"):
            continue

        out.append(f"# {label}")
        out.append("")
        for commit_hash, subject, body in buckets[label]:
            out.append(format_entry(commit_hash, subject, body))
        out.append("")

    leftovers = (buckets.get("构建与依赖") or []) + (buckets.get("内部调整") or [])
    docs = buckets.get("文档") or []

    if docs or leftovers:
        out.append("<details><summary>内部调整 / 构建 / 文档</summary>")
        out.append("")
        for commit_hash, subject, body in docs + leftovers:
            out.append(format_entry(commit_hash, subject, body))
        out.append("")
        out.append("</details>")

    text = "\n".join(out).rstrip() + "\n"

    if output:
        with open(output, "w", encoding="utf-8", newline="\n") as handle:
            handle.write(text)
    else:
        print(text, end="")


if __name__ == "__main__":
    args = sys.argv[1:]
    if not args:
        raise SystemExit(__doc__)

    output = None
    if "--output" in args:
        index = args.index("--output")
        output = args[index + 1]
        del args[index : index + 2]

    main(args[0], args[1] if len(args) > 1 else None, output)
