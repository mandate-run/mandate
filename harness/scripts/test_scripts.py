"""Local-only regressions: no keys, real sellers or network payments."""
import contextlib
import fcntl
import importlib.util
import json
import os
import pathlib
import shutil
import socket
import sqlite3
import subprocess
import tempfile
import time
import unittest

SCRIPTS = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("ledger_action", SCRIPTS / "ledger-action.py")
ledger_action = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ledger_action)

ADAPTERS = SCRIPTS.parent / "adapters"
spec = importlib.util.spec_from_file_location("brief_to_prompt", ADAPTERS / "brief-to-prompt.py")
brief_to_prompt = importlib.util.module_from_spec(spec)
spec.loader.exec_module(brief_to_prompt)


class Scripts(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="mandate-scripts-")
        self.root = pathlib.Path(self.temp.name)
        self.ledger = self.root / "run.sqlite"

    def tearDown(self):
        self.temp.cleanup()

    def database(self):
        with contextlib.closing(sqlite3.connect(self.ledger)) as db:
            db.executescript("""
                CREATE TABLE authorizations(payment_state TEXT, delivery_state TEXT);
                CREATE TABLE reservations(state TEXT);
                CREATE TABLE audit_charges(state TEXT);
                CREATE TABLE receipts(hcs_sequence INTEGER);
            """)

    def test_missing_ledger_starts_a_run(self):
        self.assertEqual(ledger_action.action(self.ledger), "run")

    def test_corrupt_or_unknown_schema_is_preserved(self):
        for contents in [b"not sqlite", b""]:
            self.ledger.write_bytes(contents)
            with self.assertRaises(sqlite3.Error):
                ledger_action.action(self.ledger)
            self.assertTrue(self.ledger.exists())
            self.assertFalse((self.root / "archive").exists())

    def test_all_recoverable_states_resume(self):
        for table, row in [
            ("authorizations", "'prepared','none'"),
            ("authorizations", "'sent','none'"),
            ("authorizations", "'unresolved','none'"),
            ("authorizations", "'settled','none'"),
            ("authorizations", "'settled','received'"),
            ("reservations", "'held'"),
            ("audit_charges", "'submitted'"),
            ("audit_charges", "'reserved'"),
            ("receipts", "NULL"),
        ]:
            with self.subTest(table=table, row=row):
                self.database()
                with contextlib.closing(sqlite3.connect(self.ledger)) as db:
                    db.execute(f"INSERT INTO {table} VALUES ({row})")
                    db.commit()
                self.assertEqual(ledger_action.action(self.ledger), "resume")
                self.assertTrue(self.ledger.exists())
                self.ledger.unlink()

    def test_completed_ledgers_are_archived_without_collisions(self):
        for _ in range(2):
            self.database()
            with contextlib.closing(sqlite3.connect(self.ledger)) as db:
                db.execute("PRAGMA journal_mode=WAL")
                db.execute("INSERT INTO authorizations VALUES ('settled','validated')")
                db.commit()
            self.assertEqual(ledger_action.action(self.ledger), "run")
            self.assertFalse(self.ledger.exists())
        copies = list((self.root / "archive").glob("*/run.sqlite"))
        self.assertEqual(len(copies), 2)
        for path in copies:
            with contextlib.closing(sqlite3.connect(path)) as db:
                self.assertEqual(db.execute("SELECT payment_state FROM authorizations").fetchone()[0], "settled")

    def test_active_ledger_cannot_be_archived(self):
        self.database()
        with pathlib.Path(str(self.ledger) + ".lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with self.assertRaises(BlockingIOError):
                ledger_action.action(self.ledger)
        self.assertTrue(self.ledger.exists())

    def test_fixture_stops_descendants_and_keeps_unrelated_listener(self):
        scripts = self.root / "harness" / "scripts"
        scripts.mkdir(parents=True)
        script = scripts / "fixture-process.py"
        shutil.copyfile(SCRIPTS / script.name, script)
        (self.root / "sellers").mkdir()
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        pnpm = bin_dir / "pnpm"
        pnpm.write_text('#!/bin/sh\npython3 -m http.server "$PORT" --bind 127.0.0.1 &\nwait\n')
        pnpm.chmod(0o755)
        with socket.socket() as port_probe:
            port_probe.bind(("127.0.0.1", 0))
            port = port_probe.getsockname()[1]
        env = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}", PROCTOR_PORT=str(port))
        def call(action):
            return subprocess.run(["python3", str(script), action], env=env, capture_output=True, text=True, timeout=8)
        def listening():
            with socket.socket() as probe:
                probe.settimeout(0.1)
                return probe.connect_ex(("127.0.0.1", port)) == 0
        try:
            self.assertEqual(call("up").returncode, 0)
            deadline = time.monotonic() + 5
            while not listening() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(listening())
            self.assertEqual(call("down").returncode, 0)
            self.assertFalse(listening(), "grandchild listener survived fixture teardown")
            with socket.socket() as unrelated:
                unrelated.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                unrelated.bind(("127.0.0.1", port))
                unrelated.listen()
                self.assertNotEqual(call("up").returncode, 0)
                self.assertTrue(listening(), "unrelated listener must stay alive")
        finally:
            call("down")


class AgentBrief(unittest.TestCase):
    """What an agent is told is the reviewable part of the loop."""

    def brief(self, **over):
        b = {
            "task_toml": '[task]\nname = "x"',
            "task_name": "x",
            "task_hash": "abc",
            "attempt": 2,
            "attempts_allowed": 3,
            "outcome": "IMPLEMENTATION_FAILURE",
            "findings": ["answer: expected value to be 3, found 1"],
            "observed": {"ledger_settled": 6},
        }
        b.update(over)
        return b

    def test_the_prompt_carries_the_contract_and_the_measurements(self):
        text = brief_to_prompt.prompt(self.brief())
        # The contract itself, so the agent reads what Proctor hashed.
        self.assertIn('name = "x"', text)
        self.assertIn("found 1", text)
        # The independent half is labelled as independent: an agent that
        # thinks it can satisfy `observe` by printing will waste an attempt.
        self.assertIn("ledger_settled = 6", text)
        self.assertIn("application does not write", text)
        self.assertIn("attempt 2 of 3", text)

    def test_the_prompt_forbids_editing_the_contract(self):
        # The one instruction that keeps a run meaningful.
        text = brief_to_prompt.prompt(self.brief())
        self.assertIn("never the task contract", text)
        self.assertIn("outstanding", text, "a payment rule must reach the agent")

    def test_a_brief_without_findings_still_says_what_to_do(self):
        # PAYMENT_UNRESOLVED never reaches an agent, but an attempt can fail
        # on measurements alone, so an empty findings list is not a crash.
        text = brief_to_prompt.prompt(self.brief(findings=[], observed={}))
        self.assertIn("no assertion failed", text)
        self.assertIn("Make the change now", text)

    def test_a_brief_that_is_not_a_brief_is_refused(self):
        def run(payload):
            return subprocess.run(
                ["python3", str(ADAPTERS / "brief-to-prompt.py")],
                input=payload, capture_output=True, text=True, check=False,
            )

        self.assertNotEqual(run("not json").returncode, 0)
        # A JSON document missing what the prompt needs is refused rather
        # than turned into a prompt with holes in it.
        self.assertNotEqual(run(json.dumps({"attempt": 1})).returncode, 0)
        self.assertEqual(run(json.dumps(self.brief())).returncode, 0)


if __name__ == "__main__":
    unittest.main()
