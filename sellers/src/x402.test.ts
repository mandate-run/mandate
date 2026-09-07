import assert from "node:assert/strict";
import type { AddressInfo } from "node:net";
import test from "node:test";
import type { FacilitatorClient } from "@x402/core/server";
import type { PaymentPayload, PaymentRequired } from "@x402/core/types";
import { MemoryStore } from "./idempotency.js";
import { Journal, sha256Hex } from "./journal.js";
import { USDC_TESTNET, createSeller } from "./x402.js";

interface Fake {
  client: FacilitatorClient;
  calls: { verify: number; settle: number };
  settleOk: boolean;
}

function fakeFacilitator(): Fake {
  const fake: Fake = {
    calls: { verify: 0, settle: 0 },
    settleOk: true,
    client: {
      async getSupported() {
        return {
          kinds: [{ x402Version: 2, scheme: "exact", network: "hedera:testnet", extra: { feePayer: "0.0.7162784" } }],
          extensions: ["payment-identifier"],
          signers: {},
        } as unknown as Awaited<ReturnType<FacilitatorClient["getSupported"]>>;
      },
      async verify() {
        fake.calls.verify += 1;
        return { isValid: true, payer: "0.0.5" };
      },
      async settle() {
        fake.calls.settle += 1;
        return fake.settleOk
          ? { success: true, transaction: "0.0.7162784@1.2", network: "hedera:testnet", payer: "0.0.5" }
          : { success: false, errorReason: "settle_failed", transaction: "", network: "hedera:testnet" };
      },
    },
  };
  return fake;
}

interface Running {
  base: string;
  store: MemoryStore;
  journal: Journal;
  fake: Fake;
  close(): void;
}

let counter = 0;

function start(): Running {
  const store = new MemoryStore();
  const journal = new Journal(null);
  const fake = fakeFacilitator();
  const app = createSeller(
    {
      network: "hedera:testnet",
      payTo: "0.0.111",
      facilitatorUrl: "http://127.0.0.1:9",
      asset: USDC_TESTNET,
      store,
      journal,
      facilitator: fake.client,
    },
    [
      { path: "/spike", amount: "1000", description: "spike", handler: (_req, res) => void res.json({ ok: true, nonce: ++counter }) },
      {
        method: "POST",
        path: "/facts",
        amount: "2000",
        description: "facts",
        handler: (req, res) => void res.json({ pool: (req.body as { pool: string }).pool, nonce: ++counter }),
      },
    ],
  );
  const server = app.listen(0);
  const port = (server.address() as AddressInfo).port;
  return { base: `http://127.0.0.1:${port}`, store, journal, fake, close: () => void server.close() };
}

const decode = (header: string): PaymentRequired => JSON.parse(Buffer.from(header, "base64").toString("utf8")) as PaymentRequired;

async function quote(base: string, path: string, method = "GET", body?: string): Promise<PaymentRequired> {
  const res = await fetch(`${base}${path}`, { method, body, headers: body === undefined ? {} : { "content-type": "application/json" } });
  assert.equal(res.status, 402);
  const header = res.headers.get("payment-required");
  assert.ok(header);
  return decode(header);
}

function headerFor(required: PaymentRequired, id: string, transaction: string): string {
  const declared = (required.extensions?.["payment-identifier"] ?? { info: {} }) as { info?: Record<string, unknown> };
  const payload: PaymentPayload = {
    x402Version: 2,
    resource: required.resource,
    accepted: required.accepts[0]!,
    payload: { transaction },
    extensions: { "payment-identifier": { ...declared, info: { ...(declared.info ?? {}), id } } },
  } as PaymentPayload;
  return Buffer.from(JSON.stringify(payload)).toString("base64");
}

async function paid(base: string, path: string, header: string, method = "GET", body?: string): Promise<{ status: number; text: string; paymentResponse: string | null }> {
  const headers: Record<string, string> = { "PAYMENT-SIGNATURE": header };
  if (body !== undefined) headers["content-type"] = "application/json";
  const res = await fetch(`${base}${path}`, { method, body, headers });
  return { status: res.status, text: await res.text(), paymentResponse: res.headers.get("payment-response") };
}

const ID = "pay_0123456789abcdef";

test("a paid request settles once and an identical resend is served from the store", async () => {
  const s = start();
  try {
    const required = await quote(s.base, "/spike");
    assert.equal(required.accepts[0]?.extra?.["feePayer"], "0.0.7162784");
    const header = headerFor(required, ID, "AA==");
    const first = await paid(s.base, "/spike", header);
    assert.equal(first.status, 200);
    assert.ok(first.paymentResponse);
    const second = await paid(s.base, "/spike", header);
    assert.equal(second.status, 200);
    assert.equal(second.text, first.text);
    assert.equal(second.paymentResponse, first.paymentResponse);
    assert.deepEqual(s.fake.calls, { verify: 1, settle: 1 });
    const [, live, store] = s.journal.all();
    assert.equal(live?.source, "live");
    assert.equal(live?.settle_called, true);
    assert.equal(live?.settle_ok, true);
    assert.equal(store?.source, "store");
    assert.equal(store?.settle_called, false);
    assert.equal(store?.served_hash, sha256Hex(first.text));
  } finally {
    s.close();
  }
});

test("a replay must present the original signed transaction", async () => {
  const s = start();
  try {
    const required = await quote(s.base, "/spike");
    assert.equal((await paid(s.base, "/spike", headerFor(required, ID, "AA=="))).status, 200);
    const forged = await paid(s.base, "/spike", headerFor(required, ID, "AQ=="));
    assert.equal(forged.status, 409);
    assert.deepEqual(s.fake.calls, { verify: 1, settle: 1 });
  } finally {
    s.close();
  }
});

test("the request body is part of the binding", async () => {
  const s = start();
  try {
    const required = await quote(s.base, "/facts", "POST", '{"pool":"A"}');
    const header = headerFor(required, ID, "AA==");
    const a = await paid(s.base, "/facts", header, "POST", '{"pool":"A"}');
    assert.equal(a.status, 200);
    assert.match(a.text, /"pool":"A"/);
    const b = await paid(s.base, "/facts", header, "POST", '{"pool":"B"}');
    assert.equal(b.status, 409);
    const again = await paid(s.base, "/facts", header, "POST", '{"pool":"A"}');
    assert.equal(again.status, 200);
    assert.equal(again.text, a.text);
    assert.deepEqual(s.fake.calls, { verify: 1, settle: 1 });
  } finally {
    s.close();
  }
});

test("concurrent attempts with one id and two transactions settle at most once", async () => {
  const s = start();
  try {
    const required = await quote(s.base, "/spike");
    const [a, b] = await Promise.all([
      paid(s.base, "/spike", headerFor(required, ID, "AA==")),
      paid(s.base, "/spike", headerFor(required, ID, "AQ==")),
    ]);
    assert.deepEqual([a.status, b.status].sort(), [200, 409]);
    assert.equal(s.fake.calls.settle, 1);
    const live = s.journal.all().filter((e) => e.source === "live");
    assert.equal(live.length, 1);
    assert.equal(live[0]?.settle_called, true);
  } finally {
    s.close();
  }
});

test("identical concurrent attempts coalesce into one settlement and one body", async () => {
  const s = start();
  try {
    const required = await quote(s.base, "/spike");
    const header = headerFor(required, ID, "AA==");
    const [a, b] = await Promise.all([paid(s.base, "/spike", header), paid(s.base, "/spike", header)]);
    assert.equal(a.status, 200);
    assert.equal(b.status, 200);
    assert.equal(a.text, b.text);
    assert.deepEqual(s.fake.calls, { verify: 1, settle: 1 });
    assert.deepEqual(s.journal.all().map((e) => e.source).sort(), ["live", "store", "unpaid"]);
  } finally {
    s.close();
  }
});

test("a failed settlement is not stored and a retry settles again", async () => {
  const s = start();
  try {
    const required = await quote(s.base, "/spike");
    const header = headerFor(required, ID, "AA==");
    s.fake.settleOk = false;
    const failed = await paid(s.base, "/spike", header);
    assert.equal(failed.status, 402);
    assert.equal(s.store.size, 0);
    const entry = s.journal.all().at(-1);
    assert.equal(entry?.settle_called, true);
    assert.equal(entry?.settle_ok, false);
    s.fake.settleOk = true;
    const retry = await paid(s.base, "/spike", header);
    assert.equal(retry.status, 200);
    assert.equal(s.store.size, 1);
    assert.deepEqual(s.fake.calls, { verify: 2, settle: 2 });
  } finally {
    s.close();
  }
});
