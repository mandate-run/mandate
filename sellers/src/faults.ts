// Fault switches for Proctor and the demo, read from FAULT as a comma list.
// `drop-response-after-settle` is applied in the shell; the two quote faults
// change the 402 amount and nothing else.
import type { ListingId } from "./tariffs.js";

export const FAULT_DROP_RESPONSE = "drop-response-after-settle";
export const FAULT_QUOTE_ABOVE_CEILING = "quote-above-ceiling";
export const FAULT_QUOTE_DRIFT = "quote-drift";
export const FAULTS = [FAULT_DROP_RESPONSE, FAULT_QUOTE_ABOVE_CEILING, FAULT_QUOTE_DRIFT] as const;

export function parseFaults(text: string | undefined): Set<string> {
  const out = new Set<string>();
  for (const f of (text ?? "").split(",")) {
    const name = f.trim();
    if (name === "") continue;
    if (!(FAULTS as readonly string[]).includes(name)) throw new Error(`unknown fault ${name}; known: ${FAULTS.join(", ")}`);
    out.add(name);
  }
  return out;
}

/**
 * The amount a listing quotes under the active faults. `quote-above-ceiling`
 * quotes a third above the ceiling on every listing; `quote-drift` makes
 * investigate quote 0.0030 in the asset's decimals, the demo's live price
 * below its ceiling, whatever the request.
 */
export function quotedAmount(listing: ListingId | string, ceilingAmount: number, faults: ReadonlySet<string>, scale: number): number {
  if (faults.has(FAULT_QUOTE_ABOVE_CEILING)) return ceilingAmount + Math.ceil(ceilingAmount / 3);
  if (faults.has(FAULT_QUOTE_DRIFT) && listing === "investigate") return 3000 * scale;
  return ceilingAmount;
}
