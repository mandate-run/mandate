// Request shapes for the listings. Anything invalid is a 400 before the 402,
// so an unpaid request costs the seller no Graph query and the buyer no
// payment. The screen's window must be hour-aligned; events and investigate
// take any window.
import { HOUR } from "./graph.js";
import { KB, MAX_BRIEF_KB, type ListingId, type RequestShape } from "./tariffs.js";

export interface Window {
  from: number;
  to: number;
}

export interface Inputs {
  materiality: string;
  min_event_usd: string;
}

export interface EvidenceRequest {
  pools: string[];
  window: Window;
  inputs: Inputs;
  cap: number;
}

export interface ExplainRequest {
  brief: Record<string, unknown>;
}

export const DEFAULT_CAP = 5000;
export const MAX_CAP = 20_000;
const POOL = /^0x[0-9a-f]{40}$/;
const DECIMAL = /^\d+(\.\d+)?$/;

export class RequestError extends Error {
  readonly status = 400;
  constructor(message: string) {
    super(message);
    this.name = "RequestError";
  }
}

/**
 * A plain JSON object, which is the only thing a request body may be. An
 * object whose fields come from a prototype is not one: `Object.keys` would
 * see nothing, so the unknown-field check below would pass trivially while
 * the parser read values the body does not own. `JSON.parse` never produces
 * such an object, and the parser does not rely on that.
 */
function isRecord(v: unknown): v is Record<string, unknown> {
  if (typeof v !== "object" || v === null || Array.isArray(v)) return false;
  const proto = Object.getPrototypeOf(v) as unknown;
  return proto === Object.prototype || proto === null;
}

/** An own property, so nothing is read through a prototype. */
function own(o: Record<string, unknown>, key: string): unknown {
  return Object.hasOwn(o, key) ? o[key] : undefined;
}

function integer(v: unknown, name: string): number {
  if (typeof v !== "number" || !Number.isInteger(v) || v < 0) throw new RequestError(`${name} must be a non-negative integer`);
  return v;
}

export function parseEvidenceRequest(
  body: unknown,
  opts: { listing: ListingId; maxPools: number; hourAligned: boolean },
): EvidenceRequest {
  if (!isRecord(body)) throw new RequestError("body must be a JSON object");
  const rawPools = own(body, "pools");
  if (!Array.isArray(rawPools) || rawPools.length === 0) throw new RequestError("pools must be a non-empty array");
  if (rawPools.length > opts.maxPools) throw new RequestError(`pools: ${rawPools.length} exceed max_units ${opts.maxPools}`);
  const pools: string[] = [];
  for (const p of rawPools) {
    if (typeof p !== "string" || !POOL.test(p.toLowerCase())) throw new RequestError(`pools: ${String(p)} is not a pool address`);
    const lower = p.toLowerCase();
    if (pools.includes(lower)) throw new RequestError(`pools: ${lower} listed twice`);
    pools.push(lower);
  }
  const rawWindow = own(body, "window");
  if (!isRecord(rawWindow)) throw new RequestError("window must be an object with from and to");
  const from = integer(own(rawWindow, "from"), "window.from");
  const to = integer(own(rawWindow, "to"), "window.to");
  if (to <= from) throw new RequestError("window.to must be after window.from");
  if (opts.hourAligned && (from % HOUR !== 0 || to % HOUR !== 0)) {
    throw new RequestError(`${opts.listing} windows must be hour-aligned unix seconds`);
  }
  const rawInputs = own(body, "inputs");
  if (!isRecord(rawInputs)) throw new RequestError("inputs must be an object with materiality and min_event_usd");
  const materiality = own(rawInputs, "materiality");
  const minEventUsd = own(rawInputs, "min_event_usd");
  if (typeof materiality !== "string" || !DECIMAL.test(materiality)) throw new RequestError("inputs.materiality must be a decimal string");
  if (typeof minEventUsd !== "string" || !DECIMAL.test(minEventUsd)) throw new RequestError("inputs.min_event_usd must be a decimal string");
  let cap = DEFAULT_CAP;
  if (own(body, "cap") !== undefined) {
    cap = integer(own(body, "cap"), "cap");
    if (cap === 0 || cap > MAX_CAP) throw new RequestError(`cap must be between 1 and ${MAX_CAP}`);
  }
  for (const key of Object.keys(body)) {
    if (!["pools", "window", "inputs", "cap"].includes(key)) throw new RequestError(`unknown field ${key}`);
  }
  return { pools, window: { from, to }, inputs: { materiality, min_event_usd: minEventUsd }, cap };
}

/** The brief is the only input explain receives; at most 8 KB of body. */
export function parseExplainRequest(body: unknown, bodyBytes: number): ExplainRequest {
  if (bodyBytes > MAX_BRIEF_KB * KB) throw new RequestError(`brief is ${bodyBytes} bytes; at most ${MAX_BRIEF_KB * KB}`);
  if (!isRecord(body) || !isRecord(own(body, "brief"))) throw new RequestError("body must be a JSON object with a brief object");
  for (const key of Object.keys(body)) {
    if (key !== "brief") throw new RequestError(`unknown field ${key}`);
  }
  return { brief: own(body, "brief") as Record<string, unknown> };
}

export function shapeOf(request: EvidenceRequest, bodyBytes: number): RequestShape {
  return { pools: request.pools.length, windowSeconds: request.window.to - request.window.from, bodyBytes };
}
