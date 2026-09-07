// Issue 3 step 1: the reference TypeScript client pays the spike seller once
// and prints the three headers decoded, so the Rust payload can be diffed
// against a known-good one. Credentials come only from the process
// environment, never from a file:
//   ORACLE_ACCOUNT_ID=0.0.x ORACLE_PRIVATE_KEY=... pnpm exec tsx src/oracle.ts http://localhost:4021/spike
import { decodePaymentResponseHeader, wrapFetchWithPayment, x402Client } from "@x402/fetch";
import type { PaymentPayload, PaymentRequired } from "@x402/core/types";
import { PAYMENT_IDENTIFIER, appendPaymentIdentifierToExtensions, generatePaymentId } from "@x402/extensions";
import { PrivateKey, createClientHederaSigner } from "@x402/hedera";
import { ExactHederaScheme } from "@x402/hedera/exact/client";

const url = process.argv[2];
const accountId = process.env["ORACLE_ACCOUNT_ID"];
const privateKey = process.env["ORACLE_PRIVATE_KEY"];
if (url === undefined || accountId === undefined || privateKey === undefined) {
  console.error("usage: ORACLE_ACCOUNT_ID=0.0.x ORACLE_PRIVATE_KEY=... pnpm exec tsx src/oracle.ts <url>");
  process.exit(2);
}

const network = process.env["HEDERA_NETWORK"] === "mainnet" ? "hedera:mainnet" : "hedera:testnet";
const paymentId = generatePaymentId("pay_");
const decode = (header: string): unknown => JSON.parse(Buffer.from(header, "base64").toString("utf8"));

const signer = createClientHederaSigner(accountId, PrivateKey.fromString(privateKey), { network });
// Spend controls are the buyer runtime's job, not this probe's; allow any asset the seller quotes.
const client = new x402Client()
  .setSpendControls(false)
  .register(network, new ExactHederaScheme(signer))
  .registerExtension({
    key: PAYMENT_IDENTIFIER,
    enrichPaymentPayload: async (payload: PaymentPayload, required: PaymentRequired) => {
      const extensions = structuredClone({ ...(required.extensions ?? {}), ...(payload.extensions ?? {}) });
      return { ...payload, extensions: appendPaymentIdentifierToExtensions(extensions, paymentId) };
    },
  })
  .onBeforePaymentCreation(async (ctx) => {
    console.log("PAYMENT-REQUIRED (selected)");
    console.log(JSON.stringify(ctx.paymentRequired, null, 2));
  });

const seen = { signature: null as string | null };
const observing: typeof fetch = async (input, init) => {
  const headers = new Headers(input instanceof Request ? input.headers : undefined);
  new Headers(init?.headers).forEach((value, key) => headers.set(key, value));
  const sent = headers.get("PAYMENT-SIGNATURE");
  if (sent !== null) {
    seen.signature = sent;
    console.log("PAYMENT-SIGNATURE");
    console.log(JSON.stringify(decode(sent), null, 2));
  }
  return fetch(input, init);
};

const paidFetch = wrapFetchWithPayment(observing, client);
const response = await paidFetch(url);
console.log(`status ${response.status}`);
const paymentResponse = response.headers.get("PAYMENT-RESPONSE");
if (paymentResponse !== null) {
  console.log("PAYMENT-RESPONSE");
  console.log(JSON.stringify(decodePaymentResponseHeader(paymentResponse), null, 2));
}
const body = await response.text();
console.log(`body ${body}`);
console.log(`payment_id ${paymentId}`);
console.log(`signature_bytes ${seen.signature === null ? 0 : seen.signature.length}`);
