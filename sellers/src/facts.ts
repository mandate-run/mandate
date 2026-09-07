// Prints the spec section 7 objects for one pool and one window. Issue #4.
//
//   pnpm facts <pool> <from> <to> [materiality] [min_event_usd]
//
// `from` and `to` are unix seconds or ISO 8601. Reads GRAPH_API_KEY and
// GRAPH_SUBGRAPH_ID from sellers/.env when that file exists.

import { existsSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";

import { GraphClient, HOUR } from "./graph.js";

const EVENTS_CAP = 5000;

export function parseWhen(value: string): number {
  if (/^\d+$/.test(value)) return Number(value);
  const ms = Date.parse(value);
  if (Number.isNaN(ms)) throw new Error(`not a time: ${value}`);
  return Math.floor(ms / 1000);
}

async function main(argv: string[]): Promise<number> {
  const [pool, fromArg, toArg, materiality = "0.05", minEventUsd = "100000"] = argv;
  if (!pool || !fromArg || !toArg) {
    console.error("usage: pnpm facts <pool> <from> <to> [materiality] [min_event_usd]");
    return 2;
  }
  const envPath = fileURLToPath(new URL("../.env", import.meta.url));
  if (existsSync(envPath)) process.loadEnvFile(envPath);
  const apiKey = process.env["GRAPH_API_KEY"];
  const subgraphId = process.env["GRAPH_SUBGRAPH_ID"];
  if (!apiKey || !subgraphId) {
    console.error("GRAPH_API_KEY and GRAPH_SUBGRAPH_ID must be set, see sellers/.env.example");
    return 2;
  }
  const requested = { from: parseWhen(fromArg), to: parseWhen(toArg) };
  const window = { from: requested.from - (requested.from % HOUR), to: requested.to - (requested.to % HOUR) };
  if (window.from !== requested.from || window.to !== requested.to) {
    console.error(`window aligned down to whole hours: ${window.from} to ${window.to}`);
  }
  const client = new GraphClient({ subgraphId, apiKey });

  const t0 = performance.now();
  const screen = await client.screen([pool], window, { materiality, min_event_usd: minEventUsd });
  const t1 = performance.now();
  console.log(JSON.stringify(screen, null, 2));

  const events = await client.eventsProduct([pool], window, EVENTS_CAP);
  const t2 = performance.now();
  console.log(JSON.stringify(events, null, 2));

  console.log(
    `screen: ${screen.requests} requests, ${Math.round(t1 - t0)} ms; events: ${events.requests} requests, ${Math.round(t2 - t1)} ms`,
  );
  return 0;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main(process.argv.slice(2)).then(
    (code) => process.exit(code),
    (err: unknown) => {
      console.error(err instanceof Error ? err.message : String(err));
      process.exit(1);
    },
  );
}
