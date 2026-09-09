// The trust boundary. Everything here arrives from a stranger over HTTP,
// before any payment, so a request that should not be priced must be refused
// rather than quoted. A `RequestError` is a 400 with no 402 and no Graph
// query, which costs the seller nothing and the buyer nothing.
import assert from "node:assert/strict";
import { describe, test } from "node:test";

import { HOUR } from "./graph.js";
import { DEFAULT_CAP, MAX_CAP, RequestError, parseEvidenceRequest, parseExplainRequest } from "./requests.js";
import { KB, MAX_BRIEF_KB } from "./tariffs.js";

const POOL = "0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640";
const OTHER = "0x4585fe77225b41b697c938b018e2ac67ac5a20c0";
const FROM = 1_788_800_400;
const TO = FROM + 86_400;
const OPTS = { listing: "screen", maxPools: 20, hourAligned: true } as const;

function body(over: Record<string, unknown> = {}): unknown {
  return {
    pools: [POOL],
    window: { from: FROM, to: TO },
    inputs: { materiality: "0.05", min_event_usd: "100000" },
    ...over,
  };
}

describe("evidence requests", () => {
  test("a well formed request is normalised, not merely accepted", () => {
    const r = parseEvidenceRequest(body({ pools: [POOL.toUpperCase()] }), OPTS);
    assert.deepEqual(r.pools, [POOL], "addresses are lowercased for the fact ids");
    assert.deepEqual(r.window, { from: FROM, to: TO });
    assert.equal(r.cap, DEFAULT_CAP, "an absent cap takes the default");
  });

  test("a body that is not an object is refused before anything is priced", () => {
    for (const b of [null, undefined, 42, "pools", true, [], () => {}]) {
      assert.throws(() => parseEvidenceRequest(b, OPTS), RequestError, `${String(b)}`);
    }
  });

  test("pools must be distinct addresses within the tariff's maximum", () => {
    assert.throws(() => parseEvidenceRequest(body({ pools: [] }), OPTS), /non-empty/);
    assert.throws(() => parseEvidenceRequest(body({ pools: {} }), OPTS), /non-empty/);
    // A repeated pool would be charged twice and screened once.
    assert.throws(() => parseEvidenceRequest(body({ pools: [POOL, POOL] }), OPTS), /listed twice/);
    assert.throws(
      () => parseEvidenceRequest(body({ pools: [POOL, POOL.toUpperCase()] }), OPTS),
      /listed twice/,
      "case does not make a pool distinct",
    );
    // Anything that is not an address, including things that look close.
    for (const p of [
      "0x88e6a0c2ddd26feeb64f039a2c41296fcb3f564",
      "0x88e6a0c2ddd26feeb64f039a2c41296fcb3f56400",
      "88e6a0c2ddd26feeb64f039a2c41296fcb3f5640",
      "0xZZe6a0c2ddd26feeb64f039a2c41296fcb3f5640",
      "",
      42,
      null,
      { toString: () => POOL },
    ]) {
      assert.throws(() => parseEvidenceRequest(body({ pools: [p] }), OPTS), /not a pool address/);
    }
    // The tariff's ceiling is refused, never clamped: a larger request must
    // not be silently priced as a smaller one.
    const many = Array.from({ length: 21 }, (_, i) => `0x${String(i + 1).padStart(40, "0")}`);
    assert.throws(() => parseEvidenceRequest(body({ pools: many }), OPTS), /exceed max_units 20/);
  });

  test("the window must be a real interval, hour aligned where the listing says", () => {
    assert.throws(() => parseEvidenceRequest(body({ window: null }), OPTS), /window must be/);
    assert.throws(() => parseEvidenceRequest(body({ window: { from: FROM } }), OPTS), /window.to/);
    // Empty and inverted windows buy nothing and cannot be priced.
    assert.throws(() => parseEvidenceRequest(body({ window: { from: FROM, to: FROM } }), OPTS), /must be after/);
    assert.throws(() => parseEvidenceRequest(body({ window: { from: TO, to: FROM } }), OPTS), /must be after/);
    // Not integers, or not finite.
    for (const from of [1.5, -1, NaN, Infinity, "1788800400", null]) {
      assert.throws(() => parseEvidenceRequest(body({ window: { from, to: TO } }), OPTS), /window.from/);
    }
    // Alignment is required where the hourly rows must lie inside the window.
    assert.throws(() => parseEvidenceRequest(body({ window: { from: FROM + 1, to: TO } }), OPTS), /hour-aligned/);
    assert.throws(() => parseEvidenceRequest(body({ window: { from: FROM, to: TO + 1800 } }), OPTS), /hour-aligned/);
    // And not required where events are selected by timestamp.
    const events = parseEvidenceRequest(body({ window: { from: FROM + 1, to: TO } }), {
      ...OPTS,
      listing: "events",
      hourAligned: false,
    });
    assert.equal(events.window.from, FROM + 1);
    // A one hour window is the smallest aligned interval.
    const hour = parseEvidenceRequest(body({ window: { from: FROM, to: FROM + HOUR } }), OPTS);
    assert.equal(hour.window.to - hour.window.from, HOUR);
  });

  test("thresholds must be decimal strings, since they decide materiality", () => {
    for (const materiality of [0.05, null, "", "-0.05", "5%", "1e-2", "0.05 ", "abc", true]) {
      assert.throws(() => parseEvidenceRequest(body({ inputs: { materiality, min_event_usd: "1" } }), OPTS), /materiality/);
    }
    for (const min of [100000, null, "1,000", "$100", "-1"]) {
      assert.throws(
        () => parseEvidenceRequest(body({ inputs: { materiality: "0.05", min_event_usd: min } }), OPTS),
        /min_event_usd/,
      );
    }
    // Zero is a legitimate threshold: every event is then material.
    const all = parseEvidenceRequest(body({ inputs: { materiality: "0", min_event_usd: "0" } }), OPTS);
    assert.equal(all.inputs.materiality, "0");
  });

  test("the cap is bounded on both sides", () => {
    assert.equal(parseEvidenceRequest(body({ cap: 1 }), OPTS).cap, 1);
    assert.equal(parseEvidenceRequest(body({ cap: MAX_CAP }), OPTS).cap, MAX_CAP);
    assert.throws(() => parseEvidenceRequest(body({ cap: 0 }), OPTS), /between 1 and/);
    assert.throws(() => parseEvidenceRequest(body({ cap: MAX_CAP + 1 }), OPTS), /between 1 and/);
    assert.throws(() => parseEvidenceRequest(body({ cap: -1 }), OPTS), /cap/);
    assert.throws(() => parseEvidenceRequest(body({ cap: 1.5 }), OPTS), /cap/);
  });

  test("an unknown field is refused rather than ignored", () => {
    // A field the seller does not understand may be one the buyer thinks it
    // is paying for, so it is a 400 rather than a silent drop.
    assert.throws(() => parseEvidenceRequest(body({ extra: 1 }), OPTS), /unknown field extra/);
    assert.throws(() => parseEvidenceRequest(body({ POOLS: [POOL] }), OPTS), /unknown field POOLS/);
  });

  test("inherited properties do not satisfy a required field", () => {
    // A body whose prototype carries the fields must not pass as one that
    // has them: only own properties count.
    const sneaky = Object.create({ pools: [POOL], window: { from: FROM, to: TO }, inputs: { materiality: "0.05", min_event_usd: "1" } });
    assert.throws(() => parseEvidenceRequest(sneaky, OPTS), RequestError);
    // And an object whose prototype was replaced is no longer a plain body,
    // so it is refused rather than read through.
    const polluted = body();
    Object.setPrototypeOf(polluted as object, { cap: MAX_CAP });
    assert.throws(() => parseEvidenceRequest(polluted, OPTS), /must be a JSON object/);
    // A body with a null prototype is still plain, and its own fields count.
    const bare = Object.assign(Object.create(null), body());
    assert.equal(parseEvidenceRequest(bare, OPTS).cap, DEFAULT_CAP);
  });

  test("two pools cost two pools", () => {
    const r = parseEvidenceRequest(body({ pools: [POOL, OTHER] }), OPTS);
    assert.equal(r.pools.length, 2);
    assert.deepEqual(r.pools, [POOL, OTHER], "order is the buyer's, so fact ids match");
  });
});

describe("explain requests", () => {
  const brief = { o: [{ p: POOL, o: "supported" }] };

  test("a brief is accepted whole and nothing else is", () => {
    assert.deepEqual(parseExplainRequest({ brief }, 100).brief, brief);
    assert.throws(() => parseExplainRequest({}, 100), /brief object/);
    assert.throws(() => parseExplainRequest({ brief: "text" }, 100), /brief object/);
    assert.throws(() => parseExplainRequest({ brief: [1] }, 100), /brief object/);
    assert.throws(() => parseExplainRequest(null, 100), /brief object/);
    assert.throws(() => parseExplainRequest({ brief, extra: 1 }, 100), /unknown field extra/);
  });

  test("the size bound is the listing's, checked before the body is read", () => {
    const limit = MAX_BRIEF_KB * KB;
    assert.doesNotThrow(() => parseExplainRequest({ brief }, limit));
    assert.throws(() => parseExplainRequest({ brief }, limit + 1), /at most 8192/);
    // The bound is on the request as sent, so an empty brief still counts
    // whatever the transport carried.
    assert.throws(() => parseExplainRequest({ brief: {} }, limit + 1), /at most/);
  });
});
