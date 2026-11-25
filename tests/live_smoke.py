#!/usr/bin/env python3
"""Live smoke test: the parts that need no login session.

Proves the binary reaches the real service and that the header set is
sufficient. The one check that costs an SMS (phone-code) is skipped unless
PICKLE_SMOKE_PHONE is set, so running this by accident never sends a message.

Assertions:
  * `pickle doctor` exits 0 and reports the resolver as ok
  * `pickle status` reports signed_in=false when no session is stored
  * an unauthenticated GET against a real endpoint returns a JSON 401, not a
    CloudFront HTML block (which is what a missing header looks like)
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = ROOT / "target" / "release" / "pickle.exe"
if not BIN.is_file():
    BIN = ROOT / "target" / "release" / "pickle"
if not BIN.is_file():
    BIN = ROOT / "target" / "debug" / "pickle"


def run(args: list[str], env: dict[str, str]) -> subprocess.CompletedProcess:
    return subprocess.run(
        [str(BIN), *args], capture_output=True, text=True, env=env, timeout=60, encoding="utf-8"
    )


def main() -> int:
    sys.stdout.reconfigure(encoding="utf-8")
    state = Path(tempfile.mkdtemp()) / "state.json"
    env = dict(os.environ, PICKLE_CLI_STATE=str(state))
    problems: list[str] = []

    doctor = run(["doctor", "--json"], env)
    if doctor.returncode != 0:
        problems.append(f"doctor exit={doctor.returncode}: {doctor.stderr.strip()[:200]}")
    else:
        layers = json.loads(doctor.stdout)
        resolver = next((row for row in layers if row["layer"] == "resolver"), None)
        if not resolver or resolver["status"] != "ok":
            problems.append(f"resolver not ok: {resolver}")
        else:
            print(f"resolver ok: {resolver['detail']}")

    status = json.loads(run(["status", "--json"], env).stdout)
    if status.get("signed_in") is not False:
        problems.append(f"status.signed_in={status.get('signed_in')}")
    print(f"api_base: {status.get('api_base')}")

    # A real endpoint without a session: the server answers JSON 401.
    unauth = run(["raw", "api/sync", "--json"], env)
    if unauth.returncode != 1:
        problems.append(f"unauth exit={unauth.returncode} (expected 1)")
    if "401" not in unauth.stderr:
        problems.append(f"unauth stderr lacks 401: {unauth.stderr.strip()[:160]}")
    if "cloudfront" in unauth.stderr.lower() or "<html" in unauth.stderr.lower():
        problems.append("unauth hit the CDN block page instead of the API")
    print(f"unauth: {unauth.stderr.strip()[:120]}")

    # Measured 2026-11-23: login-sessions/phone-code returns the CDN HTML 403
    # unless X-Client-Build is present, and one request without it spends nothing.
    import urllib.request

    def probe(headers: dict[str, str], label: str) -> tuple[int, str]:
        body = b'{"phone":"+819012345678","region":"jp"}'
        request = urllib.request.Request(
            "https://character-api-prod.picklemem.com/api/login-sessions/phone-code",
            data=body,
            headers={"Content-Type": "application/json", **headers},
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=15) as response:
                return response.status, response.read(60).decode("utf-8", "replace")
        except urllib.error.HTTPError as err:
            return err.code, err.read(60).decode("utf-8", "replace")

    status, text = probe({"X-Client-Version": "1.1.16"}, "version-only")
    print(f"header probe (version only): {status} {'JSON' if text.lstrip().startswith('{') else 'HTML'}")
    if status == 200:
        problems.append("version-only unexpectedly succeeded; the header rule changed")

    phone = os.environ.get("PICKLE_SMOKE_PHONE")
    if phone:
        start = run(["login", "--start", phone, "--region", "jp", "--json"], env)
        if start.returncode != 0:
            problems.append(f"login --start failed: {start.stderr.strip()[:200]}")
        else:
            payload = json.loads(start.stdout)
            print(f"pending_id issued, expires_in={payload.get('expires_in')}")
            if not payload.get("pending_id"):
                problems.append("no pending_id in response")
            # Never print the pending id itself beyond the prefix: it is a
            # short-lived credential for the OTP exchange.
            print(f"pending_id prefix: {payload['pending_id'][:8]}…")
    else:
        print("login --start skipped (set PICKLE_SMOKE_PHONE to include it)")

    if problems:
        for problem in problems:
            print(f"FAIL {problem}")
        return 1
    print("live smoke OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
