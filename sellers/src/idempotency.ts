// Payment-identifier binding, spec section 13. The id is bound to payer, route,
// request fingerprint and payment terms. A matching resend or retrieval is
// served from the store without a second settlement; a different request under
// the same id is answered 409. Retrieval needs the original PAYMENT-SIGNATURE:
// the id alone retrieves nothing.
import { appendFileSync, existsSync, mkdirSync, readFileSync } from "node:fs";
import { dirname } from "node:path";
import type { PaymentPayload } from "@x402/core/types";
import { extractPaymentIdentifier } from "@x402/extensions";
import { inspectHederaTransaction } from "@x402/hedera";
import type { NextFunction, Request, RequestHandler, Response } from "express";
import { sha256Hex } from "./journal.js";

export interface StoredResult {
  fingerprint: string;
  status: number;
  content_type: string | null;
  payment_response: string | null;
  body_base64: string;
  stored_at: string;
}

export interface ResultStore {
  get(id: string): StoredResult | undefined;
  set(id: string, result: StoredResult): void;
}

export class MemoryStore implements ResultStore {
  protected readonly map = new Map<string, StoredResult>();

  get(id: string): StoredResult | undefined {
    return this.map.get(id);
  }

  set(id: string, result: StoredResult): void {
    this.map.set(id, result);
  }

  get size(): number {
    return this.map.size;
  }
}

/** Append-only JSON lines on disk, loaded at start, written before the response leaves. */
export class FileStore extends MemoryStore {
  constructor(private readonly path: string) {
    super();
    if (!existsSync(path)) return;
    for (const line of readFileSync(path, "utf8").split("\n")) {
      if (line.trim() === "") continue;
      const { id, result } = JSON.parse(line) as { id: string; result: StoredResult };
      this.map.set(id, result);
    }
  }

  override set(id: string, result: StoredResult): void {
    mkdirSync(dirname(this.path), { recursive: true });
    appendFileSync(this.path, `${JSON.stringify({ id, result })}\n`);
    super.set(id, result);
  }
}

/** Decodes a base64 JSON payment header; null when it is not one. */
export function decodePaymentHeader(header: string): PaymentPayload | null {
  try {
    const parsed: unknown = JSON.parse(Buffer.from(header, "base64").toString("utf8"));
    return parsed !== null && typeof parsed === "object" ? (parsed as PaymentPayload) : null;
  } catch {
    return null;
  }
}

/** The account debited by an exact Hedera payload, or null when unreadable. */
export function payerOf(payload: PaymentPayload): string | null {
  const inner = payload.payload as { transaction?: unknown } | undefined;
  const transaction = inner?.transaction;
  if (typeof transaction !== "string") return null;
  try {
    const seen = inspectHederaTransaction(transaction);
    const entries = [...seen.hbarTransfers, ...Object.values(seen.tokenTransfers).flat()];
    const debit = entries.find((t) => BigInt(t.amount) < 0n);
    return debit?.accountId ?? null;
  } catch {
    return null;
  }
}

export interface FingerprintInput {
  payer: string | null;
  method: string;
  path: string;
  query: Record<string, unknown>;
  bodyHash: string;
  accepted: unknown;
}

/** Stable hash of who pays, for what request, on which terms. */
export function fingerprint(input: FingerprintInput): string {
  const query = Object.keys(input.query)
    .sort()
    .map((key) => [key, input.query[key]]);
  return sha256Hex(
    JSON.stringify([input.payer, input.method, input.path, query, input.bodyHash, input.accepted]),
  );
}

export function fingerprintRequest(req: Request, payload: PaymentPayload): string {
  return fingerprint({
    payer: payerOf(payload),
    method: req.method,
    path: req.path,
    query: req.query as Record<string, unknown>,
    bodyHash: sha256Hex(""),
    accepted: payload.accepted,
  });
}

/**
 * Runs before verification. Serves a stored result for a matching resend,
 * answers 409 for a mismatched one, and otherwise records the id and
 * fingerprint in `res.locals` for the capture layer.
 */
export function replayMiddleware(store: ResultStore): RequestHandler {
  return (req: Request, res: Response, next: NextFunction) => {
    const header = req.header("payment-signature");
    if (header === undefined) return next();
    const payload = decodePaymentHeader(header);
    if (payload === null) return next();
    const id = extractPaymentIdentifier(payload);
    if (id === null) return next();
    const print = fingerprintRequest(req, payload);
    res.locals.paymentId = id;
    res.locals.fingerprint = print;
    res.locals.signedPayloadHash = sha256Hex(header);
    const stored = store.get(id);
    if (stored === undefined) return next();
    if (stored.fingerprint !== print) {
      res.locals.source = "conflict";
      res.status(409).json({ error: "payment-identifier is bound to a different request" });
      return;
    }
    res.locals.source = "store";
    if (stored.payment_response !== null) res.setHeader("PAYMENT-RESPONSE", stored.payment_response);
    if (stored.content_type !== null) res.setHeader("Content-Type", stored.content_type);
    res.status(stored.status).end(Buffer.from(stored.body_base64, "base64"));
  };
}
