import assert from "node:assert/strict";
import { describe, test } from "node:test";

import {
  GraphClient,
  type FetchLike,
  type PoolFacts,
  assertHourAligned,
  cmpDec,
  dec,
  decToString,
  materiality,
  productAtLeast,
  relativeChangeAtLeast,
} from "./graph.js";

interface Call {
  op: string;
  vars: Record<string, unknown>;
  auth: string | undefined;
  url: string;
  query: string;
}

type Handler = (vars: Record<string, unknown>) => unknown;

function fakeFetch(handlers: Record<string, Handler>): FetchLike & { calls: Call[] } {
  const calls: Call[] = [];
  const impl: FetchLike = async (url, init) => {
    const body = JSON.parse(String(init.body)) as { operationName: string; variables: Record<string, unknown>; query: string };
    const headers = init.headers as Record<string, string>;
    calls.push({ op: body.operationName, vars: body.variables, auth: headers["authorization"], url, query: body.query });
    const handler = handlers[body.operationName];
    if (!handler) return json({ errors: [{ message: `no handler for ${body.operationName}` }] });
    const out = handler(body.variables);
    if (out instanceof Response) return out;
    return json({ data: out });
  };
  return Object.assign(impl, { calls });
}

function json(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), { status, headers: { "content-type": "application/json" } });
}

const POOL = "0x88E6A0C2DDD26FEEB64F039A2C41296FCB3F5640";
const POOL_LC = POOL.toLowerCase();
const WINDOW = { from: 1_700_000_000 - (1_700_000_000 % 3600), to: 1_700_000_000 - (1_700_000_000 % 3600) + 86_400 };
const HEAD = 18_007_100;
const HEAD_TS = WINDOW.to + 120;
const DEPLOYMENT = "QmDeployment";

function snapshot(tvl0: string, tvl1: string, over: Partial<Record<string, unknown>> = {}) {
  return {
    totalValueLockedToken0: tvl0,
    totalValueLockedToken1: tvl1,
    totalValueLockedUSD: "1000000",
    liquidity: "123",
    token0: { derivedETH: "0.0005" },
    token1: { derivedETH: "1" },
    ...over,
  };
}

function screenRows(over: Record<string, unknown> = {}) {
  return {
    start: snapshot("1000", "500"),
    end: snapshot("1060", "500"),
    bundleStart: { ethPriceUSD: "2000" },
    bundleEnd: { ethPriceUSD: "2000" },
    hours: [
      { periodStartUnix: WINDOW.from, tvlUSD: "1000000", volumeUSD: "50000", txCount: "12" },
      { periodStartUnix: WINDOW.from + 3600, tvlUSD: "1001000", volumeUSD: "60000", txCount: "15" },
    ],
    swap: [],
    mint: [],
    burn: [],
    mintUnvalued: [],
    burnUnvalued: [],
    _meta: { deployment: DEPLOYMENT },
    ...over,
  };
}

function shared(overrides: Record<string, Handler> = {}): Record<string, Handler> {
  return {
    Meta: () => ({ _meta: { block: { number: HEAD, timestamp: HEAD_TS }, hasIndexingErrors: false, deployment: DEPLOYMENT } }),
    ObservationBlock: (vars) => {
      const ts = Number(vars["ts"]);
      return ts === WINDOW.from
        ? { transactions: [{ blockNumber: "18000000", timestamp: String(WINDOW.from - 7) }], _meta: { deployment: DEPLOYMENT } }
        : { transactions: [{ blockNumber: "18007000", timestamp: String(WINDOW.to - 11) }], _meta: { deployment: DEPLOYMENT } };
    },
    ScreenPool: () => screenRows(),
    ...overrides,
  };
}

const INPUTS = { materiality: "0.05", min_event_usd: "100000" };

function client(fetch: FetchLike): GraphClient {
  return new GraphClient({ subgraphId: "SUBGRAPH", apiKey: "KEY", fetch });
}

describe("decimals", () => {
  test("parses plain and exponent forms exactly", () => {
    assert.equal(decToString(dec("1.50")), "1.5");
    assert.equal(decToString(dec("1.5e-7")), "0.00000015");
    assert.equal(decToString(dec("2E3")), "2000");
    assert.equal(decToString(dec("-0.25")), "-0.25");
    assert.equal(cmpDec(dec("0.1"), dec("0.10")), 0);
    assert.equal(cmpDec(dec("100000000000000000000.000000000000000001"), dec("100000000000000000000")), 1);
  });

  test("relative change and product thresholds compare exactly", () => {
    assert.equal(relativeChangeAtLeast("100", "104.999999999999999999", "0.05"), false);
    assert.equal(relativeChangeAtLeast("100", "105", "0.05"), true);
    assert.equal(relativeChangeAtLeast("100", "95", "0.05"), true);
    assert.equal(productAtLeast("2000", "50", "100000"), true);
    assert.equal(productAtLeast("2000", "49.999999999999", "100000"), false);
  });
});

describe("window alignment", () => {
  test("accepts whole hours and rejects anything else", () => {
    assertHourAligned(WINDOW);
    assert.throws(() => assertHourAligned({ from: WINDOW.from + 1, to: WINDOW.to }), /hour-aligned/);
    assert.throws(() => assertHourAligned({ from: WINDOW.from, to: WINDOW.to + 1800 }), /hour-aligned/);
    assert.throws(() => assertHourAligned({ from: WINDOW.to, to: WINDOW.from }), /hour-aligned/);
  });
});

describe("materiality", () => {
  const base: PoolFacts = {
    start: { totalValueLockedToken0: "1000", totalValueLockedToken1: "500", totalValueLockedUSD: "1", liquidity: "1", token0PriceUSD: "1", token1PriceUSD: "2000" },
    end: { totalValueLockedToken0: "1040", totalValueLockedToken1: "500", totalValueLockedUSD: "1", liquidity: "1", token0PriceUSD: "1", token1PriceUSD: "2000" },
    absent_at_start: false,
    hours: [],
    large_events: { swap: null, mint: null, burn: null },
    unvalued_events: { mint: false, burn: false },
    truncated: false,
    coverage_shortfall: false,
  };

  test("below both thresholds is non_material", () => {
    assert.deepEqual(materiality(base, INPUTS), { verdict: "non_material", reasons: [] });
  });

  test("relative token change at the threshold is material", () => {
    const facts = { ...base, end: { ...base.end!, totalValueLockedToken0: "1050" } };
    assert.deepEqual(materiality(facts, INPUTS), { verdict: "material", reasons: ["tvl_change:token0"] });
  });

  test("a large event hit is material on its own", () => {
    const facts = { ...base, large_events: { swap: { transaction: { id: "0xabc" }, amountUSD: "250000" }, mint: null, burn: null } };
    assert.deepEqual(materiality(facts, INPUTS), { verdict: "material", reasons: ["large_event:swap"] });
  });

  test("zero starting tvl uses the usd value of the change, never a ratio", () => {
    const zeroStart = { ...base.start!, totalValueLockedToken1: "0" };
    const big = { ...base, start: zeroStart, end: { ...base.end!, totalValueLockedToken1: "60" } };
    assert.deepEqual(materiality(big, INPUTS), { verdict: "material", reasons: ["tvl_change:token1"] });
    const small = { ...base, start: zeroStart, end: { ...base.end!, totalValueLockedToken1: "40" } };
    assert.deepEqual(materiality(small, INPUTS), { verdict: "non_material", reasons: [] });
    const noPrice = { ...base, start: zeroStart, end: { ...base.end!, totalValueLockedToken1: "40", token1PriceUSD: null } };
    assert.deepEqual(materiality(noPrice, INPUTS), { verdict: "undetermined", reasons: ["undetermined:null_usd:token1"] });
  });

  test("a pool absent at block_start is undetermined, never a zero baseline", () => {
    const facts = { ...base, start: null, absent_at_start: true, end: { ...base.end!, totalValueLockedToken0: "60", totalValueLockedToken1: "60" } };
    assert.deepEqual(materiality(facts, INPUTS), { verdict: "undetermined", reasons: ["undetermined:absent_at_start"] });
    const withHit = { ...facts, large_events: { swap: null, mint: { transaction: { id: "0xmint" }, amountUSD: "500000" }, burn: null } };
    assert.equal(materiality(withHit, INPUTS).verdict, "material");
  });

  test("an unvalued mint or burn blocks non_material but not material", () => {
    const unvalued = { ...base, unvalued_events: { mint: true, burn: false } };
    assert.deepEqual(materiality(unvalued, INPUTS), { verdict: "undetermined", reasons: ["undetermined:unvalued_event:mint"] });
    const material = { ...unvalued, end: { ...base.end!, totalValueLockedToken0: "2000" } };
    assert.equal(materiality(material, INPUTS).verdict, "material");
  });

  test("absent at both blocks, truncated or short coverage is undetermined", () => {
    assert.deepEqual(materiality({ ...base, start: null, end: null }, INPUTS), { verdict: "undetermined", reasons: ["undetermined:pool_absent"] });
    assert.deepEqual(materiality({ ...base, truncated: true }, INPUTS), { verdict: "undetermined", reasons: ["undetermined:truncated"] });
    assert.deepEqual(materiality({ ...base, coverage_shortfall: true }, INPUTS), { verdict: "undetermined", reasons: ["undetermined:coverage_shortfall"] });
  });

  test("a material finding stands even when coverage is short", () => {
    const facts = { ...base, coverage_shortfall: true, end: { ...base.end!, totalValueLockedToken0: "2000" } };
    assert.equal(materiality(facts, INPUTS).verdict, "material");
  });
});

describe("GraphClient.screen", () => {
  test("assembles the section 7 response with one request per pool, every query pinned to the head", async () => {
    const fetch = fakeFetch(shared());
    const response = await client(fetch).screen([POOL], WINDOW, INPUTS);

    assert.equal(response.requests, 4);
    assert.deepEqual(fetch.calls.map((c) => c.op), ["Meta", "ObservationBlock", "ObservationBlock", "ScreenPool"]);
    assert.equal(fetch.calls[0]?.auth, "Bearer KEY");
    assert.equal(fetch.calls[0]?.url, "https://gateway.thegraph.com/api/subgraphs/id/SUBGRAPH");
    assert.deepEqual(fetch.calls.slice(1, 3).map((c) => c.vars["head"]), [HEAD, HEAD]);
    assert.match(fetch.calls[1]?.query ?? "", /block: \{ number: \$head \}/);
    assert.match(fetch.calls[3]?.query ?? "", /amountUSD: null/);

    assert.equal(response.deployment_id, DEPLOYMENT);
    assert.equal(response.block_start, 18_000_000);
    assert.equal(response.block_end, 18_007_000);
    assert.equal(response.block_end_timestamp, WINDOW.to - 11);
    assert.equal(response.indexed_block, HEAD);
    assert.equal(response.indexed_block_timestamp, HEAD_TS);
    assert.equal(response.indexing_errors, false);
    assert.deepEqual(response.window_requested, WINDOW);
    assert.deepEqual(response.window_covered, WINDOW);
    assert.equal(response.coverage_shortfall, false);
    assert.equal(response.truncated, false);

    const pool = response.pools[POOL_LC];
    assert.ok(pool);
    assert.equal(pool.verdict, "material");
    assert.deepEqual(pool.reasons, ["tvl_change:token0"]);
    assert.equal(pool.start?.totalValueLockedToken0, "1000");
    assert.equal(pool.end?.token0PriceUSD, "1");
    assert.equal(pool.end?.token1PriceUSD, "2000");
    assert.equal(pool.hours.length, 2);
    assert.deepEqual(pool.large_events, { swap: null, mint: null, burn: null });
    assert.deepEqual(pool.unvalued_events, { mint: false, burn: false });

    const vars = fetch.calls[3]?.vars;
    assert.equal(vars?.["pool"], POOL_LC);
    assert.equal(vars?.["startBlock"], 18_000_000);
    assert.equal(vars?.["endBlock"], 18_007_000);
    assert.equal(vars?.["head"], HEAD);
    assert.equal(vars?.["hourFrom"], WINDOW.from);
    assert.equal(vars?.["hourTo"], WINDOW.to);
    assert.equal(vars?.["from"], String(WINDOW.from));
    assert.equal(vars?.["minUsd"], "100000");
  });

  test("an unaligned window is rejected before any request", async () => {
    const fetch = fakeFetch(shared());
    await assert.rejects(client(fetch).screen([POOL], { from: WINDOW.from + 1800, to: WINDOW.to }, INPUTS), /hour-aligned/);
    assert.equal(fetch.calls.length, 0);
  });

  test("request count does not grow with pool activity", async () => {
    const busy = fakeFetch(
      shared({
        ScreenPool: () =>
          screenRows({
            end: snapshot("1000", "500"),
            hours: Array.from({ length: 24 }, (_, i) => ({ periodStartUnix: WINDOW.from + i * 3600, tvlUSD: "1", volumeUSD: "9999999", txCount: "50000" })),
            swap: [{ transaction: { id: "0xswap" }, amountUSD: "5000000" }],
            mint: [{ transaction: { id: "0xmint" }, amountUSD: "120000" }],
          }),
      }),
    );
    const response = await client(busy).screen([POOL, "0x" + "1".repeat(40)], WINDOW, INPUTS);
    assert.equal(response.requests, 5);
    const pool = response.pools[POOL_LC];
    assert.equal(pool?.verdict, "material");
    assert.deepEqual(pool?.reasons, ["large_event:swap", "large_event:mint"]);
    assert.equal(pool?.large_events.swap?.transaction.id, "0xswap");
  });

  test("coverage falls short when the head has not reached the window end", async () => {
    const lagging = fakeFetch(
      shared({
        Meta: () => ({ _meta: { block: { number: HEAD, timestamp: WINDOW.to - 600 }, hasIndexingErrors: false, deployment: DEPLOYMENT } }),
        ScreenPool: () => screenRows({ end: snapshot("1000", "500") }),
      }),
    );
    const response = await client(lagging).screen([POOL], WINDOW, INPUTS);
    assert.equal(response.coverage_shortfall, true);
    assert.deepEqual(response.window_covered, { from: WINDOW.from, to: WINDOW.to - 600 });
    assert.deepEqual(response.pools[POOL_LC]?.verdict, "undetermined");
    assert.deepEqual(response.pools[POOL_LC]?.reasons, ["undetermined:coverage_shortfall"]);
  });

  test("an old observation block is exact, not a shortfall, once the head is past the boundary", async () => {
    const quiet = fakeFetch(
      shared({
        ObservationBlock: (vars) =>
          Number(vars["ts"]) === WINDOW.from
            ? { transactions: [{ blockNumber: "17990000", timestamp: String(WINDOW.from - 7200) }], _meta: { deployment: DEPLOYMENT } }
            : { transactions: [{ blockNumber: "18001000", timestamp: String(WINDOW.to - 5400) }], _meta: { deployment: DEPLOYMENT } },
        ScreenPool: () => screenRows({ end: snapshot("1000", "500") }),
      }),
    );
    const response = await client(quiet).screen([POOL], WINDOW, INPUTS);
    assert.equal(response.coverage_shortfall, false);
    assert.deepEqual(response.window_covered, WINDOW);
    assert.equal(response.block_end_timestamp, WINDOW.to - 5400);
    assert.equal(response.pools[POOL_LC]?.verdict, "non_material");
  });

  test("an observation block beyond the pinned head is an error", async () => {
    const racing = fakeFetch(
      shared({
        ObservationBlock: () => ({ transactions: [{ blockNumber: String(HEAD + 10), timestamp: String(WINDOW.to - 5) }], _meta: { deployment: DEPLOYMENT } }),
      }),
    );
    await assert.rejects(client(racing).screen([POOL], WINDOW, INPUTS), /beyond the pinned head/);
  });

  test("a different deployment mid-response marks the pool undetermined", async () => {
    const drifted = fakeFetch(shared({ ScreenPool: () => screenRows({ _meta: { deployment: "QmOther" } }) }));
    const pool = (await client(drifted).screen([POOL], WINDOW, INPUTS)).pools[POOL_LC];
    assert.equal(pool?.verdict, "undetermined");
    assert.deepEqual(pool?.reasons, ["undetermined:graph_error"]);
    assert.match(pool?.error ?? "", /deployment QmOther differs/);
  });

  test("a pool absent at block_start is undetermined and flagged", async () => {
    const created = fakeFetch(shared({ ScreenPool: () => screenRows({ start: null, end: snapshot("10", "0") }) }));
    const pool = (await client(created).screen([POOL], WINDOW, INPUTS)).pools[POOL_LC];
    assert.equal(pool?.absent_at_start, true);
    assert.equal(pool?.start, null);
    assert.equal(pool?.verdict, "undetermined");
    assert.deepEqual(pool?.reasons, ["undetermined:absent_at_start"]);
  });

  test("an unvalued mint in the window leaves the large-event check incomplete", async () => {
    const unvalued = fakeFetch(shared({ ScreenPool: () => screenRows({ end: snapshot("1000", "500"), mintUnvalued: [{ id: "m1" }] }) }));
    const pool = (await client(unvalued).screen([POOL], WINDOW, INPUTS)).pools[POOL_LC];
    assert.deepEqual(pool?.unvalued_events, { mint: true, burn: false });
    assert.equal(pool?.verdict, "undetermined");
    assert.deepEqual(pool?.reasons, ["undetermined:unvalued_event:mint"]);
  });

  test("graphql errors and http failures for a pool mark it undetermined", async () => {
    const failing = fakeFetch(shared({ ScreenPool: () => json({ errors: [{ message: "indexer has no block 18000000" }] }) }));
    const pool = (await client(failing).screen([POOL], WINDOW, INPUTS)).pools[POOL_LC];
    assert.equal(pool?.verdict, "undetermined");
    assert.deepEqual(pool?.reasons, ["undetermined:graph_error"]);
    assert.match(pool?.error ?? "", /indexer has no block/);

    const down = fakeFetch(shared({ ScreenPool: () => json({ message: "bad gateway" }, 502) }));
    const pool2 = (await client(down).screen([POOL], WINDOW, INPUTS)).pools[POOL_LC];
    assert.equal(pool2?.verdict, "undetermined");
    assert.match(pool2?.error ?? "", /502/);
  });

  test("a shared query failure fails the whole screen", async () => {
    const noMeta = fakeFetch(shared({ Meta: () => json({ errors: [{ message: "boom" }] }) }));
    await assert.rejects(client(noMeta).screen([POOL], WINDOW, INPUTS), /boom/);
  });
});

describe("GraphClient.eventsProduct", () => {
  function pages(total: number): Record<string, Handler> {
    return {
      EventsSwaps: (vars) => {
        const first = Number(vars["first"]);
        const lastId = String(vars["lastId"]);
        const offset = lastId === "" ? 0 : Number(lastId.slice(1));
        const count = Math.max(0, Math.min(first, total - offset));
        return {
          swaps: Array.from({ length: count }, (_, i) => ({
            id: `s${String(offset + i + 1).padStart(6, "0")}`,
            transaction: { id: `0xtx${offset + i + 1}` },
            logIndex: offset + i === 1 ? null : String(i),
            timestamp: String(WINDOW.from + offset + i),
            amount0: "1",
            amount1: "-2",
            amountUSD: offset + i === 3 ? null : "10",
            origin: "0xorigin",
          })),
          _meta: { deployment: DEPLOYMENT },
        };
      },
      EventsMints: () => ({
        mints: [
          {
            id: "m000001",
            transaction: { id: "0xmint" },
            logIndex: "7",
            timestamp: String(WINDOW.from + 5),
            amount0: "3",
            amount1: "4",
            amountUSD: "120000",
            origin: "0xorigin",
            owner: "0xowner",
            tickLower: "-100",
            tickUpper: "100",
          },
        ],
        _meta: { deployment: DEPLOYMENT },
      }),
      EventsBurns: () => ({
        burns: [
          {
            id: "b000001",
            transaction: { id: "0xburn" },
            logIndex: null,
            timestamp: String(WINDOW.from + 9),
            amount0: "1",
            amount1: "1",
            amountUSD: null,
            origin: "0xorigin",
            owner: null,
            tickLower: "-10",
            tickUpper: "10",
          },
        ],
        _meta: { deployment: DEPLOYMENT },
      }),
    };
  }

  test("paginates by id to completion with every page pinned to the head block", async () => {
    const fetch = fakeFetch({ ...shared(), ...pages(2005) });
    const response = await client(fetch).eventsProduct([POOL], WINDOW, 10_000);
    const pool = response.pools[POOL_LC];
    assert.ok(pool);
    assert.equal(pool.swaps.length, 2005);
    assert.equal(pool.mints.length, 1);
    assert.equal(pool.burns.length, 1);
    assert.equal(pool.truncated, false);
    assert.equal(response.truncated, false);
    assert.deepEqual(pool.counts, { swap: 2005, mint: 1, burn: 1 });
    assert.deepEqual(pool.sum_amount_usd, { swap: "20040", mint: "120000", burn: "0" });
    assert.equal(pool.amount_usd_nulls, 2);
    assert.equal(pool.mints[0]?.tickLower, -100);
    assert.equal(pool.swaps[0]?.transaction.id, "0xtx1");

    const eventCalls = fetch.calls.filter((c) => c.op.startsWith("Events"));
    assert.deepEqual(eventCalls.map((c) => c.op), ["EventsSwaps", "EventsSwaps", "EventsSwaps", "EventsMints", "EventsBurns"]);
    assert.deepEqual(eventCalls.slice(0, 3).map((c) => c.vars["lastId"]), ["", "s001000", "s002000"]);
    assert.ok(eventCalls.every((c) => c.vars["block"] === HEAD));
    assert.ok(eventCalls.every((c) => c.vars["first"] === 1000));
    assert.equal(response.requests, 3 + 5);
  });

  test("null log indices and burn owners are preserved, never zero", async () => {
    const fetch = fakeFetch({ ...shared(), ...pages(3) });
    const pool = (await client(fetch).eventsProduct([POOL], WINDOW, 100)).pools[POOL_LC];
    assert.equal(pool?.swaps[0]?.logIndex, 0);
    assert.equal(pool?.swaps[1]?.logIndex, null);
    assert.equal(pool?.burns[0]?.logIndex, null);
    assert.equal(pool?.burns[0]?.owner, null);
    assert.equal(pool?.burns[0]?.amountUSD, null);
    assert.equal(pool?.mints[0]?.owner, "0xowner");
  });

  test("a deployment change between pages fails the product", async () => {
    const fetch = fakeFetch({ ...shared(), ...pages(3), EventsMints: () => ({ mints: [], _meta: { deployment: "QmOther" } }) });
    await assert.rejects(client(fetch).eventsProduct([POOL], WINDOW, 100), /deployment QmOther differs/);
  });

  test("stops at the cap and reports truncated", async () => {
    const fetch = fakeFetch({ ...shared(), ...pages(2005) });
    const response = await client(fetch).eventsProduct([POOL], WINDOW, 1500);
    const pool = response.pools[POOL_LC];
    assert.equal(pool?.swaps.length, 1500);
    assert.equal(pool?.truncated, true);
    assert.equal(response.truncated, true);
    assert.equal(response.cap, 1500);
    const eventCalls = fetch.calls.filter((c) => c.op.startsWith("Events"));
    assert.deepEqual(eventCalls.map((c) => c.vars["first"]), [1000, 500]);
    assert.ok(!eventCalls.some((c) => c.op === "EventsMints"));
  });

  test("an exactly full last page below the cap is not truncated", async () => {
    const fetch = fakeFetch({ ...shared(), ...pages(1000) });
    const response = await client(fetch).eventsProduct([POOL], WINDOW, 5000);
    assert.equal(response.pools[POOL_LC]?.swaps.length, 1000);
    assert.equal(response.pools[POOL_LC]?.truncated, false);
    const swapCalls = fetch.calls.filter((c) => c.op === "EventsSwaps");
    assert.equal(swapCalls.length, 2);
  });
});
