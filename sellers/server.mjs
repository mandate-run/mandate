// Mandate reference sellers. Four x402-gated endpoints on Hedera testnet,
// settled through Blocky402, serving live evidence from the Uniswap v3
// subgraph on The Graph Network.
//
// Wire format is x402 v2:
//   - unpaid request  -> 402 + PAYMENT-REQUIRED header (base64 JSON)
//   - paid request    -> PAYMENT-SIGNATURE header (base64 PaymentPayload)
//   - success         -> 200 + evidence body + PAYMENT-RESPONSE header
//
// Fault switches (SELLER_FAULTS, comma-separated) reproduce the demo
// scenarios: `drop-response-after-settle` and `quote-drift`.

import express from "express";
import crypto from "node:crypto";

import {
  NETWORK, ASSET, PAY_TO, FEE_PAYER, MAX_TIMEOUT_S, ROUTE_TO_CAPABILITY,
  unitsFor, priceFor, httpError, listingsDocument, TARIFFS,
} from "./lib/tariffs.mjs";
import { indexedBlock, screenPool, poolEvents } from "./lib/graph.mjs";
import * as store from "./lib/store.mjs";
import { defaultWindow, evidenceResponse, generateProse } from "./lib/evidence.mjs";

const FACILITATOR = process.env.FACILITATOR_URL || "https://api.testnet.blocky402.com";
const PORT = Number(process.env.SELLER_PORT || 4021);
const FAULTS = new Set((process.env.SELLER_FAULTS || "").split(",").filter(Boolean));
const BASE_URL = process.env.SELLER_BASE_URL || `http://localhost:${PORT}`;

// Rough mainnet seconds-per-block, used to translate a requested window into
// block heights for the block-height TVL queries.
const SECONDS_PER_BLOCK = 12;

const app = express();
app.use(express.json({ limit: "1mb" }));

// Bazaar discovery: the pinned manifest is diffable against this live listing.
app.get("/manifest.json", (_req, res) => {
  res.json(listingsDocument(BASE_URL));
});

app.get("/healthz", (_req, res) => res.json({ ok: true }));

// ---- x402 helpers ---------------------------------------------------------

function b64encode(obj) {
  return Buffer.from(JSON.stringify(obj), "utf8").toString("base64");
}

function b64decode(s) {
  return JSON.parse(Buffer.from(s, "base64").toString("utf8"));
}

function paymentRequirements(capability, units) {
  const amount = priceFor(capability, units);
  if (FAULTS.has("quote-drift") && capability === "events") {
    // Scenario 4: quote above the ceiling; the buyer must refuse OFF_TARIFF.
    return {
      scheme: "exact",
      network: NETWORK,
      asset: ASSET,
      amount: String(amount + 500),
      payTo: PAY_TO,
      maxTimeoutSeconds: MAX_TIMEOUT_S,
      extra: { feePayer: FEE_PAYER },
    };
  }
  return {
    scheme: "exact",
    network: NETWORK,
    asset: ASSET,
    amount: String(amount),
    payTo: PAY_TO,
    maxTimeoutSeconds: MAX_TIMEOUT_S,
    extra: { feePayer: FEE_PAYER },
  };
}

function sendPaymentRequired(res, req, capability, units) {
  const requirements = paymentRequirements(capability, units);
  const paymentRequired = {
    x402Version: 2,
    error: "Payment required",
    resource: { method: req.method, url: `${BASE_URL}${req.path}`, contentType: "application/json" },
    accepts: [requirements],
    extensions: {
      bazaar: {
        version: "1.0.0",
        listings: `${BASE_URL}/manifest.json`,
      },
    },
  };
  res.setHeader("PAYMENT-REQUIRED", b64encode(paymentRequired));
  res.status(402).json(paymentRequired);
}

async function verifyWithFacilitator(paymentPayload, paymentRequirements) {
  const res = await fetch(`${FACILITATOR}/verify`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ x402Version: 2, paymentPayload, paymentRequirements }),
    signal: AbortSignal.timeout(30_000),
  });
  if (!res.ok) throw new httpError(502, `facilitator verify ${res.status}`);
  return res.json();
}

async function settleWithFacilitator(paymentPayload, paymentRequirements) {
  const res = await fetch(`${FACILITATOR}/settle`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ x402Version: 2, paymentPayload, paymentRequirements }),
    signal: AbortSignal.timeout(30_000),
  });
  if (!res.ok) throw new httpError(502, `facilitator settle ${res.status}`);
  return res.json();
}

// Verify -> do the work -> settle -> store -> respond (spec section 13: a
// failed handler must not settle).
async function handlePaid(req, res, capability, units, doWork) {
  const signature = req.get("payment-signature");
  if (!signature) {
    return sendPaymentRequired(res, req, capability, units);
  }

  let paymentPayload;
  try {
    paymentPayload = b64decode(signature);
  } catch {
    return res.status(400).json({ error: "malformed PAYMENT-SIGNATURE" });
  }
  const requirements = paymentPayload?.accepted;
  if (paymentPayload?.x402Version !== 2 || !requirements) {
    return res.status(400).json({ error: "unrecognized payment payload" });
  }

  const paymentId = paymentPayload?.extensions?.["payment-identifier"]?.info?.id;
  const fingerprint = store.fingerprint(req.method, `${BASE_URL}${req.path}`, JSON.stringify(req.body ?? {}));
  const route = req.path;

  // Idempotent storage: a matching resend or retrieval is served without a
  // second verify/settle; a mismatched one is a conflict (spec section 13).
  const stored = paymentId ? store.load(paymentId) : null;
  if (stored && store.matches(stored, signature, fingerprint, PAY_TO, route)) {
    console.log(`[store] ${paymentId} served from storage (retrieval/resend)`);
    res.setHeader("PAYMENT-RESPONSE", b64encode({ transaction: stored.transactionId, reused: true }));
    return res.status(200).json(stored.response);
  }
  if (stored) {
    return res.status(409).json({ error: "payment id already bound to a different request" });
  }

  const verification = await verifyWithFacilitator(paymentPayload, requirements);
  if (!verification?.isValid) {
    console.warn(`[x402] verification failed: ${verification?.invalidReason ?? "unknown"}`);
    return sendPaymentRequired(res, req, capability, units);
  }

  let response;
  try {
    response = await doWork(req.body ?? {});
  } catch (e) {
    // Work failed: never settle (spec section 13).
    console.error(`[work] ${capability} failed: ${e.message}`);
    return res.status(500).json({ error: "seller work failed; payment not settled" });
  }

  const settlement = await settleWithFacilitator(paymentPayload, requirements);
  if (!settlement?.success) {
    console.error(`[settle] failed: ${settlement?.errorReason ?? settlement?.errorMessage ?? "unknown"}`);
    return res.status(502).json({ error: "settlement failed" });
  }
  const transactionId = settlement.transaction ?? null;

  if (paymentId) {
    store.save({
      paymentId,
      route,
      payTo: PAY_TO,
      paymentSignatureHash: store.sha256(signature),
      requestFingerprint: fingerprint,
      transactionId,
      response,
      servedAt: new Date().toISOString(),
    });
    console.log(`[x402] ${capability} paid ${requirements.amount} ${requirements.asset} -> ${requirements.payTo} tx ${transactionId} id ${paymentId}`);
  } else {
    console.log(`[x402] ${capability} paid ${requirements.amount} (no payment-identifier extension)`);
  }

  res.setHeader("PAYMENT-RESPONSE", b64encode(settlement));

  // Fault switch: settle, then drop the response (scenario 5). The buyer
  // retrieves with the same payment id and is served from storage.
  if (FAULTS.has("drop-response-after-settle")) {
    console.log(`[fault] drop-response-after-settle: settled ${transactionId}, dropping response`);
    return res.status(500).end();
  }
  return res.status(200).json(response);
}

// ---- evidence handlers ----------------------------------------------------

async function windowFor(body) {
  const window = defaultWindow(body.window);
  const head = await indexedBlock();
  const endTs = Math.min(head.timestamp, Math.floor(new Date(window.end).getTime() / 1000));
  const startTs = Math.floor(new Date(window.start).getTime() / 1000);
  const blockEnd = head.number;
  const blockStart = Math.max(0, blockEnd - Math.round((endTs - startTs) / SECONDS_PER_BLOCK));
  return {
    window,
    head,
    blockStart,
    blockEnd,
    blockEndTimestamp: new Date(endTs * 1000).toISOString(),
    startTs,
    endTs,
  };
}

async function screenEvidence(body) {
  const w = await windowFor(body);
  const pools = [];
  for (const pool of body.pools) {
    const s = await screenPool(pool, {
      blockStart: w.blockStart,
      blockEnd: w.blockEnd,
      windowStart: w.startTs,
      windowEnd: w.endTs,
    });
    pools.push({ ...s, events: [] });
  }
  return evidenceResponse({
    deploymentId: w.head.deploymentId,
    blockStart: w.blockStart,
    blockEnd: w.blockEnd,
    blockEndTimestamp: w.blockEndTimestamp,
    windowRequested: { start: w.window.start, end: w.window.end },
    pools,
  });
}

async function eventsEvidence(body) {
  const w = await windowFor(body);
  const pools = [];
  for (const pool of body.pools) {
    const s = await screenPool(pool, {
      blockStart: w.blockStart,
      blockEnd: w.blockEnd,
      windowStart: w.startTs,
      windowEnd: w.endTs,
    });
    const { facts, truncated } = await poolEvents(pool, { windowStart: w.startTs, windowEnd: w.endTs });
    pools.push({ ...s, events: facts, truncated: s.truncated || truncated });
  }
  return evidenceResponse({
    deploymentId: w.head.deploymentId,
    blockStart: w.blockStart,
    blockEnd: w.blockEnd,
    blockEndTimestamp: w.blockEndTimestamp,
    windowRequested: { start: w.window.start, end: w.window.end },
    pools,
  });
}

// ---- routes ----------------------------------------------------------------

for (const [route, capability] of Object.entries(ROUTE_TO_CAPABILITY)) {
  app.post(route, async (req, res) => {
    try {
      const units = unitsFor(capability, req.body ?? {});
      const work =
        capability === "screen" ? screenEvidence
        : capability === "events" ? eventsEvidence
        : capability === "investigate" ? eventsEvidence
        : async (body) => {
            const { prose, model } = await generateProse(body, {
              modelApiKey: process.env.MODEL_API_KEY,
              modelUrl: process.env.MODEL_URL,
            });
            return { prose, model };
          };
      return await handlePaid(req, res, capability, units, work);
    } catch (e) {
      if (e instanceof httpError) {
        return res.status(e.status).json({ error: e.message });
      }
      console.error(`[${capability}] ${e.stack ?? e}`);
      return res.status(500).json({ error: "seller error" });
    }
  });
}

app.listen(PORT, () => {
  console.log(`mandate sellers listening on ${BASE_URL}`);
  console.log(`  facilitator ${FACILITATOR}, asset ${ASSET} on ${NETWORK}, payTo ${PAY_TO}, fee payer ${FEE_PAYER}`);
  console.log(`  tariffs: ${Object.entries(TARIFFS).map(([k, t]) => `${k}=${t.base}+${t.unit_price}/${t.unit}`).join(", ")}`);
  if (FAULTS.size) console.log(`  faults: ${[...FAULTS].join(", ")}`);
});