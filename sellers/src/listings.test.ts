import assert from "node:assert/strict";
import type { AddressInfo } from "node:net";
import test from "node:test";
import type { FacilitatorClient } from "@x402/core/server";
import type { PaymentPayload, PaymentRequired } from "@x402/core/types";
import { incompleteness, outcomesAndClaims } from "./claims.js";
import type { Explainer } from "./explain.js";
import { FAULT_QUOTE_ABOVE_CEILING, FAULT_QUOTE_DRIFT, parseFaults } from "./faults.js";
import type { EventsResponse, ScreenResponse } from "./graph.js";
import { MemoryStore, type ResultStore } from "./idempotency.js";
import { Journal } from "./journal.js";
import { buildManifest, listings } from "./listings.js";
import { parseEvidenceRequest, parseExplainRequest } from "./requests.js";
import { type ListingId, type Tariff, ceiling, tariffsFor, unitsFor } from "./tariffs.js";
import { USDC_TESTNET, createSeller } from "./x402.js";

const POOL = "0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640";
const WINDOW = { from: 1_788_800_400, to: 1_788_886_800 };
const INPUTS = { materiality: "0.05", min_event_usd: "100000" };
const ID = "pay_0123456789abcdef";

function snapshot(t0: string, t1: string) {
  return { totalValueLockedToken0: t0, totalValueLockedToken1: t1, totalValueLockedUSD: "1000", liquidity: "1", token0PriceUSD: "1", token1PriceUSD: "2000" };
}

interface ScreenOpts {
  verdict?: "material" | "non_material";
  coverageShortfall?: boolean;
  truncated?: boolean;
  unvaluedMint?: boolean;
  largeSwap?: boolean;
}

function screenResponse(pools: string[], verdictOrOpts: "material" | "non_material" | ScreenOpts = "material"): ScreenResponse {
  const opts: ScreenOpts = typeof verdictOrOpts === "string" ? { verdict: verdictOrOpts } : verdictOrOpts;
  const verdict = opts.verdict ?? "material";
  const largeSwap = opts.largeSwap ?? verdict === "material";
  const entries = pools.map((pool) => [
    pool,
    {
      start: snapshot("1000", "500"),
      end: snapshot(verdict === "material" ? "1100" : "1010", "500"),
      absent_at_start: false,
      hours: [
        { periodStartUnix: WINDOW.from, tvlUSD: "1", volumeUSD: "100.5", txCount: "3" },
        { periodStartUnix: WINDOW.from + 3600, tvlUSD: "1", volumeUSD: "200", txCount: "4" },
      ],
      large_events: { swap: largeSwap ? { transaction: { id: "0xswap" }, amountUSD: "250000" } : null, mint: null, burn: null },
      unvalued_events: { mint: opts.unvaluedMint ?? false, burn: false },
      truncated: opts.truncated ?? false,
      coverage_shortfall: opts.coverageShortfall ?? false,
      verdict,
      reasons: verdict === "material" ? (largeSwap ? ["tvl_change:token0", "large_event:swap"] : ["tvl_change:token0"]) : [],
    },
  ]);
  return {
    deployment_id: "Qm",
    block_start: 100,
    block_end: 200,
    block_end_timestamp: WINDOW.to - 5,
    indexed_block: 210,
    indexed_block_timestamp: WINDOW.to + 60,
    indexing_errors: false,
    window_requested: WINDOW,
    window_covered: WINDOW,
    coverage_shortfall: opts.coverageShortfall ?? false,
    truncated: opts.truncated ?? false,
    pools: Object.fromEntries(entries) as ScreenResponse["pools"],
    requests: 3 + pools.length,
  };
}

function eventsResponse(pools: string[], opts: { amountUSD?: string | null; truncated?: boolean } = {}): EventsResponse {
  const amount = opts.amountUSD === undefined ? "250000" : opts.amountUSD;
  const entries = pools.map((pool) => [
    pool,
    {
      swaps: [{ id: "s1", transaction: { id: "0xswap" }, logIndex: 1, timestamp: WINDOW.from + 1, amount0: "1", amount1: "-1", amountUSD: amount, origin: "0xo" }],
      mints: [],
      burns: [],
      counts: { swap: 1, mint: 0, burn: 0 },
      sum_amount_usd: { swap: amount ?? "0", mint: "0", burn: "0" },
      amount_usd_nulls: amount === null ? 1 : 0,
      truncated: opts.truncated ?? false,
    },
  ]);
  return {
    deployment_id: "Qm",
    block_start: 100,
    block_end: 200,
    block_end_timestamp: WINDOW.to - 5,
    indexed_block: 210,
    indexed_block_timestamp: WINDOW.to + 60,
    indexing_errors: false,
    window_requested: WINDOW,
    window_covered: WINDOW,
    coverage_shortfall: false,
    truncated: false,
    cap: 5000,
    pools: Object.fromEntries(entries) as EventsResponse["pools"],
    requests: 3 + pools.length,
  };
}

function fakeGraph(opts: { fail?: boolean } = {}) {
  const calls = { screen: 0, events: 0, investigate: 0 };
  return {
    calls,
    screen: async (pools: string[]) => {
      calls.screen += 1;
      if (opts.fail) throw new Error("gateway down");
      return screenResponse(pools);
    },
    eventsProduct: async (pools: string[]) => {
      calls.events += 1;
      if (opts.fail) throw new Error("gateway down");
      return eventsResponse(pools);
    },
    investigate: async (pools: string[]) => {
      calls.investigate += 1;
      if (opts.fail) throw new Error("gateway down");
      const screen = screenResponse(pools);
      const material = Object.entries(screen.pools).filter(([, p]) => p.verdict === "material").map(([pool]) => pool);
      return { screen, events: material.length > 0 ? eventsResponse(material) : null };
    },
  };
}

function fakeExplainer(): Explainer & { calls: number } {
  const fn = (async (brief: Record<string, unknown>) => {
    fn.calls += 1;
    return { prose: `explained ${Object.keys(brief).length} keys`, model: "fake", input_bytes: 1 };
  }) as Explainer & { calls: number };
  fn.calls = 0;
  return fn;
}

function fakeFacilitator() {
  const calls = { verify: 0, settle: 0 };
  const client: FacilitatorClient = {
    async getSupported() {
      return {
        kinds: [{ x402Version: 2, scheme: "exact", network: "hedera:testnet", extra: { feePayer: "0.0.7162784" } }],
        extensions: ["payment-identifier", "bazaar"],
        signers: {},
      } as unknown as Awaited<ReturnType<FacilitatorClient["getSupported"]>>;
    },
    async verify() {
      calls.verify += 1;
      return { isValid: true, payer: "0.0.5" };
    },
    async settle() {
      calls.settle += 1;
      return { success: true, transaction: "0.0.7162784@1.2", network: "hedera:testnet", payer: "0.0.5" };
    },
  };
  return { client, calls };
}

function start(opts: { faults?: string; failGraph?: boolean; store?: ResultStore; tariffs?: Record<ListingId, Tariff> } = {}) {
  const graph = fakeGraph({ fail: opts.failGraph ?? false });
  const explain = fakeExplainer();
  const faults = parseFaults(opts.faults);
  const tariffs = opts.tariffs ?? tariffsFor(USDC_TESTNET);
  const fac = fakeFacilitator();
  const journal = new Journal(null);
  const manifest = buildManifest({ baseUrl: "http://127.0.0.1:0", seller: "mandate-sellers", network: "hedera:testnet", asset: USDC_TESTNET, payTo: "0.0.111", tariffs });
  const app = createSeller(
    { network: "hedera:testnet", payTo: "0.0.111", facilitatorUrl: "http://127.0.0.1:9", asset: USDC_TESTNET, store: opts.store ?? new MemoryStore(), journal, faults, manifest, facilitator: fac.client },
    listings({ graph, explain, tariffs, asset: USDC_TESTNET, faults }),
  );
  const server = app.listen(0);
  const base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  return { base, graph, explain, fac, journal, tariffs, close: () => void server.close() };
}

const decode = (h: string) => JSON.parse(Buffer.from(h, "base64").toString("utf8")) as PaymentRequired;

async function post(base: string, path: string, body: unknown, header?: string) {
  const headers: Record<string, string> = { "content-type": "application/json" };
  if (header) headers["PAYMENT-SIGNATURE"] = header;
  const res = await fetch(`${base}${path}`, { method: "POST", body: JSON.stringify(body), headers });
  const text = await res.text();
  const required = res.headers.get("payment-required");
  return { status: res.status, text, required: required ? decode(required) : null };
}

function headerFor(required: PaymentRequired, transaction = "AA==", id = ID): string {
  const declared = (required.extensions?.["payment-identifier"] ?? { info: {} }) as { info?: Record<string, unknown> };
  const payload = {
    x402Version: 2,
    resource: required.resource,
    accepted: required.accepts[0]!,
    payload: { transaction },
    extensions: { "payment-identifier": { ...declared, info: { ...(declared.info ?? {}), id } } },
  } as PaymentPayload;
  return Buffer.from(JSON.stringify(payload)).toString("base64");
}

const evidenceBody = (pools: string[], extra: Record<string, unknown> = {}) => ({ pools, window: WINDOW, inputs: INPUTS, ...extra });

test("unit counting and ceilings follow the shared definition", () => {
  const t = tariffsFor(USDC_TESTNET);
  assert.equal(unitsFor("pool", { pools: 5, windowSeconds: 86_400, bodyBytes: 0 }), 5);
  assert.equal(unitsFor("pool_window", { pools: 5, windowSeconds: 86_400, bodyBytes: 0 }), 5);
  assert.equal(unitsFor("pool_window", { pools: 5, windowSeconds: 86_401, bodyBytes: 0 }), 10);
  assert.equal(unitsFor("input_kb", { pools: 0, windowSeconds: 0, bodyBytes: 1025 }), 2);
  assert.equal(ceiling(t.screen, 5), 1000);
  assert.equal(ceiling(t.events, 5), 7500);
  assert.equal(ceiling(t.investigate, 5), 8000);
  assert.equal(ceiling(t.explain, 8), 800);
  assert.throws(() => ceiling(t.screen, 21), /exceed max_units 20/);
  const hbar = tariffsFor("0.0.0");
  assert.equal(ceiling(hbar.screen, 5), 100_000, "same decimal price in an 8 decimal asset");
});

test("request validation refuses what must never be quoted", () => {
  const ok = parseEvidenceRequest(evidenceBody([POOL.toUpperCase()]), { listing: "screen", maxPools: 20, hourAligned: true });
  assert.deepEqual(ok.pools, [POOL]);
  assert.equal(ok.cap, 5000);
  assert.throws(() => parseEvidenceRequest(evidenceBody([]), { listing: "screen", maxPools: 20, hourAligned: true }), /non-empty/);
  assert.throws(() => parseEvidenceRequest(evidenceBody(Array.from({ length: 21 }, (_, i) => `0x${String(i).padStart(40, "0")}`)), { listing: "screen", maxPools: 20, hourAligned: true }), /exceed max_units/);
  assert.throws(() => parseEvidenceRequest(evidenceBody([POOL], { window: { from: WINDOW.from + 1, to: WINDOW.to } }), { listing: "screen", maxPools: 20, hourAligned: true }), /hour-aligned/);
  assert.doesNotThrow(() => parseEvidenceRequest(evidenceBody([POOL], { window: { from: WINDOW.from + 1, to: WINDOW.to } }), { listing: "events", maxPools: 20, hourAligned: false }));
  assert.throws(() => parseEvidenceRequest(evidenceBody([POOL], { cap: 0 }), { listing: "events", maxPools: 20, hourAligned: false }), /cap/);
  assert.throws(() => parseEvidenceRequest(evidenceBody([POOL], { extra: 1 }), { listing: "screen", maxPools: 20, hourAligned: true }), /unknown field/);
  assert.throws(() => parseEvidenceRequest(evidenceBody([POOL], { inputs: { materiality: "5%", min_event_usd: "1" } }), { listing: "screen", maxPools: 20, hourAligned: true }), /materiality/);
  assert.throws(() => parseExplainRequest({ brief: {} }, 8 * 1024 + 1), /at most 8192/);
  assert.throws(() => parseExplainRequest({ nope: {} }, 10), /brief object/);
});

test("the manifest has the buyer's schema", () => {
  const m = buildManifest({ baseUrl: "http://127.0.0.1:4021/", seller: "mandate-sellers", network: "hedera:testnet", asset: USDC_TESTNET, payTo: "0.0.111", tariffs: tariffsFor(USDC_TESTNET) }) as { listings: Record<string, unknown>[] };
  assert.equal(m.listings.length, 4);
  const events = m.listings.find((l) => l["id"] === "events")!;
  assert.deepEqual(Object.keys(events).sort(), ["asset", "capability", "id", "method", "network", "pay_to", "produces", "seller", "tariff", "url"]);
  assert.equal(events["url"], "http://127.0.0.1:4021/events");
  assert.equal(events["method"], "post");
  assert.equal(events["produces"], "transaction");
  assert.deepEqual(events["tariff"], { version: "2026-09-07", base: 0, unit: "pool_window", unit_price: 1500, max_units: 20, rounding: "up" });
});

test("every 402 carries the ceiling for the request, bazaar info, and does no work", async () => {
  const s = start();
  try {
    const manifest = await (await fetch(`${s.base}/manifest.json`)).json();
    assert.equal((manifest as { listings: unknown[] }).listings.length, 4);
    const two = [POOL, "0x1111111111111111111111111111111111111111"];
    const screen = await post(s.base, "/screen", evidenceBody(two));
    assert.equal(screen.status, 402);
    assert.equal(screen.required?.accepts[0]?.amount, "400");
    assert.ok(screen.required?.extensions?.["bazaar"], "bazaar discovery in the 402");
    const events = await post(s.base, "/events", evidenceBody(two, { window: { from: WINDOW.from, to: WINDOW.from + 90_000 } }));
    assert.equal(events.required?.accepts[0]?.amount, "6000", "two pools, two windows");
    const investigate = await post(s.base, "/investigate", evidenceBody(two));
    assert.equal(investigate.required?.accepts[0]?.amount, "4400");
    const brief = { brief: { outcomes: [], claims: [], pad: "x".repeat(1500) } };
    const explain = await post(s.base, "/explain", brief);
    assert.equal(explain.required?.accepts[0]?.amount, "200", "1.5 KB rounds up to 2 KB");
    assert.deepEqual(s.graph.calls, { screen: 0, events: 0, investigate: 0 }, "no Graph query for an unpaid request");
    assert.equal(s.explain.calls, 0, "no model call for an unpaid request");
    assert.deepEqual(s.fac.calls, { verify: 0, settle: 0 });
  } finally {
    s.close();
  }
});

test("invalid requests are 400 before any 402 or payment", async () => {
  const s = start();
  try {
    const tooMany = await post(s.base, "/screen", evidenceBody(Array.from({ length: 21 }, (_, i) => `0x${String(i).padStart(40, "0")}`)));
    assert.equal(tooMany.status, 400);
    assert.equal(tooMany.required, null);
    assert.match(tooMany.text, /exceed max_units/);
    const unaligned = await post(s.base, "/screen", evidenceBody([POOL], { window: { from: WINDOW.from + 60, to: WINDOW.to } }));
    assert.equal(unaligned.status, 400);
    const big = await post(s.base, "/explain", { brief: { pad: "x".repeat(9000) } });
    assert.equal(big.status, 400);
    const overWindows = await post(s.base, "/events", evidenceBody(Array.from({ length: 20 }, (_, i) => `0x${String(i).padStart(40, "0")}`), { window: { from: WINDOW.from, to: WINDOW.from + 2 * 86_400 } }));
    assert.equal(overWindows.status, 400, "20 pools over 48 h are 40 pool windows");
    assert.match(overWindows.text, /exceed max_units 20/);
    assert.equal(s.journal.all().filter((e) => e.source === "rejected").length, 4);
    assert.deepEqual(s.graph.calls, { screen: 0, events: 0, investigate: 0 });
  } finally {
    s.close();
  }
});

test("a paid screen runs one Graph query and settles once; investigate bundles facts, claims and prose", async () => {
  const s = start();
  try {
    const q = await post(s.base, "/screen", evidenceBody([POOL]));
    const paid = await post(s.base, "/screen", evidenceBody([POOL]), headerFor(q.required!));
    assert.equal(paid.status, 200);
    const body = JSON.parse(paid.text) as ScreenResponse;
    assert.equal(body.pools[POOL]?.verdict, "material");
    assert.deepEqual(s.graph.calls, { screen: 1, events: 0, investigate: 0 });
    assert.deepEqual(s.fac.calls, { verify: 1, settle: 1 });

    const q2 = await post(s.base, "/investigate", evidenceBody([POOL]));
    const paid2 = await post(s.base, "/investigate", evidenceBody([POOL]), headerFor(q2.required!, "AQ==", "pay_fedcba9876543210"));
    assert.equal(paid2.status, 200, paid2.text);
    const bundle = JSON.parse(paid2.text) as { outcomes: { outcome: string }[]; claims: { type: string; evidence: string[] }[]; explanation: { prose: string }; events: unknown };
    assert.equal(bundle.outcomes[0]?.outcome, "supported");
    assert.deepEqual(bundle.claims.map((c) => c.type), ["tvl_change", "tvl_change", "large_event", "activity_summary"]);
    assert.deepEqual(bundle.claims[2]?.evidence, ["0xswap"]);
    assert.match(bundle.explanation.prose, /explained/);
    assert.equal(s.explain.calls, 1);
    assert.deepEqual(s.graph.calls, { screen: 1, events: 0, investigate: 1 }, "the bundle is one pinned Graph call");
  } finally {
    s.close();
  }
});

test("a failed handler never settles", async () => {
  const s = start({ failGraph: true });
  try {
    const q = await post(s.base, "/screen", evidenceBody([POOL]));
    const paid = await post(s.base, "/screen", evidenceBody([POOL]), headerFor(q.required!));
    assert.equal(paid.status, 500);
    assert.equal(s.fac.calls.verify, 1);
    assert.equal(s.fac.calls.settle, 0, "no settlement for a failed handler");
    assert.equal(s.journal.all().at(-1)?.settle_ok, false);
  } finally {
    s.close();
  }
});

test("quote faults move the 402 amount and nothing else", async () => {
  const above = start({ faults: FAULT_QUOTE_ABOVE_CEILING });
  try {
    const q = await post(above.base, "/events", evidenceBody([POOL]));
    assert.equal(q.required?.accepts[0]?.amount, "2000", "1500 plus a third");
  } finally {
    above.close();
  }
  const drift = start({ faults: FAULT_QUOTE_DRIFT });
  try {
    const q = await post(drift.base, "/investigate", evidenceBody([POOL, "0x2222222222222222222222222222222222222222", "0x3333333333333333333333333333333333333333"]));
    assert.equal(q.required?.accepts[0]?.amount, "3000", "the demo's live bundle price");
    const s = await post(drift.base, "/screen", evidenceBody([POOL]));
    assert.equal(s.required?.accepts[0]?.amount, "200", "other listings unchanged");
  } finally {
    drift.close();
  }
  assert.throws(() => parseFaults("quote-above-ceiling,nope"), /unknown fault nope/);
});

test("outcomes and claims follow section 8", () => {
  const screen = screenResponse([POOL]);
  const pending = outcomesAndClaims(screen, null, "transaction", "100000");
  assert.equal(pending.outcomes[0]?.outcome, "pending");
  const screening = outcomesAndClaims(screen, null, "screening", "100000");
  assert.equal(screening.outcomes[0]?.outcome, "supported");
  const supported = outcomesAndClaims(screen, eventsResponse([POOL]), "transaction", "100000");
  assert.equal(supported.outcomes[0]?.outcome, "supported");
  const tvl = supported.claims[0]!;
  assert.equal(tvl.values["change"], "0.1");
  assert.equal(tvl.calculation, "(tvl_end - tvl_start) / tvl_start");
  assert.deepEqual(tvl.evidence, [`${POOL}:totalValueLockedToken0@100`, `${POOL}:totalValueLockedToken0@200`]);
  const activity = supported.claims[3]!;
  assert.equal(activity.values["volumeUSD"], "300.5");
  assert.equal(activity.values["txCount"], 7);
  assert.equal(activity.values["swap_count"], 1);
  const quiet = outcomesAndClaims(screenResponse([POOL], "non_material"), null, "transaction", "100000");
  assert.equal(quiet.outcomes[0]?.outcome, "non_material");
  assert.equal(quiet.claims.filter((c) => c.type === "large_event").length, 0);
});

test("a stored result is served under the terms that were paid, whatever the tariff says today", async () => {
  const store = new MemoryStore();
  const two = [POOL, "0x1111111111111111111111111111111111111111"];
  const first = start({ store });
  let header: string;
  let paidText: string;
  try {
    const q = await post(first.base, "/screen", evidenceBody(two));
    header = headerFor(q.required!);
    const paid = await post(first.base, "/screen", evidenceBody(two), header);
    assert.equal(paid.status, 200);
    paidText = paid.text;
  } finally {
    first.close();
  }
  const stricter = tariffsFor(USDC_TESTNET);
  stricter.screen = { ...stricter.screen, max_units: 1 };
  const second = start({ store, tariffs: stricter });
  try {
    const fresh = await post(second.base, "/screen", evidenceBody(two));
    assert.equal(fresh.status, 400, "a new two-pool purchase is now refused");
    const retrieved = await post(second.base, "/screen", evidenceBody(two), header);
    assert.equal(retrieved.status, 200, "the paid result is still retrievable");
    assert.equal(retrieved.text, paidText);
    assert.deepEqual(second.fac.calls, { verify: 0, settle: 0 });
    assert.deepEqual(second.graph.calls, { screen: 0, events: 0, investigate: 0 });
  } finally {
    second.close();
  }
});

test("case and trailing-slash variants of a listing path are refused, never priced by accident", async () => {
  const s = start();
  try {
    for (const path of ["/Screen", "/screen/", "/SCREEN/"]) {
      const res = await fetch(`${s.base}${path}`, { method: "POST", body: JSON.stringify(evidenceBody([POOL])), headers: { "content-type": "application/json" } });
      assert.equal(res.status, 404, path);
      assert.match(await res.text(), /the listing is POST \/screen/);
    }
    assert.deepEqual(s.fac.calls, { verify: 0, settle: 0 });
  } finally {
    s.close();
  }
});

test("incomplete facts are undetermined before materiality is considered", () => {
  const cases: [string, ScreenResponse, EventsResponse | null, string[]][] = [
    ["coverage shortfall", screenResponse([POOL], { coverageShortfall: true }), eventsResponse([POOL]), ["coverage_shortfall"]],
    ["truncated hours", screenResponse([POOL], { truncated: true }), eventsResponse([POOL]), ["truncated"]],
    ["unvalued mint on screen", screenResponse([POOL], { unvaluedMint: true }), null, ["unvalued_event:mint"]],
    ["truncated events", screenResponse([POOL]), eventsResponse([POOL], { truncated: true }), ["events_truncated"]],
    ["event without valuation", screenResponse([POOL]), eventsResponse([POOL], { amountUSD: null }), ["events_unvalued"]],
  ];
  for (const [name, screen, events, reasons] of cases) {
    const { outcomes } = outcomesAndClaims(screen, events, "transaction", "100000");
    assert.equal(outcomes[0]?.outcome, "undetermined", name);
    assert.deepEqual(outcomes[0]?.reasons, reasons, name);
  }
  const missing = screenResponse([POOL]);
  missing.pools[POOL]!.start = null;
  assert.deepEqual(incompleteness(missing, missing.pools[POOL]!, undefined), ["facts_missing"]);
});

test("a material pool whose held events are all small still cites a transaction", () => {
  const screen = screenResponse([POOL], { largeSwap: false });
  const events = eventsResponse([POOL], { amountUSD: "1500" });
  const { outcomes, claims } = outcomesAndClaims(screen, events, "transaction", "100000");
  assert.equal(outcomes[0]?.outcome, "supported");
  assert.deepEqual(claims.map((c) => c.type), ["tvl_change", "tvl_change", "largest_event", "activity_summary"]);
  const largest = claims[2]!;
  assert.deepEqual(largest.evidence, ["0xswap"]);
  assert.equal(largest.values["reaches_threshold"], false);
  assert.equal(largest.values["amountUSD"], "1500");
  assert.ok(!claims.some((c) => c.type === "large_event"), "no claim that the threshold was exceeded");
  const withLarge = outcomesAndClaims(screenResponse([POOL]), eventsResponse([POOL]), "transaction", "100000");
  assert.ok(!withLarge.claims.some((c) => c.type === "largest_event"), "one transaction claim per pool");
});
