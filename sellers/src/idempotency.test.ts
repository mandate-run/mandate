import assert from "node:assert/strict";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import type { Request, Response } from "express";
import {
  Claims,
  FileStore,
  MemoryStore,
  decodePaymentHeader,
  fingerprint,
  replayMiddleware,
  transactionHashOf,
} from "./idempotency.js";
import { sha256Hex } from "./journal.js";

const accepted = {
  scheme: "exact",
  network: "hedera:testnet",
  amount: "1000",
  asset: "0.0.429274",
  payTo: "0.0.111",
  maxTimeoutSeconds: 120,
};

function payloadFor(id: string | null, transaction = "AA=="): Record<string, unknown> {
  return {
    x402Version: 2,
    accepted,
    payload: { transaction },
    extensions: id === null ? {} : { "payment-identifier": { info: { id, required: true }, schema: {} } },
  };
}

const headerFor = (payload: unknown): string => Buffer.from(JSON.stringify(payload)).toString("base64");

function fakeReq(headers: Record<string, string>, path = "/spike", query: Record<string, unknown> = {}): Request {
  return {
    method: "GET",
    path,
    query,
    header: (name: string) => headers[name.toLowerCase()],
  } as unknown as Request;
}

interface FakeRes {
  statusCode: number;
  headers: Record<string, string>;
  locals: Record<string, unknown>;
  body: Buffer | null;
  ended: boolean;
  status(code: number): FakeRes;
  setHeader(name: string, value: string): void;
  getHeader(name: string): string | undefined;
  json(value: unknown): FakeRes;
  end(body?: Buffer): FakeRes;
  once(event: string, fn: () => void): FakeRes;
}

function fakeRes(): FakeRes {
  const res: FakeRes = {
    statusCode: 200,
    headers: {},
    locals: {},
    body: null,
    ended: false,
    status(code) {
      res.statusCode = code;
      return res;
    },
    setHeader(name, value) {
      res.headers[name.toLowerCase()] = value;
    },
    getHeader(name) {
      return res.headers[name.toLowerCase()];
    },
    json(value) {
      res.body = Buffer.from(JSON.stringify(value));
      res.ended = true;
      return res;
    },
    end(body) {
      res.body = body ?? Buffer.alloc(0);
      res.ended = true;
      return res;
    },
    once() {
      return res;
    },
  };
  return res;
}

const validId = "pay_0123456789abcdef";
const emptyBody = sha256Hex("");
const baseFingerprint = () =>
  fingerprint({ payer: null, method: "GET", path: "/spike", query: {}, bodyHash: emptyBody, accepted });
const stored = (over: Record<string, unknown> = {}) => ({
  fingerprint: baseFingerprint(),
  transaction_hash: transactionHashOf(payloadFor(validId) as never),
  status: 200,
  content_type: "application/json; charset=utf-8",
  payment_response: "cGF5bWVudC1yZXNwb25zZQ==",
  body_base64: Buffer.from('{"ok":true}').toString("base64"),
  stored_at: "2026-09-07T00:00:00.000Z",
  ...over,
});

async function run(store: MemoryStore, claims: Claims, req: Request): Promise<{ res: FakeRes; nexts: number }> {
  let nexts = 0;
  const res = fakeRes();
  await replayMiddleware(store, claims)(req, res as unknown as Response, () => void nexts++);
  return { res, nexts };
}

test("fingerprint is stable and changes with payer, path, query, body and terms", () => {
  const base = baseFingerprint();
  assert.equal(base, baseFingerprint());
  assert.notEqual(base, fingerprint({ payer: "0.0.5", method: "GET", path: "/spike", query: {}, bodyHash: emptyBody, accepted }));
  assert.notEqual(base, fingerprint({ payer: null, method: "GET", path: "/other", query: {}, bodyHash: emptyBody, accepted }));
  assert.notEqual(base, fingerprint({ payer: null, method: "GET", path: "/spike", query: { pool: "x" }, bodyHash: emptyBody, accepted }));
  assert.notEqual(base, fingerprint({ payer: null, method: "GET", path: "/spike", query: {}, bodyHash: sha256Hex("{}"), accepted }));
  assert.notEqual(base, fingerprint({ payer: null, method: "GET", path: "/spike", query: {}, bodyHash: emptyBody, accepted: { ...accepted, amount: "999" } }));
  const a = fingerprint({ payer: null, method: "GET", path: "/p", query: { b: 1, a: 2 }, bodyHash: "", accepted });
  const b = fingerprint({ payer: null, method: "GET", path: "/p", query: { a: 2, b: 1 }, bodyHash: "", accepted });
  assert.equal(a, b);
});

test("decodePaymentHeader round-trips and rejects garbage", () => {
  const payload = payloadFor(validId);
  assert.deepEqual(decodePaymentHeader(headerFor(payload)), payload);
  assert.equal(decodePaymentHeader("not base64 json"), null);
  assert.equal(decodePaymentHeader(Buffer.from("42").toString("base64")), null);
});

test("transaction hash follows the signed bytes only", () => {
  assert.equal(transactionHashOf(payloadFor(validId, "AA==") as never), sha256Hex("AA=="));
  assert.notEqual(transactionHashOf(payloadFor(validId, "AA==") as never), transactionHashOf(payloadFor(validId, "AQ==") as never));
});

test("replay passes through without a header, without an id, and without a stored result", async () => {
  const store = new MemoryStore();
  const claims = new Claims();
  const cases: Record<string, string>[] = [
    {},
    { "payment-signature": headerFor(payloadFor(null)) },
    { "payment-signature": "garbage" },
  ];
  for (const headers of cases) {
    const { res, nexts } = await run(store, claims, fakeReq(headers));
    assert.equal(nexts, 1);
    assert.equal(res.ended, false);
    assert.equal(res.locals["paymentId"], undefined);
  }
  assert.equal(claims.size, 0);
  const { res, nexts } = await run(store, claims, fakeReq({ "payment-signature": headerFor(payloadFor(validId)) }));
  assert.equal(nexts, 1);
  assert.equal(res.locals["paymentId"], validId);
  assert.equal(res.locals["fingerprint"], baseFingerprint());
  assert.equal(res.locals["transactionHash"], sha256Hex("AA=="));
  assert.equal(res.locals["signedPayloadHash"], sha256Hex(headerFor(payloadFor(validId))));
  assert.equal(claims.size, 1);
  (res.locals["release"] as () => void)();
  assert.equal(claims.size, 0);
});

test("replay serves the stored result with status, headers and body, and marks the source", async () => {
  const store = new MemoryStore();
  store.set(validId, stored());
  const { res, nexts } = await run(store, new Claims(), fakeReq({ "payment-signature": headerFor(payloadFor(validId)) }));
  assert.equal(nexts, 0);
  assert.equal(res.ended, true);
  assert.equal(res.statusCode, 200);
  assert.equal(res.headers["payment-response"], "cGF5bWVudC1yZXNwb25zZQ==");
  assert.equal(res.headers["content-type"], "application/json; charset=utf-8");
  assert.equal(res.body?.toString(), '{"ok":true}');
  assert.equal(res.locals["source"], "store");
});

test("replay answers 409 when the id is bound to a different request", async () => {
  const store = new MemoryStore();
  store.set(validId, stored());
  const { res, nexts } = await run(store, new Claims(), fakeReq({ "payment-signature": headerFor(payloadFor(validId)) }, "/spike", { pool: "0xabc" }));
  assert.equal(nexts, 0);
  assert.equal(res.statusCode, 409);
  assert.equal(res.locals["source"], "conflict");
  assert.match(res.body?.toString() ?? "", /different request or authorization/);
});

test("replay answers 409 when the same request carries a different signed transaction", async () => {
  const store = new MemoryStore();
  store.set(validId, stored());
  const { res, nexts } = await run(store, new Claims(), fakeReq({ "payment-signature": headerFor(payloadFor(validId, "AQ==")) }));
  assert.equal(nexts, 0);
  assert.equal(res.statusCode, 409);
});

test("a conflicting request while the id is in flight is refused", async () => {
  const store = new MemoryStore();
  const claims = new Claims();
  const first = await run(store, claims, fakeReq({ "payment-signature": headerFor(payloadFor(validId, "AA==")) }));
  assert.equal(first.nexts, 1);
  const second = await run(store, claims, fakeReq({ "payment-signature": headerFor(payloadFor(validId, "AQ==")) }));
  assert.equal(second.nexts, 0);
  assert.equal(second.res.statusCode, 409);
  assert.match(second.res.body?.toString() ?? "", /in flight/);
  (first.res.locals["release"] as () => void)();
});

test("an identical request while the id is in flight waits and is served from the store", async () => {
  const store = new MemoryStore();
  const claims = new Claims();
  const first = await run(store, claims, fakeReq({ "payment-signature": headerFor(payloadFor(validId)) }));
  assert.equal(first.nexts, 1);
  let nexts = 0;
  const res = fakeRes();
  const pending = replayMiddleware(store, claims)(
    fakeReq({ "payment-signature": headerFor(payloadFor(validId)) }),
    res as unknown as Response,
    () => void nexts++,
  );
  assert.equal(res.ended, false);
  store.set(validId, stored());
  (first.res.locals["release"] as () => void)();
  await pending;
  assert.equal(nexts, 0);
  assert.equal(res.statusCode, 200);
  assert.equal(res.locals["source"], "store");
  assert.equal(res.body?.toString(), '{"ok":true}');
});

test("an identical request whose first attempt stored nothing proceeds as a fresh attempt", async () => {
  const store = new MemoryStore();
  const claims = new Claims();
  const first = await run(store, claims, fakeReq({ "payment-signature": headerFor(payloadFor(validId)) }));
  let nexts = 0;
  const res = fakeRes();
  const pending = replayMiddleware(store, claims)(
    fakeReq({ "payment-signature": headerFor(payloadFor(validId)) }),
    res as unknown as Response,
    () => void nexts++,
  );
  (first.res.locals["release"] as () => void)();
  await pending;
  assert.equal(nexts, 1);
  assert.equal(claims.size, 1);
  (res.locals["release"] as () => void)();
});

test("file store survives a restart", () => {
  const path = join(tmpdir(), `mandate-store-${process.pid}-${Date.now()}`, "store.jsonl");
  const first = new FileStore(path);
  first.set(validId, stored({ payment_response: "r" }));
  const second = new FileStore(path);
  assert.equal(second.size, 1);
  assert.equal(second.get(validId)?.payment_response, "r");
  assert.equal(second.get(validId)?.transaction_hash, sha256Hex("AA=="));
});
