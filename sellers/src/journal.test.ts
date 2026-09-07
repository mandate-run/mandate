import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { Journal, sha256Hex } from "./journal.js";

test("sha256Hex matches the known vector", () => {
  assert.equal(sha256Hex("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
  assert.equal(sha256Hex(Buffer.from("abc")), sha256Hex("abc"));
});

test("journal appends one JSON line per entry and keeps them in memory", () => {
  const path = join(tmpdir(), `mandate-journal-${process.pid}-${Date.now()}`, "j.jsonl");
  const journal = new Journal(path);
  const entry = {
    ts: "2026-09-07T00:00:00.000Z",
    route: "GET /spike",
    payment_id: "pay_1",
    signed_payload_hash: "h",
    settle_called: true,
    settle_ok: true,
    served_hash: "s",
    status: 200,
    source: "live" as const,
  };
  journal.write(entry);
  journal.write({ ...entry, source: "store", settle_called: false });
  const lines = readFileSync(path, "utf8").trim().split("\n");
  assert.equal(lines.length, 2);
  assert.deepEqual(JSON.parse(lines[0] ?? ""), entry);
  assert.equal(journal.all().length, 2);
  assert.equal(journal.all()[1]?.source, "store");
});

test("journal without a path keeps entries in memory only", () => {
  const journal = new Journal(null);
  journal.write({
    ts: "t",
    route: "GET /x",
    payment_id: null,
    signed_payload_hash: null,
    settle_called: false,
    settle_ok: false,
    served_hash: null,
    status: 402,
    source: "unpaid",
  });
  assert.equal(journal.all().length, 1);
});
