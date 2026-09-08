// Fault switches for Proctor and the demo, read from FAULT as a comma list.
// `drop-response-after-settle` is applied in the shell; the two quote faults
// change the 402 amount and nothing else. `quote-above-ceiling:<listing>`
// overquotes one listing; the bare form overquotes every listing.
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
    const base = name.split(":")[0] ?? name;
    if (!(FAULTS as readonly string[]).includes(base)) throw new Error(`unknown fault ${name}; known: ${FAULTS.join(", ")}`);
    if (name.includes(":") && base !== FAULT_QUOTE_ABOVE_CEILING) throw new Error(`fault ${base} takes no listing`);
    out.add(name);
  }
  return out;
}

/** Whether `quote-above-ceiling` applies to `listing`, bare or qualified. */
export function overquotes(faults: ReadonlySet<string>, listing: string): boolean {
  return faults.has(FAULT_QUOTE_ABOVE_CEILING) || faults.has(`${FAULT_QUOTE_ABOVE_CEILING}:${listing}`);
}

/**
 * The amount a listing quotes under the active faults. `quote-above-ceiling`
 * quotes a third above the ceiling on every listing; `quote-drift` makes
 * investigate quote 0.0030 in the asset's decimals, the demo's live price
 * below its ceiling, whatever the request.
 */
export function quotedAmount(listing: ListingId | string, ceilingAmount: number, faults: ReadonlySet<string>, scale: number): number {
  if (overquotes(faults, listing)) return ceilingAmount + Math.ceil(ceilingAmount / 3);
  if (faults.has(FAULT_QUOTE_DRIFT) && listing === "investigate") return 3000 * scale;
  return ceilingAmount;
}
