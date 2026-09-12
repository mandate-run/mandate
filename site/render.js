// Renders two real run reports. Nothing here invents a number: every value on
// the page comes from JSON that `mandate run --json` produced against Hedera's
// test network. Where a term is jargon in the report, it is translated once
// here rather than explained on the page.
const DECIMALS = 8;

// What the runtime calls a thing, and what a reader should see.
const ROUTE_NAME = {
  staged: "Step by step",
  hybrid: "Mixed",
  bundle: "All at once",
};
const BOUGHT_NAME = {
  screen: "Quick scan of all 5 pools",
  events: "Detailed history for 1 pool",
  investigate: "Full investigation",
  explain: "Written explanation",
};
const OUTCOME_NAME = {
  supported: "moved",
  non_material: "quiet",
  pending: "undecided",
};

function amount(tinybar) {
  if (typeof tinybar !== "number") return String(tinybar ?? "");
  const s = String(Math.abs(tinybar)).padStart(DECIMALS + 1, "0");
  return `${tinybar < 0 ? "-" : ""}${s.slice(0, -DECIMALS)}.${s.slice(-DECIMALS)}`;
}

// Trailing zeros are noise to a reader; keep at least four decimals.
function tidy(text) {
  const t = String(text);
  if (!t.includes(".")) return t;
  return t.replace(/(\.\d{4}?\d*?)0+$/, "$1");
}

function el(tag, attrs = {}, text) {
  const n = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "class") n.className = v;
    else n.setAttribute(k, v);
  }
  if (text !== undefined) n.textContent = text;
  return n;
}

function hashscanTx(txId) {
  // The signed form is 0.0.x@sec.nanos; the explorer wants 0.0.x-sec-nanos.
  return `https://hashscan.io/testnet/transaction/${String(txId)
    .replace("@", "-")
    .replace(/\.(\d+)$/, "-$1")}`;
}

function renderPlans(report) {
  const host = document.getElementById("plans");
  const round = report.planning?.[0];
  if (!round || !host) return;

  for (const p of round.plans) {
    const chosen = round.chosen === p.kind;
    const card = el("div", { class: `plan${chosen ? " chosen" : ""}` });

    const head = el("div", { class: "head" });
    head.appendChild(el("span", { class: "kind" }, ROUTE_NAME[p.kind] ?? p.kind));
    head.appendChild(el("span", { class: `tag${chosen ? "" : " no"}` },
      chosen ? "chosen" : "affordable"));
    card.appendChild(head);

    card.appendChild(el("div", { class: "amt" }, tidy(amount(p.expected))));
    card.appendChild(el("div", { class: "lbl" }, "HBAR, likely cost"));

    const bound = el("div", { class: "bound" });
    bound.appendChild(document.createTextNode("worst case "));
    bound.appendChild(el("b", {}, tidy(amount(p.bound))));
    card.appendChild(bound);

    const ul = el("ul");
    for (const s of p.expected_steps ?? []) {
      ul.appendChild(el("li", {}, BOUGHT_NAME[s.listing_id] ?? s.listing_id));
    }
    card.appendChild(ul);
    host.appendChild(card);
  }

  const note = document.getElementById("round-note");
  if (!note) return;
  const chosenPlan = round.plans.find(p => p.kind === round.chosen);
  const dearest = round.plans.reduce((a, b) => (a.expected > b.expected ? a : b));
  note.textContent =
    `It chose "${ROUTE_NAME[round.chosen] ?? round.chosen}" at ${tidy(amount(chosenPlan.expected))} HBAR ` +
    `over the most expensive route at ${tidy(amount(dearest.expected))}. ` +
    `It then re-checked its plan after every purchase, ${report.planning.length} times in all, ` +
    `because a cheap scan can change what is still worth buying.`;
}

function renderSteps(report) {
  const t = document.getElementById("steps");
  if (!t) return;
  t.appendChild(rowOf("th", ["What it bought", "Cost", "Payment on Hedera", "Confirmed"]));

  for (const s of report.steps ?? []) {
    const tr = el("tr");
    tr.appendChild(el("td", {}, BOUGHT_NAME[s.listing_id] ?? s.listing_id));
    tr.appendChild(el("td", { class: "mono" }, tidy(s.amount)));

    const td = el("td", { class: "mono" });
    td.appendChild(el("a",
      { href: hashscanTx(s.tx_id), target: "_blank", rel: "noopener" }, s.tx_id));
    tr.appendChild(td);

    const st = el("td");
    st.appendChild(el("span", { class: "pill" },
      s.payment_state === "settled" ? "yes, on the ledger" : s.payment_state));
    tr.appendChild(st);
    t.appendChild(tr);
  }

  const totals = report.totals ?? {};
  const filed = (report.receipts ?? []).filter(r => r.hcs_sequence != null).length;
  kv("totals", [
    ["total spent", tidy(totals.settled)],
    ["budget left over", tidy(totals.unspent)],
    ["still owed", tidy(totals.outstanding)],
    ["receipts filed", String(filed)],
  ]);
}

function renderOutcomes(report) {
  const t = document.getElementById("outcomes");
  if (!t) return;
  t.appendChild(rowOf("th", ["Pool", "Verdict", "What changed"]));

  for (const o of report.outcomes ?? []) {
    const moved = o.outcome === "supported";
    const tr = el("tr", moved ? { class: "moved" } : {});
    tr.appendChild(el("td", { class: "mono" },
      `${o.pool.slice(0, 8)}…${o.pool.slice(-4)}`));

    const c = el("td");
    c.appendChild(el("span", { class: `pill${moved ? "" : " grey"}` },
      OUTCOME_NAME[o.outcome] ?? o.outcome));
    tr.appendChild(c);

    const reasons = (o.reasons ?? []).map(readableReason);
    tr.appendChild(el("td", {}, reasons.join(", ") || "nothing significant"));
    t.appendChild(tr);
  }

  const v = report.validation;
  const node = document.getElementById("validation");
  if (v && node) {
    node.textContent =
      `Before showing any of this, Mandate checked its own report: all ${v.coverage.required} pools ` +
      `accounted for, ${v.calculations} calculations redone from the raw data, and ` +
      `${v.references} facts traced back to evidence it actually paid for. ` +
      `It found ${v.prose_numbers} numbers in the written explanation that the evidence did not support.`;
  }
}

function renderRefusal(report) {
  const raw = (report.refusals ?? [])[0] ?? "";
  const totals = report.totals ?? {};

  // Pull the three figures out of the refusal line so it can be said plainly.
  const m = raw.match(/bound ([\d.]+) expected ([\d.]+) available ([\d.]+)/);
  const plain = document.getElementById("refusal-plain");
  if (plain && m) {
    plain.textContent =
      `The cheapest route could have cost up to ${tidy(m[1])} HBAR in the worst case, ` +
      `but only ${tidy(m[3])} was available. Mandate stopped and spent nothing.`;
  }
  const code = document.getElementById("refusal-text");
  if (code) code.textContent = raw;

  kv("refusal-totals", [
    ["total spent", tidy(totals.settled)],
    ["budget untouched", tidy(totals.unspent)],
  ]);
}

// "tvl_change:token0" and friends are how the runtime names evidence. Say
// them the way someone who does not trade would.
function readableReason(raw) {
  const [kind, detail] = String(raw).split(":");
  const named = {
    large_event: "a large trade",
    tvl_change: "the pool's size changed",
    price_change: "the price moved",
  }[kind];
  if (!named) return String(raw).replace(/_/g, " ");
  if (kind === "large_event" && detail) return `a large ${detail}`;
  return named;
}

function rowOf(tag, cells) {
  const tr = el("tr");
  for (const c of cells) tr.appendChild(el(tag, {}, c));
  return tr;
}

function kv(id, pairs) {
  const host = document.getElementById(id);
  if (!host) return;
  for (const [label, value] of pairs) {
    if (value === undefined || value === null) continue;
    const d = el("div");
    d.appendChild(el("span", {}, label));
    d.appendChild(el("b", {}, String(value)));
    host.appendChild(d);
  }
}

async function main() {
  try {
    const [delivered, refused] = await Promise.all([
      fetch("data/delivered.json").then(r => r.json()),
      fetch("data/refused.json").then(r => r.json()),
    ]);
    renderPlans(delivered);
    renderSteps(delivered);
    renderOutcomes(delivered);
    renderRefusal(refused);

    const charts = window.MandateCharts;
    if (charts) {
      charts.heroFigure(delivered, document.getElementById("hero-viz"));
      charts.loopDiagram(document.getElementById("loop"));
      charts.budgetBar(delivered, document.getElementById("budget"));
      charts.replanChart(delivered, document.getElementById("replan"));
    }

    if (delivered.topic) {
      const link = document.getElementById("topic-link");
      if (link) link.href = `https://hashscan.io/testnet/topic/${delivered.topic}`;
    }
  } catch {
    // A page about honest accounting should not fake its own contents.
    const note = document.getElementById("round-note");
    if (note) note.textContent =
      "The run reports could not be loaded. The figures they hold are in the repository.";
  }
}

main();
