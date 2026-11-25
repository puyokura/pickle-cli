#!/usr/bin/env python3
"""Drive every CLI subcommand that needs no login session and check its shape.

This is the branch coverage the unit tests cannot reach: each command goes
through argument parsing, state loading, dispatch and output formatting in a
real process. Anything that would hit a write path is checked only for its
refusal, so running this never touches a real account.

Usage:  python tests/cli_branches.py
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


def run(args: list[str], env: dict[str, str]) -> tuple[int, str, str]:
    proc = subprocess.run(
        [str(BIN), *args], capture_output=True, text=True, env=env, timeout=90, encoding="utf-8"
    )
    return proc.returncode, proc.stdout, proc.stderr


def main() -> int:
    sys.stdout.reconfigure(encoding="utf-8")
    state = Path(tempfile.mkdtemp()) / "state.json"
    env = dict(os.environ, PICKLE_CLI_STATE=str(state))
    problems: list[str] = []
    checks = 0

    def expect(label: str, args: list[str], code: int, needle: str | None = None) -> str:
        nonlocal checks
        checks += 1
        rc, out, err = run(args, env)
        if rc != code:
            problems.append(f"{label}: exit {rc} != {code} ({err.strip()[:120]})")
        if needle is not None:
            haystack = out + err
            if needle not in haystack:
                problems.append(f"{label}: missing {needle!r} in output")
        return out

    # help / version
    expect("help", ["help"], 0, "Usage:")
    expect("version", ["version"], 0, "pickle 0.1.0")

    # reference tables: no network, no auth
    out = expect("contract", ["contract", "--json"], 0)
    try:
        payload = json.loads(out)
        if len(payload["endpoints"]) != 128:
            problems.append(f"contract: {len(payload['endpoints'])} endpoints, expected 128")
        if len(payload["error_types"]) != 23:
            problems.append(f"contract: {len(payload['error_types'])} error types, expected 23")
    except json.JSONDecodeError:
        problems.append("contract: output is not JSON")

    out = expect("tools", ["tools"], 0)
    try:
        tools = json.loads(out)
        if len(tools) < 20:
            problems.append(f"tools: {len(tools)} entries, expected >= 19")
        names = {tool["name"] for tool in tools}
        if "pickle_upload_cancel" not in names:
            problems.append("tools: pickle_upload_cancel missing")
    except json.JSONDecodeError:
        problems.append("tools: output is not JSON")

    # status / doctor without a session
    out = expect("status", ["status", "--json"], 0)
    payload = json.loads(out)
    if payload.get("signed_in") is not False:
        problems.append("status: signed_in should be false")

    expect("doctor", ["doctor"], 0, "resolver")
    expect("doctor-live", ["doctor", "--live"], 0, "skip")

    # refusals: these must not reach the network
    expect("send-no-body", ["send", "r1"], 1, "needs text or attachments")
    expect("react-no-target", ["react", "r1", "--set", "emoji=x"], 1, "target_id")
    expect("read-no-seq", ["read", "r1"], 1, "--seq")
    expect("raw-post-no-confirm", ["raw", "api/profile", "--method", "POST"], 1, "--confirm")
    expect("raw-unknown-path", ["raw", "api/not-a-real-path"], 1, "not an endpoint")
    expect("raw-unknown-method", ["raw", "api/sync", "--method", "BREW"], 1, "BREW")
    # A bare `login` now runs both steps; with stdin at EOF the first prompt
    # reports that nothing was entered instead of printing usage.
    expect("login-no-flag", ["login"], 1, "nothing was entered")
    expect("login-no-pending", ["login", "--finish", "123456"], 1, "no pending phone code")
    expect("unknown-command", ["definitely-not-a-command"], 1, "unknown command")
    # A repeated flag in the known list is a confusing message, not a feature.
    expect("unknown-flag-dedup", ["send", "r1", "--definitely-unknown"], 1, "json-file")
    # The JSON body routes are accepted by every body command.
    expect("json-body-accepted", ["send", "r1", "--json-body", '{"text":"x"}'], 1, "no stored session")
    expect("json-file-missing", ["send", "r1", "--json-file", "no-such.json"], 1, "no-such.json")
    expect("unknown-flag", ["status", "--nope"], 1, "unknown flag")
    expect("bad-env", ["status", "--env", "nope"], 1, "unknown environment")
    # mute/pin default to on, so they proceed to the network and stop at auth.
    expect("mute-default-on", ["mute", "r1"], 1, "no stored session")
    expect("pin-default-on", ["pin", "r1"], 1, "no stored session")
    expect("action-no-kind", ["action", "--set", "unrelated=1"], 1, "kind")
    # room_demand is a server-side literal set; refuse locally instead of
    # spending a round trip on a guaranteed 422.
    expect(
        "session-bad-demand",
        ["session", "--room-demand", "launch"],
        1,
        "room_demand must be one of",
    )
    expect("upload-missing-file", ["upload", "/definitely/not/here.png"], 1, "not/here.png")
    expect("upload-cancel-unauth", ["upload-cancel", "u1"], 1, "no stored session")
    expect("live-view-unauth", ["live-view", "req1"], 1, "no stored session")
    expect("leave-unauth", ["leave", "r1"], 1, "no stored session")
    expect("room-create-no-user", ["room-create"], 1, "user-id")
    expect("screen-unauth", ["screen", "profile"], 1, "no stored session")

    # unauthenticated reads: real server, JSON 401 (not a CDN HTML block)
    expect("rooms-unauth", ["rooms", "--json"], 1, "no stored session")

    # An expired JWT that cannot be refreshed is a different diagnosis.
    import base64, json as _json

    exp_state = Path(tempfile.mkdtemp()) / "expired.json"
    def _b64(obj):
        return base64.urlsafe_b64encode(_json.dumps(obj).encode()).rstrip(b"=").decode()
    exp_state.write_text(
        _json.dumps({
            "env": "prod",
            "api_base": "https://character-api-prod.picklemem.com",
            "device_id": "expired-probe",
            "session": {
                "user_id": "x",
                "login_session_token": "t",
                "login_session_jwt": f"{_b64({'alg':'none'})}.{_b64({'sub':'x','exp':1000000000})}.sig",
                "expires_at": "2001-09-09T01:46:40Z",
            },
            "pending_phone_code": None,
            "bootstrap": None,
            "base_resolved_at": 1795407266,
        }),
        encoding="utf-8",
    )
    expired_env = dict(os.environ, PICKLE_CLI_STATE=str(exp_state))
    rc_proc = subprocess.run(
        [str(BIN), "rooms", "--json"],
        capture_output=True, text=True, env=expired_env, timeout=60, encoding="utf-8",
    )
    checks += 1
    if rc_proc.returncode != 1 or "session expired" not in rc_proc.stderr:
        problems.append(
            f"expired-session: exit={rc_proc.returncode} stderr={rc_proc.stderr.strip()[:120]}"
        )

    # state is created on first run and reused
    status_a = json.loads(run(["status", "--json"], env)[1])
    status_b = json.loads(run(["status", "--json"], env)[1])
    if status_a["device_id"] != status_b["device_id"]:
        problems.append("device_id changed between runs")
    if not state.is_file():
        problems.append("state file was not created")

    print(f"checks: {checks}")
    if problems:
        for problem in problems:
            print(f"FAIL {problem}")
        return 1
    print("CLI branch coverage OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
