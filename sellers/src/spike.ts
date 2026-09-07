// Issue 3 spike seller: one route priced 0.0010 USDC on hedera:testnet through
// the facilitator in FACILITATOR_URL, payment-identifier store and journal on
// disk under JOURNAL_DIR, fault switches from FAULT. Run from sellers/:
//   pnpm exec tsx src/spike.ts
import { randomUUID } from "node:crypto";
import { FileStore } from "./idempotency.js";
import { Journal } from "./journal.js";
import { HBAR, USDC_TESTNET, createSeller } from "./x402.js";

try {
  process.loadEnvFile(".env");
} catch {
  // process environment only
}
const env = process.env;
const payTo = env["SELLER_PAY_TO"];
if (payTo === undefined || payTo === "") {
  console.error("SELLER_PAY_TO is not set");
  process.exit(2);
}
const port = Number(env["PORT"] ?? 4021);
const dir = env["JOURNAL_DIR"] ?? ".journal";
const facilitatorUrl = env["FACILITATOR_URL"] ?? "https://api.testnet.blocky402.com";
const faults = new Set(
  (env["FAULT"] ?? "")
    .split(",")
    .map((s) => s.trim())
    .filter((s) => s !== ""),
);

const app = createSeller(
  {
    network: env["HEDERA_NETWORK"] === "mainnet" ? "hedera:mainnet" : "hedera:testnet",
    payTo,
    facilitatorUrl,
    asset: env["SELLER_ASSET"]?.toUpperCase() === "HBAR" ? HBAR : (env["SELLER_ASSET"] ?? USDC_TESTNET),
    store: new FileStore(`${dir}/spike-store.jsonl`),
    journal: new Journal(`${dir}/spike-journal.jsonl`),
    faults,
  },
  [
    {
      path: "/spike",
      amount: "1000",
      description: "Spike: one paid JSON body with a fresh nonce. Issue 3.",
      handler: (_req, res) => {
        res.json({ ok: true, served_at: new Date().toISOString(), nonce: randomUUID() });
      },
    },
  ],
);

app.listen(port, () => {
  console.log(
    `spike seller on :${port} payTo ${payTo} facilitator ${facilitatorUrl} faults [${[...faults].join(",")}] journal ${dir}`,
  );
});
