// Gateway queries against the Uniswap v3 subgraph. Issue #4, spec section 7.
//
// Facts are stated at observation blocks: the last block at or before each
// window end in which subgraph state changed, read from the `transactions`
// entity at the indexed head captured first. Every query of one product is
// pinned to that head and checked against its deployment. Coverage is a
// property of the head, not of observation-block age: the state at a boundary
// equals the state at its observation block whenever the head is past the
// boundary. Screen windows are hour-aligned so hourly rows lie inside them.
// The screen makes one request per pool plus three shared requests, whatever
// the pool's activity. Missing or null data yields `undetermined`, never a
// zero. Numeric amounts stay strings; comparisons are exact decimals.

export const DEFAULT_GATEWAY_URL = "https://gateway.thegraph.com/api/subgraphs/id";
export const HOUR = 3600;
export const PAGE_SIZE = 1000;

export type FetchLike = (input: string, init: RequestInit) => Promise<Response>;

export interface GraphClientConfig {
  subgraphId: string;
  apiKey: string;
  fetch?: FetchLike;
  gatewayUrl?: string;
}

export type GraphErrorKind = "http" | "graphql" | "null" | "window" | "deployment";

export class GraphError extends Error {
  readonly kind: GraphErrorKind;
  constructor(kind: GraphErrorKind, message: string) {
    super(message);
    this.name = "GraphError";
    this.kind = kind;
  }
}

// Exact decimals. value = m / 10^e.

export interface Dec {
  m: bigint;
  e: number;
}

const DECIMAL = /^([+-])?(\d+)(?:\.(\d+))?(?:[eE]([+-]?\d+))?$/;

export function dec(s: string): Dec {
  const match = DECIMAL.exec(s.trim());
  if (!match) throw new Error(`not a decimal: ${s}`);
  const sign = match[1] === "-" ? -1n : 1n;
  const int = match[2] ?? "0";
  const frac = match[3] ?? "";
  const exp = match[4] ? Number(match[4]) : 0;
  let m = BigInt(int + frac) * sign;
  let e = frac.length - exp;
  if (e < 0) {
    m *= 10n ** BigInt(-e);
    e = 0;
  }
  return { m, e };
}

function aligned(a: Dec, b: Dec): [bigint, bigint, number] {
  const e = Math.max(a.e, b.e);
  return [a.m * 10n ** BigInt(e - a.e), b.m * 10n ** BigInt(e - b.e), e];
}

export function cmpDec(a: Dec, b: Dec): -1 | 0 | 1 {
  const [x, y] = aligned(a, b);
  return x < y ? -1 : x > y ? 1 : 0;
}

export function subDec(a: Dec, b: Dec): Dec {
  const [x, y, e] = aligned(a, b);
  return { m: x - y, e };
}

export function addDec(a: Dec, b: Dec): Dec {
  const [x, y, e] = aligned(a, b);
  return { m: x + y, e };
}

export function mulDec(a: Dec, b: Dec): Dec {
  return { m: a.m * b.m, e: a.e + b.e };
}

export function absDec(a: Dec): Dec {
  return { m: a.m < 0n ? -a.m : a.m, e: a.e };
}

export function isZeroDec(a: Dec): boolean {
  return a.m === 0n;
}

export function decToString(d: Dec): string {
  const neg = d.m < 0n;
  let digits = (neg ? -d.m : d.m).toString();
  if (d.e === 0) return (neg ? "-" : "") + digits;
  if (digits.length <= d.e) digits = "0".repeat(d.e - digits.length + 1) + digits;
  const cut = digits.length - d.e;
  let out = `${digits.slice(0, cut)}.${digits.slice(cut)}`;
  out = out.replace(/\.?0+$/, "");
  return (neg ? "-" : "") + out;
}

/** |end - start| >= threshold * start. Requires start > 0. */
export function relativeChangeAtLeast(start: string, end: string, threshold: string): boolean {
  const s = dec(start);
  const change = absDec(subDec(dec(end), s));
  return cmpDec(change, mulDec(dec(threshold), s)) >= 0;
}

/** a * b >= threshold. */
export function productAtLeast(a: string, b: string, threshold: string): boolean {
  return cmpDec(mulDec(dec(a), dec(b)), dec(threshold)) >= 0;
}

// Types. Amounts are decimal strings as the subgraph returns them.

export interface Window {
  from: number;
  to: number;
}

/** Screen windows are hour-aligned, so `poolHourDatas` rows lie inside them. */
export function assertHourAligned(window: Window): void {
  if (!(window.to > window.from) || window.from % HOUR !== 0 || window.to % HOUR !== 0) {
    throw new GraphError("window", `screen windows must be hour-aligned and non-empty: ${window.from} to ${window.to}`);
  }
}

export interface Inputs {
  materiality: string;
  min_event_usd: string;
}

export interface ObservationBlock {
  number: number;
  timestamp: number;
}

export interface Meta {
  block: { number: number; timestamp: number | null };
  hasIndexingErrors: boolean;
  deployment: string;
}

export interface PoolSnapshot {
  totalValueLockedToken0: string;
  totalValueLockedToken1: string;
  totalValueLockedUSD: string | null;
  liquidity: string;
  /** derivedETH times the bundle's ethPriceUSD at the same block; null when either is null. */
  token0PriceUSD: string | null;
  token1PriceUSD: string | null;
}

export interface HourRow {
  periodStartUnix: number;
  tvlUSD: string;
  volumeUSD: string;
  txCount: string;
}

export interface EventHit {
  transaction: { id: string };
  amountUSD: string;
}

export interface LargeEventHits {
  swap: EventHit | null;
  mint: EventHit | null;
  burn: EventHit | null;
}

export type Verdict = "material" | "non_material" | "undetermined";

export interface MaterialityResult {
  verdict: Verdict;
  reasons: string[];
}

/** Event types whose `amountUSD` the schema allows to be null. */
export interface UnvaluedEvents {
  mint: boolean;
  burn: boolean;
}

export interface PoolFacts {
  start: PoolSnapshot | null;
  end: PoolSnapshot | null;
  /** The pool entity is absent at block_start. Not a zero: the pool is undetermined until events are held. */
  absent_at_start: boolean;
  hours: HourRow[];
  large_events: LargeEventHits;
  /** An event of that type in the window has a null `amountUSD`, so the large-event check is incomplete. */
  unvalued_events: UnvaluedEvents;
  truncated: boolean;
  coverage_shortfall: boolean;
}

export interface PoolScreen extends PoolFacts {
  verdict: Verdict;
  reasons: string[];
  error?: string;
}

export interface ResponseHeader {
  deployment_id: string;
  block_start: number;
  block_end: number;
  block_end_timestamp: number;
  indexed_block: number;
  indexed_block_timestamp: number | null;
  indexing_errors: boolean;
  window_requested: Window;
  window_covered: Window;
  coverage_shortfall: boolean;
  truncated: boolean;
}

export interface ScreenResponse extends ResponseHeader {
  pools: Record<string, PoolScreen>;
  requests: number;
}

export interface SwapEvent {
  id: string;
  transaction: { id: string };
  /** Null when the subgraph has none; a citation then rests on the transaction hash alone. */
  logIndex: number | null;
  timestamp: number;
  amount0: string;
  amount1: string;
  amountUSD: string | null;
  origin: string;
}

export interface LiquidityEvent extends SwapEvent {
  /** Null for burns without an owner in the subgraph. */
  owner: string | null;
  tickLower: number;
  tickUpper: number;
}

export type EventKind = "swap" | "mint" | "burn";
export const EVENT_KINDS: readonly EventKind[] = ["swap", "mint", "burn"];

export interface PoolEvents {
  swaps: SwapEvent[];
  mints: LiquidityEvent[];
  burns: LiquidityEvent[];
  counts: Record<EventKind, number>;
  sum_amount_usd: Record<EventKind, string>;
  amount_usd_nulls: number;
  truncated: boolean;
}

/** What every query of one product or bundle shares. */
export interface Context {
  window: Window;
  header: ResponseHeader;
  head: number;
  deployment: string;
  /** `requests` at the start of what this context counts; the shared queries count toward the first product. */
  requestsBefore: number;
}

/** The investigate bundle's facts: one context for both parts. */
export interface Investigation {
  screen: ScreenResponse;
  events: EventsResponse | null;
}

export interface EventsResponse extends ResponseHeader {
  cap: number;
  pools: Record<string, PoolEvents>;
  requests: number;
}

// Materiality, spec section 7. Material reasons win; undetermined reasons
// only block a non_material verdict.

export function materiality(facts: PoolFacts, inputs: Inputs): MaterialityResult {
  const reasons: string[] = [];
  if (facts.truncated) reasons.push("undetermined:truncated");
  if (facts.coverage_shortfall) reasons.push("undetermined:coverage_shortfall");
  for (const kind of EVENT_KINDS) {
    if (facts.large_events[kind]) reasons.push(`large_event:${kind}`);
  }
  for (const kind of ["mint", "burn"] as const) {
    if (facts.unvalued_events[kind]) reasons.push(`undetermined:unvalued_event:${kind}`);
  }
  const { start, end } = facts;
  if (!end) {
    reasons.push("undetermined:pool_absent");
  } else if (!start) {
    reasons.push("undetermined:absent_at_start");
  } else {
    for (const i of [0, 1] as const) {
      const s = tvlOf(start, i);
      const e = tvlOf(end, i);
      if (isZeroDec(dec(s))) {
        if (isZeroDec(dec(e))) continue;
        const price = i === 0 ? end.token0PriceUSD : end.token1PriceUSD;
        if (price === null) {
          reasons.push(`undetermined:null_usd:token${i}`);
        } else if (productAtLeast(e, price, inputs.min_event_usd)) {
          reasons.push(`tvl_change:token${i}`);
        }
      } else if (relativeChangeAtLeast(s, e, inputs.materiality)) {
        reasons.push(`tvl_change:token${i}`);
      }
    }
  }
  if (reasons.some((r) => r.startsWith("tvl_change:") || r.startsWith("large_event:"))) {
    return { verdict: "material", reasons };
  }
  if (reasons.length > 0) return { verdict: "undetermined", reasons };
  return { verdict: "non_material", reasons };
}

function tvlOf(snap: PoolSnapshot, i: 0 | 1): string {
  return i === 0 ? snap.totalValueLockedToken0 : snap.totalValueLockedToken1;
}

// Documents. Timestamps and block numbers are BigInt in this schema and
// travel as strings; block heights in `block: { number }` are Int.

const META_FIELDS = `_meta { deployment }`;

const OBSERVATION_BLOCK = `query ObservationBlock($ts: BigInt!, $head: Int!) {
  transactions(first: 1, orderBy: timestamp, orderDirection: desc, block: { number: $head }, where: { timestamp_lte: $ts }) {
    blockNumber
    timestamp
  }
  ${META_FIELDS}
}`;

const META = `query Meta {
  _meta {
    block { number timestamp }
    hasIndexingErrors
    deployment
  }
}`;

const SNAPSHOT_FIELDS = `totalValueLockedToken0 totalValueLockedToken1 totalValueLockedUSD liquidity token0 { derivedETH } token1 { derivedETH }`;
const HIT_WHERE = `where: { pool: $poolStr, timestamp_gte: $from, timestamp_lt: $to, amountUSD_gte: $minUsd }`;
const UNVALUED_WHERE = `where: { pool: $poolStr, timestamp_gte: $from, timestamp_lt: $to, amountUSD: null }`;

const SCREEN_POOL = `query ScreenPool($pool: ID!, $poolStr: String!, $startBlock: Int!, $endBlock: Int!, $head: Int!, $hourFrom: Int!, $hourTo: Int!, $from: BigInt!, $to: BigInt!, $minUsd: BigDecimal!) {
  start: pool(id: $pool, block: { number: $startBlock }) { ${SNAPSHOT_FIELDS} }
  end: pool(id: $pool, block: { number: $endBlock }) { ${SNAPSHOT_FIELDS} }
  bundleStart: bundle(id: "1", block: { number: $startBlock }) { ethPriceUSD }
  bundleEnd: bundle(id: "1", block: { number: $endBlock }) { ethPriceUSD }
  hours: poolHourDatas(first: ${PAGE_SIZE}, orderBy: periodStartUnix, orderDirection: asc, block: { number: $head }, where: { pool: $poolStr, periodStartUnix_gte: $hourFrom, periodStartUnix_lt: $hourTo }) {
    periodStartUnix tvlUSD volumeUSD txCount
  }
  swap: swaps(first: 1, orderBy: amountUSD, orderDirection: desc, block: { number: $head }, ${HIT_WHERE}) { transaction { id } amountUSD }
  mint: mints(first: 1, orderBy: amountUSD, orderDirection: desc, block: { number: $head }, ${HIT_WHERE}) { transaction { id } amountUSD }
  burn: burns(first: 1, orderBy: amountUSD, orderDirection: desc, block: { number: $head }, ${HIT_WHERE}) { transaction { id } amountUSD }
  mintUnvalued: mints(first: 1, block: { number: $head }, ${UNVALUED_WHERE}) { id }
  burnUnvalued: burns(first: 1, block: { number: $head }, ${UNVALUED_WHERE}) { id }
  ${META_FIELDS}
}`;

const SWAP_FIELDS = `id transaction { id } logIndex timestamp amount0 amount1 amountUSD origin`;
const LIQUIDITY_FIELDS = `${SWAP_FIELDS} owner tickLower tickUpper`;

function eventsDocument(entity: "swaps" | "mints" | "burns", operation: string, fields: string): string {
  return `query ${operation}($poolStr: String!, $from: BigInt!, $to: BigInt!, $lastId: ID!, $first: Int!, $block: Int!) {
  ${entity}(first: $first, orderBy: id, orderDirection: asc, block: { number: $block }, where: { pool: $poolStr, timestamp_gte: $from, timestamp_lt: $to, id_gt: $lastId }) { ${fields} }
  ${META_FIELDS}
}`;
}

const EVENT_DOCS: Record<EventKind, { entity: "swaps" | "mints" | "burns"; operation: string; document: string }> = {
  swap: { entity: "swaps", operation: "EventsSwaps", document: eventsDocument("swaps", "EventsSwaps", SWAP_FIELDS) },
  mint: { entity: "mints", operation: "EventsMints", document: eventsDocument("mints", "EventsMints", LIQUIDITY_FIELDS) },
  burn: { entity: "burns", operation: "EventsBurns", document: eventsDocument("burns", "EventsBurns", LIQUIDITY_FIELDS) },
};

// Raw shapes as the gateway returns them.

interface RawSnapshot {
  totalValueLockedToken0: string;
  totalValueLockedToken1: string;
  totalValueLockedUSD: string | null;
  liquidity: string;
  token0: { derivedETH: string | null } | null;
  token1: { derivedETH: string | null } | null;
}

interface RawScreenPool {
  start: RawSnapshot | null;
  end: RawSnapshot | null;
  bundleStart: { ethPriceUSD: string | null } | null;
  bundleEnd: { ethPriceUSD: string | null } | null;
  hours: { periodStartUnix: number; tvlUSD: string; volumeUSD: string; txCount: string }[];
  swap: EventHit[];
  mint: EventHit[];
  burn: EventHit[];
  mintUnvalued?: { id: string }[];
  burnUnvalued?: { id: string }[];
}

interface RawSwap {
  id: string;
  transaction: { id: string };
  logIndex: string | null;
  timestamp: string;
  amount0: string;
  amount1: string;
  amountUSD: string | null;
  origin: string;
}

interface RawLiquidity extends RawSwap {
  owner: string | null;
  tickLower: string;
  tickUpper: string;
}

export class GraphClient {
  readonly subgraphId: string;
  readonly gatewayUrl: string;
  private readonly apiKey: string;
  private readonly fetchImpl: FetchLike;
  /** Requests sent since construction or the last reset. */
  requests = 0;

  constructor(config: GraphClientConfig) {
    this.subgraphId = config.subgraphId;
    this.apiKey = config.apiKey;
    this.gatewayUrl = config.gatewayUrl ?? DEFAULT_GATEWAY_URL;
    this.fetchImpl = config.fetch ?? ((input, init) => fetch(input, init));
  }

  /**
   * One gateway request. When `expectDeployment` is given and the response
   * carries `_meta`, a different deployment is an error: every query of one
   * product must read the same indexed deployment.
   */
  async query<T>(document: string, variables: Record<string, unknown>, operationName: string, expectDeployment?: string): Promise<T> {
    this.requests += 1;
    const response = await this.fetchImpl(`${this.gatewayUrl}/${this.subgraphId}`, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${this.apiKey}` },
      body: JSON.stringify({ query: document, variables, operationName }),
    });
    if (!response.ok) throw new GraphError("http", `${operationName}: gateway returned ${response.status}`);
    const json = (await response.json()) as { data?: T | null; errors?: { message: string }[] };
    if (json.errors && json.errors.length > 0) {
      throw new GraphError("graphql", `${operationName}: ${json.errors.map((e) => e.message).join("; ")}`);
    }
    if (json.data === null || json.data === undefined) throw new GraphError("null", `${operationName}: no data`);
    if (expectDeployment !== undefined) {
      const seen = (json.data as { _meta?: { deployment?: string } | null })._meta?.deployment;
      if (seen !== undefined && seen !== expectDeployment) {
        throw new GraphError("deployment", `${operationName}: deployment ${seen} differs from ${expectDeployment}`);
      }
    }
    return json.data;
  }

  async meta(): Promise<Meta> {
    const data = await this.query<{ _meta: { block: { number: number; timestamp: number | null }; hasIndexingErrors: boolean; deployment: string } | null }>(META, {}, "Meta");
    if (!data._meta) throw new GraphError("null", "Meta: _meta is null");
    return {
      block: { number: Number(data._meta.block.number), timestamp: data._meta.block.timestamp === null ? null : Number(data._meta.block.timestamp) },
      hasIndexingErrors: data._meta.hasIndexingErrors,
      deployment: data._meta.deployment,
    };
  }

  /**
   * Last block at or before `ts` in which subgraph state changed, as seen at
   * `head`; null before the first transaction.
   */
  async observationBlock(ts: number, head: number, deployment?: string): Promise<ObservationBlock | null> {
    const data = await this.query<{ transactions: { blockNumber: string; timestamp: string }[] }>(
      OBSERVATION_BLOCK,
      { ts: String(ts), head },
      "ObservationBlock",
      deployment,
    );
    const row = data.transactions[0];
    if (!row) return null;
    const block = { number: Number(row.blockNumber), timestamp: Number(row.timestamp) };
    if (block.number > head) throw new GraphError("graphql", `ObservationBlock: block ${block.number} is beyond the pinned head ${head}`);
    return block;
  }

  /**
   * Shared header: the head first, then both observation blocks pinned to it.
   * Coverage falls short when the head has not reached the window end; an
   * observation block older than its boundary is exact, not stale, because
   * nothing in the subgraph changed in between.
   */
  private async header(window: Window): Promise<{ header: ResponseHeader; head: number; deployment: string }> {
    const meta = await this.meta();
    const head = meta.block.number;
    const start = await this.observationBlock(window.from, head, meta.deployment);
    const end = await this.observationBlock(window.to, head, meta.deployment);
    if (!start || !end) throw new GraphError("null", "no observation block at or before the window");
    const headTs = meta.block.timestamp;
    const shortfall = headTs === null || headTs < window.to;
    return {
      head,
      deployment: meta.deployment,
      header: {
        deployment_id: meta.deployment,
        block_start: start.number,
        block_end: end.number,
        block_end_timestamp: end.timestamp,
        indexed_block: head,
        indexed_block_timestamp: headTs,
        indexing_errors: meta.hasIndexingErrors,
        window_requested: { from: window.from, to: window.to },
        window_covered: { from: window.from, to: shortfall ? Math.min(window.to, headTs ?? end.timestamp) : window.to },
        coverage_shortfall: shortfall,
        truncated: false,
      },
    };
  }

  /** One pinned context: the header, head and deployment every query of a product or bundle shares. */
  async context(window: Window): Promise<Context> {
    const requestsBefore = this.requests;
    const { header, head, deployment } = await this.header(window);
    return { window, header, head, deployment, requestsBefore };
  }

  /** Screening product: three shared requests plus one per pool. The window must be hour-aligned. */
  async screen(pools: string[], window: Window, inputs: Inputs): Promise<ScreenResponse> {
    assertHourAligned(window);
    return this.screenWith(await this.context(window), pools, inputs);
  }

  /** The screen inside an existing context. */
  async screenWith(ctx: Context, pools: string[], inputs: Inputs): Promise<ScreenResponse> {
    const { header, head, deployment, window } = ctx;
    const before = ctx.requestsBefore;
    const out: Record<string, PoolScreen> = {};
    let anyTruncated = false;
    for (const raw of pools) {
      const pool = raw.toLowerCase();
      try {
        const data = await this.query<RawScreenPool>(
          SCREEN_POOL,
          {
            pool,
            poolStr: pool,
            startBlock: header.block_start,
            endBlock: header.block_end,
            head,
            hourFrom: window.from,
            hourTo: window.to,
            from: String(window.from),
            to: String(window.to),
            minUsd: inputs.min_event_usd,
          },
          "ScreenPool",
          deployment,
        );
        const facts = assemblePoolFacts(data, header.coverage_shortfall);
        const verdict = materiality(facts, inputs);
        anyTruncated ||= facts.truncated;
        out[pool] = { ...facts, ...verdict };
      } catch (err) {
        if (!(err instanceof GraphError)) throw err;
        out[pool] = {
          start: null,
          end: null,
          absent_at_start: false,
          hours: [],
          large_events: { swap: null, mint: null, burn: null },
          unvalued_events: { mint: false, burn: false },
          truncated: false,
          coverage_shortfall: header.coverage_shortfall,
          verdict: "undetermined",
          reasons: ["undetermined:graph_error"],
          error: err.message,
        };
      }
    }
    return { ...header, truncated: anyTruncated, pools: out, requests: this.requests - before };
  }

  /** Events product: every mint, burn and swap in the window, paginated at one head block. */
  async eventsProduct(pools: string[], window: Window, cap: number): Promise<EventsResponse> {
    return this.eventsWith(await this.context(window), pools, cap);
  }

  /** The events product inside an existing context. */
  async eventsWith(ctx: Context, pools: string[], cap: number): Promise<EventsResponse> {
    const { header, head, deployment, window } = ctx;
    const before = ctx.requestsBefore;
    const out: Record<string, PoolEvents> = {};
    let anyTruncated = false;
    for (const raw of pools) {
      const pool = raw.toLowerCase();
      const events = await this.eventsForPool(pool, window, head, cap, deployment);
      anyTruncated ||= events.truncated;
      out[pool] = events;
    }
    return { ...header, truncated: anyTruncated, cap, pools: out, requests: this.requests - before };
  }

  /**
   * The investigate bundle: the screen and, for the pools it finds material,
   * the events, all read at one head from one deployment. A deployment change
   * between the queries is an error, never a mixed bundle.
   */
  async investigate(pools: string[], window: Window, inputs: Inputs, cap: number): Promise<Investigation> {
    assertHourAligned(window);
    const ctx = await this.context(window);
    const screen = await this.screenWith(ctx, pools, inputs);
    const material = Object.entries(screen.pools)
      .filter(([, p]) => p.verdict === "material")
      .map(([pool]) => pool);
    const events =
      material.length > 0 ? await this.eventsWith({ ...ctx, requestsBefore: this.requests }, material, cap) : null;
    return { screen, events };
  }

  async eventsForPool(pool: string, window: Window, head: number, cap: number, deployment?: string): Promise<PoolEvents> {
    const swaps: SwapEvent[] = [];
    const mints: LiquidityEvent[] = [];
    const burns: LiquidityEvent[] = [];
    const counts: Record<EventKind, number> = { swap: 0, mint: 0, burn: 0 };
    const sums: Record<EventKind, Dec> = { swap: dec("0"), mint: dec("0"), burn: dec("0") };
    let nulls = 0;
    let total = 0;
    let truncated = false;
    for (const kind of EVENT_KINDS) {
      const doc = EVENT_DOCS[kind];
      let lastId = "";
      for (;;) {
        const remaining = cap - total;
        if (remaining <= 0) {
          truncated = true;
          break;
        }
        const first = Math.min(PAGE_SIZE, remaining);
        const data = await this.query<Record<string, RawLiquidity[]>>(
          doc.document,
          { poolStr: pool, from: String(window.from), to: String(window.to), lastId, first, block: head },
          doc.operation,
          deployment,
        );
        const rows = data[doc.entity] ?? [];
        for (const row of rows) {
          const event = toEvent(row);
          if (kind === "swap") swaps.push(event);
          else if (kind === "mint") mints.push(toLiquidityEvent(row, event));
          else burns.push(toLiquidityEvent(row, event));
          counts[kind] += 1;
          if (event.amountUSD === null) nulls += 1;
          else sums[kind] = addDec(sums[kind], dec(event.amountUSD));
        }
        total += rows.length;
        if (rows.length < first) break;
        const last = rows[rows.length - 1];
        if (!last) break;
        lastId = last.id;
        if (total >= cap) {
          truncated = true;
          break;
        }
      }
      if (truncated) break;
    }
    return {
      swaps,
      mints,
      burns,
      counts,
      sum_amount_usd: { swap: decToString(sums.swap), mint: decToString(sums.mint), burn: decToString(sums.burn) },
      amount_usd_nulls: nulls,
      truncated,
    };
  }
}

function toEvent(row: RawSwap): SwapEvent {
  return {
    id: row.id,
    transaction: { id: row.transaction.id },
    logIndex: row.logIndex === null || row.logIndex === undefined ? null : Number(row.logIndex),
    timestamp: Number(row.timestamp),
    amount0: row.amount0,
    amount1: row.amount1,
    amountUSD: row.amountUSD,
    origin: row.origin,
  };
}

function toLiquidityEvent(row: RawLiquidity, event: SwapEvent): LiquidityEvent {
  return { ...event, owner: row.owner ?? null, tickLower: Number(row.tickLower), tickUpper: Number(row.tickUpper) };
}

function priceUSD(derivedETH: string | null | undefined, ethPriceUSD: string | null | undefined): string | null {
  if (derivedETH === null || derivedETH === undefined || ethPriceUSD === null || ethPriceUSD === undefined) return null;
  return decToString(mulDec(dec(derivedETH), dec(ethPriceUSD)));
}

function toSnapshot(raw: RawSnapshot | null, bundle: { ethPriceUSD: string | null } | null): PoolSnapshot | null {
  if (!raw) return null;
  return {
    totalValueLockedToken0: raw.totalValueLockedToken0,
    totalValueLockedToken1: raw.totalValueLockedToken1,
    totalValueLockedUSD: raw.totalValueLockedUSD,
    liquidity: raw.liquidity,
    token0PriceUSD: priceUSD(raw.token0?.derivedETH, bundle?.ethPriceUSD),
    token1PriceUSD: priceUSD(raw.token1?.derivedETH, bundle?.ethPriceUSD),
  };
}

export function assemblePoolFacts(data: RawScreenPool, coverageShortfall: boolean): PoolFacts {
  const start = toSnapshot(data.start, data.bundleStart);
  const end = toSnapshot(data.end, data.bundleEnd);
  return {
    start,
    end,
    absent_at_start: start === null && end !== null,
    hours: data.hours.map((h) => ({ periodStartUnix: Number(h.periodStartUnix), tvlUSD: h.tvlUSD, volumeUSD: h.volumeUSD, txCount: h.txCount })),
    large_events: { swap: data.swap[0] ?? null, mint: data.mint[0] ?? null, burn: data.burn[0] ?? null },
    unvalued_events: { mint: (data.mintUnvalued ?? []).length > 0, burn: (data.burnUnvalued ?? []).length > 0 },
    truncated: data.hours.length >= PAGE_SIZE,
    coverage_shortfall: coverageShortfall,
  };
}
