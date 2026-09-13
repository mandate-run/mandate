// Three drawings, each built from the same run reports the tables use. No
// chart library: the shapes are simple and the numbers must stay honest, so
// every coordinate is computed from a real figure rather than hand-placed.
const NS = "http://www.w3.org/2000/svg";

// The drawings share one palette with the page. SVG attributes cannot read
// CSS custom properties reliably across browsers, so the values live here
// once rather than scattered through each figure.
const C = {
  sealed:   "#2f6b4f",   // money committed
  sealedDim:"#4d8168",
  retained: "#9d4e1b",   // money held back: the page's point
  leaf:     "#3d8560",
  mid:      "#8a7a4a",   // the middle route
  far:      "#9a6b4a",   // the dearest route
  ink:      "#1b2a24",
  rule:     "#cfc3a8",   // the printed rule
  rule2:    "#b9ab8c",
  track:    "#ded4bd",   // unfilled column
  soft:     "rgba(47,107,79,.1)",
  onDeep:   "#24402f",
  onDeep2:  "#3a5a46",
};

// One drawing system, so four figures look like one family. Stroke weights,
// corner treatment and bar heights are chosen once here rather than per chart.
const D = {
  hair:   1,      // rules and dividers
  line:   1.75,   // connectors and unemphasised series
  bold:   2.75,   // the series that matters
  bar:    18,     // every horizontal bar is this tall
  pill:   9,      // radius that makes a bar a pill: bar / 2
  card:   12,     // matches the page's --r
  dot:    5,      // end caps
  dotBig: 7,
};

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

// Gradients and the one soft shadow, defined once per figure that needs them.
function sharedDefs(idPrefix, pairs) {
  const defs = svg("defs");
  for (const [name, from, to] of pairs) {
    const lg = svg("linearGradient", {
      id: `${idPrefix}-${name}`, x1: 0, y1: 0, x2: 1, y2: 0,
    });
    lg.appendChild(svg("stop", { offset: "0%", "stop-color": from }));
    lg.appendChild(svg("stop", { offset: "100%", "stop-color": to }));
    defs.appendChild(lg);
  }
  const f = svg("filter", {
    id: `${idPrefix}-lift`, x: "-20%", y: "-40%", width: "140%", height: "200%",
  });
  f.appendChild(svg("feDropShadow", {
    dx: 0, dy: 1, stdDeviation: 1.5,
    "flood-color": "#16241d", "flood-opacity": .16,
  }));
  defs.appendChild(f);
  return defs;
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
  const W = 1000, H = 72, BAR = D.bar, TOP = 26;
  const budget = 1_000_000; // 0.0100 HBAR, the mandate's service budget
  const s = svg("svg", {
    viewBox: `0 0 ${W} ${H}`, class: "chart", role: "img",
    "aria-label": "How much of the budget was spent",
  });

  s.appendChild(sharedDefs("bb", [
    ["a", C.sealed, C.leaf],
    ["b", C.leaf, "#57c294"],
    ["c", "#57c294", C.leaf],
  ]));

  s.appendChild(svg("rect", {
    x: 0, y: TOP, width: W, height: BAR, rx: D.pill, fill: C.track,
  }));

  // Every segment is drawn square and clipped to one pill, so the filled
  // run has clean ends however many purchases there were.
  const clip = svg("clipPath", { id: "bb-clip" });
  clip.appendChild(svg("rect", { x: 0, y: TOP, width: W, height: BAR, rx: D.pill }));
  s.appendChild(clip);
  const band = svg("g", { "clip-path": "url(#bb-clip)" });

  const colours = ["url(#bb-a)", "url(#bb-b)", "url(#bb-c)"];
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
    band.appendChild(g);
    x += w;
  });

  s.appendChild(band);

  // The line where spending stopped.
  const mark = svg("line", {
    x1: x, y1: TOP - 6, x2: x, y2: TOP + BAR + 6,
    stroke: C.ink, "stroke-width": D.line,
  });
  animate(mark, "opacity", 0, 1, "0.3s", "1.6s");
  s.appendChild(mark);

  // Only the proportion. The table below itemises every purchase, so a
  // legend here would repeat it line for line.
  const spent = text(x - 10, TOP - 14, `${trim(report.totals.settled)} spent`, {
    class: "c-key", "text-anchor": "end",
  });
  animate(spent, "opacity", 0, 1, "0.4s", "1.7s");
  s.appendChild(spent);

  s.appendChild(text(W, TOP - 14, "of a 0.0100 HBAR budget", {
    class: "c-mute", "text-anchor": "end",
  }));
  s.appendChild(text(W, TOP + BAR + 22, `${trim(report.totals.unspent)} never spent`, {
    class: "c-mute", "text-anchor": "end",
  }));

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

  const W = 470, H = 340;
  const s = svg("svg", {
    viewBox: `0 0 ${W} ${H}`, class: "hero-figure", role: "img",
    "aria-label":
      `A job with a ${"0.0100"} HBAR budget. A cheap scan of ${outcomes.length} pools finds ` +
      `${moved} that moved, so only that one is investigated. ` +
      `${trim(report.totals.settled)} spent, ${trim(report.totals.unspent)} left.`,
  });

  // Ruled entries with the prices in one right-hand column, the way a
  // ledger is set. Boxes and arrows were fighting each other for the same
  // space; a rule between rows says the same thing and takes none.
  const PRICE_X = W;                 // prices right-align to the edge
  const LINE = "rgba(255,255,255,.13)";

  // --- the premise
  let y = 22;
  s.appendChild(text(0, y, "The job", { class: "hf-cap" }));
  s.appendChild(text(PRICE_X, y, "Budget", { class: "hf-cap", "text-anchor": "end" }));
  y += 22;
  s.appendChild(text(0, y, `Check ${outcomes.length} pools, explain what moved`, { class: "hf-line" }));
  s.appendChild(text(PRICE_X, y, "0.0100 HBAR", { class: "hf-line mono", "text-anchor": "end" }));

  y += 20;
  s.appendChild(svg("line", { x1: 0, y1: y, x2: W, y2: y, stroke: LINE, "stroke-width": D.hair }));

  // --- what it bought, one entry per purchase
  const BOUGHT = [
    [`Looked at all ${outcomes.length}`, `${quiet} quiet, ${moved} moved`],
    [`Bought the detail for that ${moved}`, "the trades that moved it"],
    ["Wrote the answer", "every figure traced to evidence"],
  ];
  const ROW = 56;

  BOUGHT.forEach(([what, found], i) => {
    const step = steps[i];
    if (!step) return;
    const ry = y + 30 + i * ROW;
    const g = svg("g");
    g.appendChild(text(0, ry, what, { class: "hf-line" }));
    g.appendChild(text(0, ry + 17, found, { class: "hf-found" }));
    g.appendChild(text(PRICE_X, ry, trim(step.amount), {
      class: "hf-pay", "text-anchor": "end",
    }));
    animate(g, "opacity", 0, 1, "0.45s", `${0.2 + i * 0.16}s`);
    s.appendChild(g);

    if (i < BOUGHT.length - 1) {
      const ly = ry + 32;
      s.appendChild(svg("line", {
        x1: 0, y1: ly, x2: W, y2: ly, stroke: LINE, "stroke-width": D.hair,
      }));
    }
  });

  // --- the balance, under a heavier rule as a ledger closes its column
  const by = y + 30 + BOUGHT.length * ROW - 8;
  const foot = svg("g");
  foot.appendChild(svg("line", {
    x1: 0, y1: by, x2: W, y2: by,
    stroke: "rgba(255,255,255,.3)", "stroke-width": 1.5,
  }));
  foot.appendChild(text(0, by + 36, trim(report.totals.settled), { class: "hf-big" }));
  foot.appendChild(text(0, by + 60, "spent of the budget", { class: "hf-sub" }));
  foot.appendChild(text(W, by + 30, trim(report.totals.unspent), {
    class: "hf-left", "text-anchor": "end",
  }));
  foot.appendChild(text(W, by + 54, "never touched", {
    class: "hf-sub", "text-anchor": "end",
  }));
  animate(foot, "opacity", 0, 1, "0.5s", "0.75s");
  s.appendChild(foot);

  host.appendChild(s);
}

/* ---------- 2. How the cheap scan changed every price ---------- */

const ROUTE = { staged: "Step by step", hybrid: "Mixed", bundle: "All at once" };
const ROUTE_SUB = {
  staged: "scan, then buy only what is needed",
  hybrid: "scan, then one full investigation",
  bundle: "investigate everything up front",
};

// The centrepiece: what a cheap look did to the price of everything else.
//
// Route names sit in fixed rows on the left rather than floating at their
// data positions. Two routes can land within 23px of each other, which is
// not enough room for a name and a description, so anchoring labels to the
// data was what made this chart look untidy.
function replanChart(report, host) {
  const rounds = (report.planning ?? []).filter(r => r.plans.some(p => p.expected > 0)).slice(0, 2);
  if (rounds.length < 2 || !host) return;

  const W = 936, H = 382;
  const KEY = 250;                 // fixed legend column
  // Left prices are drawn right-aligned into the gap between the legend and
  // the plot, so that gap must hold the widest price at its largest size.
  const PLOT_L = KEY + 130, PLOT_R = 116;
  const T = 64, B = 88;
  const plotW = W - PLOT_L - PLOT_R, plotH = H - T - B;
  const max = Math.max(...rounds.flatMap(r => r.plans.map(p => p.expected)));
  const xs = [PLOT_L, PLOT_L + plotW];
  const y = v => T + plotH - (v / (max * 1.1)) * plotH;

  const s = svg("svg", {
    viewBox: `0 0 ${W} ${H}`, class: "chart chart-hero", role: "img",
    "aria-label": "How the price of every route changed after a cheap first scan",
  });

  const order = ["bundle", "hybrid", "staged"];
  const colour = { staged: C.sealed, hybrid: C.mid, bundle: C.far };

  s.appendChild(sharedDefs("rp", order.map(k => [k, colour[k], colour[k]])));

  // Column headings.
  [["Before the scan", `all ${rounds[0].pending.length} pools unknown`, xs[0], "start"],
   ["After the scan", `${rounds[1].pending.length} pool actually moved`, xs[1], "end"]]
    .forEach(([a, b, x, anchor]) => {
      s.appendChild(text(x, T - 50, a, { class: "c-head", "text-anchor": anchor }));
      s.appendChild(text(x, T - 28, b, { class: "c-mute", "text-anchor": anchor }));
    });
  s.appendChild(svg("line", {
    x1: 0, y1: T - 14, x2: W, y2: T - 14, stroke: C.rule, "stroke-width": D.hair,
  }));

  // Fixed legend rows: name, description, and the price at each end.
  const rowH = plotH / 3, rowTop = T + 4;
  order.forEach((kind, i) => {
    const a = rounds[0].plans.find(p => p.kind === kind);
    const b = rounds[1].plans.find(p => p.kind === kind);
    if (!a || !b) return;
    const chosen = rounds[1].chosen === kind;
    const ry = rowTop + i * rowH;

    const key = svg("g");
    key.appendChild(svg("rect", {
      x: 0, y: ry - 4, width: KEY, height: rowH - 10, rx: D.card,
      fill: chosen ? C.soft : "transparent",
    }));
    key.appendChild(svg("rect", {
      x: 14, y: ry + 12, width: 14, height: 4, rx: 2, fill: colour[kind],
    }));
    key.appendChild(text(38, ry + 18, ROUTE[kind], {
      class: chosen ? "c-route strong" : "c-route",
    }));
    key.appendChild(text(38, ry + 36, ROUTE_SUB[kind], { class: "c-mute" }));
    animate(key, "opacity", 0, 1, "0.4s", `${0.15 + i * 0.1}s`);
    s.appendChild(key);
  });

  // The lines themselves, with prices only at the ends.
  const placed = [];
  order.forEach((kind, i) => {
    const a = rounds[0].plans.find(p => p.kind === kind);
    const b = rounds[1].plans.find(p => p.kind === kind);
    if (!a || !b) return;
    const chosen = rounds[1].chosen === kind;
    const ya = y(a.expected);
    let yb = y(b.expected);

    // Two routes can converge on the same purchase. Keep both readable.
    while (placed.some(v => Math.abs(v - yb) < 20)) yb += 20;
    placed.push(yb);

    const g = svg("g");
    const spine = svg("path", {
      d: `M${xs[0]} ${ya} C${xs[0] + plotW * .45} ${ya}, ${xs[1] - plotW * .45} ${yb}, ${xs[1]} ${yb}`,
      fill: "none", stroke: colour[kind],
      "stroke-width": chosen ? D.bold : D.line,
      opacity: chosen ? 1 : .42,
      "stroke-linecap": "round", "stroke-dasharray": 900,
    });
    animate(spine, "stroke-dashoffset", 900, 0, "1s", "0.35s");
    g.appendChild(spine);

    [[0, a.expected, ya], [1, b.expected, yb]].forEach(([i2, v, yy]) => {
      g.appendChild(svg("circle", {
        cx: xs[i2], cy: yy, r: chosen ? D.dotBig : D.dot,
        fill: "#fff", stroke: colour[kind],
        "stroke-width": chosen ? D.bold : D.line, opacity: chosen ? 1 : .6,
      }));
      g.appendChild(text(
        i2 === 0 ? xs[0] - 18 : xs[1] + 18, yy + 5, trim(amountOf(v)),
        { class: chosen ? "c-price strong" : "c-price",
          "text-anchor": i2 === 0 ? "end" : "start" },
      ));
    });

    if (chosen) {
      const bx = xs[1] + 18, by = yb + 16;
      const badge = svg("g");
      badge.appendChild(svg("rect", {
        x: bx, y: by, width: 66, height: D.bar, rx: D.pill, fill: C.sealed,
      }));
      badge.appendChild(text(bx + 33, by + 13, "chosen",
        { class: "c-badge", "text-anchor": "middle" }));
      animate(badge, "opacity", 0, 1, "0.4s", "1.25s");
      g.appendChild(badge);
    }
    s.appendChild(g);
  });

  // The takeaway.
  const before = rounds[0].plans.find(p => p.kind === "bundle");
  const after = rounds[1].plans.find(p => p.kind === "bundle");
  const foot = svg("g");
  foot.appendChild(svg("line", {
    x1: 0, y1: H - 62, x2: W, y2: H - 62, stroke: C.rule, "stroke-width": D.hair,
  }));
  foot.appendChild(text(0, H - 34,
    `A ${trim(amountOf(100000))} scan cut the dearest route from ${trim(amountOf(before.expected))} to ${trim(amountOf(after.expected))}.`,
    { class: "c-foot" }));
  foot.appendChild(text(0, H - 14,
    "Knowing which pools were quiet made every remaining option cheaper.",
    { class: "c-mute" }));
  animate(foot, "opacity", 0, 1, "0.5s", "1.45s");
  s.appendChild(foot);

  host.appendChild(s);
}

/* ---------- 3. The loop it runs ---------- */

function loopDiagram(host) {
  if (!host) return;
  const W = 1000, H = 150;
  const s = svg("svg", {
    viewBox: `0 0 ${W} ${H}`, class: "chart loop", role: "img",
    "aria-label": "Ask the price, compare routes, buy one thing, look again",
  });
  const steps = [
    ["Ask the price", "sellers quote the job"],
    ["Compare routes", "cheapest that finishes"],
    ["Buy one thing", "one payment, confirmed"],
    ["Look again", "is the plan still right?"],
  ];
  const boxW = 214, gap = (W - boxW * 4) / 3, top = 8, boxH = 78;

  steps.forEach(([title, sub], i) => {
    const x = i * (boxW + gap);
    const g = svg("g");
    g.appendChild(svg("rect", {
      x, y: top, width: boxW, height: boxH, rx: D.card,
      fill: "#fff", stroke: C.rule,
    }));
    g.appendChild(svg("circle", { cx: x + 26, cy: top + 27, r: 12, fill: C.soft }));
    g.appendChild(text(x + 26, top + 31, String(i + 1), {
      class: "lp-n", "text-anchor": "middle",
    }));
    g.appendChild(text(x + 48, top + 32, title, { class: "lp-t" }));
    g.appendChild(text(x + 20, top + 58, sub, { class: "lp-s" }));

    if (i < 3) {
      const ax = x + boxW;
      g.appendChild(svg("path", {
        d: `M${ax + 9} ${top + boxH / 2} L${ax + gap - 9} ${top + boxH / 2}`,
        stroke: C.rule2, "stroke-width": D.line, "marker-end": "url(#ar)",
      }));
    }
    animate(g, "opacity", 0, 1, "0.4s", `${i * 0.16}s`);
    s.appendChild(g);
  });

  // The return arc is the point: it re-plans rather than running once.
  const y0 = top + boxH;
  const back = svg("path", {
    d: `M${W - boxW / 2} ${y0 + 4} L${W - boxW / 2} ${y0 + 26} Q${W - boxW / 2} ${y0 + 38} ${W - boxW / 2 - 12} ${y0 + 38} L${W / 2 + 96} ${y0 + 38} M${W / 2 - 96} ${y0 + 38} L${boxW / 2 + 12} ${y0 + 38} Q${boxW / 2} ${y0 + 38} ${boxW / 2} ${y0 + 26} L${boxW / 2} ${y0 + 8}`,
    fill: "none", stroke: C.sealed, "stroke-width": D.line,
    "stroke-dasharray": "6 5", "marker-end": "url(#ar-g)",
  });
  animate(back, "opacity", 0, 1, "0.5s", "0.8s");
  s.appendChild(back);
  const lbl = text(W / 2, y0 + 42, "after every purchase", {
    class: "lp-loop", "text-anchor": "middle",
  });
  animate(lbl, "opacity", 0, 1, "0.4s", "1s");
  s.appendChild(lbl);

  const defs = sharedDefs("lp", []);
  for (const [id, fill] of [["ar", C.rule2], ["ar-g", C.sealed]]) {
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
