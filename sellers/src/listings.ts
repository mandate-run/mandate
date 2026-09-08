// The four listings of docs/mandate.md on the seller shell: screen, events,
// investigate, explain. Each prices its request at the tariff ceiling before
// the 402, refuses anything above max_units before payment, and does no Graph
// query and no model call until verification has passed.
import type { RequestHandler } from "express";
import { type Claim, type Evidence, type PoolOutcome, outcomesAndClaims } from "./claims.js";
import type { Explainer } from "./explain.js";
import { quotedAmount } from "./faults.js";
import type { EventsResponse, GraphClient, ScreenResponse } from "./graph.js";
import { type EvidenceRequest, parseEvidenceRequest, parseExplainRequest, shapeOf } from "./requests.js";
import { type ListingId, type Tariff, ceiling, decimalsFor, unitsFor } from "./tariffs.js";
import type { Listing } from "./x402.js";

/** What the handlers need; the Graph client and the model are injected so tests can fake them. */
export interface Deps {
  graph: Pick<GraphClient, "screen" | "eventsProduct" | "investigate">;
  explain: Explainer;
  tariffs: Record<ListingId, Tariff>;
  asset: string;
  faults: ReadonlySet<string>;
  /** `requirements.evidence` assumed for the investigate bundle's outcomes. */
  evidence?: Evidence;
}

export interface InvestigateResponse {
  screen: ScreenResponse;
  events: EventsResponse | null;
  outcomes: PoolOutcome[];
  claims: Claim[];
  explanation: { prose: string; model: string; input_bytes: number };
  requests: number;
}

const POOLS_INPUT = {
  pools: ["0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640"],
  window: { from: 1_788_800_400, to: 1_788_886_800 },
  inputs: { materiality: "0.05", min_event_usd: "100000" },
};

function evidencePrice(id: ListingId, deps: Deps, hourAligned: boolean): Listing["price"] {
  const tariff = deps.tariffs[id];
  const scale = 10 ** (decimalsFor(deps.asset) - 6);
  return (body, bodyBytes) => {
    const request = parseEvidenceRequest(body, { listing: id, maxPools: tariff.max_units, hourAligned });
    const units = unitsFor(tariff.unit, shapeOf(request, bodyBytes));
    return quotedAmount(id, ceiling(tariff, units), deps.faults, scale);
  };
}

function asyncHandler(fn: (body: unknown) => Promise<unknown>): RequestHandler {
  return (req, res, next) => {
    fn(req.body).then(
      (out) => void res.json(out),
      (err: unknown) => next(err),
    );
  };
}

export function listings(deps: Deps): Listing[] {
  const evidence = deps.evidence ?? "transaction";
  const investigateScale = 10 ** (decimalsFor(deps.asset) - 6);
  return [
    {
      method: "POST",
      path: "/screen",
      description: "Token TVL and activity at the window's ends per pool, from block-height queries. Priced per pool.",
      price: evidencePrice("screen", deps, true),
      discovery: { bodyType: "json", input: POOLS_INPUT },
      handler: asyncHandler(async (body) => {
        const r = parseEvidenceRequest(body, { listing: "screen", maxPools: deps.tariffs.screen.max_units, hourAligned: true });
        return deps.graph.screen(r.pools, r.window, r.inputs);
      }),
    },
    {
      method: "POST",
      path: "/events",
      description: "Mints, burns and swaps in the window with transaction hashes. Priced per pool-window.",
      price: evidencePrice("events", deps, false),
      discovery: { bodyType: "json", input: { ...POOLS_INPUT, cap: 5000 } },
      handler: asyncHandler(async (body) => {
        const r = parseEvidenceRequest(body, { listing: "events", maxPools: deps.tariffs.events.max_units, hourAligned: false });
        return deps.graph.eventsProduct(r.pools, r.window, r.cap);
      }),
    },
    {
      method: "POST",
      path: "/investigate",
      description: "Screen plus events plus explanation, bundled: facts, outcomes, claims with evidence, prose. Priced per pool plus a base.",
      price: (body, bodyBytes) => {
        const tariff = deps.tariffs.investigate;
        const request = parseEvidenceRequest(body, { listing: "investigate", maxPools: tariff.max_units, hourAligned: true });
        const units = unitsFor(tariff.unit, shapeOf(request, bodyBytes));
        return quotedAmount("investigate", ceiling(tariff, units), deps.faults, investigateScale);
      },
      discovery: { bodyType: "json", input: { ...POOLS_INPUT, cap: 5000 } },
      handler: asyncHandler(async (body) => investigate(deps, body, evidence)),
    },
    {
      method: "POST",
      path: "/explain",
      description: "Prose from a bounded brief of claims the buyer computed. Priced per input KB, at most 8 KB.",
      price: (body, bodyBytes) => {
        parseExplainRequest(body, bodyBytes);
        const tariff = deps.tariffs.explain;
        const units = unitsFor(tariff.unit, { pools: 0, windowSeconds: 0, bodyBytes });
        return quotedAmount("explain", ceiling(tariff, Math.max(units, 1)), deps.faults, investigateScale);
      },
      discovery: { bodyType: "json", input: { brief: { outcomes: [], claims: [] } } },
      handler: asyncHandler(async (body) => {
        const r = parseExplainRequest(body, Buffer.byteLength(JSON.stringify(body)));
        return deps.explain(r.brief);
      }),
    },
  ];
}

async function investigate(deps: Deps, body: unknown, evidence: Evidence): Promise<InvestigateResponse> {
  const r: EvidenceRequest = parseEvidenceRequest(body, { listing: "investigate", maxPools: deps.tariffs.investigate.max_units, hourAligned: true });
  // One pinned context for the whole bundle: the screen and the events for
  // the material pools come from one head and one deployment.
  const { screen, events } = await deps.graph.investigate(r.pools, r.window, r.inputs, r.cap);
  const { outcomes, claims } = outcomesAndClaims(screen, events, evidence, r.inputs.min_event_usd);
  const brief = {
    outcomes,
    claims,
    events: events === null ? {} : Object.fromEntries(Object.entries(events.pools).map(([pool, p]) => [pool, supporting(p)])),
  };
  const explanation = await deps.explain(brief);
  return { screen, events, outcomes, claims, explanation, requests: screen.requests + (events?.requests ?? 0) };
}

function supporting(p: EventsResponse["pools"][string]): { kind: string; tx: string; amountUSD: string | null }[] {
  const all = [
    ...p.swaps.map((e) => ({ kind: "swap", tx: e.transaction.id, amountUSD: e.amountUSD })),
    ...p.mints.map((e) => ({ kind: "mint", tx: e.transaction.id, amountUSD: e.amountUSD })),
    ...p.burns.map((e) => ({ kind: "burn", tx: e.transaction.id, amountUSD: e.amountUSD })),
  ];
  all.sort((a, b) => Number(b.amountUSD ?? 0) - Number(a.amountUSD ?? 0));
  return all.slice(0, 5);
}

/** The manifest of section 2.2, in the shape the buyer's loader reads. */
export function buildManifest(params: {
  baseUrl: string;
  seller: string;
  network: string;
  asset: string;
  payTo: string;
  tariffs: Record<ListingId, Tariff>;
}): Record<string, unknown> {
  const capability: Record<ListingId, { capability: string; produces: string }> = {
    screen: { capability: "screen", produces: "screening" },
    events: { capability: "events", produces: "transaction" },
    investigate: { capability: "investigate", produces: "report" },
    explain: { capability: "explain", produces: "report" },
  };
  return {
    version: params.tariffs.screen.version,
    listings: (Object.keys(params.tariffs) as ListingId[]).map((id) => ({
      id,
      seller: params.seller,
      url: `${params.baseUrl.replace(/\/$/, "")}/${id}`,
      method: "post",
      capability: capability[id].capability,
      produces: capability[id].produces,
      tariff: params.tariffs[id],
      network: params.network,
      asset: params.asset,
      pay_to: params.payTo,
    })),
  };
}
