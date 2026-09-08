// Spec section 8 for the investigate bundle: per-pool outcomes and claims
// computed from the seller's own facts, each claim with its calculation and
// the fact ids or transaction hashes it rests on. The buyer recomputes both
// from the delivered facts and requires equality.
//
// Fact ids: `<pool>:<field>@<block>` for a block-height snapshot field,
// `<pool>:hours@<from>-<to>` for the hourly aggregates of a window, and
// `<pool>:events@<from>-<to>` for the delivered event lists.
import {
  type Dec,
  type EventKind,
  EVENT_KINDS,
  type EventsResponse,
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
  type: "tvl_change" | "large_event" | "activity_summary";
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
    let outcome: Outcome;
    if (facts.verdict === "undetermined") outcome = "undetermined";
    else if (facts.verdict === "non_material") outcome = "non_material";
    else if (evidence === "screening" || held !== undefined) outcome = "supported";
    else outcome = "pending";
    outcomes.push({ pool, outcome, reasons: facts.reasons });

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
      calculation: held === undefined ? "sum(hours.volumeUSD), sum(hours.txCount)" : "sum(hours.volumeUSD), sum(hours.txCount), count and sum(amountUSD) per event type",
      evidence: ev,
    });
  }
  return { outcomes, claims };
}
