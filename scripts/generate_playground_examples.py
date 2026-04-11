#!/usr/bin/env python3

from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys


REPO_ROOT = Path(__file__).resolve().parent.parent
EXAMPLES_DIR = REPO_ROOT / "examples" / "shadorial"
OUTPUT_PATH = REPO_ROOT / "playground" / "example-sources.js"


def build_output() -> str:
    entries: list[tuple[str, str]] = []
    for path in sorted(EXAMPLES_DIR.glob("*.shadml")):
        chapter = path.name.split("-", 1)[0]
        key = f"shadorial-{chapter}"
        entries.append((key, path.read_text(encoding="utf-8")))

    lines = [
        "'use strict';",
        "",
        "// Generated from examples/shadorial/*.shadml by",
        "// `python3 scripts/generate_playground_examples.py`.",
        "window.SHADML_EMBEDDED_EXAMPLES = {",
    ]

    for index, (key, source) in enumerate(entries):
        suffix = "," if index + 1 < len(entries) else ""
        lines.append(f"    {json.dumps(key)}: {json.dumps(source)}{suffix}")

    lines.append("};")
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Generate playground/example-sources.js from examples/shadorial/*.shadml",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="exit non-zero if the generated file is out of date",
    )
    args = parser.parse_args()

    generated = build_output()

    if args.check:
        current = OUTPUT_PATH.read_text(encoding="utf-8") if OUTPUT_PATH.exists() else ""
        if current != generated:
            print(
                "playground/example-sources.js is out of date. "
                "Run `mise run playground:examples`.",
                file=sys.stderr,
            )
            return 1
        return 0

    OUTPUT_PATH.write_text(generated, encoding="utf-8")
    print(f"updated {OUTPUT_PATH.relative_to(REPO_ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
