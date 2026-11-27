// Authentic Figments Spatial Canvas UI (OKLCH Darkroom & Paper/Daylight Theme Rooms)
//   - Powered by pure-Hirsel `hirsel-board` Place (cards, elbow connectors, pan/zoom camera)
//   - Rendered via REAL Chrome 160 HTML-in-Canvas (`<canvas content="drawable" layoutsubtree>` +
//     `<div drawable>` + `paint` event + `requestPaint()` + `ctx.drawElementImage()` + `DOMMatrix`)
//   - Live Pure-Hirsel Autonomous Agent (`hirsel-agent`), `hirsel-js` Codemode, Linux 6.1.102 `vm-dev`,
//     and Isolated `egress_gw.cm` (TLS 1.3 + SAS-only sealed secret vault).

pub const FIGMENTS_INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<title>Hirsel-OS // Figments Spatial Workstation (Chromium 160 HTML-in-Canvas)</title>
<style>
  * { box-sizing: border-box; margin: 0; padding: 0; }
  :root {
    --ground: oklch(0.17 0.007 62);
    --paper: oklch(0.21 0.008 64);
    --raised: oklch(0.25 0.009 66);
    --line: oklch(0.935 0.013 82 / 0.14);
    --ink: oklch(0.935 0.013 82);
    --ink-2: oklch(0.75 0.014 78);
    --cobalt: #3b82f6;
    --iris: #8b5cf6;
    --vermilion: #ef4444;
    --sap: #10b981;
    --ochre: #f59e0b;
    --dot: rgba(238, 232, 220, 0.12);
  }
  body.daylight {
    --ground: oklch(0.952 0.012 78);
    --paper: oklch(0.978 0.008 82);
    --raised: oklch(0.992 0.004 85);
    --line: oklch(0.205 0.01 60 / 0.15);
    --ink: oklch(0.205 0.01 60);
    --ink-2: oklch(0.42 0.013 62);
    --dot: rgba(40, 35, 30, 0.14);
  }
  body {
    background: var(--ground);
    color: var(--ink);
    font-family: ui-sans-serif, system-ui, -apple-system, "Noto Sans", sans-serif;
    height: 100vh;
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 7px 12px;
    background: var(--paper);
    border-bottom: 1px solid var(--line);
    gap: 8px;
  }
  .title { font-weight: 700; font-size: 13px; letter-spacing: 0.35px; }
  .ua { font-family: ui-monospace, monospace; font-size: 10px; color: var(--ink-2); }
  .toolbar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 5px;
    padding: 6px 12px;
    background: var(--paper);
    border-bottom: 1px solid var(--line);
  }
  button {
    background: #2563eb;
    color: #fff;
    border: 1px solid rgba(255,255,255,0.14);
    border-radius: 6px;
    padding: 5px 8px;
    font-size: 10.5px;
    font-weight: 600;
    cursor: pointer;
  }
  button.sec { background: #1e293b; color: #e2e8f0; }
  body.daylight button.sec { background: #e2e8f0; color: #0f172a; border-color: rgba(15,23,42,0.15); }
  button.green { background: #047857; }
  button.purple { background: #6d28d9; }
  button.amber { background: #b45309; }
  button.red { background: #b91c1c; }
  main {
    flex: 1;
    display: grid;
    grid-template-columns: minmax(0, 1fr) 310px;
    min-height: 0;
  }
  @media (max-width: 960px) {
    main { grid-template-columns: minmax(0, 1fr) 244px; }
  }
  .stage {
    position: relative;
    overflow: hidden;
    min-width: 0;
    background: var(--ground);
  }
  canvas#spatial {
    width: 100%;
    height: 100%;
    display: block;
    cursor: grab;
  }
  canvas#spatial:active { cursor: grabbing; }
  /* Real Chrome 160 HTML-in-Canvas drawable Figment cards */
  .hic-card {
    width: 244px;
    padding: 9px 11px;
    border-radius: 10px;
    background: #181a20;
    border: 2px solid var(--cobalt);
    color: #eef1f7;
    font-family: ui-sans-serif, system-ui, sans-serif;
    box-shadow: 0 8px 22px rgba(0, 0, 0, 0.36);
  }
  body.daylight .hic-card {
    background: #fdfcf9;
    color: #1c1917;
    box-shadow: 0 6px 18px rgba(45, 35, 25, 0.14);
  }
  .hic-card .h { font-weight: 700; font-size: 11.5px; margin-bottom: 4px; color: #93c5fd; }
  body.daylight .hic-card .h { color: #1d4ed8; }
  .hic-card .s { font-family: ui-monospace, monospace; font-size: 10px; color: #cbd5e1; line-height: 1.36; word-break: break-word; }
  body.daylight .hic-card .s { color: #44403c; }
  .hic-card .pill {
    display: inline-block;
    margin-top: 6px;
    padding: 2px 6px;
    border-radius: 4px;
    font-size: 9px;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.4px;
    background: rgba(59,130,246,0.22);
    color: #93c5fd;
  }
  body.daylight .hic-card .pill { background: rgba(37,99,235,0.14); color: #1d4ed8; }
  .hic-card.iris, .hic-card.purple { border-color: var(--iris); }
  .hic-card.iris .h, .hic-card.purple .h { color: #c4b5fd; }
  body.daylight .hic-card.iris .h { color: #6d28d9; }
  .hic-card.iris .pill { background: rgba(139,92,246,0.24); color: #ddd6fe; }
  body.daylight .hic-card.iris .pill { background: rgba(109,40,217,0.12); color: #6d28d9; }
  .hic-card.sap, .hic-card.green { border-color: var(--sap); }
  .hic-card.sap .h, .hic-card.green .h { color: #6ee7b7; }
  body.daylight .hic-card.sap .h { color: #047857; }
  .hic-card.sap .pill { background: rgba(16,185,129,0.24); color: #a7f3d0; }
  body.daylight .hic-card.sap .pill { background: rgba(4,120,87,0.12); color: #047857; }
  .hic-card.vermilion, .hic-card.red { border-color: var(--vermilion); }
  .hic-card.vermilion .h, .hic-card.red .h { color: #fca5a5; }
  body.daylight .hic-card.vermilion .h { color: #b91c1c; }
  .hic-card.vermilion .pill { background: rgba(239,68,68,0.24); color: #fecaca; }
  body.daylight .hic-card.vermilion .pill { background: rgba(185,28,28,0.12); color: #b91c1c; }
  .hic-card.ochre, .hic-card.amber { border-color: var(--ochre); }
  .hic-card.ochre .h, .hic-card.amber .h { color: #fcd34d; }
  body.daylight .hic-card.ochre .h { color: #b45309; }
  .hic-card.ochre .pill { background: rgba(245,158,11,0.24); color: #fde68a; }
  body.daylight .hic-card.ochre .pill { background: rgba(180,83,9,0.14); color: #92400e; }
  aside {
    background: var(--paper);
    border-left: 1px solid var(--line);
    padding: 9px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .card {
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 8px 9px;
    background: var(--raised);
  }
  .card h3 { font-size: 9.5px; text-transform: uppercase; letter-spacing: 0.75px; color: var(--ink-2); margin-bottom: 5px; }
  input {
    width: 100%;
    background: var(--ground);
    color: var(--ink);
    border: 1px solid var(--line);
    border-radius: 5px;
    padding: 5px 7px;
    font-family: ui-monospace, monospace;
    font-size: 10px;
    margin-bottom: 5px;
  }
  pre {
    font-family: ui-monospace, monospace;
    font-size: 9.5px;
    line-height: 1.36;
    white-space: pre-wrap;
    word-break: break-word;
    color: var(--ink);
  }
  .badge {
    display: inline-block;
    padding: 2px 7px;
    border-radius: 999px;
    font-size: 9.5px;
    font-weight: 700;
    margin-right: 4px;
  }
  .ok { background: rgba(16,185,129,0.2); color: #34d399; }
  body.daylight .ok { background: rgba(4,120,87,0.14); color: #047857; }
  .warn { background: rgba(245,158,11,0.2); color: #fbbf24; }
</style>
</head>
<body>
  <header>
    <div>
      <div class="title">FIGMENTS SPATIAL WORKSTATION // PURE-HIRSEL BOARD + AGENT + CHROMIUM 160 HTML-IN-CANVAS</div>
      <div class="ua" id="ua">Probing Chromium runtime...</div>
    </div>
    <div id="caps"></div>
  </header>

  <div class="toolbar">
    <button class="purple" onclick="runAgent()">Run Pure-Hirsel Agent</button>
    <button class="red" onclick="stageEffect()">Stage Outbox (F5)</button>
    <button class="green" onclick="cmd('TOGGLE_DEV')">Linux vm-dev (F2)</button>
    <button class="amber" onclick="cmd('TOGGLE_WEB')">Isolated Web #2 (F3)</button>
    <button class="sec" onclick="cmd('TOGGLE_ROOM')">Darkroom / Paper (F4)</button>
    <button class="sec" onclick="addNote()">+ Note Card</button>
    <button class="sec" onclick="zoomBy(10)">Zoom +</button>
    <button class="sec" onclick="zoomBy(-10)">Zoom -</button>
    <button class="sec" onclick="powerboxDemo()">Powerbox -&gt; vm-dev</button>
  </div>

  <main>
    <div class="stage">
      <canvas id="spatial" content="drawable" layoutsubtree></canvas>
    </div>
    <aside>
      <div class="card">
        <h3>Autonomous Pure-Hirsel Agent (js@1 -&gt; Linux -&gt; Board -&gt; Ceremony)</h3>
        <input id="agent-in" value="Audit Zircon + Linux vm-dev and stage report" />
        <button class="purple" onclick="runAgent()">Execute Verified Agent Loop</button>
        <pre id="agent-out" style="margin-top:5px;">Idle (zero ambient caps).</pre>
      </div>
      <div class="card">
        <h3>Pure-Hirsel js@1 Codemode Sandbox</h3>
        <input id="js-in" value="console.log('hi from Zircon'); return [6*7, 'hirsel-js@1']" />
        <button class="sec" onclick="runCodemode()">Eval in Zero-Ambient Job</button>
        <pre id="js-out" style="margin-top:5px;">Ready.</pre>
      </div>
      <div class="card">
        <h3>Isolated Web #2 Origin Switcher</h3>
        <input id="web-url" value="http://127.0.0.1:7712/untrusted" />
        <button class="amber" onclick="navWeb()">Open in Fresh Isolated Context</button>
      </div>
      <div class="card">
        <h3>Pure-Hirsel WM + Board Live State</h3>
        <pre id="wm-state">Connecting...</pre>
      </div>
    </aside>
  </main>

<script>
const HIRSEL_TOKEN = "__HIRSEL_TOKEN__";
function apiPost(url, body) {
  return fetch(url, { method: 'POST', headers: { 'X-Hirsel-Token': HIRSEL_TOKEN }, body });
}
document.getElementById('ua').textContent = navigator.userAgent;
const canvas = document.getElementById('spatial');
const ctx = canvas.getContext('2d');
const hasDrawElement = !!(ctx && typeof ctx.drawElementImage === 'function');
const hasRequestPaint = !!(canvas && typeof canvas.requestPaint === 'function');
const chromeMatch = navigator.userAgent.match(/Chrome\/([\d.]+)/);
const chromeVer = chromeMatch ? chromeMatch[1] : 'unknown';

let drawElementWorked = false;
let drawElementErr = '';
let paintEvents = 0;
let reportedDiag = false;
let boardSnap = { rev: 0, camera: { cx: 0, cy: 0, zoom_pct: 100 }, cards: [], edges: [] };
let lastWm = null;
const domCards = new Map();
let dragCard = null, dragOx = 0, dragOy = 0;
let panActive = false, panSx = 0, panSy = 0, panCx0 = 0, panCy0 = 0;

function updateBadges() {
  document.getElementById('caps').innerHTML =
    `<span class="badge ok">Chromium ${chromeVer}</span>` +
    `<span class="badge ok">Board rev=${boardSnap.rev} (${boardSnap.cards.length} cards)</span>` +
    `<span class="badge ${hasDrawElement ? 'ok' : 'warn'}">HTML-in-Canvas: ${drawElementWorked ? 'drawElementImage LIVE' : 'init'}</span>`;
}
updateBadges();

function reportDiagNow() {
  apiPost('/_hirsel/client-diag', JSON.stringify({
      chromeVer,
      hasDrawElement,
      hasRequestPaint,
      drawElementWorked,
      drawElementErr,
      paintEvents,
      boardRev: boardSnap.rev,
      boardCards: boardSnap.cards.length,
      boardEdges: boardSnap.edges.length
    })).catch(() => {});
}

function escapeHtml(s) {
  return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');
}

function syncBoardDom() {
  const activeIds = new Set();
  const cw = Math.max(300, canvas.width || 600);
  const compactW = cw < 540 ? Math.max(196, Math.floor((cw - 48) / 2)) : 244;
  for (const c of boardSnap.cards) {
    activeIds.add(c.id);
    let el = domCards.get(c.id);
    if (!el) {
      el = document.createElement('div');
      el.id = 'hic-' + c.id;
      el.setAttribute('drawable', '');
      canvas.appendChild(el);
      domCards.set(c.id, el);
    }
    el.className = 'hic-card ' + (c.tone || 'cobalt');
    el.style.width = compactW + 'px';
    let bodyTxt = c.body || '';
    if (c.id === 'wm' && lastWm) {
      bodyTxt = `focus=${lastWm.focused} cap=${lastWm.capture_allowed} | powerbox=${lastWm.powerbox_transfers} room=${lastWm.room}`;
    } else if (c.id === 'outbox' && lastWm) {
      bodyTxt = `outbox=${lastWm.outbox.status} ok=${lastWm.approved} deny=${lastWm.denied} | bank=$${(lastWm.bank_spent_micros/1e6).toFixed(2)}/$${(lastWm.bank_limit_micros/1e6).toFixed(2)}`;
    }
    el.innerHTML =
      `<div class="h">${escapeHtml(c.title)}</div>` +
      `<div class="s">${escapeHtml(bodyTxt)}</div>` +
      `<span class="pill">${escapeHtml(c.kind || 'figment')} // rev ${c.rev}</span>`;
  }
  for (const [id, el] of domCards.entries()) {
    if (!activeIds.has(id)) {
      el.remove();
      domCards.delete(id);
    }
  }
}

function cardScreenPos(c, idx, cw) {
  const zoom = (boardSnap.camera.zoom_pct || 100) / 100;
  const cx = boardSnap.camera.cx || 0;
  const cy = boardSnap.camera.cy || 0;
  // Responsive fit when pane is narrow in 3-way split
  if (cw < 540) {
    const pad = 14, gap = 14;
    const cardW = Math.max(196, Math.floor((cw - pad * 2 - gap) / 2));
    const col = idx % 2;
    const row = Math.floor(idx / 2);
    return { x: pad + col * (cardW + gap) - cx, y: 24 + row * 126 - cy, w: cardW, h: 96 };
  }
  return { x: (c.x - cx) * zoom, y: (c.y - cy) * zoom, w: (c.w || 244) * zoom, h: (c.h || 98) * zoom };
}

function drawElbowEdge(ax, ay, bx, by, label) {
  const midX = (ax + bx) / 2;
  ctx.beginPath();
  ctx.moveTo(ax, ay);
  ctx.lineTo(midX, ay);
  ctx.lineTo(midX, by);
  ctx.lineTo(bx, by);
  ctx.stroke();
  if (label) {
    ctx.fillStyle = document.body.classList.contains('daylight') ? '#475569' : '#94a3b8';
    ctx.font = '9.5px ui-monospace, monospace';
    ctx.fillText(label, midX + 4, (ay + by) / 2 - 4);
  }
}

function drawSpatial() {
  const rect = canvas.getBoundingClientRect();
  const rw = Math.max(300, Math.floor(rect.width));
  const rh = Math.max(240, Math.floor(rect.height));
  if (canvas.width !== rw || canvas.height !== rh) {
    canvas.width = rw;
    canvas.height = rh;
  }
  syncBoardDom();
  ctx.clearRect(0, 0, canvas.width, canvas.height);

  // Figments architectural dot grid
  const step = 28;
  const ox = -((boardSnap.camera.cx || 0) % step);
  const oy = -((boardSnap.camera.cy || 0) % step);
  ctx.fillStyle = document.body.classList.contains('daylight') ? 'rgba(35,30,25,0.16)' : 'rgba(230,224,210,0.13)';
  for (let gx = ox; gx < canvas.width; gx += step) {
    for (let gy = oy; gy < canvas.height; gy += step) {
      ctx.fillRect(gx, gy, 1.5, 1.5);
    }
  }

  // Compute screen coordinates for all cards
  const posById = new Map();
  boardSnap.cards.forEach((c, idx) => {
    posById.set(c.id, cardScreenPos(c, idx, canvas.width));
  });

  // Elbow connectors between cards (from pure-Hirsel hirsel-board)
  ctx.lineWidth = 1.75;
  ctx.strokeStyle = document.body.classList.contains('daylight') ? 'rgba(37,99,235,0.55)' : 'rgba(96,165,250,0.55)';
  for (const e of boardSnap.edges) {
    const a = posById.get(e.from);
    const b = posById.get(e.to);
    if (!a || !b) continue;
    drawElbowEdge(a.x + a.w / 2, a.y + a.h / 2, b.x + b.w / 2, b.y + b.h / 2, e.label);
  }

  // Composite real DOM Figment cards onto the canvas via Chrome 160 ctx.drawElementImage!
  for (const c of boardSnap.cards) {
    const p = posById.get(c.id);
    const el = domCards.get(c.id);
    if (hasDrawElement && el) {
      try {
        const tr = ctx.drawElementImage(el, Math.round(p.x), Math.round(p.y));
        if (tr && typeof tr.toString === 'function') {
          el.style.transform = tr.toString();
        }
        if (!drawElementWorked) {
          drawElementWorked = true;
          drawElementErr = '';
          updateBadges();
          reportDiagNow();
        }
      } catch (err) {
        drawElementErr = String(err && err.message ? err.message : err);
      }
    }
  }

  // Bottom Figments status rail inside canvas
  ctx.fillStyle = drawElementWorked ? '#10b981' : '#f59e0b';
  ctx.font = '10px ui-monospace, monospace';
  ctx.fillText(
    drawElementWorked
      ? `FIGMENTS HTML-IN-CANVAS LIVE (hirsel-board rev=${boardSnap.rev} | zoom=${boardSnap.camera.zoom_pct}% | paints=${paintEvents})`
      : `AWAITING PAINT [${drawElementErr}]`,
    14,
    canvas.height - 14
  );

  if (!reportedDiag && boardSnap.cards.length > 0 && drawElementWorked) {
    reportedDiag = true;
    reportDiagNow();
  }
}

function triggerPaint() {
  if (hasRequestPaint) {
    try { canvas.requestPaint(); } catch (_) {}
  }
  drawSpatial();
}

canvas.addEventListener('paint', () => {
  paintEvents++;
  drawSpatial();
});

if (typeof ResizeObserver !== 'undefined') {
  new ResizeObserver(() => triggerPaint()).observe(canvas);
}

// Drag cards or pan infinite camera; wheel zooms camera via pure-Hirsel hirsel-board!
canvas.addEventListener('pointerdown', (e) => {
  const r = canvas.getBoundingClientRect();
  const mx = e.clientX - r.left, my = e.clientY - r.top;
  for (let i = boardSnap.cards.length - 1; i >= 0; i--) {
    const c = boardSnap.cards[i];
    const p = cardScreenPos(c, i, canvas.width);
    if (mx >= p.x && mx <= p.x + p.w && my >= p.y && my <= p.y + p.h) {
      dragCard = c; dragOx = mx - p.x; dragOy = my - p.y; return;
    }
  }
  panActive = true; panSx = mx; panSy = my;
  panCx0 = boardSnap.camera.cx || 0; panCy0 = boardSnap.camera.cy || 0;
});
window.addEventListener('pointermove', (e) => {
  const r = canvas.getBoundingClientRect();
  const mx = e.clientX - r.left, my = e.clientY - r.top;
  if (dragCard) {
    dragCard.x = Math.max(0, Math.round(mx - dragOx + (boardSnap.camera.cx || 0)));
    dragCard.y = Math.max(0, Math.round(my - dragOy + (boardSnap.camera.cy || 0)));
    triggerPaint();
  } else if (panActive) {
    boardSnap.camera.cx = Math.max(0, Math.round(panCx0 - (mx - panSx)));
    boardSnap.camera.cy = Math.max(0, Math.round(panCy0 - (my - panSy)));
    triggerPaint();
  }
});
window.addEventListener('pointerup', () => {
  if (dragCard) {
    const c = dragCard; dragCard = null;
    apiPost('/_hirsel/board', JSON.stringify({ op: 'move', id: c.id, x: c.x, y: c.y }));
  }
  if (panActive) {
    panActive = false;
    apiPost('/_hirsel/board', JSON.stringify({ op: 'camera', cx: boardSnap.camera.cx, cy: boardSnap.camera.cy, zoom_pct: boardSnap.camera.zoom_pct }));
  }
});
canvas.addEventListener('wheel', (e) => {
  e.preventDefault();
  zoomBy(e.deltaY < 0 ? 10 : -10);
}, { passive: false });

async function zoomBy(delta) {
  const next = Math.max(50, Math.min(200, (boardSnap.camera.zoom_pct || 100) + delta));
  const r = await apiPost('/_hirsel/board', JSON.stringify({ op: 'camera', cx: boardSnap.camera.cx || 0, cy: boardSnap.camera.cy || 0, zoom_pct: next }));
  boardSnap = await r.json();
  updateBadges();
  triggerPaint();
}

async function addNote() {
  const id = 'note-' + (boardSnap.cards.length + 1);
  const r = await apiPost('/_hirsel/board', JSON.stringify({ op: 'put', id, kind: 'note', tone: 'ochre', title: 'Spatial Note #' + boardSnap.cards.length, body: 'Persisted via pure-Hirsel hirsel-board on Fxfs', x: 24, y: 300 }));
  boardSnap = await r.json();
  updateBadges();
  triggerPaint();
}

async function pollState() {
  try {
    const [res, bres] = await Promise.all([fetch('/_hirsel/state'), fetch('/_hirsel/board')]);
    lastWm = await res.json();
    boardSnap = await bres.json();
    document.body.classList.toggle('daylight', lastWm.room === 'daylight');
    document.getElementById('wm-state').textContent = JSON.stringify({ wm: lastWm, board_rev: boardSnap.rev, cards: boardSnap.cards.length, edges: boardSnap.edges.length }, null, 2);
    updateBadges();
    triggerPaint();
  } catch (e) {
    document.getElementById('wm-state').textContent = 'Poll error: ' + e;
  }
}
setInterval(pollState, 1000);
pollState();

async function cmd(c) { await apiPost('/_hirsel/cmd', c); pollState(); }
async function stageEffect() { await apiPost('/_hirsel/stage', ''); pollState(); }
async function runCodemode() {
  const r = await apiPost('/_hirsel/codemode', document.getElementById('js-in').value);
  document.getElementById('js-out').textContent = JSON.stringify(await r.json(), null, 2);
}
async function runAgent() {
  document.getElementById('agent-out').textContent = 'Running pure-Hirsel agent loop...';
  const r = await apiPost('/_hirsel/agent-run', document.getElementById('agent-in').value);
  document.getElementById('agent-out').textContent = JSON.stringify(await r.json(), null, 2);
  pollState();
}
async function navWeb() {
  await apiPost('/_hirsel/web-navigate', JSON.stringify({ url: document.getElementById('web-url').value }));
  pollState();
}
async function powerboxDemo() {
  await apiPost('/_hirsel/clipboard', JSON.stringify({ domain: 'figments', text: 'POWERBOX_SECRET_42_FROM_FIGMENTS' }));
  await apiPost('/_hirsel/powerbox-paste', JSON.stringify({ from: 'figments', to: 'vm_dev' }));
  pollState();
}
</script>
</body>
</html>
"#;

pub const UNTRUSTED_WEB_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<title>Untrusted Web Context // Isolated Chromium Process #2</title>
<style>
  body {
    margin: 0; padding: 16px;
    background: #1c1308; color: #fef3c7;
    font-family: ui-sans-serif, system-ui, sans-serif;
  }
  .banner {
    background: #92400e; color: #fff;
    padding: 9px 12px; border-radius: 8px;
    font-weight: 700; font-size: 12px; margin-bottom: 12px;
  }
  .origin {
    font-family: ui-monospace, monospace; font-size: 11px;
    background: rgba(245,158,11,0.18); border: 1px solid #d97706;
    border-radius: 6px; padding: 6px 9px; margin-bottom: 12px;
  }
  button {
    background: #d97706; color: #fff; border: 0;
    border-radius: 6px; padding: 7px 11px; font-weight: 700; cursor: pointer;
  }
  pre {
    margin-top: 10px; padding: 10px;
    background: rgba(0,0,0,0.4); border-radius: 8px;
    font-family: ui-monospace, monospace; font-size: 11px;
  }
</style>
</head>
<body>
  <div class="banner">ISOLATED CHROMIUM CONTEXT #2 // WEB_UNTRUSTED (AMBER TRUST BORDER)</div>
  <div class="origin" id="orig"></div>
  <p style="font-size:12px;">Separate <code>fuchsia.web.Context</code> (separate browser process, cookie jar, and renderer pool). Navigating to a new origin spawns a brand-new OS-isolated Context.</p>
  <p style="margin-top:10px;"><button onclick="trySteal()">Try Stealing Focus from Figments</button></p>
  <pre id="out">Waiting...</pre>
  <script>
    document.getElementById('orig').textContent = 'ORIGIN: ' + location.origin + ' | PATH: ' + location.pathname;
    document.getElementById('out').textContent = 'User-Agent: ' + navigator.userAgent;
    async function trySteal() {
      const r = await fetch('/_untrusted/spoof-focus', { method: 'POST', body: 'web_untrusted' });
      const st = await r.json();
      document.getElementById('out').textContent =
        'Spoof attempt sent CLIENT_FOCUS web_untrusted.\n' +
        'hirsel-wm authoritative focus: ' + st.focused + '\n' +
        'capture_allowed: ' + st.capture_allowed;
    }
  </script>
</body>
</html>
"#;
