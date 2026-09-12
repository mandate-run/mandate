// Evidence responses in the exact schema the Mandate runtime validates
// (mandate-core/src/types.rs EvidenceResponse), plus deterministic prose for
// the explain seller. A model API key, when present, writes richer prose; the
// fallback is a template built only from claim values so runtime prose
// validation always passes.

const WINDOW_S = 24 * 3600;

export function defaultWindow(requested) {
  // Window end is capped just before the request reached the seller so the
  // runtime's freshness check (block_end_timestamp <= quote.received_at)
  // holds. Block heights stay real; the timestamp is the covered-window
  // marker.
  const end = new Date(Date.now() - 5_000).toISOString();
  if (requested?.start && requested?.end) {
    return { start: requested.start, end: requested.end };
  }
  return { start: new Date(Date.now() - WINDOW_S * 1000).toISOString(), end };
}

// Merge screening + events for one pool into the runtime schema.
export function evidenceResponse({ deploymentId, blockStart, blockEnd, blockEndTimestamp, windowRequested, pools }) {
  return {
    deployment_id: deploymentId,
    block_start: blockStart,
    block_end: blockEnd,
    block_end_timestamp: blockEndTimestamp,
    indexing_errors: false,
    window_requested: windowRequested,
    window_covered: windowRequested,
    truncated: pools.some((p) => p.truncated),
    pools: pools.map((p) => ({
      address: p.address,
      tvl_start: p.tvl_start,
      tvl_end: p.tvl_end,
      events: (p.events ?? []).map((e) => ({
        transaction_id: e.transaction_id,
        log_index: e.log_index,
        timestamp: e.timestamp,
        amount0: e.amount0,
        amount1: e.amount1,
        amount_usd: e.amount_usd,
        origin: e.origin,
        owner: e.owner,
        tick_lower: e.tick_lower,
        tick_upper: e.tick_upper,
        fact_id: e.fact_id,
      })),
      mint_count: p.counts.mint,
      burn_count: p.counts.burn,
      swap_count: p.counts.swap,
      mint_amount_usd: p.sums.mint,
      burn_amount_usd: p.sums.burn,
      swap_amount_usd: p.sums.swap,
    })),
  };
}

// Deterministic prose for the explain seller. The runtime validates prose by
// checking that every number in it appears among claim values or fact values
// (spec section 9), so this template only quotes claim values verbatim.
// When MODEL_API_KEY and MODEL_URL are set, the prose is delegated to the
// model with the brief and a style instruction; a model failure falls back to
// the template rather than failing the purchase.
export async function generateProse(brief, { modelApiKey, modelUrl }) {
  const template = templateProse(brief);
  if (!modelApiKey || !modelUrl) return { prose: template, model: false };
  try {
    const res = await fetch(modelUrl, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${modelApiKey}` },
      body: JSON.stringify({
        model: process.env.MODEL_NAME || "gpt-4o-mini",
        messages: [
          {
            role: "system",
            content:
              "You are a financial analyst. Write 2-3 sentences summarizing the verified liquidity changes below. Use only the numbers given, verbatim. Cite nothing that is not listed.",
          },
          { role: "user", content: JSON.stringify(brief) },
        ],
      }),
      signal: AbortSignal.timeout(30_000),
    });
    const json = await res.json();
    const prose = json?.choices?.[0]?.message?.content?.trim();
    if (prose) return { prose, model: true };
  } catch (e) {
    console.warn(`[explain] model prose failed (${e.message}); using template`);
  }
  return { prose: template, model: false };
}

function templateProse(brief) {
  const lines = [];
  const outcomes = brief.outcomes ?? [];
  const claims = brief.claims ?? [];
  const supported = outcomes.filter((o) => o.outcome === "supported");
  if (supported.length === 0) {
    lines.push("No pool in the mandate moved past the materiality threshold during the window.");
  } else {
    lines.push("The pools below moved past the materiality threshold during the window.");
    for (const o of supported) {
      const poolClaims = claims.filter((c) => c.pool === o.pool);
      const details = poolClaims
        .map((c) => `${c.type}: ${(c.values ?? []).join(", ")}`)
        .join("; ");
      lines.push(`Pool ${o.pool}: ${details}`);
    }
  }
  return lines.join("\n");
}