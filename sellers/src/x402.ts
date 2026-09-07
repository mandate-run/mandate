// 402 shell shared by all listings: resource server, facilitator client, Hedera
// exact scheme, payment-identifier, capture of every response into the store
// and the journal, fault switches. Per request: replay check, verify, work,
// settle, respond. A failed handler never settles; the middleware does that.
import { HTTPFacilitatorClient, type RoutesConfig, x402ResourceServer } from "@x402/core/server";
import type { Network, PaymentPayload } from "@x402/core/types";
import { paymentMiddleware } from "@x402/express";
import {
  PAYMENT_IDENTIFIER,
  declarePaymentIdentifierExtension,
  extractPaymentIdentifier,
  paymentIdentifierResourceServerExtension,
} from "@x402/extensions";
import { ExactHederaScheme } from "@x402/hedera/exact/server";
import express, {
  type Express,
  type NextFunction,
  type Request,
  type RequestHandler,
  type Response,
} from "express";
import { type ResultStore, replayMiddleware } from "./idempotency.js";
import { type Journal, type Source, sha256Hex } from "./journal.js";

export const USDC_TESTNET = "0.0.429274";
export const FAULT_DROP_RESPONSE = "drop-response-after-settle";

export interface SellerConfig {
  /** CAIP-2 network, `hedera:testnet`. */
  network: Network;
  payTo: string;
  facilitatorUrl: string;
  /** HTS token id or `HBAR`. */
  asset: string;
  store: ResultStore;
  journal: Journal;
  faults?: ReadonlySet<string>;
  /** Fetch the facilitator's supported kinds at start. Off in tests. */
  syncFacilitatorOnStart?: boolean;
}

export interface Listing {
  method?: "GET" | "POST";
  path: string;
  /** Atomic units of `asset`. */
  amount: string;
  description: string;
  maxTimeoutSeconds?: number;
  handler: RequestHandler;
}

export function createSeller(cfg: SellerConfig, listings: Listing[]): Express {
  const faults = cfg.faults ?? new Set<string>();
  const server = new x402ResourceServer(new HTTPFacilitatorClient({ url: cfg.facilitatorUrl }));
  server.register(cfg.network, new ExactHederaScheme());
  server.registerExtension(paymentIdentifierResourceServerExtension);
  const settleCalled = new Set<string>();
  server.onBeforeSettle(async (ctx) => {
    const id = extractPaymentIdentifier(ctx.paymentPayload as PaymentPayload);
    if (id !== null) settleCalled.add(id);
  });

  const routes: RoutesConfig = {};
  for (const listing of listings) {
    routes[`${listing.method ?? "GET"} ${listing.path}`] = {
      accepts: {
        scheme: "exact",
        network: cfg.network,
        payTo: cfg.payTo,
        price: { amount: listing.amount, asset: cfg.asset },
        maxTimeoutSeconds: listing.maxTimeoutSeconds ?? 120,
      },
      description: listing.description,
      mimeType: "application/json",
      extensions: { [PAYMENT_IDENTIFIER]: declarePaymentIdentifierExtension(true) },
    };
  }

  const app = express();
  app.disable("x-powered-by");
  app.use(capture(cfg, faults, settleCalled));
  app.use(replayMiddleware(cfg.store));
  app.use(paymentMiddleware(routes, server, undefined, undefined, cfg.syncFacilitatorOnStart ?? true));
  for (const listing of listings) {
    if ((listing.method ?? "GET") === "GET") app.get(listing.path, listing.handler);
    else app.post(listing.path, listing.handler);
  }
  return app;
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
 * Outermost layer. Buffers the body, stores a settled fresh result under its
 * payment id before it leaves, writes one journal line per request, and
 * applies the drop-response fault.
 */
function capture(cfg: SellerConfig, faults: ReadonlySet<string>, settleCalled: Set<string>): RequestHandler {
  return (req: Request, res: Response, next: NextFunction) => {
    const chunks: Buffer[] = [];
    const originalWrite = res.write.bind(res) as (...args: unknown[]) => boolean;
    const originalEnd = res.end.bind(res) as (...args: unknown[]) => Response;

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
      const source: Source =
        (res.locals.source as Source | undefined) ?? (paymentResponse !== null ? "live" : "unpaid");
      const fresh = source === "live" && paymentId !== null && paymentResponse !== null;
      if (fresh) {
        cfg.store.set(paymentId, {
          fingerprint: res.locals.fingerprint as string,
          status: res.statusCode,
          content_type: headerString(res.getHeader("Content-Type")),
          payment_response: paymentResponse,
          body_base64: body.toString("base64"),
          stored_at: new Date().toISOString(),
        });
      }
      const entry = {
        ts: new Date().toISOString(),
        route: `${req.method} ${req.path}`,
        payment_id: paymentId,
        signed_payload_hash: (res.locals.signedPayloadHash as string | undefined) ?? null,
        settle_called: paymentId !== null && settleCalled.delete(paymentId),
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
