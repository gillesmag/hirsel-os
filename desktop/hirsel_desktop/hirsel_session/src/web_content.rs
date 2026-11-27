// Embedded Figments-style spatial desktop + REAL Chrome 160 HTML-in-Canvas
// (layoutsubtree + drawable + paint event + requestPaint + ctx.drawElementImage) +
// Pure-Hirsel js@1 Codemode + Persistent Fxfs Figments Board + Live Linux vm-dev UI.

pub const FIGMENTS_INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<title>Hirsel-OS // Figments Spatial Shell (Native Chromium 160 + HTML-in-Canvas)</title>
<style>
  * { box-sizing: border-box; margin: 0; padding: 0; }
  :root {
    --bg: #0b111c;
    --panel: #111a2b;
    --border: #22324d;
    --text: #e6eefb;
    --muted: #8ea2c2;
    --accent: #3b82f6;
    --green: #10b981;
    --amber: #f59e0b;
    --red: #ef4444;
  }
  body.daylight {
    --bg: #edf2f8;
    --panel: #ffffff;
    --border: #cbd5e1;
    --text: #0f172a;
    --muted: #475569;
  }
  body {
    background: var(--bg);
    color: var(--text);
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
    padding: 8px 14px;
    background: var(--panel);
    border-bottom: 1px solid var(--border);
    gap: 10px;
  }
  .title { font-weight: 700; font-size: 13.5px; letter-spacing: 0.4px; }
  .ua { font-family: ui-monospace, monospace; font-size: 10px; color: var(--muted); }
  .toolbar { display: flex; flex-wrap: wrap; gap: 6px; padding: 7px 14px; border-bottom: 1px solid var(--border); }
  button {
    background: #1d4ed8;
    color: #fff;
    border: 1px solid rgba(255,255,255,0.14);
    border-radius: 6px;
    padding: 5px 9px;
    font-size: 11px;
    font-weight: 600;
    cursor: pointer;
  }
  button.sec { background: #1e293b; color: #e2e8f0; }
  button.green { background: #047857; }
  button.purple { background: #6d28d9; }
  button.amber { background: #b45309; }
  button.red { background: #b91c1c; }
  main {
    flex: 1;
    display: grid;
    grid-template-columns: minmax(0, 1fr) 320px;
    min-height: 0;
  }
  @media (max-width: 960px) {
    main { grid-template-columns: minmax(0, 1fr) 250px; }
  }
  .stage {
    position: relative;
    overflow: hidden;
    min-width: 0;
    background: radial-gradient(circle at 20% 20%, rgba(59,130,246,0.12), transparent 55%),
                radial-gradient(circle at 80% 80%, rgba(16,185,129,0.10), transparent 55%),
                var(--bg);
  }
  canvas#spatial {
    width: 100%;
    height: 100%;
    display: block;
  }
  /* Real Chrome 160 HTML-in-Canvas layoutsubtree + drawable children */
  .hic-card {
    width: 236px;
    padding: 9px 11px;
    border-radius: 10px;
    background: #111c30;
    border: 2px solid #3b82f6;
    color: #e6eefb;
    font-family: ui-sans-serif, system-ui, sans-serif;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.35);
  }
  body.daylight .hic-card {
    background: #ffffff;
    color: #0f172a;
  }
  .hic-card .h { font-weight: 700; font-size: 12px; margin-bottom: 4px; color: #93c5fd; }
  body.daylight .hic-card .h { color: #1d4ed8; }
  .hic-card .s { font-family: ui-monospace, monospace; font-size: 10.5px; color: #cbd5e1; line-height: 1.35; }
  body.daylight .hic-card .s { color: #334155; }
  .hic-card .pill {
    display: inline-block;
    margin-top: 6px;
    padding: 2px 6px;
    border-radius: 4px;
    font-size: 9.5px;
    font-weight: 700;
    background: rgba(59,130,246,0.22);
    color: #93c5fd;
  }
  .hic-card.purple { border-color: #8b5cf6; }
  .hic-card.purple .h { color: #c4b5fd; }
  .hic-card.purple .pill { background: rgba(139,92,246,0.24); color: #ddd6fe; }
  .hic-card.green { border-color: #10b981; }
  .hic-card.green .h { color: #6ee7b7; }
  .hic-card.green .pill { background: rgba(16,185,129,0.24); color: #a7f3d0; }
  .hic-card.red { border-color: #ef4444; }
  .hic-card.red .h { color: #fca5a5; }
  .hic-card.red .pill { background: rgba(239,68,68,0.24); color: #fecaca; }
  .hic-card.amber { border-color: #f59e0b; }
  .hic-card.amber .h { color: #fcd34d; }
  .hic-card.amber .pill { background: rgba(245,158,11,0.24); color: #fde68a; }
  aside {
    background: var(--panel);
    border-left: 1px solid var(--border);
    padding: 10px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .card {
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 8px 10px;
    background: rgba(15, 23, 42, 0.35);
  }
  body.daylight .card { background: #f8fafc; }
  .card h3 { font-size: 10px; text-transform: uppercase; letter-spacing: 0.8px; color: var(--muted); margin-bottom: 5px; }
  input {
    width: 100%;
    background: #0b1220;
    color: #e6eefb;
    border: 1px solid #334155;
    border-radius: 5px;
    padding: 5px 7px;
    font-family: ui-monospace, monospace;
    font-size: 10.5px;
    margin-bottom: 5px;
  }
  body.daylight input { background: #ffffff; color: #0f172a; border-color: #cbd5e1; }
  pre {
    font-family: ui-monospace, monospace;
    font-size: 10px;
    line-height: 1.38;
    white-space: pre-wrap;
    word-break: break-word;
    color: var(--text);
  }
  .badge {
    display: inline-block;
    padding: 2px 7px;
    border-radius: 999px;
    font-size: 10px;
    font-weight: 700;
    margin-right: 4px;
  }
  .ok { background: rgba(16,185,129,0.2); color: #34d399; }
  .warn { background: rgba(245,158,11,0.2); color: #fbbf24; }
</style>
</head>
<body>
  <header>
    <div>
      <div class="title">FIGMENTS SPATIAL WORKSTATION // CHROMIUM 160 + HTML-IN-CANVAS + AMD-SVM LINUX</div>
      <div class="ua" id="ua">Probing Chromium runtime...</div>
    </div>
    <div id="caps"></div>
  </header>

  <div class="toolbar">
    <button class="red" onclick="stageEffect()">Stage Outbox Effect (F5)</button>
    <button class="amber" onclick="spoofFocus()">Test Focus Spoof</button>
    <button class="green" onclick="cmd('TOGGLE_DEV')">Toggle Linux vm-dev (F2)</button>
    <button class="sec" onclick="cmd('TOGGLE_WEB')">Isolated Web #2 (F3)</button>
    <button class="sec" onclick="cmd('TOGGLE_ROOM')">Darkroom / Daylight (F4)</button>
    <button class="purple" onclick="runCodemode()">Run Pure-Hirsel js@1</button>
    <button class="sec" onclick="powerboxDemo()">Powerbox -&gt; vm-dev</button>
    <button class="sec" onclick="proxyDemo()">Real egress-gw Fetch</button>
  </div>

  <main>
    <div class="stage">
      <canvas id="spatial" content="drawable" layoutsubtree>
        <div id="hic-zircon" class="hic-card" drawable>
          <div class="h">Zircon + AMD SVM / NPT</div>
          <div class="s">Flatland + sysmem2 + virtio-gpu<br/>Real Linux 6.1.102 2-vCPU MicroVM</div>
          <span class="pill">HTML-IN-CANVAS DOM NODE</span>
        </div>
        <div id="hic-wm" class="hic-card purple" drawable>
          <div class="h">hirsel-wm + hirsel-js</div>
          <div class="s" id="hic-wm-sub">Zero ambient caps | model-checked<br/>Figments js@1 sandbox ready</div>
          <span class="pill">PURE HIRSEL PLACE</span>
        </div>
        <div id="hic-outbox" class="hic-card red" drawable>
          <div class="h">Effect Outbox + egress-gw</div>
          <div class="s" id="hic-outbox-sub">Hash-bound compositor ceremony<br/>Host-only credential injection</div>
          <span class="pill">CAPABILITY BOUNDARY</span>
        </div>
        <div id="hic-vmdev" class="hic-card green" drawable>
          <div class="h">vm-dev (Linux 6.1.102)</div>
          <div class="s" id="hic-vmdev-sub">Persistent Fxfs /mnt/data + /dev/hvc0<br/>Powerbox clipboard isolated</div>
          <span class="pill">TIER-2 MICROVM</span>
        </div>
      </canvas>
    </div>
    <aside>
      <div class="card">
        <h3>Pure-Hirsel js@1 Codemode Sandbox</h3>
        <input id="js-in" value="console.log('hi from Zircon'); return [6*7, 'hirsel-js@1']" />
        <button class="purple" onclick="runCodemode()">Eval in Zero-Ambient Hirsel Job</button>
        <pre id="js-out" style="margin-top:5px;">Ready.</pre>
      </div>
      <div class="card">
        <h3>Live Linux 6.1.102 vm-dev Exec</h3>
        <input id="vm-in" value="uname -srm && nproc && ls -la /mnt/data" />
        <button class="green" onclick="runVmExec()">Run Inside AMD-SVM Linux</button>
        <pre id="vm-out" style="margin-top:5px;">Waiting...</pre>
      </div>
      <div class="card">
        <h3>Pure-Hirsel WM Live State</h3>
        <pre id="wm-state">Connecting...</pre>
      </div>
      <div class="card">
        <h3>Security &amp; Action Log</h3>
        <pre id="log">Ready.</pre>
      </div>
    </aside>
  </main>

<script>
const logEl = document.getElementById('log');
function addLog(msg) {
  const ts = new Date().toISOString().slice(11, 19);
  logEl.textContent = `[${ts}] ${msg}\n` + logEl.textContent.slice(0, 1200);
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
let userDragged = false;

const nodes = [
  { id: 'zircon', el: document.getElementById('hic-zircon'), x: 28,  y: 36,  w: 236, h: 92, col: '#3b82f6', title: 'Zircon + AMD SVM / NPT', sub: 'Flatland + Linux 6.1.102 MicroVM' },
  { id: 'wm',     el: document.getElementById('hic-wm'),     x: 300, y: 36,  w: 236, h: 92, col: '#8b5cf6', title: 'hirsel-wm + hirsel-js', sub: 'Zero ambient caps | model-checked' },
  { id: 'outbox', el: document.getElementById('hic-outbox'), x: 28,  y: 168, w: 236, h: 92, col: '#ef4444', title: 'Effect Outbox + egress-gw', sub: 'Hash-bound ceremony + host secrets' },
  { id: 'vmdev',  el: document.getElementById('hic-vmdev'),  x: 300, y: 168, w: 236, h: 92, col: '#10b981', title: 'vm-dev (Linux 6.1.102)', sub: 'Persistent Fxfs /mnt/data + /dev/hvc0' },
];
const edges = [['zircon','wm'], ['zircon','outbox'], ['wm','outbox'], ['wm','vmdev'], ['zircon','vmdev']];
let boardCards = [];
let drag = null, dx = 0, dy = 0, frame = 0;

function updateBadges() {
  document.getElementById('caps').innerHTML =
    `<span class="badge ok">Chromium ${chromeVer}</span>` +
    `<span class="badge ${hasDrawElement ? 'ok' : 'warn'}">HTML-in-Canvas: ${drawElementWorked ? 'drawElementImage LIVE' : (hasDrawElement ? 'drawElementImage API' : 'Canvas2D')}</span>`;
}
updateBadges();

function reportDiagNow() {
  fetch('/_hirsel/client-diag', {
    method: 'POST',
    body: JSON.stringify({
      chromeVer,
      hasDrawElement,
      hasRequestPaint,
      drawElementWorked,
      drawElementErr,
      paintEvents,
      boardCards: boardCards.length
    })
  }).catch(() => {});
}

function layoutNodesForWidth(cw) {
  if (userDragged) return;
  const pad = 16;
  const gap = 16;
  const cols = cw >= 420 ? 2 : 1;
  const cardW = cols === 2
    ? Math.min(248, Math.max(180, Math.floor((cw - pad * 2 - gap) / 2)))
    : Math.min(280, Math.max(180, cw - pad * 2));
  const col2 = Math.min(cw - cardW - pad, pad + cardW + gap);
  nodes.forEach((n, idx) => {
    const col = idx % cols;
    const row = Math.floor(idx / cols);
    n.w = cardW;
    n.x = col === 0 ? pad : Math.max(pad + 16, col2);
    n.y = 28 + row * 134;
    if (n.el) n.el.style.width = cardW + 'px';
  });
}

function syncBoardDomCards() {
  const existing = new Set(nodes.map(n => n.id));
  for (const c of boardCards) {
    let el = document.getElementById('hic-card-' + c.id);
    if (!el) {
      el = document.createElement('div');
      el.id = 'hic-card-' + c.id;
      el.className = 'hic-card ' + (c.color || 'amber');
      el.setAttribute('drawable', '');
      canvas.appendChild(el);
    }
    el.innerHTML = `<div class="h">${c.title}</div><div class="s">${c.sub}</div><span class="pill">FXFS FIGMENT CARD</span>`;
    if (!existing.has('card-' + c.id)) {
      nodes.push({
        id: 'card-' + c.id,
        el,
        x: c.x || 28,
        y: c.y || 306,
        w: 236,
        h: 92,
        col: '#f59e0b',
        title: c.title,
        sub: c.sub,
        custom: true,
      });
      existing.add('card-' + c.id);
    }
  }
}

function drawSpatial(fromPaintEvent) {
  const rect = canvas.getBoundingClientRect();
  const rw = Math.max(300, Math.floor(rect.width));
  const rh = Math.max(240, Math.floor(rect.height));
  if (canvas.width !== rw || canvas.height !== rh) {
    canvas.width = rw;
    canvas.height = rh;
  }
  layoutNodesForWidth(canvas.width);
  frame++;
  ctx.clearRect(0, 0, canvas.width, canvas.height);

  // Spatial dot/line grid
  ctx.strokeStyle = document.body.classList.contains('daylight') ? 'rgba(15,23,42,0.08)' : 'rgba(148,163,184,0.09)';
  ctx.lineWidth = 1;
  for (let gx = 0; gx < canvas.width; gx += 32) {
    ctx.beginPath(); ctx.moveTo(gx, 0); ctx.lineTo(gx, canvas.height); ctx.stroke();
  }
  for (let gy = 0; gy < canvas.height; gy += 32) {
    ctx.beginPath(); ctx.moveTo(0, gy); ctx.lineTo(canvas.width, gy); ctx.stroke();
  }

  // Edges
  ctx.lineWidth = 2;
  ctx.strokeStyle = '#3b82f6';
  for (const [aId, bId] of edges) {
    const a = nodes.find(n => n.id === aId);
    const b = nodes.find(n => n.id === bId);
    if (!a || !b) continue;
    ctx.beginPath();
    ctx.moveTo(a.x + a.w / 2, a.y + a.h / 2);
    ctx.lineTo(b.x + b.w / 2, b.y + b.h / 2);
    ctx.stroke();
  }

  // Nodes: real Chrome 160 HTML-in-Canvas drawElementImage with Canvas2D fallback
  for (const n of nodes) {
    let drewDom = false;
    if (hasDrawElement && n.el) {
      try {
        const tr = ctx.drawElementImage(n.el, Math.round(n.x), Math.round(n.y));
        if (tr && typeof tr.toString === 'function') {
          n.el.style.transform = tr.toString();
        }
        drewDom = true;
        if (!drawElementWorked) {
          drawElementWorked = true;
          drawElementErr = '';
          updateBadges();
          reportDiagNow();
        }
      } catch (err) {
        drawElementErr = String(err && err.message ? err.message : err);
        drewDom = false;
      }
    }
    if (!drewDom) {
      ctx.fillStyle = document.body.classList.contains('daylight') ? '#ffffff' : '#111c30';
      ctx.strokeStyle = n.col;
      ctx.lineWidth = 2.5;
      ctx.beginPath();
      ctx.roundRect(n.x, n.y, n.w, n.h, 10);
      ctx.fill();
      ctx.stroke();
      ctx.fillStyle = n.col;
      ctx.fillRect(n.x + 2, n.y + 2, n.w - 4, 24);
      ctx.fillStyle = '#ffffff';
      ctx.font = 'bold 11.5px ui-sans-serif, sans-serif';
      ctx.fillText(n.title, n.x + 10, n.y + 18);
      ctx.fillStyle = document.body.classList.contains('daylight') ? '#334155' : '#cbd5e1';
      ctx.font = '10.5px ui-monospace, monospace';
      ctx.fillText(n.sub.slice(0, 34), n.x + 10, n.y + 50);
    }
  }

  ctx.fillStyle = drawElementWorked ? '#10b981' : '#f59e0b';
  ctx.font = '10.5px ui-monospace, monospace';
  const statusLine = drawElementWorked
    ? `CHROME 160 HTML-IN-CANVAS LIVE (layoutsubtree + drawElementImage, paints=${paintEvents})`
    : `CANVAS2D FALLBACK [${drawElementErr || 'awaiting paint'}]`;
  ctx.fillText(statusLine, 16, canvas.height - 16);

  if (!reportedDiag && frame >= 3) {
    reportedDiag = true;
    reportDiagNow();
  }
}

function triggerPaint() {
  if (hasRequestPaint) {
    try { canvas.requestPaint(); } catch (_) {}
  }
  drawSpatial(false);
}

canvas.addEventListener('paint', () => {
  paintEvents++;
  drawSpatial(true);
});

if (typeof ResizeObserver !== 'undefined') {
  new ResizeObserver(() => triggerPaint()).observe(canvas);
}

// Kick off initial paint + gentle periodic refresh
requestAnimationFrame(() => triggerPaint());
setInterval(() => triggerPaint(), 1000);

canvas.addEventListener('pointerdown', (e) => {
  const r = canvas.getBoundingClientRect();
  const mx = e.clientX - r.left, my = e.clientY - r.top;
  for (let i = nodes.length - 1; i >= 0; i--) {
    const n = nodes[i];
    if (mx >= n.x && mx <= n.x + n.w && my >= n.y && my <= n.y + n.h) {
      drag = n; dx = mx - n.x; dy = my - n.y; userDragged = true; break;
    }
  }
});
window.addEventListener('pointermove', (e) => {
  if (!drag) return;
  const r = canvas.getBoundingClientRect();
  drag.x = Math.max(8, e.clientX - r.left - dx);
  drag.y = Math.max(8, e.clientY - r.top - dy);
  triggerPaint();
});
window.addEventListener('pointerup', () => { drag = null; });

async function pollState() {
  try {
    const [res, bres] = await Promise.all([fetch('/_hirsel/state'), fetch('/_hirsel/board')]);
    const st = await res.json();
    const b = await bres.json();
    if (Array.isArray(b.cards)) {
      boardCards = b.cards;
      syncBoardDomCards();
    }
    document.body.classList.toggle('daylight', st.room === 'daylight');
    document.getElementById('wm-state').textContent = JSON.stringify(st, null, 2);
    document.getElementById('hic-wm-sub').innerHTML =
      `focus=${st.focused} cap=${st.capture_allowed}<br/>powerbox=${st.powerbox_transfers} room=${st.room}`;
    document.getElementById('hic-outbox-sub').innerHTML =
      `outbox=${st.outbox.status} ok=${st.approved} deny=${st.denied}<br/>bank=$${(st.bank_spent_micros/1e6).toFixed(2)}/$${(st.bank_limit_micros/1e6).toFixed(2)}`;
    triggerPaint();
  } catch (e) {
    document.getElementById('wm-state').textContent = 'Poll error: ' + e;
  }
}
setInterval(pollState, 1000);
pollState();

async function cmd(c) {
  const res = await fetch('/_hirsel/cmd', { method: 'POST', body: c });
  const st = await res.json();
  addLog(`cmd(${c}) -> focused=${st.focused}`);
  pollState();
}

async function stageEffect() {
  const res = await fetch('/_hirsel/stage', { method: 'POST' });
  const st = await res.json();
  addLog(`stage -> status=${st.outbox.status} cap=${st.capture_allowed}`);
  pollState();
}

async function spoofFocus() {
  const res = await fetch('/_hirsel/spoof-focus', { method: 'POST', body: 'web_untrusted' });
  const st = await res.json();
  addLog(`spoofFocus -> BLOCKED (focused=${st.focused})`);
  pollState();
}

async function runCodemode() {
  const code = document.getElementById('js-in').value;
  const res = await fetch('/_hirsel/codemode', { method: 'POST', body: code });
  const data = await res.json();
  document.getElementById('js-out').textContent = JSON.stringify(data, null, 2);
  addLog(`js@1 -> ${JSON.stringify(data)}`);
}

async function runVmExec() {
  const c = document.getElementById('vm-in').value;
  const res = await fetch('/_hirsel/dev-exec', { method: 'POST', body: c });
  const data = await res.json();
  document.getElementById('vm-out').textContent = (data.stdout || JSON.stringify(data));
  addLog(`vm-dev rc=${data.exit_code}`);
}

async function powerboxDemo() {
  await fetch('/_hirsel/clipboard', {
    method: 'POST',
    body: JSON.stringify({ domain: 'figments', text: 'POWERBOX_SECRET_42_FROM_FIGMENTS' })
  });
  const res = await fetch('/_hirsel/powerbox-paste', {
    method: 'POST',
    body: JSON.stringify({ from: 'figments', to: 'vm_dev' })
  });
  const data = await res.json();
  addLog(`powerbox -> transfers=${data.powerbox_transfers} delivered=${data.delivered}`);
  pollState();
}

async function proxyDemo() {
  const res = await fetch('/_hirsel/proxy', {
    method: 'POST',
    body: JSON.stringify({ host: '127.0.0.1', port: 7780, path: '/_hirsel/echo-target' })
  });
  const data = await res.json();
  addLog(`egress-gw -> status=${data.http_status} injected=${data.secret_injected}`);
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
    margin: 0; padding: 18px;
    background: #1c1308; color: #fef3c7;
    font-family: ui-sans-serif, system-ui, sans-serif;
  }
  .banner {
    background: #92400e; color: #fff;
    padding: 10px 14px; border-radius: 8px;
    font-weight: 700; margin-bottom: 14px;
  }
  button {
    background: #d97706; color: #fff; border: 0;
    border-radius: 6px; padding: 8px 12px; font-weight: 700; cursor: pointer;
  }
  pre {
    margin-top: 12px; padding: 12px;
    background: rgba(0,0,0,0.4); border-radius: 8px;
    font-family: ui-monospace, monospace; font-size: 12px;
  }
</style>
</head>
<body>
  <div class="banner">ISOLATED CHROMIUM CONTEXT #2 // WEB_UNTRUSTED (AMBER TRUST BORDER)</div>
  <p>This viewport runs in a completely separate <code>fuchsia.web.Context</code> (separate browser process, cookie jar, and renderer pool) from the Figments Shell.</p>
  <p><button onclick="trySteal()">Try Stealing Focus from Figments</button></p>
  <pre id="out">Waiting...</pre>
  <script>
    document.getElementById('out').textContent = 'User-Agent: ' + navigator.userAgent;
    async function trySteal() {
      const r = await fetch('/_hirsel/spoof-focus', { method: 'POST', body: 'web_untrusted' });
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
