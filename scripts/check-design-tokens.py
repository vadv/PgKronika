#!/usr/bin/env python3
"""Design-token gate for web/src: no literal colors, no off-grid px.

The UI contract (web/src/design/tokens.css + pgkronika-frontend skill) is:
colors and spacing come from CSS custom properties; components may use px
only from the 4px grid set. Prompt-level rules fail 10-100% of the time,
so the check lives in CI instead.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "web" / "src"
SKIP_PARTS = {".test.", "testkit", "fixtures", "tokens.css", "schema.d.ts"}

SAFE_KEYWORDS = {
    "transparent", "currentColor", "inherit", "initial", "unset", "none",
}
NAMED_COLORS = {
    "red", "blue", "green", "yellow", "orange", "purple", "pink", "brown",
    "gray", "grey", "white", "black", "cyan", "magenta", "lime", "navy",
    "teal", "olive", "maroon", "aqua", "fuchsia", "silver", "indigo", "violet",
}


def on_grid(value: float) -> bool:
    # The v2 rhythm: 4px space scale plus even micro-gaps (6px chips, 18px
    # line-height); 1px hairlines ok; ad-hoc odd values are the defect.
    return value <= 1 or value % 2 == 0


HEX_RE = re.compile(r"#[0-9a-fA-F]{3,8}\b")
PX_RE = re.compile(r"(?<![\w.])(\d+(?:\.\d+)?)px\b")
TOKEN_DEF_RE = re.compile(r"^\s*--[\w-]+\s*:")


def violations(path: Path) -> list[str]:
    out = []
    text = path.read_text(encoding="utf-8")
    for lineno, line in enumerate(text.splitlines(), 1):
        if path.suffix == ".css" and TOKEN_DEF_RE.match(line):
            continue  # token definitions are where literals belong
        for match in HEX_RE.finditer(line):
            out.append(f"{path.relative_to(ROOT)}:{lineno}: hex color {match.group(0)}")
        for match in PX_RE.finditer(line):
            if not on_grid(float(match.group(1))):
                out.append(
                    f"{path.relative_to(ROOT)}:{lineno}: off-grid {match.group(0)}"
                )
        # In CSS, inspect the declaration value rather than the property name:
        # `white-space` is layout syntax, not the named color `white`.
        named_color_source = line
        if path.suffix == ".css" and ":" in line:
            named_color_source = line.split(":", 1)[1]
        for word in re.findall(r"\b[A-Za-z]+\b", named_color_source):
            if word in NAMED_COLORS and word not in SAFE_KEYWORDS:
                out.append(f"{path.relative_to(ROOT)}:{lineno}: named color {word}")
    return out


def main() -> int:
    found: list[str] = []
    for path in sorted(SRC.rglob("*")):
        if path.suffix not in {".ts", ".tsx", ".css"}:
            continue
        if any(part in str(path) for part in SKIP_PARTS):
            continue
        found.extend(violations(path))
    if found:
        print("design-token violations (use tokens.css and the space scale):")
        for line in found:
            print(f"  {line}")
        return 1
    print("design tokens: no literal colors or off-grid px outside tokens.css")
    return 0


if __name__ == "__main__":
    sys.exit(main())
