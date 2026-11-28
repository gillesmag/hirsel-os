// Figments Spatial Workstation on Hirsel-OS (Unixporn Minimalism × Figments "One Sun"):
//   - Self-hosted Host Grotesk + Fragment Mono typography (`/assets/fonts/fonts.css`), verified via `document.fonts`
//   - All 6 curated Figments OKLCH rooms (`/assets/themes.css`) synced whole-OS via `/_hirsel/theme`
//   - Real Figments Canvas engine (`/assets/figments-canvas.js`: A* elbow router + perfect-freehand)
//   - Full-bleed breathing canvas with frosted bottom-center Floating Tool Dock
//   - Top-Right Floating Settings Panel (Network / Wi-Fi / DNS / Caretaker, 6 Rooms, MicroVMs & Hardware)
//   - On-demand Rofi/Quickshell-style Command & Agent Drawer
//   - Chrome 160 HTML-in-Canvas (`<canvas content="drawable" layoutsubtree>` + `ctx.drawElementImage`)

pub const FIGMENTS_INDEX_HTML: &str = r#"<!doctype html>
<html lang="en" data-theme="darkroom">
<head>
<meta charset="utf-8" />
<title>Hirsel-OS // Figments</title>
<link rel="stylesheet" href="/assets/fonts/fonts.css" />
<link rel="stylesheet" href="/assets/themes.css" />
<script src="/assets/figments-canvas.js"></script>
<style>
  :root {
    --font-ui: "Host Grotesk", ui-sans-serif, system-ui, -apple-system, sans-serif;
    --font-mono: "Fragment Mono", ui-monospace, "JetBrains Mono", monospace;
    --r-xs: 6px;
    --r-sm: 10px;
    --r-md: 14px;
    --r-lg: 20px;
    --t-tap: 120ms;
    --ease-settle: cubic-bezier(0.22, 1, 0.36, 1);
  }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  html, body {
    width: 100%;
    height: 100%;
    overflow: hidden;
    background: var(--ground);
    color: var(--ink);
    font-family: var(--font-ui);
    -webkit-font-smoothing: antialiased;
  }

  /* ── Full-bleed Studio Stage ── */
  .stage {
    position: relative;
    width: 100%;
    height: 100%;
    overflow: hidden;
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

  /* ── Top-Left Studio Pill & Top-Right Settings Trigger Pill ── */
  .hud-pill, .settings-pill {
    position: absolute;
    top: 14px;
    z-index: 25;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    height: 34px;
    padding: 0 12px;
    border-radius: 999px;
    background: var(--raised-a);
    backdrop-filter: blur(14px);
    border: 1px solid var(--line-strong);
    box-shadow: var(--pop-shadow);
    font-family: var(--font-ui);
    font-size: 12.5px;
    font-weight: 600;
    line-height: 1;
    letter-spacing: -0.01em;
    color: var(--ink);
    user-select: none;
    cursor: pointer;
    transition: border-color var(--t-tap) var(--ease-settle), background var(--t-tap) var(--ease-settle);
  }
  .hud-pill { left: 14px; }
  .settings-pill { right: 14px; }
  .hud-pill:hover, .settings-pill:hover { border-color: var(--line-warm); }
  .settings-pill[aria-expanded="true"] {
    background: var(--cobalt-tint);
    border-color: color-mix(in oklab, var(--cobalt) 45%, transparent);
    color: var(--cobalt);
  }
  .settings-pill svg {
    width: 14px;
    height: 14px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.75;
    stroke-linecap: round;
    stroke-linejoin: round;
    flex: none;
  }
  .hud-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--sap);
    flex: none;
  }
  .hud-sep {
    width: 1px;
    height: 12px;
    background: var(--line-strong);
  }
  .hud-meta {
    font-family: var(--font-mono);
    font-size: 10.5px;
    font-weight: 400;
    line-height: 1;
    color: var(--ink-3);
    font-variant-numeric: tabular-nums;
  }

  /* ── Bottom-Left Quiet Telemetry Micro-Pill ── */
  .status-pill {
    position: absolute;
    bottom: 14px;
    left: 14px;
    z-index: 15;
    display: inline-flex;
    align-items: center;
    gap: 7px;
    height: 28px;
    padding: 0 11px;
    border-radius: 999px;
    background: var(--raised-a);
    backdrop-filter: blur(10px);
    border: 1px solid var(--line);
    font-family: var(--font-mono);
    font-size: 10px;
    line-height: 1;
    color: var(--ink-3);
    font-variant-numeric: tabular-nums;
    pointer-events: none;
  }
  .status-pill .live-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--sap);
  }

  /* ── Bottom-Center Frosted Floating Tool Dock (Quickshell × Figments Rail) ── */
  .dock {
    position: absolute;
    bottom: 14px;
    left: 50%;
    transform: translateX(-50%);
    z-index: 30;
    display: inline-flex;
    align-items: center;
    gap: 3px;
    height: 44px;
    padding: 5px 7px;
    border-radius: 16px;
    background: var(--raised-a);
    backdrop-filter: blur(16px);
    border: 1px solid var(--line-strong);
    box-shadow: var(--pop-shadow);
  }
  .dock-sep {
    width: 1px;
    height: 20px;
    margin: 0 4px;
    background: var(--line);
    flex: none;
  }
  .tool-btn {
    appearance: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    height: 32px;
    min-width: 32px;
    padding: 0 10px;
    border: 0;
    border-radius: 10px;
    background: transparent;
    color: var(--ink-2);
    font-family: var(--font-ui);
    font-size: 12px;
    font-weight: 600;
    line-height: 1;
    letter-spacing: -0.005em;
    cursor: pointer;
    white-space: nowrap;
    transition: background var(--t-tap) var(--ease-settle), color var(--t-tap) var(--ease-settle);
  }
  .tool-btn svg {
    width: 15px;
    height: 15px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.75;
    stroke-linecap: round;
    stroke-linejoin: round;
    flex: none;
  }
  .tool-btn:hover { background: var(--hover); color: var(--ink); }
  .tool-btn[aria-pressed="true"] {
    background: var(--cobalt-tint);
    color: var(--cobalt);
    box-shadow: inset 0 0 0 1px color-mix(in oklab, var(--cobalt) 35%, transparent);
  }
  .tool-btn.icon { width: 32px; padding: 0; }
  .tool-btn.danger:hover { background: var(--vermilion-tint); color: var(--vermilion-ink); }
  .zoom-val {
    font-family: var(--font-mono);
    font-size: 10.5px;
    line-height: 1;
    color: var(--ink-2);
    min-width: 38px;
    text-align: center;
    font-variant-numeric: tabular-nums;
    user-select: none;
  }
  @media (max-width: 920px) {
    .status-pill { display: none; }
  }
  @media (max-width: 820px) {
    .tool-btn .lbl { display: none; }
    .tool-btn { padding: 0 7px; }
  }

  /* ── Room Swatch Dot + Floating 6-Room Menu ── */
  .room-swatch {
    position: relative;
    width: 15px;
    height: 15px;
    border-radius: 50%;
    flex: none;
    background: var(--swatch-ground, var(--ground));
    box-shadow: inset 0 0 0 1px color-mix(in oklab, var(--ink) 26%, transparent);
  }
  .room-swatch::after {
    content: "";
    position: absolute;
    inset: 4px;
    border-radius: 50%;
    background: var(--swatch-accent, var(--cobalt));
  }
  .room-menu {
    position: absolute;
    bottom: 66px;
    left: 50%;
    transform: translateX(-50%);
    z-index: 45;
    min-width: 196px;
    padding: 6px;
    border-radius: 14px;
    background: var(--raised);
    border: 1px solid var(--line-strong);
    box-shadow: var(--pop-shadow);
    display: none;
    gap: 2px;
  }
  .room-menu.open { display: grid; }
  .room-menu .rm-head {
    padding: 5px 9px 4px;
    font-size: 10.5px;
    font-weight: 600;
    color: var(--ink-3);
    letter-spacing: 0.02em;
  }
  .room-opt {
    appearance: none;
    display: flex;
    align-items: center;
    gap: 9px;
    height: 32px;
    padding: 0 9px;
    border: 0;
    border-radius: 8px;
    background: transparent;
    color: var(--ink);
    font-family: var(--font-ui);
    font-size: 12.5px;
    font-weight: 500;
    line-height: 1;
    cursor: pointer;
    text-align: left;
  }
  .room-opt:hover { background: var(--hover); }
  .room-opt[aria-checked="true"] { background: var(--cobalt-tint); color: var(--cobalt); font-weight: 600; }
  .room-opt .tag {
    margin-left: auto;
    font-family: var(--font-mono);
    font-size: 9.5px;
    color: var(--ink-3);
  }

  /* ── Shared Floating Sheet (Top-Right Settings Panel & Command/Agent Drawer) ── */
  .sheet {
    position: absolute;
    top: 56px;
    right: 14px;
    max-height: calc(100% - 128px);
    width: 344px;
    z-index: 40;
    display: none;
    flex-direction: column;
    gap: 10px;
    padding: 14px;
    border-radius: 16px;
    background: var(--raised-a);
    backdrop-filter: blur(20px);
    border: 1px solid var(--line-strong);
    box-shadow: var(--pop-shadow);
    overflow-y: auto;
  }
  .sheet.open { display: flex; }
  .sheet-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding-bottom: 8px;
    border-bottom: 1px solid var(--line);
  }
  .sheet-head h2 {
    font-size: 13.5px;
    font-weight: 600;
    line-height: 1;
    letter-spacing: -0.015em;
  }
  .tabs {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 4px;
    padding: 3px;
    border-radius: 10px;
    background: var(--sunk);
    border: 1px solid var(--line);
  }
  .tab-btn {
    appearance: none;
    height: 28px;
    border: 0;
    border-radius: 7px;
    background: transparent;
    color: var(--ink-2);
    font-family: var(--font-ui);
    font-size: 11.5px;
    font-weight: 600;
    line-height: 1;
    cursor: pointer;
  }
  .tab-btn[aria-selected="true"] {
    background: var(--raised);
    color: var(--ink);
    box-shadow: 0 1px 3px var(--shade-contact);
  }
  .tab-pane { display: none; flex-direction: column; gap: 10px; }
  .tab-pane.active { display: flex; }

  .panel {
    padding: 11px 12px;
    border-radius: 12px;
    background: var(--paper);
    border: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .panel-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }
  .panel h3 {
    font-size: 11px;
    font-weight: 600;
    line-height: 1;
    color: var(--ink-3);
    letter-spacing: 0.02em;
  }
  .kv {
    display: flex;
    align-items: center;
    justify-content: space-between;
    font-size: 11.5px;
    color: var(--ink-2);
  }
  .kv code {
    font-family: var(--font-mono);
    font-size: 10.5px;
    color: var(--ink);
  }
  .field-lbl {
    font-size: 10.5px;
    font-weight: 600;
    color: var(--ink-3);
    margin-bottom: -4px;
  }
  input, select {
    width: 100%;
    height: 32px;
    padding: 0 10px;
    border-radius: 8px;
    border: 1px solid var(--line-strong);
    background: var(--sunk);
    color: var(--ink);
    font-family: var(--font-mono);
    font-size: 11px;
    outline: none;
  }
  input:focus, select:focus { border-color: var(--cobalt); }
  .btn {
    appearance: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    height: 31px;
    padding: 0 12px;
    border-radius: 8px;
    border: 1px solid var(--line-strong);
    background: var(--raised);
    color: var(--ink);
    font-family: var(--font-ui);
    font-size: 12px;
    font-weight: 600;
    line-height: 1;
    cursor: pointer;
  }
  .btn:hover { background: var(--hover); }
  .btn.primary { background: var(--cobalt); color: var(--on-cobalt); border-color: transparent; }
  .btn.primary:hover { background: var(--cobalt-hi); }
  .btn.sm { height: 26px; padding: 0 9px; font-size: 11px; }
  .btn.danger { color: var(--vermilion-ink); border-color: color-mix(in oklab, var(--vermilion-ink) 40%, transparent); }

  /* Crisp iOS/Quickshell Switch */
  .switch {
    position: relative;
    width: 34px;
    height: 20px;
    border-radius: 999px;
    border: 1px solid var(--line-strong);
    background: var(--sunk);
    cursor: pointer;
    flex: none;
    transition: background var(--t-tap) var(--ease-settle);
  }
  .switch::after {
    content: "";
    position: absolute;
    top: 2px;
    left: 2px;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: var(--ink-2);
    transition: transform var(--t-tap) var(--ease-settle), background var(--t-tap);
  }
  .switch[aria-checked="true"] { background: var(--cobalt); border-color: transparent; }
  .switch[aria-checked="true"]::after { transform: translateX(14px); background: var(--on-cobalt); }

  .room-grid {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: 6px;
  }
  .room-card {
    appearance: none;
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 9px 10px;
    border-radius: 10px;
    border: 1px solid var(--line);
    background: var(--paper);
    color: var(--ink);
    font-family: var(--font-ui);
    font-size: 12px;
    font-weight: 600;
    line-height: 1;
    cursor: pointer;
  }
  .room-card:hover { border-color: var(--line-warm); }
  .room-card[aria-checked="true"] {
    background: var(--cobalt-tint);
    border-color: color-mix(in oklab, var(--cobalt) 45%, transparent);
    color: var(--cobalt);
  }
  .chip-list { display: flex; flex-wrap: wrap; gap: 5px; }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 3px 8px;
    border-radius: 999px;
    background: var(--sunk);
    border: 1px solid var(--line);
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--ink-2);
  }
  .muted-note {
    font-size: 11px;
    line-height: 1.45;
    color: var(--ink-3);
  }
  pre {
    font-family: var(--font-mono);
    font-size: 10px;
    line-height: 1.45;
    white-space: pre-wrap;
    word-break: break-word;
    color: var(--ink-2);
  }
  .kbd-row {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    font-size: 10.5px;
    color: var(--ink-3);
    padding-top: 2px;
  }
  kbd {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 2px 5px;
    border-radius: 5px;
    border: 1px solid var(--line-strong);
    background: var(--paper);
    color: var(--ink-2);
    font-family: var(--font-mono);
    font-size: 9.5px;
    line-height: 1;
  }

  /* ── Authentic Figments HTML-in-Canvas Cards (One Sun Material) ── */
  .hic-card {
    width: 268px;
    padding: 13px 15px;
    border-radius: 14px;
    background: var(--paper);
    border: 1px solid var(--studio-cobalt-stroke, var(--line-strong));
    color: var(--ink);
    font-family: var(--font-ui);
    box-shadow: inset 0 1px 0 var(--rim), 0 10px 24px var(--shade-cast);
  }
  .hic-card .top {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 7px;
  }
  .hic-card .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    flex: none;
    background: var(--studio-cobalt-stroke, var(--cobalt));
  }
  .hic-card .h {
    font-family: var(--font-ui);
    font-weight: 600;
    font-size: 13px;
    line-height: 1.15;
    letter-spacing: -0.015em;
    color: var(--ink);
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .hic-card .rev {
    font-family: var(--font-mono);
    font-size: 9.5px;
    line-height: 1;
    color: var(--ink-3);
    font-variant-numeric: tabular-nums;
  }
  .hic-card .s {
    font-family: var(--font-mono);
    font-size: 10.5px;
    color: var(--ink-2);
    line-height: 1.48;
    word-break: break-word;
  }
  .hic-card .foot {
    display: flex;
    align-items: center;
    margin-top: 9px;
  }
  .hic-card .pill {
    display: inline-flex;
    align-items: center;
    height: 18px;
    padding: 0 8px;
    border-radius: 999px;
    font-family: var(--font-mono);
    font-size: 9.2px;
    font-weight: 400;
    line-height: 1;
    letter-spacing: 0.02em;
    background: var(--studio-cobalt-fill);
    color: var(--studio-cobalt-ink);
  }
  .hic-card.iris, .hic-card.purple { border-color: color-mix(in oklab, var(--studio-iris-stroke) 42%, var(--line)); }
  .hic-card.iris .dot, .hic-card.purple .dot { background: var(--studio-iris-stroke); }
  .hic-card.iris .pill, .hic-card.purple .pill { background: var(--studio-iris-fill); color: var(--studio-iris-ink); }

  .hic-card.sap, .hic-card.green { border-color: color-mix(in oklab, var(--studio-sap-stroke) 42%, var(--line)); }
  .hic-card.sap .dot, .hic-card.green .dot { background: var(--studio-sap-stroke); }
  .hic-card.sap .pill, .hic-card.green .pill { background: var(--studio-sap-fill); color: var(--studio-sap-ink); }

  .hic-card.vermilion, .hic-card.red { border-color: color-mix(in oklab, var(--studio-vermilion-stroke) 42%, var(--line)); }
  .hic-card.vermilion .dot, .hic-card.red .dot { background: var(--studio-vermilion-stroke); }
  .hic-card.vermilion .pill, .hic-card.red .pill { background: var(--studio-vermilion-fill); color: var(--studio-vermilion-ink); }

  .hic-card.ochre, .hic-card.amber { border-color: color-mix(in oklab, var(--studio-ochre-stroke) 42%, var(--line)); }
  .hic-card.ochre .dot, .hic-card.amber .dot { background: var(--studio-ochre-stroke); }
  .hic-card.ochre .pill, .hic-card.amber .pill { background: var(--studio-ochre-fill); color: var(--studio-ochre-ink); }
</style>
</head>
<body>
  <div class="stage">
    <canvas id="spatial" content="drawable" layoutsubtree></canvas>

    <!-- Top-Left Quiet Studio HUD Pill -->
    <button class="hud-pill" onclick="toggleDrawer()" title="Toggle Command & Agent Drawer (/)">
      <span class="hud-dot"></span>
      <span>Figments</span>
      <span class="hud-sep"></span>
      <span class="hud-meta" id="hud-meta">chromium 160 · darkroom</span>
    </button>

    <!-- Top-Right Settings Trigger Pill (Network / Wi-Fi / Rooms / System) -->
    <button class="settings-pill" id="settings-btn" aria-expanded="false" onclick="toggleSettings()" title="System & Network Settings (,)">
      <svg viewBox="0 0 24 24"><path d="M5 12.55a11 11 0 0 1 14.08 0"/><path d="M1.42 9a16 16 0 0 1 21.16 0"/><path d="M8.53 16.11a6 6 0 0 1 6.95 0"/><circle cx="12" cy="20" r="1"/></svg>
      <span class="hud-sep"></span>
      <svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09a1.65 1.65 0 0 0-1-1.51 1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09a1.65 1.65 0 0 0 1.51-1 1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9c.26.6.85 1 1.51 1H21a2 2 0 1 1 0 4h-.09c-.66 0-1.25.4-1.51 1z"/></svg>
      <span>Settings</span>
    </button>

    <!-- Bottom-Left Quiet Telemetry Micro-Pill -->
    <div class="status-pill" id="status-pill">
      <span class="live-dot"></span>
      <span id="status-txt">initializing…</span>
    </div>

    <!-- 6-Room Swatch Popover Menu -->
    <div class="room-menu" id="room-menu" role="menu">
      <div class="rm-head">Room</div>
      <button class="room-opt" data-room="darkroom" onclick="setThemeRoom('darkroom')">
        <span class="room-swatch" style="--swatch-ground:#120f0c;--swatch-accent:#6393fd"></span>Darkroom<span class="tag">dark</span>
      </button>
      <button class="room-opt" data-room="paper" onclick="setThemeRoom('paper')">
        <span class="room-swatch" style="--swatch-ground:#f4eee6;--swatch-accent:#1f43d9"></span>Paper<span class="tag">light</span>
      </button>
      <button class="room-opt" data-room="folio" onclick="setThemeRoom('folio')">
        <span class="room-swatch" style="--swatch-ground:#ece2d2;--swatch-accent:#b2532a"></span>Folio<span class="tag">warm</span>
      </button>
      <button class="room-opt" data-room="gallery" onclick="setThemeRoom('gallery')">
        <span class="room-swatch" style="--swatch-ground:#f3f5f8;--swatch-accent:#1b7e78"></span>Gallery<span class="tag">cool</span>
      </button>
      <button class="room-opt" data-room="blueprint" onclick="setThemeRoom('blueprint')">
        <span class="room-swatch" style="--swatch-ground:#141f34;--swatch-accent:#63c6de"></span>Blueprint<span class="tag">draft</span>
      </button>
      <button class="room-opt" data-room="conservatory" onclick="setThemeRoom('conservatory')">
        <span class="room-swatch" style="--swatch-ground:#111c16;--swatch-accent:#74c97b"></span>Conservatory<span class="tag">sage</span>
      </button>
    </div>

    <!-- Bottom-Center Frosted Floating Tool Dock -->
    <div class="dock" role="toolbar" aria-label="Figments Tool Rail">
      <button id="tool-select" class="tool-btn" aria-pressed="true" onclick="setTool('select')" title="Select & Pan">
        <svg viewBox="0 0 24 24"><path d="M5 3l14 9-6 1.5L10.5 20z"/></svg>
        <span class="lbl">Select</span>
      </button>
      <button id="tool-ink" class="tool-btn" aria-pressed="false" onclick="setTool('ink')" title="Freehand Ink">
        <svg viewBox="0 0 24 24"><path d="M12 19l7-7 3 3-7 7-3-3z"/><path d="M18 13l-1.5-7.5L2 2l3.5 14.5L13 18l5-5z"/><path d="M2 2l7.586 7.586"/></svg>
        <span class="lbl">Ink</span>
      </button>
      <button class="tool-btn" onclick="addNote()" title="Add Spatial Note">
        <svg viewBox="0 0 24 24"><rect x="4" y="4" width="16" height="16" rx="3"/><path d="M12 8v8M8 12h8"/></svg>
        <span class="lbl">Note</span>
      </button>

      <div class="dock-sep"></div>

      <button class="tool-btn" onclick="runAgent()" title="Run Pure-Hirsel Autonomous Agent">
        <svg viewBox="0 0 24 24"><path d="M12 3l2.2 5.8L20 11l-5.8 2.2L12 19l-2.2-5.8L4 11l5.8-2.2z"/></svg>
        <span class="lbl">Agent</span>
      </button>
      <button class="tool-btn danger" onclick="stageEffect()" title="Stage Protected Outbox Ceremony (F5)">
        <svg viewBox="0 0 24 24"><path d="M12 3l8 4v6c0 5-3.5 7.5-8 9-4.5-1.5-8-4-8-9V7l8-4z"/></svg>
        <span class="lbl">Outbox</span>
      </button>
      <button class="tool-btn" onclick="cmd('TOGGLE_DEV')" title="Toggle Linux vm-dev (F2)">
        <svg viewBox="0 0 24 24"><rect x="3" y="4" width="18" height="16" rx="3"/><path d="M7 10l3 2.5L7 15M12 15h5"/></svg>
      </button>
      <button class="tool-btn" onclick="cmd('TOGGLE_WEB')" title="Toggle Isolated Web #2 (F3)">
        <svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18"/></svg>
      </button>

      <div class="dock-sep"></div>

      <button class="tool-btn icon" onclick="toggleRoomMenu()" title="Switch Curated Room">
        <span class="room-swatch" id="dock-swatch"></span>
      </button>
      <button class="tool-btn icon" onclick="zoomBy(-10)" title="Zoom Out">
        <svg viewBox="0 0 24 24"><path d="M6 12h12"/></svg>
      </button>
      <span class="zoom-val" id="zoom-val">100%</span>
      <button class="tool-btn icon" onclick="zoomBy(10)" title="Zoom In">
        <svg viewBox="0 0 24 24"><path d="M12 6v12M6 12h12"/></svg>
      </button>

      <div class="dock-sep"></div>

      <button id="drawer-btn" class="tool-btn icon" aria-pressed="false" onclick="toggleDrawer()" title="Command & Inspector Drawer (/)">
        <svg viewBox="0 0 24 24"><rect x="3" y="4" width="18" height="16" rx="3"/><path d="M15 4v16"/></svg>
      </button>
    </div>

    <!-- Top-Right Floating Settings Panel (Network / Wi-Fi / Rooms / System) -->
    <aside class="sheet" id="settings-sheet" aria-label="System Settings">
      <div class="sheet-head">
        <h2>Settings</h2>
        <button class="tool-btn icon" onclick="toggleSettings(false)" title="Close">
          <svg viewBox="0 0 24 24"><path d="M18 6L6 18M6 6l12 12"/></svg>
        </button>
      </div>

      <div class="tabs" role="tablist">
        <button class="tab-btn" role="tab" aria-selected="true" data-tab="net" onclick="setSettingsTab('net')">Network</button>
        <button class="tab-btn" role="tab" aria-selected="false" data-tab="rooms" onclick="setSettingsTab('rooms')">Rooms</button>
        <button class="tab-btn" role="tab" aria-selected="false" data-tab="sys" onclick="setSettingsTab('sys')">System</button>
      </div>

      <!-- Network & Wi-Fi Tab -->
      <div class="tab-pane active" id="tab-net">
        <div class="panel">
          <div class="panel-row">
            <h3>Host &amp; Egress</h3>
            <button class="btn sm" id="caretaker-btn" onclick="toggleCaretaker()">Active</button>
          </div>
          <div class="kv"><span>Ethernet</span><code id="net-eth">virtio-net · up</code></div>
          <div class="kv"><span>vm-dev Subnet</span><code id="net-vm">10.0.3.2/24 -> FIDL</code></div>
          <div class="kv"><span>Egress Gateway</span><code id="net-gw">egress_gw.cm (99 TLS)</code></div>
        </div>

        <div class="panel">
          <div class="panel-row">
            <h3>Wi-Fi (WLAN)</h3>
            <button class="switch" id="wifi-toggle" role="switch" aria-checked="true" onclick="toggleWifi()" title="Toggle Wi-Fi Radio"></button>
          </div>
          <div class="muted-note" id="wifi-hw-note">Driver ready for bare-metal WLAN NIC · config persisted on Fxfs.</div>
          <div class="field-lbl">Network SSID</div>
          <input id="wifi-ssid" placeholder="e.g. Studio-5G" />
          <div class="field-lbl">Security</div>
          <select id="wifi-sec">
            <option value="WPA3-SAE">WPA3-SAE (Recommended)</option>
            <option value="WPA2-PSK">WPA2-PSK</option>
            <option value="Open">Open</option>
          </select>
          <div class="field-lbl">Passphrase (write-only)</div>
          <input id="wifi-pass" type="password" placeholder="Enter passphrase…" />
          <div class="panel-row">
            <button class="btn primary" style="flex:1" onclick="saveNetworkSettings()">Save Wi-Fi &amp; Network</button>
          </div>
          <div class="chip-list" id="wifi-saved"></div>
        </div>

        <div class="panel">
          <h3>IP &amp; DNS</h3>
          <div class="panel-row">
            <span style="font-size:11.5px;color:var(--ink-2)">Addressing</span>
            <select id="net-mode" style="width:120px;height:28px">
              <option value="dhcp">DHCP</option>
              <option value="static">Static</option>
            </select>
          </div>
          <div class="field-lbl">Primary / Secondary DNS</div>
          <div class="panel-row">
            <input id="dns-1" value="1.1.1.1" placeholder="1.1.1.1" />
            <input id="dns-2" value="9.9.9.9" placeholder="9.9.9.9" />
          </div>
          <div class="muted-note" id="settings-save-msg"></div>
        </div>
      </div>

      <!-- 6 Curated OKLCH Rooms & Display Scale Tab -->
      <div class="tab-pane" id="tab-rooms">
        <div class="panel">
          <div class="panel-row">
            <h3>Display &amp; UI Scale</h3>
            <code id="scale-readout" style="font-size:10.5px;color:var(--ink-2)">125%</code>
          </div>
          <div class="muted-note">Rescales the entire Flatland compositor, Top Bar, and Chromium UI live (persisted on Fxfs).</div>
          <div style="display:grid;grid-template-columns:repeat(5,1fr);gap:5px">
            <button class="btn sm scale-opt" data-scale="100" onclick="setUiScale(100)">100%</button>
            <button class="btn sm scale-opt" data-scale="125" onclick="setUiScale(125)">125%</button>
            <button class="btn sm scale-opt" data-scale="150" onclick="setUiScale(150)">150%</button>
            <button class="btn sm scale-opt" data-scale="175" onclick="setUiScale(175)">175%</button>
            <button class="btn sm scale-opt" data-scale="200" onclick="setUiScale(200)">200%</button>
          </div>
        </div>
        <div class="panel">
          <h3>Curated Rooms (Whole-OS Sync)</h3>
          <div class="muted-note">Updates Flatland compositor, top bar islands, vm-dev terminal, and Figments together.</div>
          <div class="room-grid">
            <button class="room-card" data-room="darkroom" onclick="setThemeRoom('darkroom')">
              <span class="room-swatch" style="--swatch-ground:#120f0c;--swatch-accent:#6393fd"></span>Darkroom
            </button>
            <button class="room-card" data-room="paper" onclick="setThemeRoom('paper')">
              <span class="room-swatch" style="--swatch-ground:#f4eee6;--swatch-accent:#1f43d9"></span>Paper
            </button>
            <button class="room-card" data-room="folio" onclick="setThemeRoom('folio')">
              <span class="room-swatch" style="--swatch-ground:#ece2d2;--swatch-accent:#b2532a"></span>Folio
            </button>
            <button class="room-card" data-room="gallery" onclick="setThemeRoom('gallery')">
              <span class="room-swatch" style="--swatch-ground:#f3f5f8;--swatch-accent:#1b7e78"></span>Gallery
            </button>
            <button class="room-card" data-room="blueprint" onclick="setThemeRoom('blueprint')">
              <span class="room-swatch" style="--swatch-ground:#141f34;--swatch-accent:#63c6de"></span>Blueprint
            </button>
            <button class="room-card" data-room="conservatory" onclick="setThemeRoom('conservatory')">
              <span class="room-swatch" style="--swatch-ground:#111c16;--swatch-accent:#74c97b"></span>Conservatory
            </button>
          </div>
        </div>
      </div>

      <!-- System, MicroVMs & Vault Tab -->
      <div class="tab-pane" id="tab-sys">
        <div class="panel">
          <div class="panel-row">
            <h3>Linux MicroVMs (AMD-SVM)</h3>
            <button class="btn sm" onclick="spawnExtraVm()">+ Spawn VM</button>
          </div>
          <div id="sys-vms" class="muted-note">Loading…</div>
        </div>
        <div class="panel">
          <div class="panel-row">
            <h3>Sealed Egress Vault</h3>
            <button class="btn sm danger" onclick="cmd('TOGGLE_SAS')">SAS Vault (F6)</button>
          </div>
          <div id="sys-secrets" class="muted-note">Loading…</div>
        </div>
        <div class="panel">
          <h3>Hardware &amp; Kernel</h3>
          <div class="kv"><span>Kernel</span><code>Zircon + AMD-SVM</code></div>
          <div class="kv"><span>Compositor</span><code>Flatland + sysmem2</code></div>
          <div class="kv"><span>Storage</span><code>Fxfs (/data)</code></div>
        </div>
      </div>
    </aside>

    <!-- On-Demand Slide-Over Command & Inspector Drawer -->
    <aside class="sheet" id="drawer" aria-label="Command and Inspector">
      <div class="sheet-head">
        <h2>Command &amp; Inspector</h2>
        <button class="tool-btn icon" onclick="toggleDrawer(false)" title="Close">
          <svg viewBox="0 0 24 24"><path d="M18 6L6 18M6 6l12 12"/></svg>
        </button>
      </div>
      <div class="panel">
        <h3>Autonomous Pure-Hirsel Agent</h3>
        <input id="agent-in" value="Audit Zircon + Linux vm-dev and stage report" />
        <button class="btn primary" onclick="runAgent()">Execute Verified Loop</button>
        <pre id="agent-out">Idle · zero ambient capabilities.</pre>
      </div>
      <div class="panel">
        <h3>js@1 Codemode Sandbox</h3>
        <input id="js-in" value="console.log('hi from Zircon'); return [6*7, 'hirsel-js@1']" />
        <button class="btn" onclick="runCodemode()">Evaluate in Zero-Ambient Job</button>
        <pre id="js-out">Ready.</pre>
      </div>
      <div class="panel">
        <h3>Isolated Web #2 Origin</h3>
        <input id="web-url" value="http://127.0.0.1:7712/untrusted" />
        <button class="btn" onclick="navWeb()">Open in Fresh Context</button>
      </div>
      <div class="panel">
        <h3>Live Workstation State</h3>
        <pre id="wm-state">Connecting…</pre>
      </div>
      <div class="kbd-row">
        <span><kbd>F1</kbd> Figments</span>
        <span><kbd>F2</kbd> vm-dev</span>
        <span><kbd>F3</kbd> Web</span>
        <span><kbd>F4</kbd> Room</span>
        <span><kbd>F5</kbd> Outbox</span>
        <span><kbd>F6</kbd> SAS</span>
        <span><kbd>,</kbd> Settings</span>
        <span><kbd>/</kbd> Drawer</span>
      </div>
    </aside>
  </div>

<script>
const HIRSEL_TOKEN = "__HIRSEL_TOKEN__";
function apiPost(url, body) {
  return fetch(url, { method: 'POST', headers: { 'X-Hirsel-Token': HIRSEL_TOKEN }, body });
}
const canvas = document.getElementById('spatial');
const ctx = canvas.getContext('2d');
const hasDrawElement = !!(ctx && typeof ctx.drawElementImage === 'function');
const hasRequestPaint = !!(canvas && typeof canvas.requestPaint === 'function');
const FC = window.FigmentsCanvas || null;
const chromeMatch = navigator.userAgent.match(/Chrome\/([\d.]+)/);
const chromeVer = chromeMatch ? chromeMatch[1] : '160';

let drawElementWorked = false;
let drawElementErr = '';
let paintEvents = 0;
let reportedDiag = false;
let hostGroteskLoaded = false;
let fragmentMonoLoaded = false;
let activeTool = 'select';
let activeTheme = 'darkroom';
let wifiEnabled = true;
let lastSettingsSeq = 0;
let boardSnap = { rev: 0, camera: { cx: 0, cy: 0, zoom_pct: 100 }, cards: [], edges: [] };
let inkStrokes = [];
let activeInk = null;
let lastWm = null;
const domCards = new Map();
let dragCard = null, dragOx = 0, dragOy = 0;
let panActive = false, panSx = 0, panSy = 0, panCx0 = 0, panCy0 = 0;

function setTool(t) {
  activeTool = t;
  document.getElementById('tool-select').setAttribute('aria-pressed', String(t === 'select'));
  document.getElementById('tool-ink').setAttribute('aria-pressed', String(t === 'ink'));
  canvas.classList.toggle('ink-mode', t === 'ink');
}

function toggleDrawer(force) {
  const dr = document.getElementById('drawer');
  const open = typeof force === 'boolean' ? (dr.classList.toggle('open', force), force) : dr.classList.toggle('open');
  document.getElementById('drawer-btn').setAttribute('aria-pressed', String(open));
  if (open) toggleSettings(false);
}

function toggleSettings(force) {
  const sh = document.getElementById('settings-sheet');
  const open = typeof force === 'boolean' ? (sh.classList.toggle('open', force), force) : sh.classList.toggle('open');
  document.getElementById('settings-btn').setAttribute('aria-expanded', String(open));
  if (open) {
    toggleDrawer(false);
    refreshSettingsData();
  }
}

function setSettingsTab(tab) {
  document.querySelectorAll('.tab-btn').forEach((b) => b.setAttribute('aria-selected', String(b.dataset.tab === tab)));
  document.querySelectorAll('.tab-pane').forEach((p) => p.classList.toggle('active', p.id === 'tab-' + tab));
  if (tab === 'sys' || tab === 'net') refreshSettingsData();
}

function toggleWifi() {
  wifiEnabled = !wifiEnabled;
  document.getElementById('wifi-toggle').setAttribute('aria-checked', String(wifiEnabled));
  saveNetworkSettings();
}

async function toggleCaretaker() {
  const act = lastWm && lastWm.agent_conn ? 'REVOKE' : 'RESTORE';
  await cmd(act);
  refreshSettingsData();
}

async function refreshSettingsData() {
  try {
    const [sres, nres, vres, secres] = await Promise.all([
      fetch('/_hirsel/settings'),
      fetch('/_hirsel/network'),
      fetch('/_hirsel/vms'),
      fetch('/_hirsel/secrets'),
    ]);
    const st = await sres.json();
    const net = await nres.json();
    const vms = await vres.json();
    const secs = await secres.json();

    wifiEnabled = st.wifi_enabled !== false;
    document.getElementById('wifi-toggle').setAttribute('aria-checked', String(wifiEnabled));
    applyScaleButtons(st.ui_scale_pct || 125, st.phys_w, st.phys_h);
    if (document.activeElement !== document.getElementById('wifi-ssid') && st.wifi_ssid) {
      document.getElementById('wifi-ssid').value = st.wifi_ssid;
    }
    if (st.wifi_security) document.getElementById('wifi-sec').value = st.wifi_security;
    if (st.net_mode) document.getElementById('net-mode').value = st.net_mode;
    if (st.dns_primary && document.activeElement !== document.getElementById('dns-1')) document.getElementById('dns-1').value = st.dns_primary;
    if (st.dns_secondary && document.activeElement !== document.getElementById('dns-2')) document.getElementById('dns-2').value = st.dns_secondary;

    const saved = Array.isArray(st.wifi_saved) ? st.wifi_saved : [];
    document.getElementById('wifi-saved').innerHTML = saved.length
      ? saved.map((s) => `<span class="chip">${escapeHtml(s)}</span>`).join('')
      : '<span class="muted-note">No saved Wi-Fi networks yet.</span>';

    document.getElementById('net-eth').textContent = net.ethernet || 'virtio-net · up';
    document.getElementById('net-vm').textContent = net.vm_subnet || '10.0.3.2/24 -> FIDL';
    document.getElementById('net-gw').textContent = `egress_gw.cm (${net.tls_roots || 99} TLS)`;
    document.getElementById('wifi-hw-note').textContent = net.wlan_note || 'Driver ready for bare-metal WLAN NIC · config persisted on Fxfs.';
    const cbtn = document.getElementById('caretaker-btn');
    const active = lastWm ? lastWm.agent_conn : true;
    cbtn.textContent = active ? 'Egress: Active' : 'Egress: Revoked';
    cbtn.className = active ? 'btn sm' : 'btn sm danger';

    const vmList = Array.isArray(vms.vms) ? vms.vms : [];
    document.getElementById('sys-vms').innerHTML = vmList
      .map((v) => `<div class="kv"><span>${escapeHtml(v.name)}</span><code>${v.ready ? 'ready' : 'booting'} · 64M</code></div>`)
      .join('');

    const secList = Array.isArray(secs.secrets) ? secs.secrets : [];
    document.getElementById('sys-secrets').innerHTML = secList
      .map((s) => `<div class="kv"><span>${escapeHtml(s.host)}</span><code>${s.live ? 'live key' : 'demo'} (${s.len}B)</code></div>`)
      .join('');
  } catch (_) {}
}

async function saveNetworkSettings() {
  const payload = {
    wifi_enabled: wifiEnabled,
    wifi_ssid: document.getElementById('wifi-ssid').value.trim(),
    wifi_security: document.getElementById('wifi-sec').value,
    wifi_passphrase: document.getElementById('wifi-pass').value,
    net_mode: document.getElementById('net-mode').value,
    dns_primary: document.getElementById('dns-1').value.trim() || '1.1.1.1',
    dns_secondary: document.getElementById('dns-2').value.trim() || '9.9.9.9',
  };
  const r = await apiPost('/_hirsel/settings', JSON.stringify(payload));
  if (r.ok) {
    document.getElementById('wifi-pass').value = '';
    document.getElementById('settings-save-msg').textContent = 'Saved to Fxfs /data/settings.json.';
    refreshSettingsData();
  }
}

async function spawnExtraVm() {
  const name = 'vm-' + (Math.floor(Math.random() * 90) + 10);
  await apiPost('/_hirsel/vms', JSON.stringify({ op: 'spawn', name }));
  refreshSettingsData();
}

function toggleRoomMenu() {
  document.getElementById('room-menu').classList.toggle('open');
}

function applyThemeLocal(room) {
  activeTheme = room;
  document.documentElement.setAttribute('data-theme', room);
  document.querySelectorAll('.room-opt, .room-card').forEach((btn) => {
    btn.setAttribute('aria-checked', String(btn.dataset.room === room));
  });
}

function applyScaleButtons(pct, pw, ph) {
  const cur = Number(pct) || 125;
  const el = document.getElementById('scale-readout');
  if (el) {
    const lw = pw ? Math.round(pw * 100 / cur) : 0;
    const lh = ph ? Math.round(ph * 100 / cur) : 0;
    el.textContent = pw && ph ? `${cur}% · ${pw}x${ph} (${lw}x${lh})` : `${cur}%`;
  }
  document.querySelectorAll('.scale-opt').forEach((btn) => {
    const active = Number(btn.dataset.scale) === cur;
    btn.className = active ? 'btn sm primary scale-opt' : 'btn sm scale-opt';
  });
}

async function setUiScale(pct) {
  applyScaleButtons(pct);
  const r = await apiPost('/_hirsel/settings', JSON.stringify({ ui_scale_pct: pct }));
  if (r.ok && r.ui_scale_pct) {
    applyScaleButtons(r.ui_scale_pct, r.phys_w, r.phys_h);
  }
}

async function setThemeRoom(room) {
  applyThemeLocal(room);
  document.getElementById('room-menu').classList.remove('open');
  await apiPost('/_hirsel/theme', JSON.stringify({ theme: room })).catch(() => {});
  triggerPaint();
}

window.addEventListener('keydown', (e) => {
  if (document.activeElement.tagName === 'INPUT' || document.activeElement.tagName === 'SELECT') return;
  if (e.key === '/') {
    e.preventDefault();
    toggleDrawer();
  } else if (e.key === ',') {
    e.preventDefault();
    toggleSettings();
  } else if (e.key === 'Escape') {
    document.getElementById('room-menu').classList.remove('open');
    toggleSettings(false);
    toggleDrawer(false);
  }
});

async function ensureFontsLoaded() {
  if (!document.fonts) return;
  try {
    await Promise.all([
      document.fonts.load('600 13px "Host Grotesk"'),
      document.fonts.load('400 12px "Fragment Mono"'),
      document.fonts.ready,
    ]);
    hostGroteskLoaded = document.fonts.check('600 13px "Host Grotesk"');
    fragmentMonoLoaded = document.fonts.check('400 12px "Fragment Mono"');
    triggerPaint();
    reportDiagNow();
  } catch (_) {}
}
ensureFontsLoaded();

function updateHud() {
  const z = boardSnap.camera.zoom_pct || 100;
  document.getElementById('zoom-val').textContent = z + '%';
  document.getElementById('hud-meta').textContent = `chromium ${chromeVer} · ${activeTheme}`;
  document.getElementById('status-txt').textContent = drawElementWorked
    ? `rev ${boardSnap.rev} · ${boardSnap.cards.length} cards · ${inkStrokes.length} ink`
    : `awaiting paint ${drawElementErr}`;
}
updateHud();

function reportDiagNow() {
  apiPost('/_hirsel/client-diag', JSON.stringify({
    chromeVer,
    hasDrawElement,
    hasRequestPaint,
    drawElementWorked,
    drawElementErr,
    figmentsEngineLoaded: !!(FC && typeof FC.routeElbow === 'function' && typeof FC.getStroke === 'function'),
    hostGroteskLoaded,
    fragmentMonoLoaded,
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
  const cw = Math.max(320, canvas.width || 640);
  const compactW = cw < 580 ? Math.max(204, Math.floor((cw - 64) / 2)) : 284;
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
      bodyTxt = `focus ${lastWm.focused} · powerbox ${lastWm.powerbox_transfers} · ${activeTheme}`;
    } else if (c.id === 'outbox' && lastWm) {
      bodyTxt = `outbox ${lastWm.outbox.status} · ok ${lastWm.approved} · bank $${(lastWm.bank_spent_micros/1e6).toFixed(2)}/$${(lastWm.bank_limit_micros/1e6).toFixed(2)}`;
    }
    el.innerHTML =
      `<div class="top"><span class="dot"></span><span class="h">${escapeHtml(c.title)}</span><span class="rev">r${c.rev}</span></div>` +
      `<div class="s">${escapeHtml(bodyTxt)}</div>` +
      `<div class="foot"><span class="pill">${escapeHtml(c.kind || 'figment')}</span></div>`;
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
  if (cw < 580) {
    const pad = 22, gap = 22;
    const cardW = Math.max(204, Math.floor((cw - pad * 2 - gap) / 2));
    const col = idx % 2;
    const row = Math.floor(idx / 2);
    return { x: pad + col * (cardW + gap) - cx, y: 74 + row * 142 - cy, w: cardW, h: 108 };
  }
  const colShift = cw > 900 ? Math.round((cw - 650) * 0.34) : 44;
  return {
    x: colShift + (c.x - cx) * zoom * 1.22,
    y: 74 + (c.y - cy) * zoom * 1.08,
    w: (c.w || 268) * zoom,
    h: (c.h || 110) * zoom,
  };
}

function drawStrokeOutline(pts, color) {
  if (!pts || pts.length < 2) return;
  const outline = FC && typeof FC.getStroke === 'function'
    ? FC.getStroke(pts, { size: 5, thinning: 0.48, smoothing: 0.55, streamline: 0.5, last: true })
    : pts;
  if (!outline || outline.length < 2) return;
  ctx.fillStyle = color;
  ctx.beginPath();
  ctx.moveTo(outline[0][0], outline[0][1]);
  for (let i = 1; i < outline.length; i++) ctx.lineTo(outline[i][0], outline[i][1]);
  ctx.closePath();
  ctx.fill();
}

function drawRoundedPill(x, y, w, h, r) {
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.lineTo(x + w - r, y);
  ctx.quadraticCurveTo(x + w, y, x + w, y + r);
  ctx.lineTo(x + w, y + h - r);
  ctx.quadraticCurveTo(x + w, y + h, x + w - r, y + h);
  ctx.lineTo(x + r, y + h);
  ctx.quadraticCurveTo(x, y + h, x, y + h - r);
  ctx.lineTo(x, y + r);
  ctx.quadraticCurveTo(x, y, x + r, y);
  ctx.closePath();
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
  const dotCol = cs.getPropertyValue('--dot').trim() || 'rgba(230,224,210,0.12)';
  const lineCol = cs.getPropertyValue('--line-strong').trim() || 'rgba(160,155,148,0.28)';
  const paperCol = cs.getPropertyValue('--paper').trim() || '#1b1815';
  const inkCol = cs.getPropertyValue('--ink-3').trim() || '#8e877f';
  const accentCol = cs.getPropertyValue('--cobalt').trim() || '#6393fd';

  const step = 26;
  const ox = -((boardSnap.camera.cx || 0) % step);
  const oy = -((boardSnap.camera.cy || 0) % step);
  ctx.fillStyle = dotCol;
  for (let gx = ox; gx < canvas.width; gx += step) {
    for (let gy = oy; gy < canvas.height; gy += step) {
      ctx.fillRect(gx, gy, 1.4, 1.4);
    }
  }

  const posById = new Map();
  const obstacles = [];
  boardSnap.cards.forEach((c, idx) => {
    const p = cardScreenPos(c, idx, canvas.width);
    posById.set(c.id, p);
    obstacles.push({ id: c.id, x: p.x, y: p.y, w: p.w, h: p.h });
  });

  ctx.lineWidth = 1.5;
  ctx.lineJoin = 'round';
  ctx.lineCap = 'round';
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
    ctx.strokeStyle = lineCol;
    ctx.beginPath();
    ctx.moveTo(pts[0].x, pts[0].y);
    for (let i = 1; i < pts.length; i++) ctx.lineTo(pts[i].x, pts[i].y);
    ctx.stroke();

    const last = pts[pts.length - 1];
    ctx.fillStyle = accentCol;
    ctx.beginPath();
    ctx.arc(last.x, last.y, 2.5, 0, Math.PI * 2);
    ctx.fill();

    if (e.label && pts.length >= 2) {
      let bestLen = -1, lx = pts[0].x, ly = pts[0].y;
      for (let i = 0; i < pts.length - 1; i++) {
        const segLen = Math.hypot(pts[i + 1].x - pts[i].x, pts[i + 1].y - pts[i].y);
        if (segLen > bestLen) {
          bestLen = segLen;
          lx = (pts[i].x + pts[i + 1].x) * 0.5;
          ly = (pts[i].y + pts[i + 1].y) * 0.5;
        }
      }
      if (bestLen > 30) {
        ctx.font = '400 10px "Fragment Mono", ui-monospace, monospace';
        const tw = ctx.measureText(e.label).width;
        const pw = tw + 14, ph = 18;
        drawRoundedPill(lx - pw / 2, ly - ph / 2, pw, ph, 9);
        ctx.fillStyle = paperCol;
        ctx.fill();
        ctx.strokeStyle = lineCol;
        ctx.lineWidth = 1;
        ctx.stroke();
        ctx.fillStyle = inkCol;
        ctx.textAlign = 'center';
        ctx.textBaseline = 'middle';
        ctx.fillText(e.label, lx, ly + 0.5);
        ctx.textAlign = 'start';
        ctx.textBaseline = 'alphabetic';
        ctx.lineWidth = 1.5;
      }
    }
  }

  for (const s of inkStrokes) {
    drawStrokeOutline(s.points, s.color || '#ebb353');
  }
  if (activeInk) drawStrokeOutline(activeInk.points, activeInk.color);

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
          updateHud();
          reportDiagNow();
        }
      } catch (err) {
        drawElementErr = String(err && err.message ? err.message : err);
      }
    }
  }

  updateHud();
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
  document.getElementById('room-menu').classList.remove('open');
  const r = canvas.getBoundingClientRect();
  const mx = e.clientX - r.left, my = e.clientY - r.top;
  if (activeTool === 'ink') {
    activeInk = { color: '#ebb353', points: [[mx, my, 0.5]] };
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
  triggerPaint();
}

async function addNote() {
  const id = 'note-' + (boardSnap.cards.length + 1);
  const r = await apiPost('/_hirsel/board', JSON.stringify({ op: 'put', id, kind: 'note', tone: 'ochre', title: 'Spatial Note #' + boardSnap.cards.length, body: 'Persisted via pure-Hirsel hirsel-board on Fxfs', x: 24, y: 300 }));
  boardSnap = await r.json();
  triggerPaint();
}

async function pollState() {
  try {
    const [res, bres, ires, tres, sres] = await Promise.all([
      fetch('/_hirsel/state'),
      fetch('/_hirsel/board'),
      fetch('/_hirsel/ink'),
      fetch('/_hirsel/theme'),
      fetch('/_hirsel/settings'),
    ]);
    lastWm = await res.json();
    boardSnap = await bres.json();
    const inkJson = await ires.json();
    if (Array.isArray(inkJson.strokes)) inkStrokes = inkJson.strokes;
    const themeJson = await tres.json();
    if (themeJson && themeJson.theme && themeJson.theme !== activeTheme) {
      applyThemeLocal(themeJson.theme);
    }
    const setJson = await sres.json();
    if (setJson && typeof setJson.settings_open_seq === 'number' && setJson.settings_open_seq !== lastSettingsSeq) {
      const firstPoll = lastSettingsSeq === 0;
      lastSettingsSeq = setJson.settings_open_seq;
      if (!firstPoll || setJson.settings_open_seq > 0) {
        toggleSettings();
      }
    }
    document.getElementById('wm-state').textContent = JSON.stringify({
      theme: activeTheme,
      focused: lastWm.focused,
      powerbox: lastWm.powerbox_transfers,
      cards: boardSnap.cards.length,
      edges: boardSnap.edges.length,
      ink: inkStrokes.length,
    }, null, 2);
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
  document.getElementById('agent-out').textContent = 'Running pure-Hirsel agent loop…';
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
<title>Isolated Web Context #2</title>
<link rel="stylesheet" href="/assets/fonts/fonts.css" />
<style>
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body {
    padding: 18px;
    background: #17130e;
    color: #efe7db;
    font-family: "Host Grotesk", ui-sans-serif, system-ui, -apple-system, sans-serif;
    line-height: 1.5;
    -webkit-font-smoothing: antialiased;
  }
  .pill {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    height: 26px;
    padding: 0 11px;
    border-radius: 999px;
    background: #332410;
    border: 1px solid rgba(235, 179, 83, 0.35);
    color: #f4c26a;
    font-size: 11.5px;
    font-weight: 600;
    line-height: 1;
    margin-bottom: 12px;
  }
  .dot { width: 7px; height: 7px; border-radius: 50%; background: #ebb353; }
  .card {
    background: #201a14;
    border: 1px solid rgba(239, 231, 219, 0.1);
    border-radius: 12px;
    padding: 14px;
  }
  .orig {
    font-family: "Fragment Mono", ui-monospace, monospace;
    font-size: 11px;
    color: #b9b3ab;
    margin-bottom: 10px;
  }
  p { font-size: 12.5px; color: #cbc3b8; margin-bottom: 12px; }
  code { font-family: "Fragment Mono", ui-monospace, monospace; font-size: 11px; }
  button {
    display: inline-flex;
    align-items: center;
    height: 30px;
    background: #3d2a08;
    color: #f4c26a;
    border: 1px solid rgba(235, 179, 83, 0.45);
    border-radius: 8px;
    padding: 0 12px;
    font-family: "Host Grotesk", ui-sans-serif, sans-serif;
    font-size: 12px;
    font-weight: 600;
    line-height: 1;
    cursor: pointer;
  }
  button:hover { background: #4d360d; }
  pre {
    margin-top: 10px;
    padding: 10px;
    background: #120f0c;
    border: 1px solid rgba(239, 231, 219, 0.08);
    border-radius: 8px;
    font-family: "Fragment Mono", ui-monospace, monospace;
    font-size: 10.5px;
    color: #b9b3ab;
    white-space: pre-wrap;
    word-break: break-word;
  }
</style>
</head>
<body>
  <div class="pill"><span class="dot"></span>Isolated Chromium Context #2 · :7712</div>
  <div class="card">
    <div class="orig" id="orig"></div>
    <p>Separate <code>fuchsia.web.Context</code> with zero <code>/_hirsel/*</code> routes and an unforgeable compositor-owned Origin Bar above this viewport.</p>
    <button onclick="trySteal()">Test Focus Isolation</button>
    <pre id="out">Waiting…</pre>
  </div>
  <script>
    document.getElementById('orig').textContent = location.origin + location.pathname;
    document.getElementById('out').textContent = 'UA: ' + navigator.userAgent;
    async function trySteal() {
      const r = await fetch('/_untrusted/spoof-focus', { method: 'POST', body: 'web_untrusted' });
      const st = await r.json();
      document.getElementById('out').textContent =
        'Sent CLIENT_FOCUS web_untrusted\n' +
        'hirsel-wm authoritative focus: ' + st.focused + '\n' +
        'capture_allowed: ' + st.capture_allowed;
    }
  </script>
</body>
</html>
"#;
