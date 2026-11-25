#!/usr/bin/env python3
"""Impersonate the Pickle API on loopback so the whole chain can be run.

Serves the shapes the app expects (session, sync, history, messages, read) with
the required keys from `extract/serializers.json`. Used by `e2e_live.py --fixture`
to prove session -> sync -> history -> send -> read wiring without a real account.

Not a substitute for the live service: it only replays documented shapes.
"""
from __future__ import annotations

import json
import sys
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

SESSION = {
    "session_id": "fixture-session",
    "room": "fixture-room",
    "livekit_url": "wss://fixture.invalid/rtc",
    "token": "fixture-livekit-token",
    "participant_identity": "fixture-identity",
    "account_rev": 1,
    "user_id": "fixture-user",
    "plan": "free",
    "plan_rank": 0,
    "character_name": "Fixture",
}

ROOMS = {
    "v": 1,
    "rev": 1,
    "rooms": [
        {
            "room_id": "fixture-room",
            "kind": "character_direct",
            "title": "Fixture room",
            "member_count": 2,
            "last_seq": 5,
            "read_seq": 3,
            "role": "owner",
        }
    ],
}

DELTA = {
    "v": 1,
    "items": [],
    "reactions": [],
    "card_states": [],
    "next_after": 5,
    "has_more": False,
    "head_seq": 5,
    "projection_version": 1,
}

PAGE = {"v": 1, "messages": [], "has_more": False, "next_before": None}


SEEN_BODIES: list[dict] = []


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):  # keep stdout clean for the harness
        pass

    def _read_body(self) -> dict:
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b""
        try:
            body = json.loads(raw) if raw else {}
        except json.JSONDecodeError:
            return {}
        SEEN_BODIES.append({"path": self.path.split("?")[0], "body": body})
        return body

    def _send(self, status: int, body: dict):
        raw = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_GET(self):
        path = self.path.split("?")[0]
        if path == "/api/sync":
            return self._send(200, ROOMS)
        if path.endswith("/history/delta"):
            return self._send(200, DELTA)
        if path.endswith("/history/page"):
            return self._send(200, PAGE)
        if path.endswith("/history"):
            # Measured shape: ChatSnapshot (v + messages), not the rooms list.
            return self._send(200, {
                "v": 1, "messages": [], "reactions": [], "card_states": [],
                "window_max_seq": 5, "log_epoch": "fixture-room",
                "projection_version": 1, "read_seq": 3,
            })
        if path.startswith("/api/screens/"):
            return self._send(200, {"tree": {"atom": "view"}})
        # The dedicated routes a screen name maps to (see README.md).
        if path in (
            "/api/profile",
            "/api/vault",
            "/api/character",
            "/api/character/settings",
            "/api/character/colors",
            "/api/character/avatar/history",
            "/api/notification-settings",
            "/api/profile/handle/check",
            "/api/bubbles",
        ) or path.startswith("/api/bubbles/"):
            return self._send(200, {"tree": {"atom": "view"}})
        if path == "/api/session/establishment":
            return self._send(200, {"phase": "ready"})
        if path == "/__seen":
            return self._send(200, {"seen": SEEN_BODIES})
        if path == "/api/events":
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            self.wfile.write(b"event: account\ndata: {}\n\n")
            return
        return self._send(404, {"detail": "not_found"})

    def do_POST(self):
        path = self.path.split("?")[0]
        if path == "/api/login-sessions/refresh":
            self._read_body()
            return self._send(200, {
                "login_session_jwt": "eyJhbGciOiJub25lIn0.eyJzdWIiOiJmIiwiZXhwIjoyMDAwMDAwMDAwfQ.refreshed",
                "expires_at": "2033-05-18T00:00:00Z",
            })
        if path == "/api/login-sessions/phone-code":
            self._read_body()
            return self._send(200, {"pending_id": "fixture-pending", "expires_in": 600})
        if path == "/api/login-sessions":
            body = self._read_body()
            # `code` values drive the retry path: "wrong" answers invalid_code.
            if body.get("code") == "wrong":
                return self._send(401, {"detail": "invalid_code"})
            return self._send(200, {
                "status": "issued",
                "user_id": "fixture-user",
                "login_session_token": "fixture-token",
                "login_session_jwt": "eyJhbGciOiJub25lIn0.eyJzdWIiOiJmIiwiZXhwIjoyMDAwMDAwMDAwfQ.sig",
                "expires_at": "2033-05-18T00:00:00Z",
            })
        if path == "/api/session":
            self._read_body()
            return self._send(200, SESSION)
        if path.endswith("/messages"):
            self._read_body()
            return self._send(200, {"status": "ok", "seq": 6, "responder": "character"})
        if path == "/api/attachments":
            return self._send(200, {"status": "ok", "ref": "fixture-ref"})
        if path == "/api/attachments/pending":
            return self._send(200, {"status": "ok"})
        if path.endswith("/leave"):
            return self._send(200, {"status": "ok"})
        if path.endswith("/reactions"):
            return self._send(200, {"status": "ok"})
        if path == "/api/actions":
            return self._send(200, {"status": "accepted"})
        if path.endswith("/answer"):
            return self._send(200, {"status": "ok"})
        return self._send(404, {"detail": "not_found"})

    def do_PUT(self):
        return self._send(200, {})

    def do_DELETE(self):
        return self._send(200, {})


def main() -> int:
    port = 0
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    actual = server.server_address[1]
    # The harness needs the port on stdout before it starts the CLI.
    print(json.dumps({"port": actual}), flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
