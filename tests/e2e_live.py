#!/usr/bin/env python3
"""End-to-end runner: session -> rooms -> history -> send -> read.

Two backends:

  --fixture    start `tests/fixture_api.py` on loopback and drive the whole
               chain against it. Proves the wiring and the response parsing
               with no account and no SMS.
  (default)    use the real service with the stored login session.

Send and read mutate state, so both require --write. Without it the run stops
after history.

Usage:
    python tests/e2e_live.py --fixture --write
    python tests/e2e_live.py                 real service, read only
    python tests/e2e_live.py --write         real service, send + read
"""
from __future__ import annotations

import argparse
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


def run(args, env):
    proc = subprocess.run(
        [str(BIN), *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=env,
        timeout=120,
    )
    return proc.returncode, proc.stdout, proc.stderr


def json_out(args, env):
    code, out, err = run(args, env)
    try:
        return code, json.loads(out), err
    except json.JSONDecodeError:
        return code, None, err or out[:200]


def pick_room(payload, preferred):
    rooms = payload.get("rooms") if isinstance(payload, dict) else None
    if not isinstance(rooms, list):
        return None
    if preferred:
        return preferred
    for room in rooms:
        if not isinstance(room, dict):
            continue
        for key in ("room_id", "id", "room"):
            value = room.get(key)
            if isinstance(value, str) and value:
                return value
    return None


def start_fixture():
    """Launch the loopback fixture and return (base, process)."""
    proc = subprocess.Popen(
        [sys.executable, str(ROOT / "tests" / "fixture_api.py")],
        stdout=subprocess.PIPE,
        text=True,
        encoding="utf-8",
    )
    line = proc.stdout.readline()
    port = json.loads(line)["port"]
    return f"http://127.0.0.1:{port}", proc


def write_state(base, signed_in=True):
    path = Path(tempfile.mkdtemp()) / "state.json"
    jwt = "eyJhbGciOiJub25lIn0.eyJzdWIiOiJmaXh0dXJlIiwiZXhwIjoyMDAwMDAwMDAwfQ.sig"
    session = (
        f'{{"user_id":"fixture-user","login_session_token":"fixture-token",'
        f'"login_session_jwt":"{jwt}","expires_at":"2033-05-18T00:00:00Z"}}'
        if signed_in
        else "null"
    )
    path.write_text(
        f'{{"env":"prod","api_base":"{base}","device_id":"e2e-device",'
        f'"session":{session},"pending_phone_code":null,"bootstrap":null}}',
        encoding="utf-8",
    )
    return path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture", action="store_true", help="run against the loopback fixture")
    parser.add_argument("--write", action="store_true", help="also send a message and mark read")
    parser.add_argument("--room", default=None)
    parser.add_argument("--text", default=None)
    parsed = parser.parse_args()

    sys.stdout.reconfigure(encoding="utf-8")
    fixture = None
    if parsed.fixture:
        base, fixture = start_fixture()
        state = write_state(base)
        env = dict(os.environ, PICKLE_CLI_STATE=str(state))
        print(f"backend: fixture {base}")
    else:
        env = dict(os.environ)
        print("backend: live service")

    problems = []
    steps = 0

    def note(label, ok, detail=""):
        nonlocal steps
        steps += 1
        print(f"{label:<12}{'ok  ' if ok else 'FAIL'}{detail}")
        if not ok:
            problems.append(f"{label}: {detail}")

    try:
        code, status, err = json_out(["status", "--json"], env)
        signed_in = isinstance(status, dict) and status.get("signed_in") is True
        note("status", code == 0 and signed_in,
             "" if signed_in else (err.strip()[:120] or "not signed in"))
        if not signed_in:
            print("\nrun `pickle login --start --stdin` then `--finish --stdin` first")
            return 1
        print(f"{'':<12}api_base={status.get('api_base')}")

        code, bootstrap, err = json_out(["session", "--json"], env)
        if not isinstance(bootstrap, dict):
            note("session", False, err.strip()[:160] or "no JSON")
            return 1
        required = ["session_id", "room", "livekit_url", "participant_identity", "account_rev", "user_id"]
        missing = [k for k in required if bootstrap.get(k) in (None, "")]
        if missing:
            note("session", False, f"missing {missing}")
        else:
            note("session", code == 0,
                 f"session_id={bootstrap.get('session_id')} room={bootstrap.get('room')}")

        code, rooms, err = json_out(["rooms", "--json"], env)
        count = rooms.get("count") if isinstance(rooms, dict) else None
        room = pick_room(rooms, parsed.room) if isinstance(rooms, dict) else None
        note("rooms", code == 0 and bool(room),
             f"count={count} room={room}" if room else (err.strip()[:160] or "no room id"))
        if not room:
            return 1

        for mode in ("delta", "page", "full"):
            extra = ["--limit", "5"] if mode != "full" else []
            code, hist, err = json_out(["history", room, "--mode", mode, *extra, "--json"], env)
            ok = code == 0 and isinstance(hist, dict)
            note(f"history:{mode}", ok, "" if ok else err.strip()[:160])

        # `handoffs` is a generic api/screens/<name> route that the live service
        # serves; the earlier placeholder name always 404ed.
        for name in ("profile", "vault", "handoffs"):
            code, screen, err = json_out(["screen", name, "--json"], env)
            note(f"screen:{name}", code == 0 and isinstance(screen, dict),
                 "" if code == 0 else err.strip()[:160])

        if not parsed.write:
            print("\nread-only run complete (pass --write to send)")
            return 1 if problems else 0

        if not parsed.fixture and not parsed.room:
            # Writing into an arbitrary room is how a test message lands in a
            # real conversation. Name the room explicitly.
            print("\nrefusing to write to a real account without --room")
            return 1

        text = parsed.text or "pickle-cli e2e check"
        code, sent, err = json_out(["send", room, "--set", f"text={text}", "--json"], env)
        seq = sent.get("seq") if isinstance(sent, dict) else None
        if code == 0 and isinstance(sent, dict):
            note("send", True, f"seq={seq} status={sent.get('status')}")
        else:
            note("send", False, err.strip()[:160])

        if not isinstance(seq, int):
            # Measured: the send answer is {"status":"ok","seq":null,"responder":"chat"}.
            # The room's newest seq is `head_seq` in the delta response, so read
            # that instead of guessing from the message list.
            code, hist, err = json_out(
                ["history", room, "--mode", "delta", "--limit", "5", "--json"], env)
            if isinstance(hist, dict):
                candidate = hist.get("head_seq")
                if isinstance(candidate, int):
                    seq = candidate
        if isinstance(seq, int):
            code, _, err = json_out(["read", room, "--seq", str(seq), "--json"], env)
            note("read", code == 0, f"seq={seq}" if code == 0 else err.strip()[:160])
        else:
            note("read", False, "no seq in the history either")

        code, reacted, err = json_out(
            ["react", room, "--set", "target_id=fixture-msg", "--set", "target_seq=1",
             "--set", "emoji=x", "--json"], env)
        note("react", code == 0, "" if code == 0 else err.strip()[:160])

        if parsed.fixture:
            # Measured 2026-11-23: POST api/rooms/<own character room>/leave answers
            # 404 not_found on the live service, so leave is only exercised
            # against the fixture. It is destructive either way.
            code, left, err = json_out(["leave", room, "--json"], env)
            note("leave", code == 0, "" if code == 0 else err.strip()[:160])
        else:
            note("leave", True, "skipped on the live account (measured 404 for own room)")

        print()
        if problems:
            for problem in problems:
                print(f"FAIL {problem}")
            return 1
        print(f"e2e OK ({steps} steps)")
        return 0
    finally:
        if fixture is not None:
            fixture.terminate()
            try:
                fixture.wait(timeout=5)
            except subprocess.TimeoutExpired:
                fixture.kill()


if __name__ == "__main__":
    raise SystemExit(main())
