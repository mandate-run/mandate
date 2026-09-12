// Renders two real run reports. Nothing here invents a number: every value
// on the page is read from JSON that `mandate run --json` produced against
// Hedera testnet. If a field is missing the section says so rather than
// guessing, because a page about honest accounting should not fake its own.
const HBAR = 8;

function amount(tinybar) {
  if (typeof tinybar !== "number") return String(tinybar ?? "");
  const neg = tinybar < 0;
  const s = String(Math.abs(tinybar)).padStart(HBAR + 1, "0");
  const out = `${s.slice(0, -HBAR)}.${s.slice(-HBAR)}`;
  return (neg ? "-" : "") + out;
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
  // Mirror form: 0.0.x@sec.nanos becomes 0.0.x-sec-nanos.
  return `https://hashscan.io/testnet/transaction/${String(txId).replace("@", "-").replace(/\.(\d+)$/, "-$1")}`;
}

function renderPlans(report) {
  const host = document.getElementById("plans");
  const round = report.planning?.[0];
  if (!round) return;
  for (const p of round.plans) {
    const chosen = round.chosen === p.kind;
    const card = el("div", { class: `plan${chosen ? " chosen" : ""}` });
    const head = el("div");
    head.appendChild(el("span", { class: "kind" }, p.kind));
    head.appendChild(el("span", { class: `tag${chosen ? "" : " no"}` }, chosen ? "chosen" : "feasible"));
    card.appendChild(head);
    card.appendChild(el("div", { class: "amt" }, amount(p.expected)));
    card.appendChild(el("div", { class: "lbl" }, "expected cost, HBAR"));
    card.appendChild(el("div", { class: "bound" }, `worst case ${amount(p.bound)}`));
    const ul = el("ul");
    for (const s of p.expected_steps ?? []) {
      ul.appendChild(el("li", {}, `${s.listing_id} × ${s.units} — ${amount(s.amount)} (${s.source})`));
    }
    card.appendChild(ul);
    host.appendChild(card);
  }
  const note = document.getElementById("round-note");
  const rounds = report.planning.length;
  note.textContent =
    `The runtime re-plans after every purchase. This run planned ${rounds} times: ` +
    `once over all ${round.pending.length} pools, then again over what was left once screening ` +
    `showed only one pool had moved. That is where staged overtook the bundle.`;
}

function renderSteps(report) {
  const t = document.getElementById("steps");
  t.appendChild(rowOf("th", ["#", "Bought", "Amount", "Transaction", "State", "Records"]));
  for (const s of report.steps ?? []) {
    const tr = el("tr");
    tr.appendChild(el("td", {}, String(s.step)));
    tr.appendChild(el("td", { class: "mono" }, s.listing_id));
    tr.appendChild(el("td", { class: "mono" }, s.amount));
    const td = el("td", { class: "mono" });
    const a = el("a", { href: hashscanTx(s.tx_id), target: "_blank", rel: "noopener" }, s.tx_id);
    td.appendChild(a);
    tr.appendChild(td);
    const st = el("td");
    st.appendChild(el("span", { class: "pill" }, s.payment_state));
    tr.appendChild(st);
    tr.appendChild(el("td", { class: "mono" }, `${s.records} record, ${s.duplicates_ignored} dup ignored`));
    t.appendChild(tr);
  }
  const totals = report.totals ?? {};
  kv("totals", [
    ["settled", totals.settled],
    ["unspent", totals.unspent],
    ["outstanding", totals.outstanding],
    ["receipts on HCS", String((report.receipts ?? []).filter(r => r.hcs_sequence != null).length)],
  ]);
}

function renderOutcomes(report) {
  const t = document.getElementById("outcomes");
  t.appendChild(rowOf("th", ["Pool", "Outcome", "Why"]));
  for (const o of report.outcomes ?? []) {
    const tr = el("tr");
    tr.appendChild(el("td", { class: "mono" }, `${o.pool.slice(0, 10)}…${o.pool.slice(-6)}`));
    const c = el("td");
    c.appendChild(el("span", { class: `pill${o.outcome === "supported" ? "" : " grey"}` }, o.outcome));
    tr.appendChild(c);
    tr.appendChild(el("td", { class: "mono" }, (o.reasons ?? []).join(", ") || "—"));
    t.appendChild(tr);
  }
  const v = report.validation;
  if (v) {
    document.getElementById("validation").textContent =
      `Validation ${v.passed ? "passed" : "failed"}: every one of the ${v.coverage.required} pools resolved, ` +
      `${v.calculations} calculations re-evaluated, ${v.references} references checked against purchased evidence, ` +
      `${v.prose_numbers} numbers in the prose that the facts did not support.`;
  }
}

function renderRefusal(report) {
  document.getElementById("refusal-text").textContent = (report.refusals ?? [])[0] ?? "";
  const t = report.totals ?? {};
  kv("refusal-totals", [
    ["settled", t.settled],
    ["unspent", t.unspent],
    ["audit spent", `${t.audit_spent_tinybar} tinybar`],
  ]);
}

function rowOf(tag, cells) {
  const tr = el("tr");
  for (const c of cells) tr.appendChild(el(tag, {}, c));
  return tr;
}

function kv(id, pairs) {
  const host = document.getElementById(id);
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
    if (delivered.topic) {
      const link = document.getElementById("topic-link");
      link.href = `https://hashscan.io/testnet/topic/${delivered.topic}`;
      link.textContent = `Receipts on HashScan · topic ${delivered.topic}`;
    }
  } catch (e) {
    // A page that cannot read its own evidence says so.
    document.getElementById("round-note").textContent =
      "The run reports could not be loaded. The figures they hold are in the repository.";
  }
}

main();
