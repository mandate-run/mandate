// Durable result storage, keyed by the x402 payment-identifier. Spec section
// 13: bind the id to payer, route, request fingerprint and payment terms;
// return the stored result for a matching resend or retrieval without a
// second settlement; answer 409 when the id matches but the request differs.

import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";

const DATA_DIR = process.env.SELLER_DATA_DIR || path.join(import.meta.dirname, "..", "data", "results");

fs.mkdirSync(DATA_DIR, { recursive: true });

export function fingerprint(method, url, body) {
  const h = crypto.createHash("sha256");
  h.update(method);
  h.update("\n");
  h.update(url);
  h.update("\n");
  h.update(body ?? "");
  return h.digest("hex");
}

function fileFor(paymentId) {
  // Payment ids are pay_ + uuid; never allow path traversal.
  const safe = paymentId.replace(/[^a-zA-Z0-9_-]/g, "_");
  return path.join(DATA_DIR, `${safe}.json`);
}

export function load(paymentId) {
  try {
    const raw = fs.readFileSync(fileFor(paymentId), "utf8");
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

export function save(entry) {
  fs.writeFileSync(fileFor(entry.paymentId), JSON.stringify(entry, null, 2));
}

// True when the stored entry matches this payment payload and request.
export function matches(stored, paymentSignature, fingerprint, payTo, route) {
  return (
    stored &&
    stored.paymentSignatureHash === sha256(paymentSignature) &&
    stored.requestFingerprint === fingerprint &&
    stored.payTo === payTo &&
    stored.route === route
  );
}

export function sha256(s) {
  return crypto.createHash("sha256").update(s).digest("hex");
}