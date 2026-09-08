// Spec section 2.2 tariffs for the four listings, and the unit counting shared
// with the buyer: `pool` counts pools, `pool_window` counts pools times
// ceil(window_seconds / 86400), `input_kb` counts ceil(body_bytes / 1024).
// The 402 amount is the ceiling for the request's count; requests above
// max_units are refused before the 402, never clamped.

export type Unit = "pool" | "pool_window" | "input_kb";
export const LISTING_IDS = ["screen", "events", "investigate", "explain"] as const;
export type ListingId = (typeof LISTING_IDS)[number];

export interface Tariff {
  version: string;
  /** Fixed component, atomic units. */
  base: number;
  unit: Unit;
  /** Atomic units per unit. */
  unit_price: number;
  max_units: number;
  rounding: "up";
}

export interface RequestShape {
  pools: number;
  windowSeconds: number;
  bodyBytes: number;
}

export const WINDOW_SECONDS = 86_400;
export const KB = 1024;
export const TARIFF_VERSION = "2026-09-07";
export const MAX_POOLS = 20;
export const MAX_BRIEF_KB = 8;

export class TariffError extends Error {
  readonly units: number;
  readonly maxUnits: number;
  constructor(units: number, maxUnits: number) {
    super(`${units} units exceed max_units ${maxUnits}`);
    this.name = "TariffError";
    this.units = units;
    this.maxUnits = maxUnits;
  }
}

export function unitsFor(unit: Unit, shape: RequestShape): number {
  switch (unit) {
    case "pool":
      return shape.pools;
    case "pool_window":
      return shape.pools * Math.ceil(shape.windowSeconds / WINDOW_SECONDS);
    case "input_kb":
      return Math.ceil(shape.bodyBytes / KB);
  }
}

/** base + unit_price * units, or a TariffError above max_units. */
export function ceiling(tariff: Tariff, units: number): number {
  if (units > tariff.max_units) throw new TariffError(units, tariff.max_units);
  return tariff.base + tariff.unit_price * units;
}

/** Decimals of the assets the sellers price in. */
export function decimalsFor(asset: string): number {
  if (asset === "0.0.0") return 8;
  return 6;
}

/**
 * The concept doc's tariffs in USDC atomic units: screen 0.0002 per pool,
 * events 0.0015 per pool-window, investigate 0.0020 plus 0.0012 per pool,
 * explain 0.0001 per input KB up to 8 KB. Scaled for assets with other
 * decimals so the decimal price reads the same.
 */
export function tariffsFor(asset: string): Record<ListingId, Tariff> {
  const scale = 10 ** (decimalsFor(asset) - 6);
  const t = (base: number, unit: Unit, unit_price: number, max_units: number): Tariff => ({
    version: TARIFF_VERSION,
    base: base * scale,
    unit,
    unit_price: unit_price * scale,
    max_units,
    rounding: "up",
  });
  return {
    screen: t(0, "pool", 200, MAX_POOLS),
    events: t(0, "pool_window", 1500, MAX_POOLS),
    investigate: t(2000, "pool", 1200, MAX_POOLS),
    explain: t(0, "input_kb", 100, MAX_BRIEF_KB),
  };
}
