#!/usr/bin/env python3
"""The MCP tool count claimed in README.md and docs/harnesses.md must match src/mcp.rs.

Run in CI: python3 ops/check_docs_tools.py
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MCP = ROOT / "src" / "mcp.rs"
DOCS = [ROOT / "README.md", ROOT / "docs" / "harnesses.md"]

# A tool is a function under the #[tool] attribute. The attribute ends at the first `)]`,
# because descriptions quote example text such as `[drawer]` and `[[Note Name]]`.
TOOL = re.compile(
    r"#\[tool\b[^\n]*?\)\]\s*\n\s*(?:pub\s+)?async\s+fn\s+(\w+)\s*\(", re.S
)
# The claimed total is the only bare "N tools" figure in a doc.
CLAIM = re.compile(r"\b(\d+)\s+tools\b")


def declared_tools() -> dict[str, int]:
    """Tool function name -> how many times src/mcp.rs declares it."""
    counts: dict[str, int] = {}
    for name in TOOL.findall(MCP.read_text()):
        counts[name] = counts.get(name, 0) + 1
    return counts


def documented_total(path: Path) -> int:
    claims = CLAIM.findall(path.read_text())
    if len(claims) != 1:
        raise ValueError(
            f"{path.relative_to(ROOT)}: expected exactly one 'N tools' claim, found {claims}"
        )
    return int(claims[0])


def main() -> int:
    tools = declared_tools()
    drawer = sum(1 for name in tools if name.startswith("drawer_"))
    memory = sum(1 for name in tools if name.startswith("memory_"))
    actual = drawer + memory
    print(f"src/mcp.rs declares {actual} tools: {drawer} drawer_* and {memory} memory_*")
    ok = True
    for path in DOCS:
        claimed = documented_total(path)
        if claimed != actual:
            ok = False
            print(f"MISMATCH: {path.relative_to(ROOT)} claims {claimed} tools, src/mcp.rs declares {actual}")
        else:
            print(f"{path.relative_to(ROOT)}: {claimed} tools matches")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
