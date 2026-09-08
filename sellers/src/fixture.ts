// A canned Graph gateway for runs without a Subgraph Studio key: Proctor,
// CI and a clean clone. It answers the client's own GraphQL operations from
// fixed facts shaped to whatever window and pool the request names, so the
// sellers' code path is the live one and only the data is synthetic. The
// transaction hashes are synthetic too, so a mandate over the fixture sets
// `provenance_samples = 0`.
//
//   GRAPH_FIXTURE=material   token0 TVL rises 10% and a 250000 USD swap lands
//   GRAPH_FIXTURE=quiet      nothing material happens
//   GRAPH_FIXTURE=mixed      the pool in MIXED_MATERIAL is material, every other pool quiet
import type { FetchLike } from "./graph.js";

export const FIXTURES = ["material", "quiet", "mixed"] as const;
export type Fixture = (typeof FIXTURES)[number];

/** The one material pool of the mixed fixture: the demo's USDC/WETH 0.05%. */
export const MIXED_MATERIAL = "0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640";

/** What a pool looks like under a fixture. */
export function scenarioFor(fixture: Fixture, pool: string): "material" | "quiet" {
  if (fixture !== "mixed") return fixture;
  return pool.toLowerCase() === MIXED_MATERIAL ? "material" : "quiet";
}

export const DEPLOYMENT = "QmFixtureUniswapV3";
const HEAD = 23_507_100;
const BLOCK_START = 23_500_000;
const BLOCK_END = 23_507_000;

/** Deterministic 66-byte hashes, one per event. */
export function txHash(n: number): string {
  return `0x${n.toString(16).padStart(64, "0")}`;
}

function json(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), { status, headers: { "content-type": "application/json" } });
}

export function parseFixture(name: string | undefined): Fixture | undefined {
  if (name === undefined || name === "") return undefined;
  if (!(FIXTURES as readonly string[]).includes(name)) throw new Error(`unknown GRAPH_FIXTURE ${name}; known: ${FIXTURES.join(", ")}`);
  return name as Fixture;
}

interface Body {
  operationName: string;
  variables: Record<string, unknown>;
}

function snapshot(tvl0: string, tvl1: string) {
  return {
    totalValueLockedToken0: tvl0,
    totalValueLockedToken1: tvl1,
    totalValueLockedUSD: "4500000",
    liquidity: "12345678901234567890",
    token0: { derivedETH: "0.00045" },
    token1: { derivedETH: "1" },
  };
}

function swap(n: number, from: number, amountUSD: string | null, index: number) {
  return {
    id: `0xfixture${n.toString().padStart(4, "0")}#${index}`,
    transaction: { id: txHash(n) },
    logIndex: String(index),
    timestamp: String(from + 600 * n),
    amount0: "125000",
    amount1: "-56.25",
    amountUSD,
    origin: "0x000000000000000000000000000000000000fee1",
  };
}

function liquidity(n: number, from: number, amountUSD: string, index: number) {
  return {
    ...swap(n, from, amountUSD, index),
    amount0: "60000",
    amount1: "27",
    owner: "0x000000000000000000000000000000000000beef",
    tickLower: "-201000",
    tickUpper: "-199000",
  };
}

/** The fixture's events for one window: every kind, in id order. */
export function fixtureEvents(fixture: "material" | "quiet", from: number) {
  if (fixture === "quiet") {
    return {
      swaps: [swap(1, from, "1200.5", 3), swap(2, from, "980", 7)],
      mints: [],
      burns: [],
    };
  }
  return {
    swaps: [swap(1, from, "250000", 12), swap(2, from, "40000.75", 4), swap(3, from, "1500", 9)],
    mints: [liquidity(4, from, "120000", 2)],
    burns: [liquidity(5, from, "2500", 5)],
  };
}

/** A gateway that serves the fixture. Every response names the fixture deployment. */
export function fixtureFetch(fixture: Fixture, now: () => number = () => Math.floor(Date.now() / 1000)): FetchLike {
  return async (_url, init) => {
    const body = JSON.parse(String(init.body)) as Body;
    const vars = body.variables;
    const meta = { deployment: DEPLOYMENT };
    switch (body.operationName) {
      case "Meta":
        return json({ data: { _meta: { block: { number: HEAD, timestamp: now() }, hasIndexingErrors: false, deployment: DEPLOYMENT } } });
      case "ObservationBlock": {
        const ts = Number(vars["ts"]);
        const head = Number(vars["head"]);
        // The last state change at or before `ts`: a fixed block for each boundary.
        const block = head === HEAD && ts < now() - 3600 ? BLOCK_START : BLOCK_END;
        return json({ data: { transactions: [{ blockNumber: String(block), timestamp: String(ts - 7) }], _meta: meta } });
      }
      case "ScreenPool": {
        const from = Number(vars["hourFrom"]);
        const to = Number(vars["hourTo"]);
        const minUsd = String(vars["minUsd"]);
        const scenario = scenarioFor(fixture, String(vars["pool"]));
        const events = fixtureEvents(scenario, from);
        const hits = (rows: { amountUSD: string | null; transaction: { id: string } }[]) =>
          rows
            .filter((r) => r.amountUSD !== null && Number(r.amountUSD) >= Number(minUsd))
            .sort((a, b) => Number(b.amountUSD) - Number(a.amountUSD))
            .slice(0, 1)
            .map((r) => ({ transaction: r.transaction, amountUSD: r.amountUSD }));
        const hours = [];
        for (let t = from; t < to; t += 3600) {
          const i = (t - from) / 3600;
          hours.push({ periodStartUnix: t, tvlUSD: "4500000", volumeUSD: scenario === "quiet" ? "1500.25" : String(30000 + 250 * i), txCount: String(scenario === "quiet" ? 2 : 40 + i) });
        }
        return json({
          data: {
            start: snapshot("1000", "500"),
            end: snapshot(scenario === "quiet" ? "1010" : "1100", "500"),
            bundleStart: { ethPriceUSD: "2222.5" },
            bundleEnd: { ethPriceUSD: "2230" },
            hours,
            swap: hits(events.swaps),
            mint: hits(events.mints),
            burn: hits(events.burns),
            mintUnvalued: [],
            burnUnvalued: [],
            _meta: meta,
          },
        });
      }
      case "EventsSwaps":
      case "EventsMints":
      case "EventsBurns": {
        const from = Number(vars["from"]);
        const lastId = String(vars["lastId"]);
        const first = Number(vars["first"]);
        const events = fixtureEvents(scenarioFor(fixture, String(vars["poolStr"])), from);
        const entity = body.operationName === "EventsSwaps" ? "swaps" : body.operationName === "EventsMints" ? "mints" : "burns";
        const rows = events[entity].filter((r) => r.id > lastId).slice(0, first);
        return json({ data: { [entity]: rows, _meta: meta } });
      }
      default:
        return json({ errors: [{ message: `fixture has no handler for ${body.operationName}` }] });
    }
  };
}
