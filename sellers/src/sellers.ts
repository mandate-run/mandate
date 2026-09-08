// The four reference sellers on one port with the manifest. Run from sellers/:
//   pnpm sellers
// Reads sellers/.env: SELLER_PAY_TO, FACILITATOR_URL, GRAPH_API_KEY,
// GRAPH_SUBGRAPH_ID, GRAPH_FIXTURE, ANTHROPIC_API_KEY, PORT, JOURNAL_DIR, FAULT,
// SELLER_ASSET, PUBLIC_URL. GRAPH_FIXTURE serves canned facts instead of the
// gateway; without it and without GRAPH_API_KEY the evidence handlers fail
// before settling. Without ANTHROPIC_API_KEY explain uses a template built
// from the brief.
import { anthropicExplainer, templateExplainer } from "./explain.js";
import { parseFaults } from "./faults.js";
import { fixtureFetch, parseFixture } from "./fixture.js";
import { GraphClient } from "./graph.js";
import { FileStore } from "./idempotency.js";
import { Journal } from "./journal.js";
import { buildManifest, listings } from "./listings.js";
import { tariffsFor } from "./tariffs.js";
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
const network = env["HEDERA_NETWORK"] === "mainnet" ? "hedera:mainnet" : "hedera:testnet";
const asset = env["SELLER_ASSET"]?.toUpperCase() === "HBAR" ? HBAR : (env["SELLER_ASSET"] ?? USDC_TESTNET);
const faults = parseFaults(env["FAULT"]);
const baseUrl = env["PUBLIC_URL"] ?? `http://127.0.0.1:${port}`;
const tariffs = tariffsFor(asset);

const apiKey = env["GRAPH_API_KEY"];
const subgraphId = env["GRAPH_SUBGRAPH_ID"] ?? "5zvR82QoaXYFyDEKLZ9t6v9adgnptxYpKpSbxtgVENFV";
const fixture = parseFixture(env["GRAPH_FIXTURE"]);
const graph =
  fixture !== undefined
    ? new GraphClient({ apiKey: "fixture", subgraphId: "fixture", fetch: fixtureFetch(fixture) })
    : apiKey === undefined || apiKey === ""
    ? {
        screen: async () => {
          throw new Error("GRAPH_API_KEY is not set");
        },
        eventsProduct: async () => {
          throw new Error("GRAPH_API_KEY is not set");
        },
        investigate: async () => {
          throw new Error("GRAPH_API_KEY is not set");
        },
      }
    : new GraphClient({ apiKey, subgraphId });
const modelKey = env["ANTHROPIC_API_KEY"];
const explain = modelKey === undefined || modelKey === "" ? templateExplainer() : anthropicExplainer(modelKey);

const manifest = buildManifest({ baseUrl, seller: "mandate-sellers", network, asset, payTo, tariffs });
const app = createSeller(
  {
    network,
    payTo,
    facilitatorUrl,
    asset,
    store: new FileStore(`${dir}/sellers-store.jsonl`),
    journal: new Journal(`${dir}/sellers-journal.jsonl`),
    faults,
    manifest,
  },
  listings({ graph, explain, tariffs, asset, faults }),
);

app.listen(port, () => {
  console.log(
    `sellers on :${port} payTo ${payTo} asset ${asset} facilitator ${facilitatorUrl} graph ${fixture !== undefined ? `fixture:${fixture}` : apiKey ? "live" : "absent"} model ${modelKey ? "anthropic" : "template"} faults [${[...faults].join(",")}] manifest ${baseUrl}/manifest.json`,
  );
});
