#!/usr/bin/env python3
"""Run every check in one go and print a table.

Each stage is a separate process. A stage that fails does not stop the rest,
so one run shows every problem instead of only the first.

    python tests/run_all.py             everything except the live write chain
    python tests/run_all.py --live      also the read-only live chain
    python tests/run_all.py --write     also the live write chain (hits the account)

Exit code is 1 when any stage fails.
"""
from __future__ import annotations

import argparse
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = ROOT / "target" / "release" / "pickle.exe"
if not BIN.is_file():
    BIN = ROOT / "target" / "release" / "pickle"


def run(label, args, allow_fail=False):
    started = time.monotonic()
    proc = subprocess.run(args, capture_output=True, text=True, encoding="utf-8",
                          cwd=str(ROOT))
    elapsed = time.monotonic() - started
    ok = proc.returncode == 0 or allow_fail
    tail = ""
    for line in reversed((proc.stdout + proc.stderr).splitlines()):
        if line.strip():
            tail = line.strip()[:96]
            break
    print(f"{label:<26}{'ok  ' if ok else 'FAIL'}{elapsed:6.1f}s  {tail}")
    return ok, proc


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--live", action="store_true", help="also the read-only live chain")
    parser.add_argument("--write", action="store_true", help="also the live write chain")
    parser.add_argument("--room", default=None,
                        help="room id for the live write chain; required by --write")
    parsed = parser.parse_args()

    sys.stdout.reconfigure(encoding="utf-8")
    if not BIN.is_file():
        print(f"build first: {BIN} is missing")
        return 1

    stages = [
        ("cargo test", ["cargo", "test", "--quiet"]),
        ("cargo clippy", ["cargo", "clippy", "--all-targets", "--quiet"]),
        ("cargo fmt", ["cargo", "fmt", "--check"]),
        ("cli branches", [sys.executable, "tests/cli_branches.py"]),
        ("mcp stdio", [sys.executable, "tests/mcp_stdio.py"]),
        ("e2e fixture", [sys.executable, "tests/e2e_live.py", "--fixture", "--write"]),
        ("live smoke", [sys.executable, "tests/live_smoke.py"]),
    ]
    if parsed.live or parsed.write:
        stages.append(("e2e live (read)", [sys.executable, "tests/e2e_live.py"]))
    if parsed.write:
        if not parsed.room:
            print("--write needs --room <id>: a test message must not land in an "
                  "arbitrary real conversation")
            return 1
        stages.append((
            "e2e live (write)",
            [sys.executable, "tests/e2e_live.py", "--write", "--room", parsed.room],
        ))

    results = []
    for label, args in stages:
        # Live stages may fail for reasons outside the code (rate limits, no
        # login). They are reported, and still fail the run, but the point is to
        # see them rather than to hide them.
        ok, _ = run(label, args, allow_fail=False)
        results.append((label, ok))

    failed = [label for label, ok in results if not ok]
    print()
    if failed:
        for label in failed:
            print(f"FAIL {label}")
        return 1
    print(f"all {len(results)} stages passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
