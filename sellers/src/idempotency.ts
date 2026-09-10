// Payment-identifier binding, spec section 13. The id is bound to payer, route,
// query, body and payment terms, and to the signed authorization itself. A
// matching resend or retrieval is served from the store without a second
// settlement; a different request or a different authorization under the same
// id is answered 409. Retrieval needs the original PAYMENT-SIGNATURE: the id
// alone retrieves nothing. One attempt runs per id at a time: identical
// concurrent requests wait for the first, conflicting ones are refused.
import { appendFileSync, existsSync, mkdirSync, readFileSync } from "node:fs";
import { dirname } from "node:path";
import type { PaymentPayload } from "@x402/core/types";
import { extractPaymentIdentifier } from "@x402/extensions";
import { inspectHederaTransaction } from "@x402/hedera";
import type { NextFunction, Request, RequestHandler, Response } from "express";
import { sha256Hex } from "./journal.js";

export interface StoredResult {
  fingerprint: string;
  /** Hash of the signed transaction that paid for this result. */
  transaction_hash: string;
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
    appendFileSync(this.path, `${JSON.stringify({ id, result })}\n`, { flush: true });
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

/** Hash of the signed transaction bytes, the only thing a replay must present. */
export function transactionHashOf(payload: PaymentPayload): string {
  const inner = payload.payload as { transaction?: unknown } | undefined;
  const transaction = inner?.transaction;
  return sha256Hex(typeof transaction === "string" ? transaction : JSON.stringify(payload.payload ?? null));
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

export function fingerprintRequest(req: Request, payload: PaymentPayload, bodyHash: string): string {
  return fingerprint({
    payer: payerOf(payload),
    method: req.method,
    path: req.path,
    query: req.query as Record<string, unknown>,
    bodyHash,
    accepted: payload.accepted,
  });
}

interface Claim {
  fingerprint: string;
  transaction_hash: string;
  done: Promise<void>;
  resolve: () => void;
}

export type ClaimOutcome =
  | { status: "claimed"; release: () => void }
  | { status: "coalesce"; done: Promise<void> }
  | { status: "conflict" };

/** In-flight attempts, one per payment id. Check-and-set is atomic on one event loop. */
export class Claims {
  private readonly map = new Map<string, Claim>();

  take(id: string, fingerprint: string, transactionHash: string): ClaimOutcome {
    const current = this.map.get(id);
    if (current !== undefined) {
      return current.fingerprint === fingerprint && current.transaction_hash === transactionHash
        ? { status: "coalesce", done: current.done }
        : { status: "conflict" };
    }
    let resolve: () => void = () => undefined;
    const done = new Promise<void>((r) => {
      resolve = r;
    });
    const claim: Claim = { fingerprint, transaction_hash: transactionHash, done, resolve };
    this.map.set(id, claim);
    return {
      status: "claimed",
      release: () => {
        if (this.map.get(id) === claim) this.map.delete(id);
        claim.resolve();
      },
    };
  }

  get size(): number {
    return this.map.size;
  }
}

function conflict(res: Response, message: string): void {
  res.locals.source = "conflict";
  res.status(409).json({ error: message });
}

function serve(res: Response, stored: StoredResult): void {
  res.locals.source = "store";
  if (stored.payment_response !== null) res.setHeader("PAYMENT-RESPONSE", stored.payment_response);
  if (stored.content_type !== null) res.setHeader("Content-Type", stored.content_type);
  res.status(stored.status).end(Buffer.from(stored.body_base64, "base64"));
}

/**
 * Runs after the body hash and before verification. Serves a stored result for
 * a matching resend, answers 409 for a mismatched request or authorization,
 * claims the id for a fresh attempt, and makes identical concurrent attempts
 * wait for the first. Records id, fingerprint and hashes in `res.locals`.
 */
export function replayMiddleware(store: ResultStore, claims: Claims): RequestHandler {
  return async (req: Request, res: Response, next: NextFunction): Promise<void> => {
    const header = req.header("payment-signature");
    if (header === undefined) return next();
    const payload = decodePaymentHeader(header);
    if (payload === null) return next();
    const id = extractPaymentIdentifier(payload);
    if (id === null) return next();
    const bodyHash = (res.locals.bodyHash as string | undefined) ?? sha256Hex("");
    const print = fingerprintRequest(req, payload, bodyHash);
    const transactionHash = transactionHashOf(payload);
    res.locals.paymentId = id;
    res.locals.fingerprint = print;
    res.locals.transactionHash = transactionHash;
    res.locals.signedPayloadHash = sha256Hex(header);
    for (let round = 0; round < 3; round += 1) {
      const stored = store.get(id);
      if (stored !== undefined) {
        if (stored.fingerprint !== print || stored.transaction_hash !== transactionHash) {
          return conflict(res, "payment-identifier is bound to a different request or authorization");
        }
        return serve(res, stored);
      }
      const claim = claims.take(id, print, transactionHash);
      if (claim.status === "conflict") {
        return conflict(res, "payment-identifier is in flight for a different request or authorization");
      }
      if (claim.status === "coalesce") {
        await claim.done;
        continue;
      }
      res.locals.release = claim.release;
      // A disconnected buyer does not cancel the handler or settlement.
      // Hold ownership until capture has persisted its result and ended the
      // response; otherwise a retry can run the same purchase concurrently.
      res.once("finish", claim.release);
      return next();
    }
    conflict(res, "payment-identifier is contended; retry");
  };
}
