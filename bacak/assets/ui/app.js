/* ==========================================================================
   Bacak OS — Prototype interaction layer
   Window manager · Dock · On-screen keyboard · Clock · Snap previews
   ========================================================================== */

(() => {
  "use strict";

  // ---------- Utilities ----------
  const $  = (sel, root = document) => root.querySelector(sel);
  const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];
  const clamp = (n, lo, hi) => Math.max(lo, Math.min(hi, n));

  const state = {
    focusedWindowId: null,
    snapZone: null,         // 'left' | 'right' | 'top' | null
    keyboardOpen: false,
    keyboardTarget: null,
    shift: false,
  };

  // ============================================================
  // CLOCK
  // ============================================================
  const clockTime = $("#clock-time");
  const clockDate = $("#clock-date");
  function tickClock() {
    const d = new Date();
    const hh = String(d.getHours()).padStart(2, "0");
    const mm = String(d.getMinutes()).padStart(2, "0");
    clockTime.textContent = `${hh}:${mm}`;
    clockDate.textContent = d.toLocaleDateString(undefined, { weekday: "short", day: "numeric", month: "short" });
  }
  tickClock();
  setInterval(tickClock, 30_000);

  // ============================================================
  // WINDOW MANAGER — focus, drag, snap, resize
  // ============================================================
  const workspace = $("#workspace");
  const snapPreview = $("#snap-preview");

  function focusWindow(win) {
    if (!win) return;
    $$(".window").forEach(w => w.classList.remove("is-focused"));
    win.classList.add("is-focused");
    win.style.zIndex = String(++focusWindow._z);
    state.focusedWindowId = win.dataset.windowId;
  }
  focusWindow._z = 10;

  function getSnapZone(x, y) {
    const W = window.innerWidth, H = window.innerHeight;
    const edge = 24;
    if (x < edge)            return "left";
    if (x > W - edge)        return "right";
    if (y < edge)            return "top";
    return null;
  }

  function snapRect(zone) {
    const W = window.innerWidth, H = window.innerHeight - 96; // dock-safe
    if (zone === "left")  return { x: 0,     y: 0, w: W / 2, h: H };
    if (zone === "right") return { x: W / 2, y: 0, w: W / 2, h: H };
    if (zone === "top")   return { x: 0,     y: 0, w: W,     h: H };
    return null;
  }

  function showSnapPreview(zone) {
    const r = snapRect(zone);
    if (!r) { snapPreview.classList.remove("is-visible"); return; }
    Object.assign(snapPreview.style, {
      left:   r.x + "px",
      top:    r.y + "px",
      width:  r.w + "px",
      height: r.h + "px",
    });
    snapPreview.classList.add("is-visible");
  }

  function commitSnap(win, zone) {
    const r = snapRect(zone);
    if (!r) return;
    win.classList.add("is-snapping");
    win.style.setProperty("--x", r.x + "px");
    win.style.setProperty("--y", r.y + "px");
    win.style.setProperty("--w", r.w + "px");
    win.style.setProperty("--h", r.h + "px");
    setTimeout(() => win.classList.remove("is-snapping"), 320);
  }

  function makeDraggable(win) {
    const handle = $("[data-drag-handle]", win);
    let dragging = false, startX = 0, startY = 0, origX = 0, origY = 0;

    handle.addEventListener("pointerdown", (e) => {
      if (e.target.closest(".ctrl")) return;
      dragging = true;
      handle.setPointerCapture(e.pointerId);
      startX = e.clientX; startY = e.clientY;
      origX = parseFloat(getComputedStyle(win).getPropertyValue("--x")) || 0;
      origY = parseFloat(getComputedStyle(win).getPropertyValue("--y")) || 0;
      focusWindow(win);
    });

    handle.addEventListener("pointermove", (e) => {
      if (!dragging) return;
      const nx = origX + (e.clientX - startX);
      const ny = origY + (e.clientY - startY);
      win.style.setProperty("--x", clamp(nx, -40, window.innerWidth - 80) + "px");
      win.style.setProperty("--y", clamp(ny, 0, window.innerHeight - 60) + "px");

      const zone = getSnapZone(e.clientX, e.clientY);
      state.snapZone = zone;
      if (zone) showSnapPreview(zone);
      else snapPreview.classList.remove("is-visible");
    });

    handle.addEventListener("pointerup", (e) => {
      if (!dragging) return;
      dragging = false;
      handle.releasePointerCapture?.(e.pointerId);
      snapPreview.classList.remove("is-visible");
      if (state.snapZone) commitSnap(win, state.snapZone);
      state.snapZone = null;
    });

    // Resize handle
    const resize = $(".resize-handle", win);
    if (resize) {
      let rz = false, rx = 0, ry = 0, ow = 0, oh = 0;
      resize.addEventListener("pointerdown", (e) => {
        rz = true; resize.setPointerCapture(e.pointerId);
        rx = e.clientX; ry = e.clientY;
        ow = parseFloat(getComputedStyle(win).getPropertyValue("--w"));
        oh = parseFloat(getComputedStyle(win).getPropertyValue("--h"));
        e.stopPropagation();
      });
      resize.addEventListener("pointermove", (e) => {
        if (!rz) return;
        win.style.setProperty("--w", clamp(ow + (e.clientX - rx), 360, window.innerWidth)  + "px");
        win.style.setProperty("--h", clamp(oh + (e.clientY - ry), 220, window.innerHeight) + "px");
      });
      resize.addEventListener("pointerup", (e) => {
        rz = false; resize.releasePointerCapture?.(e.pointerId);
      });
    }

    // Focus on any click inside
    win.addEventListener("pointerdown", () => focusWindow(win));
  }

  $$(".window").forEach(w => makeDraggable(w));

  // Window control buttons (close / min / max)
  $$(".ctrl-close").forEach(b => b.addEventListener("click", (e) => {
    const win = e.target.closest(".window");
    win.style.transition = "transform 220ms var(--ease-out), opacity 220ms var(--ease-out)";
    win.style.transform = "scale(0.94)";
    win.style.opacity = "0";
    setTimeout(() => win.remove(), 220);
  }));
  $$(".ctrl-max").forEach(b => b.addEventListener("click", (e) => {
    const win = e.target.closest(".window");
    commitSnap(win, "top");
  }));
  $$(".ctrl-min").forEach(b => b.addEventListener("click", (e) => {
    const win = e.target.closest(".window");
    win.classList.add("is-snapping");
    win.style.transform = "scale(0.1) translateY(400px)";
    win.style.opacity = "0";
    setTimeout(() => {
      win.style.transform = "";
      win.style.opacity  = "";
      win.classList.remove("is-snapping");
      win.style.display  = "none";
    }, 320);
  }));

  // Initial focus
  focusWindow($(".window"));

  // ============================================================
  // DOCK — magnification, tooltip, dim state
  // ============================================================
  const dock = $("#dock");
  const tooltip = $("#dock-tooltip");

  function showTooltip(target, label) {
    const r = target.getBoundingClientRect();
    tooltip.textContent = label;
    tooltip.style.left = (r.left + r.width / 2) + "px";
    tooltip.style.transform = "translate(-50%, -100%)";
    tooltip.style.top  = (r.top - 8) + "px";
    tooltip.classList.add("is-visible");
  }
  function hideTooltip() { tooltip.classList.remove("is-visible"); }

  $$(".dock-app, .dock-tray, .dock-clock").forEach(el => {
    el.addEventListener("mouseenter", () => {
      const label = el.getAttribute("title") || el.dataset.tray || "";
      if (label) showTooltip(el, label);
    });
    el.addEventListener("mouseleave", hideTooltip);
  });

  // Dim dock when a window is focused (per spec)
  function updateDockDim() {
    const focused = !!document.querySelector(".window.is-focused");
    dock.classList.toggle("dock--dimmed", focused);
  }
  updateDockDim();
  document.addEventListener("pointerdown", updateDockDim);

  // ============================================================
  // ON-SCREEN KEYBOARD
  // ============================================================
  const osk = $("#osk");
  const oskRows = $("#osk-rows");

  const ROWS = [
    ["1","2","3","4","5","6","7","8","9","0"],
    ["q","w","e","r","t","y","u","i","o","p"],
    ["a","s","d","f","g","h","j","k","l"],
    ["⇧","z","x","c","v","b","n","m","⌫"],
    ["⎚","@",".","space","/","⏎"],
  ];

  function renderKeys() {
    oskRows.innerHTML = "";
    ROWS.forEach(row => {
      const r = document.createElement("div");
      r.className = "osk-row";
      row.forEach(label => {
        const k = document.createElement("button");
        k.type = "button";
        k.className = "key";
        k.dataset.key = label;
        let display = label;
        if (label === "space") { k.classList.add("key--space"); display = ""; }
        else if (["⇧","⌫","⏎","⎚"].includes(label)) {
          k.classList.add("key--wide", "key--mod");
        }
        else if (state.shift) display = label.toUpperCase();
        k.textContent = display;
        r.appendChild(k);
      });
      oskRows.appendChild(r);
    });
  }
  renderKeys();

  oskRows.addEventListener("click", (e) => {
    const btn = e.target.closest(".key");
    if (!btn || !state.keyboardTarget) return;
    const key = btn.dataset.key;
    const tgt = state.keyboardTarget;
    const v = tgt.value ?? "";

    if (key === "⌫") tgt.value = v.slice(0, -1);
    else if (key === "⇧") { state.shift = !state.shift; renderKeys(); return; }
    else if (key === "space") tgt.value = v + " ";
    else if (key === "⏎") { tgt.dispatchEvent(new Event("change")); closeKeyboard(); return; }
    else if (key === "⎚") closeKeyboard();
    else {
      const ch = state.shift ? key.toUpperCase() : key;
      tgt.value = v + ch;
      if (state.shift) { state.shift = false; renderKeys(); }
    }
    tgt.focus();
    tgt.dispatchEvent(new Event("input"));
  });

  // Suggestion pills
  $("#osk-suggestions").addEventListener("click", (e) => {
    const b = e.target.closest(".osk-sugg");
    if (!b || !state.keyboardTarget) return;
    state.keyboardTarget.value = b.textContent;
    state.keyboardTarget.focus();
  });

  function openKeyboard(target) {
    state.keyboardOpen = true;
    state.keyboardTarget = target;
    osk.classList.add("is-open");
    osk.setAttribute("aria-hidden", "false");
    // Push viewport content: ensure the focused input is in view above the keyboard.
    requestAnimationFrame(() => {
      const win = target.closest(".window");
      if (!win) return;
      const keyboardTop = window.innerHeight - osk.offsetHeight - 24;
      const targetRect = target.getBoundingClientRect();
      const overlap = targetRect.bottom - keyboardTop;
      if (overlap > 0) {
        const vp = $("#firefox-viewport");
        if (vp) vp.scrollTop += overlap + 16;
      }
    });
  }
  function closeKeyboard() {
    state.keyboardOpen = false;
    state.keyboardTarget = null;
    osk.classList.remove("is-open");
    osk.setAttribute("aria-hidden", "true");
  }

  // Bind to all keyboard-target inputs
  $$("[data-keyboard-target]").forEach(input => {
    input.addEventListener("focus", () => openKeyboard(input));
    // Note: do NOT close on blur — clicking keyboard keys would trigger blur.
    //       The keyboard closes via ⎚ key, ⏎ key, or Escape.
  });

  // Esc to dismiss keyboard
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && state.keyboardOpen) closeKeyboard();
  });

  // Click outside the OSK and outside any input closes it
  document.addEventListener("pointerdown", (e) => {
    if (!state.keyboardOpen) return;
    if (e.target.closest("#osk")) return;
    if (e.target.closest("[data-keyboard-target]")) return;
    closeKeyboard();
  });

  // ============================================================
  // SCROLL HINT — keep focused input above keyboard while typing
  // ============================================================
  $$("[data-keyboard-target]").forEach(input => {
    input.addEventListener("input", () => {
      if (!state.keyboardOpen) return;
      const r = input.getBoundingClientRect();
      const kbTop = window.innerHeight - osk.offsetHeight - 24;
      if (r.bottom > kbTop) {
        const vp = input.closest(".firefox-viewport");
        if (vp) vp.scrollTop += (r.bottom - kbTop) + 12;
      }
    });
  });

  // ============================================================
  // TRAY PANELS — Network · Bluetooth · Sound
  // ============================================================
  const panels = {
    network:   $("#panel-network"),
    bluetooth: $("#panel-bluetooth"),
    sound:     $("#panel-sound"),
  };
  let openPanel = null;

  function positionPanel(panel, trigger) {
    const tr = trigger.getBoundingClientRect();
    const cx = tr.left + tr.width / 2;
    panel.style.left = cx + "px";
    panel.style.bottom = (window.innerHeight - tr.top + 12) + "px";

    // Keep panel within viewport horizontally.
    requestAnimationFrame(() => {
      const pr = panel.getBoundingClientRect();
      const minX = 16, maxX = window.innerWidth - 16;
      if (pr.left < minX) panel.style.left = (cx + (minX - pr.left)) + "px";
      else if (pr.right > maxX) panel.style.left = (cx - (pr.right - maxX)) + "px";
    });
  }

  function openTrayPanel(name, trigger) {
    if (openPanel && openPanel !== name) closeTrayPanel();
    const p = panels[name];
    if (!p) return;
    positionPanel(p, trigger);
    p.classList.add("is-open");
    p.setAttribute("aria-hidden", "false");
    openPanel = name;
  }
  function closeTrayPanel() {
    if (!openPanel) return;
    panels[openPanel].classList.remove("is-open");
    panels[openPanel].setAttribute("aria-hidden", "true");
    openPanel = null;
  }

  // Wire tray buttons → panels (battery has no panel, we ignore it).
  $$(".dock-tray").forEach(btn => {
    const target = btn.dataset.tray;
    if (!panels[target]) return;
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      if (openPanel === target) closeTrayPanel();
      else openTrayPanel(target, btn);
    });
  });

  // Outside click & Escape
  document.addEventListener("pointerdown", (e) => {
    if (!openPanel) return;
    if (e.target.closest(".tray-panel")) return;
    if (e.target.closest(".dock-tray")) return;
    closeTrayPanel();
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && openPanel) closeTrayPanel();
  });
  window.addEventListener("resize", () => {
    if (!openPanel) return;
    const trigger = document.querySelector(`.dock-tray[data-tray="${openPanel}"]`);
    if (trigger) positionPanel(panels[openPanel], trigger);
  });

  // ============================================================
  // SOUND — volume slider, mute, output/input picker
  // ============================================================
  const volSlider = $("#vol-slider");
  const volValue  = $("#vol-value");
  const volMute   = $("#vol-mute");
  const soundSub  = $("#sound-sub");

  let muted = false;
  let currentSinkName = "MacBook Speakers";

  function updateVolumeUI() {
    const v = parseInt(volSlider.value, 10);
    volSlider.style.setProperty("--fill", v + "%");
    volValue.textContent = muted ? "—" : String(v);
    soundSub.textContent = `${currentSinkName} · ${muted ? "muted" : v + "%"}`;
    volMute.classList.toggle("is-muted", muted || v === 0);
  }
  volSlider.addEventListener("input", () => {
    if (muted && parseInt(volSlider.value, 10) > 0) muted = false;
    updateVolumeUI();
  });
  volMute.addEventListener("click", () => {
    muted = !muted;
    updateVolumeUI();
  });

  function activateRow(selector, picker) {
    $$(selector).forEach(r => {
      r.classList.remove("is-active");
      const c = r.querySelector(".sound-check");
      if (c) c.textContent = "";
    });
    picker.classList.add("is-active");
    const c = picker.querySelector(".sound-check");
    if (c) c.textContent = "✓";
  }

  $$("#panel-sound [data-sink]").forEach(row => {
    row.addEventListener("click", () => {
      activateRow("#panel-sound [data-sink]", row);
      currentSinkName = row.querySelector(".sound-name").textContent.trim();
      updateVolumeUI();
    });
  });
  $$("#panel-sound [data-source]").forEach(row => {
    row.addEventListener("click", () => activateRow("#panel-sound [data-source]", row));
  });

  updateVolumeUI();

  // ============================================================
  // NETWORK — toggle Wi-Fi, select network
  // ============================================================
  const netToggle = $("#net-toggle");
  const netSub    = $("#net-sub");
  const netPanel  = $("#panel-network");
  let currentSsid = "Bacak-Office";

  function applyWifiState(on) {
    netPanel.classList.toggle("is-disabled", !on);
    netSub.textContent = on ? currentSsid : "Wi-Fi off";
    // Dock indicator
    const indicator = document.querySelector('.dock-tray[data-tray="network"]');
    if (indicator) indicator.style.opacity = on ? "" : "0.5";
  }
  netToggle.addEventListener("change", () => applyWifiState(netToggle.checked));

  $$("#panel-network .net-row").forEach(row => {
    row.addEventListener("click", () => {
      const ssid = row.dataset.ssid;
      if (!ssid || !netToggle.checked) return;

      // Demote previous current
      $$("#panel-network .net-row--current").forEach(r => {
        r.classList.remove("net-row--current");
        const m = r.querySelector(".net-meta");
        if (m) m.remove();
      });

      row.classList.add("net-row--current");
      let meta = row.querySelector(".net-meta");
      if (!meta) {
        meta = document.createElement("span");
        meta.className = "net-meta";
        row.querySelector(".net-body").appendChild(meta);
      }
      meta.textContent = "Connecting…";

      const ip = "192.168.1." + (40 + Math.floor(Math.random() * 60));
      setTimeout(() => {
        meta.textContent = `Connected · ${ip}`;
        currentSsid = ssid;
        netSub.textContent = ssid;
      }, 650);
    });
  });

  // ============================================================
  // BLUETOOTH — toggle, scan animation, pair / connect
  // ============================================================
  const btToggle = $("#bt-toggle");
  const btScan   = $("#bt-scan");
  const btSub    = $("#bt-sub");
  const btPanel  = $("#panel-bluetooth");
  const btNearby = $("#bt-nearby");

  function applyBluetoothState(on) {
    btPanel.classList.toggle("is-disabled", !on);
    btSub.textContent = on ? "Discoverable as “bacak-os”" : "Bluetooth off";
    btScan.disabled = !on;
    const indicator = document.querySelector('.dock-tray[data-tray="bluetooth"]');
    if (indicator) indicator.style.opacity = on ? "" : "0.5";
  }
  btToggle.addEventListener("change", () => applyBluetoothState(btToggle.checked));

  btScan.addEventListener("click", () => {
    if (btScan.classList.contains("is-scanning")) return;
    btScan.classList.add("is-scanning");
    btScan.textContent = "Scanning…";
    setTimeout(() => {
      btScan.classList.remove("is-scanning");
      btScan.textContent = "Scan";
      if (!btNearby.querySelector('[data-mac="ee:ff:01"]')) {
        const row = document.createElement("button");
        row.className = "bt-row";
        row.dataset.mac = "ee:ff:01";
        row.innerHTML = `
          <span class="bt-icon" aria-hidden="true">⌚</span>
          <span class="bt-body">
            <span class="bt-name">Bacak Watch</span>
            <span class="bt-meta">Tap to pair</span>
          </span>`;
        bindBtRow(row);
        btNearby.appendChild(row);
      }
    }, 1800);
  });

  function bindBtRow(row) {
    row.addEventListener("click", () => {
      if (!btToggle.checked) return;
      const meta = row.querySelector(".bt-meta");
      if (!meta) return;
      const txt = meta.textContent.trim();
      if (txt === "Tap to pair") {
        meta.textContent = "Pairing…";
        setTimeout(() => {
          meta.textContent = "Connected";
          row.classList.add("bt-row--connected");
        }, 1100);
      } else if (txt === "Not connected") {
        meta.textContent = "Connecting…";
        setTimeout(() => {
          meta.textContent = "Connected";
          row.classList.add("bt-row--connected");
        }, 700);
      } else if (txt === "Connected") {
        meta.textContent = "Disconnecting…";
        setTimeout(() => {
          meta.textContent = "Not connected";
          row.classList.remove("bt-row--connected");
        }, 500);
      }
    });
  }
  $$("#panel-bluetooth .bt-row").forEach(bindBtRow);

  // ============================================================
  // DOCK APP LAUNCH (mock)
  // ============================================================
  $$(".dock-app").forEach(btn => {
    btn.addEventListener("click", () => {
      const app = btn.dataset.app;
      const win = document.querySelector(`.window[data-window-id="${app}"]`);
      if (win) {
        win.style.display = "";
        focusWindow(win);
      } else {
        // Visual feedback only — full app spawn is backend work
        btn.animate(
          [{ transform: "translateY(-10px) scale(1.18)" },
           { transform: "translateY(-2px)  scale(1.06)" },
           { transform: "translateY(0)     scale(1)"    }],
          { duration: 360, easing: "cubic-bezier(0.34, 1.56, 0.64, 1)" }
        );
      }
    });
  });
})();
