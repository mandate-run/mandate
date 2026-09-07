import assert from "node:assert/strict";
import type { AddressInfo } from "node:net";
import test from "node:test";
import { MemoryStore, fingerprint } from "./idempotency.js";
import { Journal, sha256Hex } from "./journal.js";
import { USDC_TESTNET, createSeller } from "./x402.js";

const accepted = {
  scheme: "exact",
  network: "hedera:testnet",
  amount: "1000",
  asset: USDC_TESTNET,
  payTo: "0.0.111",
  maxTimeoutSeconds: 120,
};

function seller() {
  const store = new MemoryStore();
  const journal = new Journal(null);
  const app = createSeller(
    {
      network: "hedera:testnet",
      payTo: "0.0.111",
      facilitatorUrl: "http://127.0.0.1:9",
      asset: USDC_TESTNET,
      store,
      journal,
      syncFacilitatorOnStart: false,
    },
    [{ path: "/spike", amount: "1000", description: "spike", handler: (_req, res) => void res.json({ ok: true }) }],
  );
  return { app, store, journal };
}

test("a stored result is served without touching the facilitator and is journaled as store", async () => {
  const { app, store, journal } = seller();
  const id = "pay_0123456789abcdef";
  const print = fingerprint({ payer: null, method: "GET", path: "/spike", query: {}, bodyHash: sha256Hex(""), accepted });
  store.set(id, {
    fingerprint: print,
    status: 200,
    content_type: "application/json",
    payment_response: "cmVzcA==",
    body_base64: Buffer.from('{"ok":true,"nonce":"first"}').toString("base64"),
    stored_at: "t",
  });
  const payload = {
    x402Version: 2,
    accepted,
    payload: { transaction: "AA==" },
    extensions: { "payment-identifier": { info: { id, required: true }, schema: {} } },
  };
  const header = Buffer.from(JSON.stringify(payload)).toString("base64");
  const server = app.listen(0);
  try {
    const port = (server.address() as AddressInfo).port;
    const res = await fetch(`http://127.0.0.1:${port}/spike`, { headers: { "PAYMENT-SIGNATURE": header } });
    assert.equal(res.status, 200);
    assert.equal(res.headers.get("payment-response"), "cmVzcA==");
    assert.equal(await res.text(), '{"ok":true,"nonce":"first"}');
    const entry = journal.all().at(-1);
    assert.equal(entry?.source, "store");
    assert.equal(entry?.payment_id, id);
    assert.equal(entry?.settle_called, false);
    assert.equal(entry?.signed_payload_hash, sha256Hex(header));
    assert.equal(entry?.served_hash, sha256Hex('{"ok":true,"nonce":"first"}'));
  } finally {
    server.close();
  }
});
