// Live data from the Uniswap v3 subgraph on The Graph Network. Every
// response is a fresh query; only resends and retrievals are served from
// storage. The Graph is the load-bearing data source (The Graph track
// requirement): no Ethereum RPC is used by the sellers.

import { SUBGRAPH_ID } from "./tariffs.mjs";

const GRAPH_KEY = process.env.GRAPH_API_KEY;
if (!GRAPH_KEY) {
  console.error("[graph] GRAPH_API_KEY is not set; live queries will fail (the Graph gateway requires a Subgraph Studio key)");
}

const GATEWAY = `https://gateway.thegraph.com/api/${GRAPH_KEY}/subgraphs/id/${SUBGRAPH_ID}`;

export async function graphQuery(query, variables) {
  const res = await fetch(GATEWAY, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ query, variables }),
    signal: AbortSignal.timeout(30_000),
  });
  if (!res.ok) {
    throw new Error(`graph gateway ${res.status}: ${await res.text()}`);
  }
  const json = await res.json();
  if (json.errors?.length) {
    throw new Error(`graph errors: ${json.errors.map((e) => e.message).join("; ")}`);
  }
  return json.data;
}

// Latest indexed block and deployment id. `deployment` is present on the
// decentralized gateway; `block.timestamp` is in seconds.
export async function indexedBlock() {
  const data = await graphQuery(
    `{ _meta { deployment block { number timestamp } } }`,
    {},
  );
  return {
    deploymentId: data._meta.deployment ?? "unknown",
    number: Number(data._meta.block.number),
    timestamp: Number(data._meta.block.timestamp), // unix seconds
  };
}

// Pool entity at a block height. Fields come back as strings from the
// subgraph; we keep them as strings and convert where arithmetic is needed.
const POOL_FIELDS = `totalValueLockedToken0 totalValueLockedToken1 totalValueLockedUSD liquidity`;

async function poolAtBlock(address, blockNumber) {
  const data = await graphQuery(
    `query PoolAtBlock($id: ID!, $block: Int!) {
       pool(id: $id, block: { number: $block }) {
         ${POOL_FIELDS}
       }
     }`,
    { id: address.toLowerCase(), block: blockNumber },
  );
  return data.pool;
}

// One page of events of a kind in the window. Returns { items, hasMore }.
async function eventsPage(kind, pool, start, end, first, skip) {
  const common = `transaction { id } logIndex timestamp amount0 amount1 amountUSD origin`;
  const extra = kind === "swap" ? "" : ` owner tickLower tickUpper`;
  const data = await graphQuery(
    `query Events($pool: String!, $start: BigInt!, $end: BigInt!, $first: Int!, $skip: Int!) {
       ${kind}s(
         where: { pool: $pool, timestamp_gte: $start, timestamp_lte: $end },
         orderBy: timestamp, orderDirection: asc,
         first: $first, skip: $skip
       ) {
         ${common}${extra}
       }
     }`,
    { pool: address(pool), start: String(start), end: String(end), first, skip },
  );
  const items = data[`${kind}s`] ?? [];
  return { items, hasMore: items.length === first };
}

function address(pool) {
  return pool.toLowerCase();
}

// Screen one pool: TVL at the two block heights plus event counts and sums in
// the window. `blockEnd` is the last block at or before the window end.
export async function screenPool(pool, { blockStart, blockEnd, windowStart, windowEnd }) {
  const [start, end] = await Promise.all([
    poolAtBlock(address(pool), blockStart),
    poolAtBlock(address(pool), blockEnd),
  ]);

  const counts = { mint: 0, burn: 0, swap: 0 };
  const sums = { mint: 0, burn: 0, swap: 0 };
  const events = [];

  // Counts and sums for all three kinds, paginated. `truncated` is decided by
  // the caller from `hasMore`.
  let anyTruncated = false;
  for (const kind of ["mint", "burn", "swap"]) {
    let skip = 0;
    const first = 1000;
    for (;;) {
      const { items, hasMore } = await eventsPage(kind, pool, windowStart, windowEnd, first, skip);
      for (const e of items) {
        counts[kind] += 1;
        sums[kind] += Number(e.amountUSD) || 0;
      }
      anyTruncated = anyTruncated || hasMore;
      if (!hasMore) break;
      skip += first;
      if (skip >= 10_000) break; // hard stop; response is marked truncated
    }
  }

  return {
    address: pool,
    tvl_start: {
      token0: Number(start?.totalValueLockedToken0) || 0,
      token1: Number(start?.totalValueLockedToken1) || 0,
      usd: Number(start?.totalValueLockedUSD) || 0,
    },
    tvl_end: {
      token0: Number(end?.totalValueLockedToken0) || 0,
      token1: Number(end?.totalValueLockedToken1) || 0,
      usd: Number(end?.totalValueLockedUSD) || 0,
    },
    counts,
    sums,
    truncated: anyTruncated,
  };
}

// Event facts for one pool in the window (for transaction-level evidence).
export async function poolEvents(pool, { windowStart, windowEnd }) {
  const facts = [];
  let truncated = false;
  for (const kind of ["mint", "burn", "swap"]) {
    let skip = 0;
    const first = 1000;
    for (;;) {
      const { items, hasMore } = await eventsPage(kind, pool, windowStart, windowEnd, first, skip);
      for (const e of items) {
        facts.push(eventFact(kind, pool, e));
      }
      truncated = truncated || hasMore;
      if (!hasMore) break;
      skip += first;
      if (skip >= 10_000) break;
    }
  }
  facts.sort((a, b) => a.timestamp - b.timestamp);
  return { facts, truncated };
}

function eventFact(kind, pool, e) {
  return {
    kind,
    transaction_id: e.transaction?.id ?? "",
    log_index: Number(e.logIndex) || 0,
    timestamp: new Date(Number(e.timestamp) * 1000).toISOString(),
    amount0: Number(e.amount0) || 0,
    amount1: Number(e.amount1) || 0,
    amount_usd: Number(e.amountUSD) || 0,
    origin: e.origin ?? "",
    owner: e.owner ?? "",
    tick_lower: Number(e.tickLower) || 0,
    tick_upper: Number(e.tickUpper) || 0,
    fact_id: `evt:${pool.toLowerCase()}:${kind}:${e.transaction?.id}:${e.logIndex ?? 0}`,
  };
}

export const testable = { poolAtBlock, eventsPage };