// Listings and tariffs for the four reference sellers. These mirror the
// pinned manifest (mandate-demo/manifest.json) exactly: same ids, same
// ceilings. The runtime computes ceilings from the manifest with the same
// arithmetic, so a quote at the ceiling is always within_tariff.

export const NETWORK = "hedera:testnet";
export const ASSET = process.env.SELLER_ASSET || "0.0.429274"; // testnet USDC
export const PAY_TO = process.env.SELLER_PAY_TO || "0.0.7777777";
export const FEE_PAYER = process.env.FEE_PAYER || "0.0.7162784"; // Blocky402 testnet fee payer
export const MAX_TIMEOUT_S = 120;
export const SERVICE_DECIMALS = 6;

// The Uniswap v3 subgraph on The Graph Network (decentralized network).
export const SUBGRAPH_ID = "5zvR82QoaXYFyDEKLZ9t6v9adgnptxYpKpSbxtgVENFV";

// base + unit_price * units, in atomic units of the service asset.
export const TARIFFS = {
  screen: { id: "screen", seller: "mandate-screen", base: 0, unit: "Pool", unit_price: 200, max_units: 100 },
  events: { id: "events", seller: "mandate-events", base: 0, unit: "PoolWindow", unit_price: 1500, max_units: 100 },
  investigate: { id: "investigate", seller: "mandate-investigate", base: 2000, unit: "Pool", unit_price: 1200, max_units: 100 },
  explain: { id: "explain", seller: "mandate-explain", base: 0, unit: "InputKb", unit_price: 100, max_units: 8 },
};

export const ROUTE_TO_CAPABILITY = {
  "/screen": "screen",
  "/events": "events",
  "/investigate": "investigate",
  "/explain": "explain",
};

// How many tariff units a request body carries.
export function unitsFor(capability, body) {
  switch (capability) {
    case "screen":
    case "events":
    case "investigate":
      if (!Array.isArray(body?.pools) || body.pools.length === 0) {
        throw new httpError(400, "request must list pools");
      }
      return body.pools.length;
    case "explain": {
      const bytes = Buffer.byteLength(JSON.stringify(body ?? {}), "utf8");
      return Math.max(1, Math.ceil(bytes / 1024));
    }
    default:
      throw new httpError(500, `unknown capability ${capability}`);
  }
}

export function priceFor(capability, units) {
  const t = TARIFFS[capability];
  if (units > t.max_units) {
    throw new httpError(413, `request of ${units} units exceeds max_units ${t.max_units}`);
  }
  return t.base + t.unit_price * units;
}

export class httpError extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

// The listings document served at GET /manifest.json, shaped like the pinned
// manifest so a buyer can diff them.
export function listingsDocument(baseUrl) {
  return Object.values(TARIFFS).map((t) => ({
    id: t.id,
    seller: t.seller,
    url: `${baseUrl}/${t.id}`,
    method: "POST",
    capability: t.id,
    produces: t.id === "screen" ? ["Screening"] : t.id === "explain" ? ["Report"] : ["Transaction", "Report"],
    tariff: {
      version: "1.0.0",
      base: t.base,
      unit: t.unit,
      unit_price: t.unit_price,
      max_units: t.max_units,
      rounding: "InputKb",
    },
    network: NETWORK,
    asset: ASSET,
    pay_to: PAY_TO,
    discovery: {
      version: "1.0.0",
      listings: `${baseUrl}/manifest.json`,
    },
  }));
}