// Three drawings, each built from the same run reports the tables use. No
// chart library: the shapes are simple and the numbers must stay honest, so
// every coordinate is computed from a real figure rather than hand-placed.
const NS = "http://www.w3.org/2000/svg";

// Someone who has asked their system for less motion gets the finished
// drawing immediately rather than a blank one.
const STILL = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;

// Adds a timed animation, unless motion is unwanted, in which case the
// element simply starts at its final value.
function animate(node, attr, from, to, dur, begin) {
  if (STILL) {
    node.setAttribute(attr, String(to));
    return;
  }
  node.setAttribute(attr, String(from));
  const a = svg("animate", { attributeName: attr, from, to, dur, begin, fill: "freeze" });
  node.appendChild(a);
}

function svg(tag, attrs = {}) {
  const n = document.createElementNS(NS, tag);
  for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, String(v));
  return n;
}
function text(x, y, s, attrs = {}) {
  const t = svg("text", { x, y, ...attrs });
  t.textContent = s;
  return t;
}

/* ---------- 1. Where the budget went ---------- */

const SEGMENT_LABEL = {
  screen: "Quick scan",
  events: "Detailed history",
  explain: "Explanation",
  investigate: "Full investigation",
};

function budgetBar(report, host) {
  const W = 880, H = 96, BAR = 34, TOP = 30;
  const budget = 1_000_000; // 0.0100 HBAR, the mandate's service budget
  const s = svg("svg", {
    viewBox: `0 0 ${W} ${H}`, class: "chart", role: "img",
    "aria-label": "How much of the budget was spent",
  });

  s.appendChild(svg("rect", {
    x: 0, y: TOP, width: W, height: BAR, rx: 6, fill: "#e9e6d9",
  }));

  const colours = ["#2d7a58", "#3f9370", "#63ad8c"];
  let x = 0;
  (report.steps ?? []).forEach((step, i) => {
    const units = Math.round(parseFloat(step.amount) * 1e8);
    const w = (units / budget) * W;
    const g = svg("g", { class: "seg" });
    const r = svg("rect", {
      x, y: TOP, height: BAR, fill: colours[i % colours.length],
    });
    // Grow each segment in turn, so the eye follows the spending.
    animate(r, "width", 0, w, "0.5s", `${0.15 + i * 0.45}s`);
    g.appendChild(r);
    g.appendChild(svg("title")).textContent =
      `${SEGMENT_LABEL[step.listing_id] ?? step.listing_id}: ${step.amount} HBAR`;
    s.appendChild(g);
    x += w;
  });

  // The line where spending stopped.
  const mark = svg("line", {
    x1: x, y1: TOP - 8, x2: x, y2: TOP + BAR + 8,
    stroke: "#14231c", "stroke-width": 1.5,
  });
  animate(mark, "opacity", 0, 1, "0.3s", "1.6s");
  s.appendChild(mark);

  const spent = text(x + 10, TOP - 12, `spent ${trim(report.totals.settled)}`, {
    class: "c-key",
  });
  animate(spent, "opacity", 0, 1, "0.4s", "1.7s");
  s.appendChild(spent);

  s.appendChild(text(0, TOP - 12, "0", { class: "c-mute" }));
  s.appendChild(text(W, TOP - 12, "budget 0.0100 HBAR", { class: "c-mute", "text-anchor": "end" }));
  s.appendChild(text(W, TOP + BAR + 22, `${trim(report.totals.unspent)} never spent`, {
    class: "c-mute", "text-anchor": "end",
  }));

  const legend = svg("g");
  let lx = 0;
  (report.steps ?? []).forEach((step, i) => {
    legend.appendChild(svg("rect", {
      x: lx, y: TOP + BAR + 12, width: 9, height: 9, rx: 2, fill: colours[i % colours.length],
    }));
    const label = `${SEGMENT_LABEL[step.listing_id] ?? step.listing_id} ${trim(step.amount)}`;
    legend.appendChild(text(lx + 14, TOP + BAR + 21, label, { class: "c-mute" }));
    lx += 14 + label.length * 6.1 + 22;
  });
  s.appendChild(legend);
  host.appendChild(s);
}

/* ---------- 0. The hero figure: what actually happens ---------- */

// A reader arriving cold needs the shape of the thing, not a price table.
// This is the run as a flow: a job and a budget go in, a cheap look narrows
// the work, only the narrowed work is paid for, and money is left over.
// Every figure is read from the run report.
function heroFigure(report, host) {
  if (!host) return;
  const steps = report.steps ?? [];
  const outcomes = report.outcomes ?? [];
  if (!steps.length) return;

  const moved = outcomes.filter(o => o.outcome === "supported").length;
  const quiet = outcomes.length - moved;
  const W = 580, H = 392;
  const s = svg("svg", {
    viewBox: `0 0 ${W} ${H}`, class: "hero-figure", role: "img",
    "aria-label":
      `A job and a budget go in. A cheap scan of ${outcomes.length} pools finds ${moved} that moved. ` +
      `Only that one is investigated. ${trim(report.totals.settled)} of ${"0.0100"} HBAR is spent.`,
  });

  const defs = svg("defs");
  const g1 = svg("linearGradient", { id: "hf-g", x1: 0, y1: 0, x2: 1, y2: 0 });
  g1.appendChild(svg("stop", { offset: "0%", "stop-color": "#3f9d76" }));
  g1.appendChild(svg("stop", { offset: "100%", "stop-color": "#6fe0ac" }));
  defs.appendChild(g1);
  const arrow = svg("marker", {
    id: "hf-ar", viewBox: "0 0 8 8", refX: 7, refY: 4,
    markerWidth: 7, markerHeight: 7, orient: "auto",
  });
  arrow.appendChild(svg("path", { d: "M0 0 L8 4 L0 8 z", fill: "#4e8a70" }));
  defs.appendChild(arrow);
  s.appendChild(defs);

  const L = 22;                       // left rail for the pills
  let y = 6;

  // --- what goes in
  const inbox = svg("g");
  inbox.appendChild(svg("rect", {
    x: L, y, width: W - L * 2, height: 56, rx: 11,
    fill: "rgba(255,255,255,.05)", stroke: "#2f5c4a",
  }));
  inbox.appendChild(text(L + 18, y + 24, "THE JOB", { class: "hf-cap" }));
  inbox.appendChild(text(L + 18, y + 44, `Check ${outcomes.length} pools, explain what moved`, { class: "hf-line" }));
  inbox.appendChild(text(W - L - 18, y + 24, "BUDGET", { class: "hf-cap", "text-anchor": "end" }));
  inbox.appendChild(text(W - L - 18, y + 44, "0.0100 HBAR", { class: "hf-line mono", "text-anchor": "end" }));
  animate(inbox, "opacity", 0, 1, "0.4s", "0.1s");
  s.appendChild(inbox);
  y += 56;

  // --- each purchase, with what it bought and what it revealed
  const BUY = [
    { key: "screen",
      what: `Look at all ${outcomes.length} cheaply`,
      found: `${quiet} quiet, ${moved} moved` },
    { key: "events",
      what: `Buy the detail for that ${moved}`,
      found: "the trades that moved it" },
    { key: "explain",
      what: "Write the answer",
      found: "every figure traced to evidence" },
  ];

  BUY.forEach((b, i) => {
    const step = steps.find(x => x.listing_id === b.key) ?? steps[i];
    if (!step) return;
    const gy = y + 16 + i * 74;

    // Connector down the left rail.
    const conn = svg("path", {
      d: `M${L + 26} ${gy - 16} L${L + 26} ${gy + 4}`,
      stroke: "#4e8a70", "stroke-width": 1.6, "marker-end": "url(#hf-ar)",
    });
    animate(conn, "opacity", 0, 1, "0.3s", `${0.35 + i * 0.3}s`);
    s.appendChild(conn);

    const g = svg("g");
    g.appendChild(svg("rect", {
      x: L, y: gy + 8, width: W - L * 2, height: 58, rx: 11,
      fill: "rgba(255,255,255,.05)", stroke: "#2f5c4a",
    }));
    // The price it paid, as a tag on the right.
    g.appendChild(svg("rect", {
      x: W - L - 96, y: gy + 21, width: 82, height: 24, rx: 12,
      fill: "rgba(111,224,172,.13)",
    }));
    g.appendChild(text(W - L - 55, gy + 37, trim(step.amount), {
      class: "hf-pay", "text-anchor": "middle",
    }));

    g.appendChild(text(L + 18, gy + 32, b.what, { class: "hf-line" }));
    g.appendChild(text(L + 18, gy + 52, b.found, { class: "hf-found" }));
    animate(g, "opacity", 0, 1, "0.4s", `${0.45 + i * 0.3}s`);
    s.appendChild(g);
  });

  // --- what is left
  const fy = y + 16 + 3 * 74 + 12;
  const conn = svg("path", {
    d: `M${L + 26} ${fy - 24} L${L + 26} ${fy - 4}`,
    stroke: "#4e8a70", "stroke-width": 1.6, "marker-end": "url(#hf-ar)",
  });
  animate(conn, "opacity", 0, 1, "0.3s", "1.35s");
  s.appendChild(conn);

  const out = svg("g");
  out.appendChild(text(L, fy + 34, trim(report.totals.settled), { class: "hf-big" }));
  out.appendChild(text(L + 128, fy + 34, "spent", { class: "hf-line" }));
  out.appendChild(text(W - L, fy + 18, `${trim(report.totals.unspent)} HBAR`, {
    class: "hf-left", "text-anchor": "end",
  }));
  out.appendChild(text(W - L, fy + 36, "never touched", {
    class: "hf-sub", "text-anchor": "end",
  }));
  animate(out, "opacity", 0, 1, "0.45s", "1.45s");
  s.appendChild(out);

  host.appendChild(s);
}

/* ---------- 2. How the cheap scan changed every price ---------- */

const ROUTE = { staged: "Step by step", hybrid: "Mixed", bundle: "All at once" };
const ROUTE_SUB = {
  staged: "scan, then buy only what is needed",
  hybrid: "scan, then one full investigation",
  bundle: "investigate everything up front",
};

// The centrepiece. Two columns of bars, one per planning round, with a ribbon
// between them showing how each route's price moved once the cheap scan came
// back. The whole point of the system is in this one picture: spending a
// little to learn something made everything else cheaper.
function replanChart(report, host) {
  const rounds = (report.planning ?? []).filter(r => r.plans.some(p => p.expected > 0)).slice(0, 2);
  if (rounds.length < 2 || !host) return;

  const W = 1000, H = 420;
  const L = 210, R = 150, T = 96, B = 92;
  const plotW = W - L - R, plotH = H - T - B;
  const max = Math.max(...rounds.flatMap(r => r.plans.map(p => p.expected)));
  const xs = [L, L + plotW];
  const y = v => T + plotH - (v / (max * 1.08)) * plotH;

  const s = svg("svg", {
    viewBox: `0 0 ${W} ${H}`, class: "chart chart-hero", role: "img",
    "aria-label": "How the price of every route changed after a cheap first scan",
  });

  const defs = svg("defs");
  for (const [id, c] of [["g-staged", "#2d7a58"], ["g-hybrid", "#9a8440"], ["g-bundle", "#8a6650"]]) {
    const lg = svg("linearGradient", { id, x1: 0, y1: 0, x2: 1, y2: 0 });
    lg.appendChild(svg("stop", { offset: "0%", "stop-color": c, "stop-opacity": .16 }));
    lg.appendChild(svg("stop", { offset: "100%", "stop-color": c, "stop-opacity": .34 }));
    defs.appendChild(lg);
  }
  s.appendChild(defs);

  // Column headings, each with what the runtime knew at that moment.
  const heads = [
    ["Before the scan", `all ${rounds[0].pending.length} pools still unknown`],
    ["After the scan", `${rounds[1].pending.length} pool actually moved`],
  ];
  heads.forEach(([a, b], i) => {
    const anchor = i === 0 ? "start" : "end";
    s.appendChild(text(xs[i], T - 52, a, { class: "c-head", "text-anchor": anchor }));
    s.appendChild(text(xs[i], T - 30, b, { class: "c-mute", "text-anchor": anchor }));
  });

  s.appendChild(svg("line", {
    x1: L - 18, y1: T - 16, x2: W - R + 18, y2: T - 16,
    stroke: "#e2dfd0",
  }));

  const colour = { staged: "#2d7a58", hybrid: "#9a8440", bundle: "#8a6650" };

  for (const kind of ["bundle", "hybrid", "staged"]) {
    const a = rounds[0].plans.find(p => p.kind === kind);
    const b = rounds[1].plans.find(p => p.kind === kind);
    if (!a || !b) continue;
    const chosen = rounds[1].chosen === kind;
    const g = svg("g", { class: chosen ? "route chosen" : "route" });

    // A filled ribbon from the old price to the new one reads as movement in
    // a way two dots joined by a line does not.
    // Two routes can converge on the same purchase once only one pool is
    // left. That is a real finding, so both stay visible and it is labelled.
    const twin = rounds[1].plans.filter(p => p.expected === b.expected).length > 1;
    const nudge = twin && kind !== "hybrid" ? 0 : (twin ? 13 : 0);
    const ya = y(a.expected), yb = y(b.expected) + nudge;
    const band = 9;
    const ribbon = svg("path", {
      d: `M${xs[0]} ${ya - band} C${xs[0] + plotW * .42} ${ya - band}, ${xs[1] - plotW * .42} ${yb - band}, ${xs[1]} ${yb - band}` +
         `L${xs[1]} ${yb + band} C${xs[1] - plotW * .42} ${yb + band}, ${xs[0] + plotW * .42} ${ya + band}, ${xs[0]} ${ya + band} Z`,
      fill: `url(#g-${kind})`, stroke: "none",
    });
    animate(ribbon, "opacity", 0, chosen ? 1 : .55, "0.7s", "0.35s");
    g.appendChild(ribbon);

    const spine = svg("path", {
      d: `M${xs[0]} ${ya} C${xs[0] + plotW * .42} ${ya}, ${xs[1] - plotW * .42} ${yb}, ${xs[1]} ${yb}`,
      fill: "none", stroke: colour[kind],
      "stroke-width": chosen ? 2.4 : 1.4, opacity: chosen ? 1 : .5,
      "stroke-dasharray": 900,
    });
    animate(spine, "stroke-dashoffset", 900, 0, "1s", "0.3s");
    g.appendChild(spine);

    // End caps and their prices.
    [[0, a, ya], [1, b, yb]].forEach(([i, plan, yy]) => {
      g.appendChild(svg("circle", {
        cx: xs[i], cy: yy, r: chosen ? 6 : 4.5,
        fill: "#fff", stroke: colour[kind], "stroke-width": chosen ? 2.6 : 1.8,
        opacity: chosen ? 1 : .65,
      }));
      const price = text(
        i === 0 ? xs[0] - 16 : xs[1] + 16, yy + 5,
        trim(amountOf(plan.expected)),
        { class: chosen ? "c-price strong" : "c-price", "text-anchor": i === 0 ? "end" : "start" },
      );
      g.appendChild(price);
    });

    // Route name on the left, outside the plot.
    g.appendChild(text(28, ya - 3, ROUTE[kind], {
      class: chosen ? "c-route strong" : "c-route",
    }));
    g.appendChild(text(28, ya + 15, ROUTE_SUB[kind], { class: "c-mute" }));

    if (chosen) {
      const badge = svg("g");
      const bx = xs[1] + 16, by = yb + 22;
      badge.appendChild(svg("rect", {
        x: bx, y: by, width: 62, height: 21, rx: 10.5, fill: "#2d7a58",
      }));
      badge.appendChild(text(bx + 31, by + 14.5, "chosen",
        { class: "c-badge", "text-anchor": "middle" }));
      animate(badge, "opacity", 0, 1, "0.4s", "1.2s");
      g.appendChild(badge);
    }
    s.appendChild(g);
  }

  // The takeaway, stated once under the plot.
  // Note the convergence where it happens, next to the pair.
  const tied = rounds[1].plans.filter(p =>
    rounds[1].plans.filter(q => q.expected === p.expected).length > 1);
  if (tied.length > 1) {
    const ty = y(tied[0].expected);
    const noteG = svg("g");
    noteG.appendChild(text(xs[1] + 92, ty + 10,
      "same purchase now", { class: "c-mute" }));
    noteG.appendChild(svg("path", {
      d: `M${xs[1] + 84} ${ty + 6} l-8 0`, stroke: "#b9b4a0", "stroke-width": 1,
    }));
    animate(noteG, "opacity", 0, 1, "0.4s", "1.3s");
    s.appendChild(noteG);
  }

  const bundleBefore = rounds[0].plans.find(p => p.kind === "bundle");
  const bundleAfter = rounds[1].plans.find(p => p.kind === "bundle");
  const foot = svg("g");
  foot.appendChild(svg("line", {
    x1: L - 18, y1: H - 54, x2: W - R + 18, y2: H - 54, stroke: "#e2dfd0",
  }));
  foot.appendChild(text(L - 18, H - 28,
    `A 0.0010 scan cut the dearest route from ${trim(amountOf(bundleBefore.expected))} to ${trim(amountOf(bundleAfter.expected))}.`,
    { class: "c-foot" }));
  foot.appendChild(text(L - 18, H - 10,
    "Knowing which pools were quiet made every remaining option cheaper.",
    { class: "c-mute" }));
  animate(foot, "opacity", 0, 1, "0.5s", "1.4s");
  s.appendChild(foot);

  host.appendChild(s);
}

/* ---------- 3. The loop it runs ---------- */

function loopDiagram(host) {
  if (!host) return;
  const W = 1000, H = 152;
  const s = svg("svg", {
    viewBox: `0 0 ${W} ${H}`, class: "chart loop", role: "img",
    "aria-label": "Ask the price, compare routes, buy one thing, look again",
  });
  const steps = [
    ["Ask the price", "sellers quote the real job"],
    ["Compare routes", "cheapest that can still finish"],
    ["Buy one thing", "one payment, confirmed on-chain"],
    ["Look again", "is the plan still right?"],
  ];
  const boxW = 214, gap = (W - boxW * 4) / 3, top = 8, boxH = 78;

  steps.forEach(([title, sub], i) => {
    const x = i * (boxW + gap);
    const g = svg("g");
    g.appendChild(svg("rect", {
      x, y: top, width: boxW, height: boxH, rx: 12,
      fill: "#fff", stroke: "#dcd8c8",
    }));
    g.appendChild(svg("circle", { cx: x + 26, cy: top + 27, r: 12, fill: "#e4efe8" }));
    g.appendChild(text(x + 26, top + 31, String(i + 1), {
      class: "lp-n", "text-anchor": "middle",
    }));
    g.appendChild(text(x + 48, top + 32, title, { class: "lp-t" }));
    g.appendChild(text(x + 20, top + 58, sub, { class: "lp-s" }));

    if (i < 3) {
      const ax = x + boxW;
      g.appendChild(svg("path", {
        d: `M${ax + 9} ${top + boxH / 2} L${ax + gap - 9} ${top + boxH / 2}`,
        stroke: "#c7c2ae", "stroke-width": 1.5, "marker-end": "url(#ar)",
      }));
    }
    animate(g, "opacity", 0, 1, "0.4s", `${i * 0.16}s`);
    s.appendChild(g);
  });

  // The return arc is the point: it re-plans rather than running once.
  const y0 = top + boxH;
  const back = svg("path", {
    d: `M${W - boxW / 2} ${y0 + 4} L${W - boxW / 2} ${y0 + 26} Q${W - boxW / 2} ${y0 + 38} ${W - boxW / 2 - 12} ${y0 + 38} L${W / 2 + 82} ${y0 + 38} M${W / 2 - 82} ${y0 + 38} L${boxW / 2 + 12} ${y0 + 38} Q${boxW / 2} ${y0 + 38} ${boxW / 2} ${y0 + 26} L${boxW / 2} ${y0 + 8}`,
    fill: "none", stroke: "#2d7a58", "stroke-width": 1.6,
    "stroke-dasharray": "6 5", "marker-end": "url(#ar-g)",
  });
  animate(back, "opacity", 0, 1, "0.5s", "0.8s");
  s.appendChild(back);
  const lbl = text(W / 2, y0 + 34, "after every purchase", {
    class: "lp-loop", "text-anchor": "middle",
  });
  animate(lbl, "opacity", 0, 1, "0.4s", "1s");
  s.appendChild(lbl);

  const defs = svg("defs");
  for (const [id, fill] of [["ar", "#c7c2ae"], ["ar-g", "#2d7a58"]]) {
    const m = svg("marker", {
      id, viewBox: "0 0 8 8", refX: 6, refY: 4,
      markerWidth: 6, markerHeight: 6, orient: "auto",
    });
    m.appendChild(svg("path", { d: "M0 0 L8 4 L0 8 z", fill }));
    defs.appendChild(m);
  }
  s.appendChild(defs);
  host.appendChild(s);
}

/* ---------- shared ---------- */

function amountOf(tinybar) {
  const s = String(Math.abs(tinybar)).padStart(9, "0");
  return `${s.slice(0, -8)}.${s.slice(-8)}`;
}
function trim(v) {
  const t = String(v);
  return t.includes(".") ? t.replace(/(\.\d{4}?\d*?)0+$/, "$1") : t;
}

window.MandateCharts = { heroFigure, budgetBar, replanChart, loopDiagram };
