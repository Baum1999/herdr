#!/bin/sh
# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=codex
# HERDR_INTEGRATION_VERSION=9

set -eu

[ "${1:-}" = session ] || exit 0
[ "${HERDR_ENV:-}" = 1 ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0
if [ -z "${HERDR_SOCKET_PATH:-}" ] || ! command -v python3 >/dev/null 2>&1; then
  echo "herdr Codex session report unavailable (pane ${HERDR_PANE_ID})" >&2
  exit 1
fi

hook_input_file="$(mktemp "${TMPDIR:-/tmp}/herdr-codex-hook.XXXXXX")" || {
  echo "herdr Codex session report unavailable (pane ${HERDR_PANE_ID})" >&2
  exit 1
}
trap 'rm -f "$hook_input_file"' EXIT HUP INT TERM
cat >"$hook_input_file" || {
  echo "herdr Codex session report unavailable (pane ${HERDR_PANE_ID})" >&2
  exit 1
}

HERDR_HOOK_INPUT_FILE="$hook_input_file" python3 - <<'PY'
import json
import os
import socket
import sys

pane_id = os.environ["HERDR_PANE_ID"]

def fail(category):
    print(f"herdr Codex session report {category} (pane {pane_id})", file=sys.stderr)
    raise SystemExit(1)

try:
    with open(os.environ["HERDR_HOOK_INPUT_FILE"], encoding="utf-8") as handle:
        payload = json.load(handle)
except (OSError, ValueError):
    fail("invalid hook input")
if not isinstance(payload, dict):
    fail("invalid hook input")

if not payload.get("hook_event_name"):
    fail("invalid hook input")
if payload.get("hook_event_name") != "SessionStart":
    raise SystemExit(0)
session_id = payload.get("session_id")
transcript_path = payload.get("transcript_path")
source = payload.get("source")
if not isinstance(transcript_path, str) or not transcript_path.strip():
    raise SystemExit(0)
if not isinstance(session_id, str) or not session_id or source not in ("startup", "resume", "clear", "compact"):
    fail("invalid hook input")
inherited_session_id = os.environ.get("CODEX_THREAD_ID")
if inherited_session_id and inherited_session_id != session_id:
    raise SystemExit(0)

request = {
    "id": f"herdr:codex:{os.getpid()}",
    "method": "pane.report_codex_session",
    "params": {
        "pane_id": pane_id,
        "agent_session_id": session_id,
        "session_start_source": source,
        "reporter_pid": os.getpid(),
    },
}
try:
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(1)
        client.connect(os.environ["HERDR_SOCKET_PATH"])
        client.sendall((json.dumps(request) + "\n").encode())
        response = bytearray()
        while not response.endswith(b"\n"):
            chunk = client.recv(4096)
            if not chunk or len(response) + len(chunk) > 65536:
                fail("outcome unknown")
            response.extend(chunk)
except OSError:
    fail("outcome unknown")

try:
    result = json.loads(response)
except ValueError:
    fail("outcome unknown")
if not isinstance(result, dict):
    fail("outcome unknown")
if result.get("id") != request["id"]:
    fail("outcome unknown")
if "error" in result:
    fail("rejected")
body = result.get("result")
status = body.get("status") if isinstance(body, dict) else None
if status in ("applied", "unchanged"):
    raise SystemExit(0)
if status in ("invalidated", "rejected"):
    fail(status)
fail("outcome unknown")
PY
