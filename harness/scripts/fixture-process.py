#!/usr/bin/env python3
"""Own the fixture's process group, including pnpm/tsx/node descendants."""
import fcntl
import json
import os
import pathlib
import signal
import socket
import subprocess
import sys
import time
import uuid

SCRIPT = pathlib.Path(__file__).resolve()
ROOT = SCRIPT.parents[2]


def stop(pidfile):
    if not pidfile.exists():
        return
    record = json.loads(pidfile.read_text())
    if not isinstance(record, dict) or not isinstance(record.get("pid"), int) or not isinstance(record.get("nonce"), str):
        raise RuntimeError("legacy or invalid fixture PID record; inspect the old process before removing this file")
    pid, nonce = record["pid"], record["nonce"]
    command = subprocess.run(["ps", "-p", str(pid), "-o", "args="], capture_output=True, text=True).stdout
    if not command:
        pidfile.unlink()
        return
    # Never signal a PID reused by an unrelated process.
    if str(SCRIPT) not in command or nonce not in command or os.getpgid(pid) != pid:
        raise RuntimeError("fixture PID is no longer owned by this run; refusing to stop it")
    os.killpg(pid, signal.SIGTERM)
    until = time.monotonic() + 2
    while time.monotonic() < until:
        try:
            os.killpg(pid, 0)
        except ProcessLookupError:
            break
        time.sleep(0.05)
    try:
        os.killpg(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    pidfile.unlink()


def main():
    if sys.argv[1] == "serve":
        # The supervisor remains identifiable until its pnpm child finishes.
        child = subprocess.Popen(["pnpm", "-s", "sellers"], cwd=ROOT / "sellers")
        signal.signal(signal.SIGTERM, lambda *_: child.terminate())
        return child.wait()
    port = int(os.environ.get("PROCTOR_PORT", "4021"))
    if not 1 <= port <= 65535:
        raise RuntimeError("PROCTOR_PORT must be between 1 and 65535")
    run = ROOT / ".proctor" / f"run-{port}"
    run.mkdir(parents=True, exist_ok=True)
    with (run / "fixture.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        pidfile = run / "sellers.pid"
        stop(pidfile)
        if sys.argv[1] == "down":
            return 0
        # An unrelated listener must not be mistaken for our fixture.
        with socket.socket() as probe:
            probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            probe.bind(("127.0.0.1", port))
        journal = run / "journal"
        journal.mkdir(exist_ok=True)
        path = journal / "sellers-journal.jsonl"
        if path.exists():
            path.rename(journal / f"sellers-journal-{uuid.uuid4().hex}.jsonl")
        env = dict(os.environ,
                   GRAPH_FIXTURE=os.environ.get("PROCTOR_FIXTURE", "mixed"),
                   SELLER_ASSET=os.environ.get("PROCTOR_ASSET", "HBAR"),
                   JOURNAL_DIR=str(journal), PORT=str(port),
                   FAULT=sys.argv[2] if len(sys.argv) > 2 else "")
        nonce = uuid.uuid4().hex
        with (run / "sellers.log").open("ab") as log:
            child = subprocess.Popen([sys.executable, str(SCRIPT), "serve", nonce],
                                     env=env, stdin=subprocess.DEVNULL, stdout=log,
                                     stderr=log, start_new_session=True)
        try:
            pidfile.write_text(json.dumps({"pid": child.pid, "nonce": nonce}))
        except OSError:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait()
            raise
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, RuntimeError) as exc:
        print(f"fixture: {exc}", file=sys.stderr)
        sys.exit(2)
