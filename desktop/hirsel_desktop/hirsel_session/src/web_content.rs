// Real Figments Spatial Canvas mounted on Hirsel-OS (Track 2):
//   - Loads REAL Figments `/assets/themes.css` (all 6 OKLCH rooms: darkroom, paper, folio, gallery, blueprint, conservatory)
//   - Loads REAL Figments `/assets/figments-canvas.js` (bundled from figments-hirsel/shell/canvas/{elbow_router,freehand,shapes}.ts)
//   - Obstacle-avoiding A* orthogonal elbow arrows (`FigmentsCanvas.routeElbow` + `autoAnchor` + `labelAnchor`)
//   - Pressure-sensitive freehand ink strokes (`FigmentsCanvas.getStroke`) persisted on Fxfs (`/data/ink.json`)
//   - Chrome 160 HTML-in-Canvas (`<canvas content="drawable" layoutsubtree>` + `<div drawable>` + `ctx.drawElementImage`)
//   - Authenticated via per-boot 128-bit `X-Hirsel-Token` nonce (Track 1)

pub const FIGMENTS_INDEX_HTML: &str = r#"<!doctype html>
<html lang="en" data-theme="darkroom">
<head>
<meta charset="utf-8" />
<title>Hirsel-OS // Figments Spatial Workstation (Real Figments Canvas + Chromium 160 HTML-in-Canvas)</title>
<link rel="stylesheet" href="/assets/themes.css" />
<script src="/assets/figments-canvas.js"></script>
<style>
  * { box-sizing: border-box; margin: 0; padding: 0; }
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
  .title { font-weight: 700; font-size: 12.5px; letter-spacing: 0.35px; }
  .ua { font-family: ui-monospace, monospace; font-size: 9.5px; color: var(--ink-2); }
  .toolbar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 5px;
    padding: 6px 12px;
    background: var(--paper);
    border-bottom: 1px solid var(--line);
  }
  button, select {
    background: var(--cobalt);
    color: var(--on-cobalt);
    border: 1px solid var(--line-strong);
    border-radius: 6px;
    padding: 4px 8px;
    font-size: 10.5px;
    font-weight: 600;
    cursor: pointer;
  }
  button.sec, select { background: var(--raised); color: var(--ink); }
  button.active-tool { background: var(--cobalt); color: var(--on-cobalt); }
  button.green { background: #047857; color: #fff; }
  button.purple { background: #6d28d9; color: #fff; }
  button.amber { background: #b45309; color: #fff; }
  button.red { background: #b91c1c; color: #fff; }
  main {
    flex: 1;
    display: grid;
    grid-template-columns: minmax(0, 1fr) 304px;
    min-height: 0;
  }
  @media (max-width: 960px) {
    main { grid-template-columns: minmax(0, 1fr) 238px; }
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
  canvas#spatial.ink-mode { cursor: crosshair; }
  canvas#spatial:active { cursor: grabbing; }
  .hic-card {
    width: 242px;
    padding: 9px 11px;
    border-radius: 10px;
    background: var(--paper);
    border: 2px solid var(--studio-cobalt-stroke, #3b82f6);
    color: var(--ink);
    font-family: ui-sans-serif, system-ui, sans-serif;
    box-shadow: 0 8px 20px var(--shade-cast, rgba(0,0,0,0.32));
  }
  .hic-card .h { font-weight: 700; font-size: 11.5px; margin-bottom: 4px; color: var(--studio-cobalt-ink, #60a5fa); }
  .hic-card .s { font-family: ui-monospace, monospace; font-size: 10px; color: var(--ink-2); line-height: 1.36; word-break: break-word; }
  .hic-card .pill {
    display: inline-block;
    margin-top: 6px;
    padding: 2px 6px;
    border-radius: 4px;
    font-size: 9px;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.4px;
    background: var(--studio-cobalt-fill, rgba(59,130,246,0.2));
    color: var(--studio-cobalt-ink, #93c5fd);
  }
  .hic-card.iris, .hic-card.purple { border-color: var(--studio-iris-stroke, #8b5cf6); }
  .hic-card.iris .h, .hic-card.purple .h { color: var(--studio-iris-ink, #c4b5fd); }
  .hic-card.iris .pill { background: var(--studio-iris-fill); color: var(--studio-iris-ink); }
  .hic-card.sap, .hic-card.green { border-color: var(--studio-sap-stroke, #10b981); }
  .hic-card.sap .h, .hic-card.green .h { color: var(--studio-sap-ink, #6ee7b7); }
  .hic-card.sap .pill { background: var(--studio-sap-fill); color: var(--studio-sap-ink); }
  .hic-card.vermilion, .hic-card.red { border-color: var(--studio-vermilion-stroke, #ef4444); }
  .hic-card.vermilion .h, .hic-card.red .h { color: var(--studio-vermilion-ink, #fca5a5); }
  .hic-card.vermilion .pill { background: var(--studio-vermilion-fill); color: var(--studio-vermilion-ink); }
  .hic-card.ochre, .hic-card.amber { border-color: var(--studio-ochre-stroke, #f59e0b); }
  .hic-card.ochre .h, .hic-card.amber .h { color: var(--studio-ochre-ink, #fcd34d); }
  .hic-card.ochre .pill { background: var(--studio-ochre-fill); color: var(--studio-ochre-ink); }
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
    border: 1px solid var(--line-strong);
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
  .ok { background: var(--studio-sap-fill); color: var(--studio-sap-ink); }
  .warn { background: var(--studio-ochre-fill); color: var(--studio-ochre-ink); }
</style>
</head>
<body>
  <header>
    <div>
      <div class="title">FIGMENTS ON HIRSEL-OS // REAL ELBOW ROUTER + PERFECT-FREEHAND + CHROMIUM 160 HTML-IN-CANVAS</div>
      <div class="ua" id="ua">Probing Chromium runtime...</div>
    </div>
    <div id="caps"></div>
  </header>

  <div class="toolbar">
    <button id="tool-select" class="active-tool" onclick="setTool('select')">Select / Pan</button>
    <button id="tool-ink" class="sec" onclick="setTool('ink')">Freehand Ink</button>
    <button class="sec" onclick="addNote()">+ Note</button>
    <button class="purple" onclick="runAgent()">Run Agent</button>
    <button class="red" onclick="stageEffect()">Outbox (F5)</button>
    <button class="green" onclick="cmd('TOGGLE_DEV')">vm-dev (F2)</button>
    <button class="amber" onclick="cmd('TOGGLE_WEB')">Web #2 (F3)</button>
    <select id="room-pick" onchange="setThemeRoom(this.value)">
      <option value="darkroom">Room: Darkroom</option>
      <option value="paper">Room: Paper</option>
      <option value="folio">Room: Folio</option>
      <option value="gallery">Room: Gallery</option>
      <option value="blueprint">Room: Blueprint</option>
      <option value="conservatory">Room: Conservatory</option>
    </select>
    <button class="sec" onclick="zoomBy(10)">+</button>
    <button class="sec" onclick="zoomBy(-10)">-</button>
  </div>

  <main>
    <div class="stage">
      <canvas id="spatial" content="drawable" layoutsubtree></canvas>
    </div>
    <aside>
      <div class="card">
        <h3>Autonomous Pure-Hirsel Agent (FIDL TLS 1.3 + Ceremony)</h3>
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
const FC = window.FigmentsCanvas || null;
const chromeMatch = navigator.userAgent.match(/Chrome\/([\d.]+)/);
const chromeVer = chromeMatch ? chromeMatch[1] : 'unknown';

let drawElementWorked = false;
let drawElementErr = '';
let paintEvents = 0;
let reportedDiag = false;
let activeTool = 'select';
let customTheme = null;
let boardSnap = { rev: 0, camera: { cx: 0, cy: 0, zoom_pct: 100 }, cards: [], edges: [] };
let inkStrokes = [];
let activeInk = null;
let lastWm = null;
const domCards = new Map();
let dragCard = null, dragOx = 0, dragOy = 0;
let panActive = false, panSx = 0, panSy = 0, panCx0 = 0, panCy0 = 0;

function setTool(t) {
  activeTool = t;
  document.getElementById('tool-select').className = t === 'select' ? 'active-tool' : 'sec';
  document.getElementById('tool-ink').className = t === 'ink' ? 'active-tool' : 'sec';
  canvas.classList.toggle('ink-mode', t === 'ink');
}

function setThemeRoom(room) {
  customTheme = room;
  document.documentElement.setAttribute('data-theme', room);
  triggerPaint();
}

function updateBadges() {
  document.getElementById('caps').innerHTML =
    `<span class="badge ok">Chromium ${chromeVer}</span>` +
    `<span class="badge ${FC ? 'ok' : 'warn'}">Figments Engine: ${FC ? 'A* Elbow + Freehand' : 'missing'}</span>` +
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
    figmentsEngineLoaded: !!(FC && typeof FC.routeElbow === 'function' && typeof FC.getStroke === 'function'),
    paintEvents,
    boardRev: boardSnap.rev,
    boardCards: boardSnap.cards.length,
    boardEdges: boardSnap.edges.length,
    inkStrokes: inkStrokes.length
  })).catch(() => {});
}

function escapeHtml(s) {
  return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');
}

function syncBoardDom() {
  const activeIds = new Set();
  const cw = Math.max(300, canvas.width || 600);
  const compactW = cw < 540 ? Math.max(196, Math.floor((cw - 48) / 2)) : 242;
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
  if (cw < 540) {
    const pad = 14, gap = 14;
    const cardW = Math.max(196, Math.floor((cw - pad * 2 - gap) / 2));
    const col = idx % 2;
    const row = Math.floor(idx / 2);
    return { x: pad + col * (cardW + gap) - cx, y: 24 + row * 126 - cy, w: cardW, h: 96 };
  }
  return { x: (c.x - cx) * zoom, y: (c.y - cy) * zoom, w: (c.w || 242) * zoom, h: (c.h || 98) * zoom };
}

function drawStrokeOutline(pts, color) {
  if (!pts || pts.length < 2) return;
  const outline = FC && typeof FC.getStroke === 'function'
    ? FC.getStroke(pts, { size: 5, thinning: 0.45, smoothing: 0.5, streamline: 0.5, last: true })
    : pts;
  if (!outline || outline.length < 2) return;
  ctx.fillStyle = color;
  ctx.beginPath();
  ctx.moveTo(outline[0][0], outline[0][1]);
  for (let i = 1; i < outline.length; i++) ctx.lineTo(outline[i][0], outline[i][1]);
  ctx.closePath();
  ctx.fill();
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

  const cs = getComputedStyle(document.documentElement);
  const dotCol = cs.getPropertyValue('--dot').trim() || 'rgba(230,224,210,0.13)';
  const edgeCol = cs.getPropertyValue('--cobalt').trim() || '#3b82f6';
  const inkCol = cs.getPropertyValue('--ink-2').trim() || '#94a3b8';

  const step = 28;
  const ox = -((boardSnap.camera.cx || 0) % step);
  const oy = -((boardSnap.camera.cy || 0) % step);
  ctx.fillStyle = dotCol;
  for (let gx = ox; gx < canvas.width; gx += step) {
    for (let gy = oy; gy < canvas.height; gy += step) {
      ctx.fillRect(gx, gy, 1.5, 1.5);
    }
  }

  const posById = new Map();
  const obstacles = [];
  boardSnap.cards.forEach((c, idx) => {
    const p = cardScreenPos(c, idx, canvas.width);
    posById.set(c.id, p);
    obstacles.push({ id: c.id, x: p.x, y: p.y, w: p.w, h: p.h });
  });

  // Real Figments A* obstacle-avoiding orthogonal elbow router (figments-hirsel/shell/canvas/elbow_router.ts)!
  ctx.lineWidth = 1.75;
  ctx.strokeStyle = edgeCol;
  for (const e of boardSnap.edges) {
    const a = posById.get(e.from);
    const b = posById.get(e.to);
    if (!a || !b) continue;
    let pts = null;
    if (FC && typeof FC.routeElbow === 'function' && typeof FC.autoAnchor === 'function') {
      try {
        const sA = FC.autoAnchor(a, { x: b.x + b.w / 2, y: b.y + b.h / 2 });
        const eA = FC.autoAnchor(b, { x: a.x + a.w / 2, y: a.y + a.h / 2 });
        const routed = FC.routeElbow({
          start: { point: sA.point, side: sA.side, rect: a, id: e.from },
          end: { point: eA.point, side: eA.side, rect: b, id: e.to },
          obstacles,
        });
        if (routed && Array.isArray(routed.points) && routed.points.length >= 2) {
          pts = routed.points;
        }
      } catch (_) {}
    }
    if (!pts) {
      const mx = (a.x + a.w / 2 + b.x + b.w / 2) / 2;
      pts = [
        { x: a.x + a.w / 2, y: a.y + a.h / 2 },
        { x: mx, y: a.y + a.h / 2 },
        { x: mx, y: b.y + b.h / 2 },
        { x: b.x + b.w / 2, y: b.y + b.h / 2 },
      ];
    }
    ctx.beginPath();
    ctx.moveTo(pts[0].x, pts[0].y);
    for (let i = 1; i < pts.length; i++) ctx.lineTo(pts[i].x, pts[i].y);
    ctx.stroke();
    if (e.label) {
      const mid = pts[Math.floor(pts.length / 2)];
      ctx.fillStyle = inkCol;
      ctx.font = '9.5px ui-monospace, monospace';
      ctx.fillText(e.label, mid.x + 4, mid.y - 4);
    }
  }

  // Real Figments perfect-freehand ink strokes (persisted on Fxfs /data/ink.json)
  for (const s of inkStrokes) {
    drawStrokeOutline(s.points, s.color || '#f59e0b');
  }
  if (activeInk) drawStrokeOutline(activeInk.points, activeInk.color);

  // Chrome 160 HTML-in-Canvas DOM cards via ctx.drawElementImage
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

  ctx.fillStyle = drawElementWorked ? '#10b981' : '#f59e0b';
  ctx.font = '10px ui-monospace, monospace';
  ctx.fillText(
    drawElementWorked
      ? `FIGMENTS REAL ENGINE + HTML-IN-CANVAS LIVE (rev=${boardSnap.rev} | ink=${inkStrokes.length} | zoom=${boardSnap.camera.zoom_pct}% | paints=${paintEvents})`
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
if (typeof ResizeObserver !== 'undefined') new ResizeObserver(() => triggerPaint()).observe(canvas);

canvas.addEventListener('pointerdown', (e) => {
  const r = canvas.getBoundingClientRect();
  const mx = e.clientX - r.left, my = e.clientY - r.top;
  if (activeTool === 'ink') {
    activeInk = { color: '#f59e0b', points: [[mx, my, 0.5]] };
    triggerPaint();
    return;
  }
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
  if (activeInk) {
    activeInk.points.push([mx, my, e.pressure || 0.5]);
    triggerPaint();
  } else if (dragCard) {
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
  if (activeInk) {
    const finished = activeInk;
    activeInk = null;
    if (finished.points.length >= 2) {
      inkStrokes.push(finished);
      apiPost('/_hirsel/ink', JSON.stringify(finished));
      triggerPaint();
    }
  }
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
    const [res, bres, ires] = await Promise.all([fetch('/_hirsel/state'), fetch('/_hirsel/board'), fetch('/_hirsel/ink')]);
    lastWm = await res.json();
    boardSnap = await bres.json();
    const inkJson = await ires.json();
    if (Array.isArray(inkJson.strokes)) inkStrokes = inkJson.strokes;
    if (!customTheme) {
      const theme = lastWm.room === 'daylight' ? 'paper' : 'darkroom';
      document.documentElement.setAttribute('data-theme', theme);
      document.getElementById('room-pick').value = theme;
    }
    document.getElementById('wm-state').textContent = JSON.stringify({ wm: lastWm, board_rev: boardSnap.rev, cards: boardSnap.cards.length, edges: boardSnap.edges.length, ink: inkStrokes.length }, null, 2);
    updateBadges();
    triggerPaint();
  } catch (e) {
    document.getElementById('wm-state').textContent = 'Poll error: ' + e;
  }
}
setInterval(pollState, 1000);
pollState();

async function cmd(c) { customTheme = null; await apiPost('/_hirsel/cmd', c); pollState(); }
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
    margin: 0; padding: 14px;
    background: #1c1308; color: #fef3c7;
    font-family: ui-sans-serif, system-ui, sans-serif;
  }
  .banner {
    background: #92400e; color: #fff;
    padding: 8px 11px; border-radius: 8px;
    font-weight: 700; font-size: 11.5px; margin-bottom: 10px;
  }
  .origin {
    font-family: ui-monospace, monospace; font-size: 10.5px;
    background: rgba(245,158,11,0.18); border: 1px solid #d97706;
    border-radius: 6px; padding: 5px 8px; margin-bottom: 10px;
  }
  button {
    background: #d97706; color: #fff; border: 0;
    border-radius: 6px; padding: 6px 10px; font-weight: 700; cursor: pointer;
  }
  pre {
    margin-top: 9px; padding: 9px;
    background: rgba(0,0,0,0.4); border-radius: 8px;
    font-family: ui-monospace, monospace; font-size: 10.5px;
  }
</style>
</head>
<body>
  <div class="banner">ISOLATED CHROMIUM CONTEXT #2 // WEB_UNTRUSTED (:7712 ZERO /_hirsel/*)</div>
  <div class="origin" id="orig"></div>
  <p style="font-size:11.5px;">Separate <code>fuchsia.web.Context</code> with an unforgeable compositor-owned Origin Bar above this viewport.</p>
  <p style="margin-top:9px;"><button onclick="trySteal()">Try Stealing Focus from Figments</button></p>
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
