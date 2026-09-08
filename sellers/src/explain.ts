// Listing explain: prose from a bounded brief of claims the buyer computed.
// The only module that holds a model key. The model may use nothing but the
// brief; the buyer checks every number in the prose against it.

export interface Explanation {
  prose: string;
  model: string;
  input_bytes: number;
}

export type Explainer = (brief: Record<string, unknown>) => Promise<Explanation>;

export const DEFAULT_MODEL = "claude-sonnet-5";

const SYSTEM = [
  "You explain DeFi liquidity findings to an analyst.",
  "You receive a brief: per-pool outcomes, claims with their values and evidence, and supporting events.",
  "A compact brief uses short keys: o outcomes (p pool, o outcome, r reasons), c claims per pool (t type: tvl with s start, e end, c change; large or largest with k kind, a amountUSD, tx; act with v volumeUSD, n txCount), e supporting events, w window, b blocks.",
  "Write at most 200 words of plain prose.",
  "Use only numbers, addresses and transaction hashes that appear in the brief, copied exactly.",
  "Do not add figures, estimates, causes or advice. If the brief holds no material pool, say so.",
].join(" ");

/** The Anthropic Messages API over fetch. */
export function anthropicExplainer(apiKey: string, model = DEFAULT_MODEL, fetchImpl: typeof fetch = fetch): Explainer {
  return async (brief) => {
    const input = JSON.stringify(brief);
    const response = await fetchImpl("https://api.anthropic.com/v1/messages", {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-api-key": apiKey,
        "anthropic-version": "2023-06-01",
      },
      body: JSON.stringify({
        model,
        max_tokens: 600,
        system: SYSTEM,
        messages: [{ role: "user", content: `Brief:\n${input}` }],
      }),
    });
    if (!response.ok) throw new Error(`model api answered ${response.status}`);
    const json = (await response.json()) as { content?: { type: string; text?: string }[] };
    const prose = (json.content ?? [])
      .filter((c) => c.type === "text" && typeof c.text === "string")
      .map((c) => c.text as string)
      .join("\n")
      .trim();
    if (prose === "") throw new Error("model returned no text");
    return { prose, model, input_bytes: Buffer.byteLength(input) };
  };
}

/**
 * The buyer's brief is compact, spec section 8: `o` holds the outcomes as
 * `{p, o, r}`, `c` the claims per pool, `e` the supporting events. The
 * seller's own bundle brief uses the long names.
 */
function outcomesOf(brief: Record<string, unknown>): { pool: string; outcome: string }[] {
  const long = Array.isArray(brief["outcomes"]) ? (brief["outcomes"] as { pool?: string; outcome?: string }[]) : [];
  const short = Array.isArray(brief["o"]) ? (brief["o"] as { p?: string; o?: string }[]) : [];
  return [
    ...long.map((o) => ({ pool: o.pool ?? "?", outcome: o.outcome ?? "?" })),
    ...short.map((o) => ({ pool: o.p ?? "?", outcome: o.o ?? "?" })),
  ];
}

/** For runs without a key: a fixed sentence built only from the brief's outcomes. */
export function templateExplainer(): Explainer {
  return async (brief) => {
    const lines = outcomesOf(brief).map((o) => `Pool ${o.pool} is ${o.outcome}.`);
    const prose = lines.length > 0 ? lines.join(" ") : "The brief holds no pool outcomes.";
    return { prose, model: "template", input_bytes: Buffer.byteLength(JSON.stringify(brief)) };
  };
}
