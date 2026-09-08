// 402 shell shared by all listings: resource server, facilitator client, Hedera
// exact scheme, payment-identifier, capture of every response into the store
// and the journal, fault switches. Per request: body hash, replay check and
// claim, verify, work, settle, respond. A failed handler never settles; the
// middleware does that. Only a successful settlement with a 2xx response is
// stored; failed and uncertain attempts leave nothing behind.
import {
  type FacilitatorClient,
  HTTPFacilitatorClient,
  type RoutesConfig,
  x402ResourceServer,
} from "@x402/core/server";
import type { Network, PaymentPayload } from "@x402/core/types";
import { paymentMiddleware } from "@x402/express";
import {
  PAYMENT_IDENTIFIER,
  bazaarResourceServerExtension,
  declareDiscoveryExtension,
  declarePaymentIdentifierExtension,
  extractPaymentIdentifier,
  paymentIdentifierResourceServerExtension,
} from "@x402/extensions";
import { AsyncLocalStorage } from "node:async_hooks";
import { ExactHederaScheme } from "@x402/hedera/exact/server";
import express, {
  type Express,
  type NextFunction,
  type Request,
  type RequestHandler,
  type Response,
} from "express";
import { FAULT_DROP_RESPONSE } from "./faults.js";
import { Claims, type ResultStore, replayMiddleware, transactionHashOf } from "./idempotency.js";
import { type Journal, type Source, sha256Hex } from "./journal.js";
import { RequestError } from "./requests.js";

export const USDC_TESTNET = "0.0.429274";
/** The asset id the x402 Hedera scheme uses for HBAR. */
export const HBAR = "0.0.0";
export { FAULT_DROP_RESPONSE };

/** The amount a request will be charged, decided before the 402 and read by the dynamic price. */
const priced = new AsyncLocalStorage<{ amount: number }>();

export interface SellerConfig {
  /** CAIP-2 network, `hedera:testnet`. */
  network: Network;
  payTo: string;
  facilitatorUrl: string;
  /** HTS token id, or `0.0.0` for HBAR. */
  asset: string;
  store: ResultStore;
  journal: Journal;
  faults?: ReadonlySet<string>;
  /** Served unpaid at `/manifest.json` when present. */
  manifest?: unknown;
  /** Fetch the facilitator's supported kinds at start. Off in tests. */
  syncFacilitatorOnStart?: boolean;
  /** Injected facilitator client; tests use a fake. */
  facilitator?: FacilitatorClient;
  claims?: Claims;
}

export interface Listing {
  method?: "GET" | "POST";
  path: string;
  /**
   * Atomic units of `asset` for this request, decided before the 402. Throw a
   * `RequestError` for a request that must not be quoted; it becomes a 400
   * before payment. Receives the parsed body and its size in bytes.
   */
  price: (body: unknown, bodyBytes: number) => number;
  description: string;
  maxTimeoutSeconds?: number;
  /** Bazaar discovery declaration, put in every 402 for this route. */
  discovery?: Parameters<typeof declareDiscoveryExtension>[0];
  handler: RequestHandler;
}

/** A fixed-price listing, for the spike. */
export function fixedPrice(amount: number): Listing["price"] {
  return () => amount;
}

/** Settlement bookkeeping for one attempt, keyed by payment id and transaction hash. */
interface Attempt {
  calls: number;
  ok: boolean;
}

export function createSeller(cfg: SellerConfig, listings: Listing[]): Express {
  const faults = cfg.faults ?? new Set<string>();
  const claims = cfg.claims ?? new Claims();
  const facilitator = cfg.facilitator ?? new HTTPFacilitatorClient({ url: cfg.facilitatorUrl });
  const server = new x402ResourceServer(facilitator);
  server.register(cfg.network, new ExactHederaScheme());
  server.registerExtension(paymentIdentifierResourceServerExtension);
  server.registerExtension(bazaarResourceServerExtension);

  const attempts = new Map<string, Attempt>();
  const attemptOf = (payload: PaymentPayload): Attempt => {
    const key = `${extractPaymentIdentifier(payload) ?? "none"}:${transactionHashOf(payload)}`;
    let attempt = attempts.get(key);
    if (attempt === undefined) {
      attempt = { calls: 0, ok: false };
      attempts.set(key, attempt);
    }
    return attempt;
  };
  server.onBeforeSettle(async (ctx) => {
    attemptOf(ctx.paymentPayload as PaymentPayload).calls += 1;
  });
  server.onAfterSettle(async (ctx) => {
    if (ctx.result.success) attemptOf(ctx.paymentPayload as PaymentPayload).ok = true;
  });

  const routes: RoutesConfig = {};
  for (const listing of listings) {
    const extensions: Record<string, unknown> = { [PAYMENT_IDENTIFIER]: declarePaymentIdentifierExtension(true) };
    if (listing.discovery !== undefined) Object.assign(extensions, declareDiscoveryExtension(listing.discovery));
    routes[`${listing.method ?? "GET"} ${listing.path}`] = {
      accepts: {
        scheme: "exact",
        network: cfg.network,
        payTo: cfg.payTo,
        price: () => {
          const store = priced.getStore();
          if (store === undefined) throw new Error("price requested outside a priced request");
          return { amount: String(store.amount), asset: cfg.asset };
        },
        maxTimeoutSeconds: listing.maxTimeoutSeconds ?? 120,
      },
      description: listing.description,
      mimeType: "application/json",
      extensions,
    };
  }

  const app = express();
  app.disable("x-powered-by");
  if (cfg.manifest !== undefined) {
    const manifest = cfg.manifest;
    app.get("/manifest.json", (_req, res) => {
      res.json(manifest);
    });
  }
  app.use(capture(cfg, faults, attempts));
  app.use(express.raw({ type: () => true, limit: "1mb" }));
  app.use(bodyHash());
  app.use(pricing(listings));
  app.use(replayMiddleware(cfg.store, claims));
  app.use(paymentMiddleware(routes, server, undefined, undefined, cfg.syncFacilitatorOnStart ?? true));
  for (const listing of listings) {
    if ((listing.method ?? "GET") === "GET") app.get(listing.path, listing.handler);
    else app.post(listing.path, listing.handler);
  }
  return app;
}

/**
 * Validates and prices the request before anything else sees it. A
 * `RequestError` is a 400 with no 402 and no work; anything else runs the
 * rest of the chain inside the priced context.
 */
function pricing(listings: Listing[]): RequestHandler {
  return (req: Request, res: Response, next: NextFunction) => {
    const listing = listings.find((l) => l.path === req.path && (l.method ?? "GET") === req.method);
    if (listing === undefined) {
      next();
      return;
    }
    let amount: number;
    try {
      amount = listing.price(req.body, (res.locals.bodyBytes as number | undefined) ?? 0);
    } catch (err) {
      if (err instanceof RequestError) {
        res.locals.source = "rejected";
        res.status(err.status).json({ error: err.message });
        return;
      }
      throw err;
    }
    res.locals.amount = amount;
    priced.run({ amount }, () => next());
  };
}

/** Hashes the raw body for the replay fingerprint, then parses JSON for the handlers. */
function bodyHash(): RequestHandler {
  return (req: Request, res: Response, next: NextFunction) => {
    const raw = Buffer.isBuffer(req.body) ? req.body : Buffer.alloc(0);
    res.locals.bodyHash = sha256Hex(raw);
    res.locals.bodyBytes = raw.length;
    if (raw.length === 0) {
      req.body = undefined;
      next();
      return;
    }
    if (req.is("application/json") !== false && req.is("application/json") !== null) {
      try {
        req.body = JSON.parse(raw.toString("utf8"));
      } catch {
        res.status(400).json({ error: "body is not JSON" });
        return;
      }
    } else {
      req.body = raw;
    }
    next();
  };
}

type Chunk = string | Uint8Array;

function toBuffer(chunk: Chunk): Buffer {
  return typeof chunk === "string" ? Buffer.from(chunk) : Buffer.from(chunk);
}

function headerString(value: number | string | string[] | undefined): string | null {
  if (value === undefined) return null;
  if (Array.isArray(value)) return value[0] ?? null;
  return String(value);
}

/**
 * Outermost layer. Buffers the body, stores a fresh result under its payment
 * id only after a successful settlement and a 2xx status, releases the claim,
 * writes one journal line per request, and applies the drop-response fault.
 */
function capture(cfg: SellerConfig, faults: ReadonlySet<string>, attempts: Map<string, Attempt>): RequestHandler {
  return (req: Request, res: Response, next: NextFunction) => {
    const chunks: Buffer[] = [];
    const originalWrite = res.write.bind(res) as (...args: unknown[]) => boolean;
    const originalEnd = res.end.bind(res) as (...args: unknown[]) => Response;
    const attempted = req.header("payment-signature") !== undefined;

    res.write = ((chunk: Chunk, ...rest: unknown[]) => {
      chunks.push(toBuffer(chunk));
      return originalWrite(chunk, ...rest);
    }) as typeof res.write;

    res.end = ((chunk?: unknown, ...rest: unknown[]) => {
      if (chunk !== undefined && chunk !== null && typeof chunk !== "function") {
        chunks.push(toBuffer(chunk as Chunk));
      }
      const body = Buffer.concat(chunks);
      const paymentResponse = headerString(res.getHeader("PAYMENT-RESPONSE"));
      const paymentId = (res.locals.paymentId as string | undefined) ?? null;
      const transactionHash = (res.locals.transactionHash as string | undefined) ?? null;
      const key = paymentId !== null && transactionHash !== null ? `${paymentId}:${transactionHash}` : null;
      const attempt = key !== null ? attempts.get(key) : undefined;
      if (key !== null) attempts.delete(key);
      const settleCalled = (attempt?.calls ?? 0) > 0;
      const settleOk = attempt?.ok ?? false;
      const source: Source = (res.locals.source as Source | undefined) ?? (attempted ? "live" : "unpaid");
      const fresh =
        source === "live" &&
        paymentId !== null &&
        transactionHash !== null &&
        settleOk &&
        res.statusCode >= 200 &&
        res.statusCode < 300;
      if (fresh) {
        cfg.store.set(paymentId, {
          fingerprint: res.locals.fingerprint as string,
          transaction_hash: transactionHash,
          status: res.statusCode,
          content_type: headerString(res.getHeader("Content-Type")),
          payment_response: paymentResponse,
          body_base64: body.toString("base64"),
          stored_at: new Date().toISOString(),
        });
      }
      (res.locals.release as (() => void) | undefined)?.();
      const entry = {
        ts: new Date().toISOString(),
        route: `${req.method} ${req.path}`,
        payment_id: paymentId,
        signed_payload_hash: (res.locals.signedPayloadHash as string | undefined) ?? null,
        settle_called: settleCalled,
        settle_ok: settleOk,
        served_hash: body.length > 0 ? sha256Hex(body) : null,
        status: res.statusCode,
        source,
      };
      if (fresh && faults.has(FAULT_DROP_RESPONSE)) {
        cfg.journal.write({ ...entry, source: "dropped" });
        res.socket?.destroy();
        return res;
      }
      cfg.journal.write(entry);
      return originalEnd(chunk, ...rest);
    }) as typeof res.end;

    next();
  };
}
