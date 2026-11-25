#!/usr/bin/env python3
"""Drive `pickle mcp` over stdio and check the JSON-RPC contract.

Sends initialize, tools/list and two tools/call requests (one read-only, one
that must be refused), then asserts on the responses. A handshake that passes
here means a real MCP client can drive the server.
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
    BIN = ROOT / "target" / "debug" / "pickle.exe"


def run_login_persistence_pass() -> list[str]:
    """MCP login must install the session and write it to the state file."""
    problems: list[str] = []
    fixture = subprocess.Popen(
        [sys.executable, str(ROOT / "tests" / "fixture_api.py")],
        stdout=subprocess.PIPE,
        text=True,
        encoding="utf-8",
    )
    try:
        port = json.loads(fixture.stdout.readline())["port"]
        base = f"http://127.0.0.1:{port}"
        state = Path(tempfile.mkdtemp()) / "state.json"
        state.write_text(
            json.dumps({
                "env": "prod", "api_base": base, "device_id": "mcp-login",
                "session": None,
                "pending_phone_code": {"pending_id": "pend-1", "region": "jp",
                                       "expires_in": 600},
                "bootstrap": None, "base_resolved_at": 9999999999,
            }),
            encoding="utf-8",
        )
        env = dict(os.environ, PICKLE_CLI_STATE=str(state))
        requests = [
            {"jsonrpc": "2.0", "id": 20, "method": "tools/call",
             "params": {"name": "pickle_login_verify",
                        "arguments": {"pending_id": "pend-1", "code": "123456"}}},
            {"jsonrpc": "2.0", "id": 21, "method": "tools/call",
             "params": {"name": "pickle_status", "arguments": {}}},
        ]
        proc = subprocess.run(
            [str(BIN), "mcp"],
            input="".join(json.dumps(r) + "\n" for r in requests),
            capture_output=True, text=True, env=env, timeout=60, encoding="utf-8",
        )
        responses = {}
        for line in proc.stdout.splitlines():
            if line.strip():
                message = json.loads(line)
                responses[message.get("id")] = message
        verify = responses.get(20, {}).get("result", {})
        if verify.get("isError") is not False:
            problems.append(f"login_verify: isError={verify.get('isError')}")
        after = json.loads(state.read_text(encoding="utf-8"))
        if after.get("session") is None:
            problems.append("login_verify did not persist the session")
        # Tool calls run concurrently, so a status in the same batch can answer
        # before the login finishes. Check the persisted session in a fresh
        # process instead: that also proves it survives a restart.
        follow = subprocess.run(
            [str(BIN), "mcp"],
            input=json.dumps({"jsonrpc": "2.0", "id": 30, "method": "tools/call",
                              "params": {"name": "pickle_status", "arguments": {}}}) + "\n",
            capture_output=True, text=True, env=env, timeout=60, encoding="utf-8",
        )
        status_text = ""
        for line in follow.stdout.splitlines():
            if line.strip():
                message = json.loads(line)
                status_text = (message.get("result", {}).get("content") or [{}])[0].get("text", "")
        if "true" not in status_text.replace(" ", "")[status_text.replace(" ", "").find("signed_in"):][:20]:
            problems.append(f"persisted session did not sign the CLI in: {status_text[:120]}")
        print(f"login persistence pass: session saved={after.get('session') is not None}")
    finally:
        fixture.terminate()
        try:
            fixture.wait(timeout=5)
        except subprocess.TimeoutExpired:
            fixture.kill()
    return problems


def run_concurrency_pass() -> list[str]:
    """Three tool calls in one batch must all answer, in any order."""
    problems: list[str] = []
    fixture = subprocess.Popen(
        [sys.executable, str(ROOT / "tests" / "fixture_api.py")],
        stdout=subprocess.PIPE,
        text=True,
        encoding="utf-8",
    )
    try:
        port = json.loads(fixture.stdout.readline())["port"]
        base = f"http://127.0.0.1:{port}"
        state = Path(tempfile.mkdtemp()) / "state.json"
        jwt = "eyJhbGciOiJub25lIn0.eyJzdWIiOiJmIiwiZXhwIjoyMDAwMDAwMDAwfQ.sig"
        state.write_text(
            f'{{"env":"prod","api_base":"{base}","device_id":"d","session":'
            f'{{"user_id":"f","login_session_token":"t","login_session_jwt":"{jwt}",'
            f'"expires_at":"2033-05-18T00:00:00Z"}},"pending_phone_code":null,'
            f'"bootstrap":null,"base_resolved_at":9999999999}}',
            encoding="utf-8",
        )
        env = dict(os.environ, PICKLE_CLI_STATE=str(state))
        requests = [
            {"jsonrpc": "2.0", "id": 1, "method": "tools/call",
             "params": {"name": "pickle_rooms", "arguments": {}}},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/call",
             "params": {"name": "pickle_session", "arguments": {}}},
            {"jsonrpc": "2.0", "id": 3, "method": "tools/call",
             "params": {"name": "pickle_events",
                        "arguments": {"max_frames": 1, "timeout_sec": 5}}},
        ]
        proc = subprocess.run(
            [str(BIN), "mcp"],
            input="".join(json.dumps(r) + "\n" for r in requests).encode("utf-8"),
            capture_output=True,
            env=env,
            timeout=90,
        )
        responses = {}
        for line in proc.stdout.decode("utf-8").splitlines():
            if line.strip():
                message = json.loads(line)
                responses[message.get("id")] = message
        if len(responses) != 3:
            problems.append(f"concurrency: {len(responses)} of 3 responses")
        for rid, message in responses.items():
            if message.get("result", {}).get("isError") is not False:
                problems.append(f"concurrency id={rid}: isError set")
        print(f"concurrency pass: {len(responses)} of 3 responses")
    finally:
        fixture.terminate()
        try:
            fixture.wait(timeout=5)
        except subprocess.TimeoutExpired:
            fixture.kill()
    return problems


REQUIRED_BY_TOOL = {
    "pickle_login_start": ["phone"],
    "pickle_login_verify": ["pending_id", "code"],
    "pickle_history": ["room_id"],
    "pickle_screen": ["name"],
    "pickle_message_send": ["room_id"],
    "pickle_react": ["room_id", "target_id", "target_seq", "emoji"],
    "pickle_mark_read": ["room_id", "seq"],
    "pickle_room_settings": ["room_id"],
    "pickle_room_create": ["kind", "user_id"],
    "pickle_attachment_upload": ["path"],
    "pickle_upload_cancel": ["upload_id"],
    "pickle_request_answer": ["request_id", "action"],
    "pickle_room_leave": ["room_id"],
    "pickle_login": ["phone", "code"],
    "pickle_raw": ["method", "path"],
}


def check_schemas(tools) -> list[str]:
    """Every tool that needs arguments must say so in its schema."""
    problems: list[str] = []
    by_name = {tool["name"]: tool for tool in tools}
    for name, expected in REQUIRED_BY_TOOL.items():
        schema = by_name.get(name, {}).get("inputSchema", {})
        if "oneOf" in schema:
            # Conditional schemas are checked by their branch requirements.
            branches = [branch.get("required", []) for branch in schema["oneOf"]]
            if not any(expected[0] in branch for branch in branches):
                problems.append(f"{name}: no oneOf branch requires {expected[0]}")
            continue
        missing = [key for key in expected if key not in schema.get("required", [])]
        if missing:
            problems.append(f"{name}: schema does not require {missing}")
    # pickle_action is conditional: run needs kind, result needs action_id.
    action = by_name.get("pickle_action", {}).get("inputSchema", {})
    branches = [branch.get("required", []) for branch in action.get("oneOf", [])]
    if not any("kind" in branch for branch in branches):
        problems.append("pickle_action: no branch requires kind")
    if not any("action_id" in branch for branch in branches):
        problems.append("pickle_action: no branch requires action_id")
    print(f"schema check: {len(REQUIRED_BY_TOOL) + 1} tools")
    return problems


def run_fixture_pass() -> list[str]:
    """Drive pickle_session and pickle_rooms against the loopback fixture."""
    problems: list[str] = []
    fixture = subprocess.Popen(
        [sys.executable, str(ROOT / "tests" / "fixture_api.py")],
        stdout=subprocess.PIPE,
        text=True,
        encoding="utf-8",
    )
    try:
        port = json.loads(fixture.stdout.readline())["port"]
        base = f"http://127.0.0.1:{port}"
        state = Path(tempfile.mkdtemp()) / "state.json"
        jwt = "eyJhbGciOiJub25lIn0.eyJzdWIiOiJmIiwiZXhwIjoyMDAwMDAwMDAwfQ.sig"
        state.write_text(
            f'{{"env":"prod","api_base":"{base}","device_id":"mcp-fixture",'
            f'"session":{{"user_id":"f","login_session_token":"t",'
            f'"login_session_jwt":"{jwt}","expires_at":"2033-05-18T00:00:00Z"}},'
            f'"pending_phone_code":null,"bootstrap":null}}',
            encoding="utf-8",
        )
        env = dict(os.environ, PICKLE_CLI_STATE=str(state))
        requests = [
            {"jsonrpc": "2.0", "id": 10, "method": "tools/call",
             "params": {"name": "pickle_session", "arguments": {}}},
            {"jsonrpc": "2.0", "id": 11, "method": "tools/call",
             "params": {"name": "pickle_rooms", "arguments": {}}},
        ]
        proc = subprocess.run(
            [str(BIN), "mcp"],
            input="".join(json.dumps(r) + "\n" for r in requests).encode("utf-8"),
            capture_output=True,
            env=env,
            timeout=60,
        )
        responses = {}
        for line in proc.stdout.decode("utf-8").splitlines():
            if not line.strip():
                continue
            message = json.loads(line)
            responses[message.get("id")] = message
        session = responses.get(10, {}).get("result", {})
        if session.get("isError") is not False:
            problems.append(f"fixture pickle_session: isError={session.get('isError')}")
        elif "fixture-session" not in session.get("content", [{}])[0].get("text", ""):
            problems.append("fixture pickle_session: session_id missing from the payload")
        rooms = responses.get(11, {}).get("result", {})
        if rooms.get("isError") is not False:
            problems.append(f"fixture pickle_rooms: isError={rooms.get('isError')}")
        elif "fixture-room" not in rooms.get("content", [{}])[0].get("text", ""):
            problems.append("fixture pickle_rooms: room id missing from the payload")
        print(f"fixture pass: {len(responses)} tool responses")
    finally:
        fixture.terminate()
        try:
            fixture.wait(timeout=5)
        except subprocess.TimeoutExpired:
            fixture.kill()
    return problems


def main() -> int:
    sys.stdout.reconfigure(encoding="utf-8")
    state = Path(tempfile.mkdtemp()) / "state.json"
    env = dict(os.environ, PICKLE_CLI_STATE=str(state))

    requests = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05", "capabilities": {}}},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
        {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "pickle_status", "arguments": {}}},
        {"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "pickle_nonexistent", "arguments": {}}},
        {"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "pickle_raw", "arguments": {"method": "POST", "path": "api/x"}}},
    ]
    stdin = "".join(json.dumps(request) + "\n" for request in requests)

    proc = subprocess.run(
        [str(BIN), "mcp"],
        input=stdin.encode("utf-8"),
        capture_output=True,
        env=env,
        timeout=60,
    )
    if proc.returncode != 0:
        print(f"FAIL exit={proc.returncode}")
        print(proc.stderr.decode("utf-8", "replace")[:600])
        return 1

    lines = [line for line in proc.stdout.decode("utf-8").splitlines() if line.strip()]
    responses = {}
    for line in lines:
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            print(f"FAIL non-JSON stdout line: {line[:120]}")
            return 1
        responses[message.get("id")] = message

    problems = []

    init = responses.get(1, {})
    if init.get("result", {}).get("protocolVersion") != "2024-11-05":
        problems.append("initialize: missing protocolVersion")
    if not init.get("result", {}).get("instructions"):
        problems.append("initialize: missing instructions")
    server = init.get("result", {}).get("serverInfo", {})
    if server.get("name") != "pickle-mcp":
        problems.append(f"initialize: serverInfo.name={server.get('name')!r}")

    tools = responses.get(2, {}).get("result", {}).get("tools", [])
    if len(tools) < 22:
        problems.append(f"tools/list: only {len(tools)} tools")
    names = [tool.get("name") for tool in tools]
    for required in ("pickle_status", "pickle_login_start", "pickle_rooms", "pickle_message_send", "pickle_events", "pickle_upload_cancel", "pickle_doctor"):
        if required not in names:
            problems.append(f"tools/list: missing {required}")

    status = responses.get(3, {}).get("result", {})
    if status.get("isError") is not False:
        problems.append(f"pickle_status: isError={status.get('isError')}")
    text = status.get("content", [{}])[0].get("text", "")
    try:
        payload = json.loads(text)
    except json.JSONDecodeError:
        problems.append("pickle_status: content is not JSON")
        payload = {}
    if payload.get("signed_in") is not False:
        problems.append(f"pickle_status: signed_in={payload.get('signed_in')}")

    unknown = responses.get(4, {}).get("result", {})
    if unknown.get("isError") is not True:
        problems.append("unknown tool: isError was not true")

    refused = responses.get(5, {}).get("result", {})
    if refused.get("isError") is not True:
        problems.append("pickle_raw POST without confirm was not refused")
    elif "confirm" not in refused.get("content", [{}])[0].get("text", ""):
        problems.append("pickle_raw refusal did not mention confirm")

    # Second pass: with the loopback fixture the read tools actually return data,
    # which proves tools/call reaches the same client as the CLI commands.
    problems.extend(check_schemas(tools))

    fixture_problems = run_fixture_pass()
    problems.extend(fixture_problems)
    problems.extend(run_concurrency_pass())
    problems.extend(run_login_persistence_pass())

    print(f"responses: {len(responses)}  tools: {len(tools)}")
    if problems:
        for problem in problems:
            print(f"FAIL {problem}")
        return 1
    print("MCP handshake OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
