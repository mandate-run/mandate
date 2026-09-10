#!/usr/bin/env python3
"""Preserve recoverable task state; archive only a fully finished ledger."""
import fcntl
import pathlib
from contextlib import closing
import sqlite3
import sys
import tempfile


def action(path):
    with pathlib.Path(str(path) + ".lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return locked_action(path)


def locked_action(path):
    if not path.exists():
        return "run"
    # Missing tools, damaged databases and unknown schemas are errors, never
    # evidence that a previous attempt spent nothing.
    with closing(sqlite3.connect(path.as_uri() + "?mode=rw", uri=True, timeout=5)) as conn:
        open_payments = conn.execute("""
            SELECT COUNT(*) FROM authorizations
            WHERE payment_state NOT IN ('settled','failed')
               OR (payment_state = 'settled' AND delivery_state IN ('none','received'))
        """).fetchone()[0]
        held = conn.execute("SELECT COUNT(*) FROM reservations WHERE state = 'held'").fetchone()[0]
        audit = conn.execute("SELECT COUNT(*) FROM audit_charges WHERE state IN ('reserved','submitted')").fetchone()[0]
        receipts = conn.execute("SELECT COUNT(*) FROM receipts WHERE hcs_sequence IS NULL").fetchone()[0]
        if open_payments or held or audit or receipts:
            return "resume"
        checkpoint = conn.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone()
        if checkpoint[0]:
            raise RuntimeError("ledger is busy; cannot archive it")
    archive = path.parent / "archive"
    archive.mkdir(exist_ok=True)
    destination = pathlib.Path(tempfile.mkdtemp(prefix=path.stem + "-", dir=archive))
    # Keep sidecars too. Never discard a WAL containing payment evidence.
    for suffix in ("", "-wal", "-shm"):
        source = pathlib.Path(str(path) + suffix)
        if source.exists():
            source.rename(destination / source.name)
    return "run"


if __name__ == "__main__":
    try:
        print(action(pathlib.Path(sys.argv[1]).resolve()))
    except (OSError, sqlite3.Error, RuntimeError) as exc:
        print(f"ledger preserved: {exc}", file=sys.stderr)
        sys.exit(2)
