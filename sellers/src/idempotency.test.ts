import assert from "node:assert/strict";
import test from "node:test";
import type { Request, Response } from "express";
import {
  FileStore,
  MemoryStore,
  decodePaymentHeader,
  fingerprint,
  replayMiddleware,
} from "./idempotency.js";
import { sha256Hex } from "./journal.js";
import { tmpdir } from "node:os";
import { join } from "node:path";

const accepted = {
  scheme: "exact",
  network: "hedera:testnet",
  amount: "1000",
  asset: "0.0.429274",
  payTo: "0.0.111",
  maxTimeoutSeconds: 120,
};

function payloadFor(id: string | null): Record<string, unknown> {
  return {
    x402Version: 2,
    accepted,
    payload: { transaction: "AA==" },
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
  };
  return res;
}

const validId = "pay_0123456789abcdef";

const baseFingerprint = () =>
  fingerprint({ payer: null, method: "GET", path: "/spike", query: {}, bodyHash: sha256Hex(""), accepted });

test("fingerprint is stable and changes with payer, path, query and terms", () => {
  const base = baseFingerprint();
  assert.equal(base, baseFingerprint());
  assert.notEqual(base, fingerprint({ payer: "0.0.5", method: "GET", path: "/spike", query: {}, bodyHash: sha256Hex(""), accepted }));
  assert.notEqual(base, fingerprint({ payer: null, method: "GET", path: "/other", query: {}, bodyHash: sha256Hex(""), accepted }));
  assert.notEqual(base, fingerprint({ payer: null, method: "GET", path: "/spike", query: { pool: "x" }, bodyHash: sha256Hex(""), accepted }));
  assert.notEqual(base, fingerprint({ payer: null, method: "GET", path: "/spike", query: {}, bodyHash: sha256Hex(""), accepted: { ...accepted, amount: "999" } }));
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

test("replay passes through without a header, without an id, and without a stored result", () => {
  const store = new MemoryStore();
  const mw = replayMiddleware(store);
  const cases: Record<string, string>[] = [
    {},
    { "payment-signature": headerFor(payloadFor(null)) },
    { "payment-signature": "garbage" },
  ];
  for (const headers of cases) {
    let called = 0;
    const res = fakeRes();
    mw(fakeReq(headers), res as unknown as Response, () => void called++);
    assert.equal(called, 1);
    assert.equal(res.ended, false);
    assert.equal(res.locals["paymentId"], undefined);
  }
  let called = 0;
  const res = fakeRes();
  mw(fakeReq({ "payment-signature": headerFor(payloadFor(validId)) }), res as unknown as Response, () => void called++);
  assert.equal(called, 1);
  assert.equal(res.locals["paymentId"], validId);
  assert.equal(res.locals["fingerprint"], baseFingerprint());
  assert.equal(res.locals["signedPayloadHash"], sha256Hex(headerFor(payloadFor(validId))));
});

test("replay serves the stored result with status, headers and body, and marks the source", () => {
  const store = new MemoryStore();
  store.set(validId, {
    fingerprint: baseFingerprint(),
    status: 200,
    content_type: "application/json; charset=utf-8",
    payment_response: "cGF5bWVudC1yZXNwb25zZQ==",
    body_base64: Buffer.from('{"ok":true}').toString("base64"),
    stored_at: "2026-09-07T00:00:00.000Z",
  });
  let called = 0;
  const res = fakeRes();
  replayMiddleware(store)(
    fakeReq({ "payment-signature": headerFor(payloadFor(validId)) }),
    res as unknown as Response,
    () => void called++,
  );
  assert.equal(called, 0);
  assert.equal(res.ended, true);
  assert.equal(res.statusCode, 200);
  assert.equal(res.headers["payment-response"], "cGF5bWVudC1yZXNwb25zZQ==");
  assert.equal(res.headers["content-type"], "application/json; charset=utf-8");
  assert.equal(res.body?.toString(), '{"ok":true}');
  assert.equal(res.locals["source"], "store");
});

test("replay answers 409 when the id is bound to a different request", () => {
  const store = new MemoryStore();
  store.set(validId, {
    fingerprint: baseFingerprint(),
    status: 200,
    content_type: null,
    payment_response: null,
    body_base64: "",
    stored_at: "t",
  });
  let called = 0;
  const res = fakeRes();
  replayMiddleware(store)(
    fakeReq({ "payment-signature": headerFor(payloadFor(validId)) }, "/spike", { pool: "0xabc" }),
    res as unknown as Response,
    () => void called++,
  );
  assert.equal(called, 0);
  assert.equal(res.statusCode, 409);
  assert.equal(res.locals["source"], "conflict");
  assert.match(res.body?.toString() ?? "", /different request/);
});

test("file store survives a restart", () => {
  const path = join(tmpdir(), `mandate-store-${process.pid}-${Date.now()}`, "store.jsonl");
  const first = new FileStore(path);
  first.set(validId, {
    fingerprint: "f",
    status: 200,
    content_type: null,
    payment_response: "r",
    body_base64: "e30=",
    stored_at: "t",
  });
  const second = new FileStore(path);
  assert.equal(second.size, 1);
  assert.equal(second.get(validId)?.payment_response, "r");
});
