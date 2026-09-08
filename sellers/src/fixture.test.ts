import assert from "node:assert/strict";
import { describe, test } from "node:test";

import { outcomesAndClaims } from "./claims.js";
import { DEPLOYMENT, fixtureFetch, parseFixture, txHash } from "./fixture.js";
import { GraphClient } from "./graph.js";

const POOL = "0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640";
const INPUTS = { materiality: "0.05", min_event_usd: "100000" };
const NOW = 1_788_900_000;
const WINDOW = { from: NOW - (NOW % 3600) - 86_400, to: NOW - (NOW % 3600) };

function client(name: "material" | "quiet"): GraphClient {
  return new GraphClient({ subgraphId: "FIXTURE", apiKey: "fixture", fetch: fixtureFetch(name, () => NOW) });
}

describe("fixture gateway", () => {
  test("material: the screen finds the pool material and the events carry 66-byte hashes", async () => {
    const screen = await client("material").screen([POOL], WINDOW, INPUTS);
    assert.equal(screen.deployment_id, DEPLOYMENT);
    assert.equal(screen.coverage_shortfall, false);
    assert.equal(screen.requests, 4);
    const pool = screen.pools[POOL]!;
    assert.deepEqual({ verdict: pool.verdict, reasons: pool.reasons }, { verdict: "material", reasons: ["large_event:swap", "large_event:mint", "tvl_change:token0"] });
    assert.equal(pool.hours.length, 24);
    assert.equal(pool.large_events.swap?.transaction.id, txHash(1));
    const events = await client("material").eventsProduct([POOL], WINDOW, 5000);
    const held = events.pools[POOL]!;
    assert.deepEqual(held.counts, { swap: 3, mint: 1, burn: 1 });
    assert.equal(held.truncated, false);
    assert.ok(held.swaps.every((s) => /^0x[0-9a-f]{64}$/.test(s.transaction.id)));
    const { outcomes, claims } = outcomesAndClaims(screen, events, "transaction", INPUTS.min_event_usd);
    assert.equal(outcomes[0]!.outcome, "supported");
    assert.equal(claims.length, 4);
  });

  test("quiet: nothing is material and the investigate bundle buys no events", async () => {
    const { screen, events } = await client("quiet").investigate([POOL], WINDOW, INPUTS, 5000);
    assert.equal(screen.pools[POOL]!.verdict, "non_material");
    assert.equal(events, null);
  });

  test("names are checked", () => {
    assert.equal(parseFixture(""), undefined);
    assert.equal(parseFixture("quiet"), "quiet");
    assert.throws(() => parseFixture("busy"), /unknown GRAPH_FIXTURE/);
  });
});
