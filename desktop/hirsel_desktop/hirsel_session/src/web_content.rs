// Embedded Figments-style spatial desktop + Native Chromium 160 HTML-in-Canvas workbench UI
// plus isolated Untrusted Web Context page served over loopback to WebEngine.

pub const FIGMENTS_INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<title>Hirsel-OS // Figments Spatial Shell (Native Chromium 160)</title>
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
    padding: 10px 16px;
    background: var(--panel);
    border-bottom: 1px solid var(--border);
    gap: 12px;
  }
  .title { font-weight: 700; font-size: 15px; letter-spacing: 0.4px; }
  .ua { font-family: ui-monospace, monospace; font-size: 11px; color: var(--muted); }
  .toolbar { display: flex; flex-wrap: wrap; gap: 8px; padding: 10px 16px; border-bottom: 1px solid var(--border); }
  button {
    background: #1d4ed8;
    color: #fff;
    border: 1px solid rgba(255,255,255,0.14);
    border-radius: 6px;
    padding: 7px 12px;
    font-size: 12px;
    font-weight: 600;
    cursor: pointer;
  }
  button.sec { background: #1e293b; color: #e2e8f0; }
  button.green { background: #047857; }
  button.amber { background: #b45309; }
  button.red { background: #b91c1c; }
  main {
    flex: 1;
    display: grid;
    grid-template-columns: 1fr 330px;
    min-height: 0;
  }
  .stage {
    position: relative;
    overflow: hidden;
    background: radial-gradient(circle at 20% 20%, rgba(59,130,246,0.12), transparent 55%),
                radial-gradient(circle at 80% 80%, rgba(16,185,129,0.10), transparent 55%),
                var(--bg);
  }
  canvas#spatial {
    width: 100%;
    height: 100%;
    display: block;
  }
  aside {
    background: var(--panel);
    border-left: 1px solid var(--border);
    padding: 14px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .card {
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 10px 12px;
    background: rgba(15, 23, 42, 0.35);
  }
  body.daylight .card { background: #f8fafc; }
  .card h3 { font-size: 11px; text-transform: uppercase; letter-spacing: 0.8px; color: var(--muted); margin-bottom: 6px; }
  pre {
    font-family: ui-monospace, monospace;
    font-size: 11px;
    line-height: 1.45;
    white-space: pre-wrap;
    word-break: break-word;
    color: var(--text);
  }
  .badge {
    display: inline-block;
    padding: 2px 7px;
    border-radius: 999px;
    font-size: 11px;
    font-weight: 700;
    margin-right: 4px;
  }
  .ok { background: rgba(16,185,129,0.2); color: #34d399; }
  .warn { background: rgba(245,158,11,0.2); color: #fbbf24; }
  .bad { background: rgba(239,68,68,0.2); color: #f87171; }
</style>
</head>
<body>
  <header>
    <div>
      <div class="title">FIGMENTS SPATIAL CANVAS // NATIVE CHROMIUM ON ZIRCON FLATLAND</div>
      <div class="ua" id="ua">Probing Chromium runtime...</div>
    </div>
    <div id="caps"></div>
  </header>

  <div class="toolbar">
    <button class="red" onclick="stageEffect()">Stage Agent Outbox Effect (F5)</button>
    <button class="amber" onclick="spoofFocus()">Test Focus Spoof (Blocked by hirsel-wm)</button>
    <button class="green" onclick="cmd('TOGGLE_DEV')">Toggle vm-dev Split (F2)</button>
    <button class="sec" onclick="cmd('TOGGLE_WEB')">Toggle Isolated Web Context (F3)</button>
    <button class="sec" onclick="cmd('TOGGLE_ROOM')">Darkroom / Daylight (F4)</button>
    <button class="sec" onclick="powerboxDemo()">Powerbox Copy -&gt; vm-dev</button>
    <button class="sec" onclick="proxyDemo()">Call egress-gw Proxy</button>
  </div>

  <main>
    <div class="stage">
      <canvas id="spatial" layoutsubtree="true">
        <div id="canvas-dom-node" style="padding:8px;font:12px sans-serif;color:#fff;background:#1e3a8a;border-radius:6px;">
          HTML-in-Canvas DOM Child (Chrome 157+ layoutsubtree)
        </div>
      </canvas>
    </div>
    <aside>
      <div class="card">
        <h3>Pure-Hirsel WM Live State</h3>
        <pre id="wm-state">Connecting to hirsel-wm...</pre>
      </div>
      <div class="card">
        <h3>Chrome 160 / HTML-in-Canvas Probe</h3>
        <pre id="probe">Running...</pre>
      </div>
      <div class="card">
        <h3>Action &amp; Security Log</h3>
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

// Probe Chromium version + HTML-in-Canvas + WebGL/Canvas2D
document.getElementById('ua').textContent = navigator.userAgent;
const canvas = document.getElementById('spatial');
const ctx = canvas.getContext('2d');
const hasDrawElement = !!(ctx && typeof ctx.drawElementImage === 'function');
const hasLayoutSubtree = 'layoutSubtree' in HTMLCanvasElement.prototype || canvas.hasAttribute('layoutsubtree');
const chromeMatch = navigator.userAgent.match(/Chrome\/([\d.]+)/);
const chromeVer = chromeMatch ? chromeMatch[1] : 'unknown';

document.getElementById('caps').innerHTML =
  `<span class="badge ok">Chromium ${chromeVer}</span>` +
  `<span class="badge ${hasDrawElement ? 'ok' : 'warn'}">HTML-in-Canvas: ${hasDrawElement ? 'drawElementImage OK' : 'Canvas2D + layoutsubtree'}</span>`;

document.getElementById('probe').textContent = JSON.stringify({
  chromiumVersion: chromeVer,
  platform: navigator.platform,
  devicePixelRatio: window.devicePixelRatio,
  drawElementImage: hasDrawElement,
  layoutSubtreeAttr: hasLayoutSubtree,
  hardwareConcurrency: navigator.hardwareConcurrency,
}, null, 2);

// Interactive spatial canvas nodes representing the Hirsel-OS architecture
const nodes = [
  { id: 'zircon',   x: 70,  y: 70,  w: 220, h: 86, title: 'Zircon + AMD SVM / NPT', sub: 'Flatland + sysmem2 + virtio-gpu', col: '#1d4ed8' },
  { id: 'wm',       x: 340, y: 70,  w: 230, h: 86, title: 'hirsel-wm (Pure Hirsel)', sub: 'Zero ambient caps | model-checked', col: '#7c3aed' },
  { id: 'chromium', x: 70,  y: 215, w: 220, h: 86, title: 'Chromium 160 (Ozone-Flatland)', sub: 'Native WebEngine on Zircon', col: '#0284c7' },
  { id: 'outbox',   x: 340, y: 215, w: 230, h: 86, title: 'Effect Outbox + Bank', sub: 'Hash-bound compositor ceremony', col: '#dc2626' },
  { id: 'vmdev',    x: 615, y: 140, w: 215, h: 86, title: 'vm-dev (Tier-2 MicroVM)', sub: 'Linux/Docker/Rust + Powerbox', col: '#059669' },
];
const edges = [['zircon','wm'], ['zircon','chromium'], ['wm','outbox'], ['chromium','outbox'], ['wm','vmdev']];
let drag = null, dx = 0, dy = 0, lastState = null, frame = 0;

function drawSpatial() {
  const rect = canvas.getBoundingClientRect();
  if (canvas.width !== Math.floor(rect.width) || canvas.height !== Math.floor(rect.height)) {
    canvas.width = Math.max(320, Math.floor(rect.width));
    canvas.height = Math.max(240, Math.floor(rect.height));
  }
  frame++;
  ctx.clearRect(0, 0, canvas.width, canvas.height);

  // Subtle spatial grid
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
    ctx.beginPath();
    ctx.moveTo(a.x + a.w / 2, a.y + a.h / 2);
    ctx.lineTo(b.x + b.w / 2, b.y + b.h / 2);
    ctx.stroke();
  }

  // Cards
  for (const n of nodes) {
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
    ctx.font = 'bold 12px ui-sans-serif, sans-serif';
    ctx.fillText(n.title, n.x + 10, n.y + 18);

    ctx.fillStyle = document.body.classList.contains('daylight') ? '#334155' : '#cbd5e1';
    ctx.font = '11px ui-monospace, monospace';
    ctx.fillText(n.sub, n.x + 10, n.y + 48);

    if (n.id === 'outbox' && lastState) {
      ctx.fillStyle = lastState.outbox.status === 'staged' ? '#f87171' : '#34d399';
      ctx.fillText(`status=${lastState.outbox.status} ok=${lastState.approved} deny=${lastState.denied}`, n.x + 10, n.y + 68);
    } else if (n.id === 'wm' && lastState) {
      ctx.fillStyle = '#a78bfa';
      ctx.fillText(`focus=${lastState.focused} cap=${lastState.capture_allowed}`, n.x + 10, n.y + 68);
    }
  }

  // Live pulse indicator
  const px = 24 + (frame % 180);
  ctx.fillStyle = '#10b981';
  ctx.fillRect(24, canvas.height - 16, px - 24, 4);
  requestAnimationFrame(drawSpatial);
}
requestAnimationFrame(drawSpatial);

canvas.addEventListener('pointerdown', (e) => {
  const r = canvas.getBoundingClientRect();
  const mx = e.clientX - r.left, my = e.clientY - r.top;
  for (let i = nodes.length - 1; i >= 0; i--) {
    const n = nodes[i];
    if (mx >= n.x && mx <= n.x + n.w && my >= n.y && my <= n.y + n.h) {
      drag = n; dx = mx - n.x; dy = my - n.y; break;
    }
  }
});
window.addEventListener('pointermove', (e) => {
  if (!drag) return;
  const r = canvas.getBoundingClientRect();
  drag.x = Math.max(8, e.clientX - r.left - dx);
  drag.y = Math.max(8, e.clientY - r.top - dy);
});
window.addEventListener('pointerup', () => { drag = null; });

async function pollState() {
  try {
    const res = await fetch('/_hirsel/state');
    const st = await res.json();
    lastState = st;
    document.body.classList.toggle('daylight', st.room === 'daylight');
    document.getElementById('wm-state').textContent = JSON.stringify(st, null, 2);
  } catch (e) {
    document.getElementById('wm-state').textContent = 'Poll error: ' + e;
  }
}
setInterval(pollState, 900);
pollState();

async function cmd(c) {
  const res = await fetch('/_hirsel/cmd', { method: 'POST', body: c });
  const st = await res.json();
  addLog(`cmd(${c}) -> focused=${st.focused} dev=${!!st.layout.vm_dev} web=${!!st.layout.web}`);
  pollState();
}

async function stageEffect() {
  const res = await fetch('/_hirsel/stage', { method: 'POST' });
  const st = await res.json();
  addLog(`stage -> status=${st.outbox.status} capture_allowed=${st.capture_allowed}`);
  pollState();
}

async function spoofFocus() {
  const res = await fetch('/_hirsel/spoof-focus', { method: 'POST', body: 'web_untrusted' });
  const st = await res.json();
  addLog(`spoofFocus(web_untrusted) -> BLOCKED, focused remains "${st.focused}"`);
  pollState();
}

async function powerboxDemo() {
  await fetch('/_hirsel/cmd', { method: 'POST', body: 'CLIP_COPY figments 42' });
  const res = await fetch('/_hirsel/cmd', { method: 'POST', body: 'POWERBOX_PASTE figments vm_dev' });
  const st = await res.json();
  addLog(`powerbox transfer -> count=${st.powerbox_transfers}`);
  pollState();
}

async function proxyDemo() {
  const res = await fetch('/_hirsel/proxy', { method: 'POST', body: 'api.github.com' });
  const data = await res.json();
  addLog(`egress-gw -> ${JSON.stringify(data)}`);
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
