// Spec section 8 for the investigate bundle: per-pool outcomes and claims
// computed from the seller's own facts, each claim with its calculation and
// the fact ids or transaction hashes it rests on. Completeness is checked
// before materiality: incomplete facts are `undetermined` with their reasons,
// never `supported`. The buyer recomputes both from the delivered facts and
// requires equality.
//
// Fact ids: `<pool>:<field>@<block>` for a block-height snapshot field,
// `<pool>:hours@<from>-<to>` for the hourly aggregates of a window, and
// `<pool>:events@<from>-<to>` for the delivered event lists.
import {
  type Dec,
  type EventKind,
  EVENT_KINDS,
  type EventsResponse,
  type PoolEvents,
  type PoolScreen,
  type ScreenResponse,
  addDec,
  cmpDec,
  dec,
  decToString,
  isZeroDec,
  subDec,
} from "./graph.js";

export type Outcome = "non_material" | "pending" | "supported" | "undetermined";
export type Evidence = "screening" | "transaction";

export interface PoolOutcome {
  pool: string;
  outcome: Outcome;
  reasons: string[];
}

export interface Claim {
  type: "tvl_change" | "large_event" | "largest_event" | "activity_summary";
  pool: string;
  values: Record<string, string | number | boolean | null>;
  calculation: string;
  evidence: string[];
}

export function factId(pool: string, field: string, at: string | number): string {
  return `${pool}:${field}@${at}`;
}

/** (a / b) to 18 decimals, exact on the scaled integers. */
export function divDec(a: Dec, b: Dec): Dec {
  const scale = 10n ** 18n;
  const e = Math.max(a.e, b.e);
  const an = a.m * 10n ** BigInt(e - a.e);
  const bn = b.m * 10n ** BigInt(e - b.e);
  return { m: (an * scale) / bn, e: 18 };
}

/** Why a pool's facts cannot support a verdict, section 8's undetermined row. */
export function incompleteness(screen: ScreenResponse, facts: PoolScreen, held: PoolEvents | undefined): string[] {
  const reasons: string[] = [];
  if (screen.coverage_shortfall) reasons.push("coverage_shortfall");
  if (facts.truncated) reasons.push("truncated");
  if (!facts.start || !facts.end) reasons.push("facts_missing");
  if (facts.unvalued_events.mint) reasons.push("unvalued_event:mint");
  if (facts.unvalued_events.burn) reasons.push("unvalued_event:burn");
  for (const reason of facts.reasons) {
    if (reason.startsWith("undetermined:")) reasons.push(reason.slice("undetermined:".length));
  }
  if (held !== undefined) {
    if (held.truncated) reasons.push("events_truncated");
    if (held.amount_usd_nulls > 0) reasons.push("events_unvalued");
  }
  return [...new Set(reasons)];
}

export function outcomesAndClaims(
  screen: ScreenResponse,
  events: EventsResponse | null,
  evidence: Evidence,
  minEventUsd: string,
): { outcomes: PoolOutcome[]; claims: Claim[] } {
  const outcomes: PoolOutcome[] = [];
  const claims: Claim[] = [];
  for (const [pool, facts] of Object.entries(screen.pools)) {
    const held = events?.pools[pool];
    const incomplete = incompleteness(screen, facts, held);
    let outcome: Outcome;
    let reasons: string[];
    if (incomplete.length > 0) {
      outcome = "undetermined";
      reasons = incomplete;
    } else if (facts.verdict === "material") {
      outcome = evidence === "screening" || held !== undefined ? "supported" : "pending";
      reasons = facts.reasons;
    } else {
      outcome = "non_material";
      reasons = facts.reasons;
    }
    outcomes.push({ pool, outcome, reasons });

    if (facts.start && facts.end) {
      for (const i of [0, 1] as const) {
        const field = i === 0 ? "totalValueLockedToken0" : "totalValueLockedToken1";
        const start = facts.start[field];
        const end = facts.end[field];
        const s = dec(start);
        const change = subDec(dec(end), s);
        const zero = isZeroDec(s);
        claims.push({
          type: "tvl_change",
          pool,
          values: {
            token: i,
            tvl_start: start,
            tvl_end: end,
            change: decToString(zero ? change : divDec(change, s)),
            relative: !zero,
          },
          calculation: zero ? "tvl_end - tvl_start" : "(tvl_end - tvl_start) / tvl_start",
          evidence: [factId(pool, field, screen.block_start), factId(pool, field, screen.block_end)],
        });
      }
    }

    let largest: { kind: EventKind; amountUSD: string; tx: string } | null = null;
    for (const kind of EVENT_KINDS) {
      const hit = facts.large_events[kind];
      if (hit && (largest === null || cmpDec(dec(hit.amountUSD), dec(largest.amountUSD)) > 0)) {
        largest = { kind, amountUSD: hit.amountUSD, tx: hit.transaction.id };
      }
    }
    if (largest !== null) {
      claims.push({
        type: "large_event",
        pool,
        values: { kind: largest.kind, amountUSD: largest.amountUSD, min_event_usd: minEventUsd },
        calculation: "amountUSD >= min_event_usd",
        evidence: [largest.tx],
      });
    } else if (held !== undefined) {
      // Section 9: a supported pool with held events must cite a transaction.
      // When no event reaches the threshold, the largest held event is
      // cited as exactly that, never as a large event.
      const top = largestHeld(held);
      if (top !== null) {
        claims.push({
          type: "largest_event",
          pool,
          values: { kind: top.kind, amountUSD: top.amountUSD, min_event_usd: minEventUsd, reaches_threshold: false },
          calculation: "max(amountUSD) over held events, below min_event_usd",
          evidence: [top.tx],
        });
      }
    }

    let volume = dec("0");
    let txCount = 0;
    for (const h of facts.hours) {
      volume = addDec(volume, dec(h.volumeUSD));
      txCount += Number(h.txCount);
    }
    const values: Record<string, string | number | boolean | null> = {
      volumeUSD: decToString(volume),
      txCount,
      largest_swap_usd: facts.large_events.swap?.amountUSD ?? null,
      largest_mint_usd: facts.large_events.mint?.amountUSD ?? null,
      largest_burn_usd: facts.large_events.burn?.amountUSD ?? null,
    };
    const ev = [factId(pool, "hours", `${screen.window_requested.from}-${screen.window_requested.to}`)];
    if (held !== undefined) {
      for (const kind of EVENT_KINDS) {
        values[`${kind}_count`] = held.counts[kind];
        values[`${kind}_amount_usd`] = held.sum_amount_usd[kind];
      }
      values["amount_usd_nulls"] = held.amount_usd_nulls;
      ev.push(factId(pool, "events", `${screen.window_requested.from}-${screen.window_requested.to}`));
    }
    claims.push({
      type: "activity_summary",
      pool,
      values,
      calculation:
        held === undefined
          ? "sum(hours.volumeUSD), sum(hours.txCount)"
          : "sum(hours.volumeUSD), sum(hours.txCount), count and sum(amountUSD) per event type",
      evidence: ev,
    });
  }
  return { outcomes, claims };
}

function largestHeld(held: PoolEvents): { kind: EventKind; amountUSD: string; tx: string } | null {
  let top: { kind: EventKind; amountUSD: string; tx: string } | null = null;
  const consider = (kind: EventKind, tx: string, amountUSD: string | null) => {
    if (amountUSD === null) return;
    if (top === null || cmpDec(dec(amountUSD), dec(top.amountUSD)) > 0) top = { kind, amountUSD, tx };
  };
  for (const e of held.swaps) consider("swap", e.transaction.id, e.amountUSD);
  for (const e of held.mints) consider("mint", e.transaction.id, e.amountUSD);
  for (const e of held.burns) consider("burn", e.transaction.id, e.amountUSD);
  return top;
}
