import "./dev-mock";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";
import "@fontsource-variable/geist";
import "@fontsource-variable/geist-mono";
import { t, applyI18n, setLang, getLang, LANGUAGES, LANGUAGE_NAMES } from "./i18n";

interface DeviceInfo {
  serial: string;
  state: string;
  model: string | null;
}

interface VideoFrame {
  serial: string;
  width: number;
  height: number;
  /** JPEG en base64. */
  jpeg: string;
}

const AKEYCODE = { BACK: 4, HOME: 3, APP_SWITCH: 187, ENTER: 66, DEL: 67, TAB: 61 } as const;
/** Teclas de la PC que se mandan tal cual al teléfono (moverse por el texto, etc.). */
const KEYMAP: Record<string, number> = {
  Enter: AKEYCODE.ENTER,
  Backspace: AKEYCODE.DEL,
  Tab: AKEYCODE.TAB,
  Escape: AKEYCODE.BACK,
  F12: AKEYCODE.BACK,
  ArrowUp: 19,
  ArrowDown: 20,
  ArrowLeft: 21,
  ArrowRight: 22,
  Home: 122,
  End: 123,
  Delete: 112,
  PageUp: 92,
  PageDown: 93,
};

// ---------- Ajustes (persistidos por usuario en localStorage) ----------
interface Settings {
  maxSize: number;
  fps: number;
  /** Mbps */
  bitrate: number;
  /** 1 (liviano) .. 10 (máxima) */
  quality: number;
  tileWidth: number;
  autoStart: boolean;
  lang: string;
  /** Cómo se nombra a los teléfonos sin nombre propio. */
  naming: "number" | "model";
}
const DEFAULTS: Settings = {
  maxSize: 720,
  fps: 12,
  bitrate: 2,
  quality: 7,
  tileWidth: 220,
  autoStart: true,
  lang: "es",
  naming: "number",
};
function loadSettings(): Settings {
  try {
    return { ...DEFAULTS, ...JSON.parse(localStorage.getItem("panda.settings.v2") ?? "{}") };
  } catch {
    return { ...DEFAULTS };
  }
}
function saveSettings() {
  try {
    localStorage.setItem("panda.settings.v2", JSON.stringify(settings));
  } catch {
    /* almacenamiento no disponible: se usa el valor en memoria */
  }
}
const settings: Settings = loadSettings();
setLang(settings.lang);

/** Calidad 1..10 -> escala `-q:v` de mjpeg (16 = liviano, 2 = máxima). */
const jpegQ = (quality: number) => Math.round(16 - ((quality - 1) * 14) / 9);

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const grid = $<HTMLDivElement>("grid");
const tpl = $<HTMLTemplateElement>("tile-tpl");

// ---------- Tiles ----------
type TileState = "idle" | "connecting" | "live" | "error";

interface Tile {
  serial: string;
  el: HTMLElement;
  screen: HTMLElement;
  canvas: HTMLCanvasElement;
  ctx: CanvasRenderingContext2D;
  sel: HTMLInputElement;
  msg: HTMLSpanElement;
  /** Tamaño del video del dispositivo (coords de toque). */
  w: number;
  h: number;
  state: TileState;
  /** Cuándo falló el último arranque (para reintentar con espera). */
  failedAt: number;
  adbState: string;
  pressing: boolean;
  decoding: boolean;
  pending: VideoFrame | null;
  rec?: { x: number; y: number; t: number; gap: number };
  /** Modelo según adb (se muestra si el teléfono no tiene nombre propio). */
  model: string;
  nameEl: HTMLButtonElement;
}

const tiles = new Map<string, Tile>();
let focused: string | null = null;

// --- Grabación de acciones ---
let recording = false;
const recLines: string[] = [];
let recLastEnd = 0;

function setTileState(tile: Tile, state: TileState, msg = "") {
  tile.state = state;
  tile.el.dataset.state = state;
  // La pantalla también lleva el estado: en la vista en grande sale de la tarjeta.
  tile.screen.dataset.state = state;
  tile.msg.textContent = msg;
  updateStatus();
}

function updateStatus() {
  const pill = $<HTMLDivElement>("status-pill");
  const status = $<HTMLSpanElement>("status");
  const all = [...tiles.values()];
  const live = all.filter((x) => x.state === "live").length;
  const connecting = all.filter((x) => x.state === "connecting").length;
  pill.classList.remove("ok", "busy", "err");
  if (connecting) {
    pill.classList.add("busy");
    status.textContent = t("status.starting", { n: connecting });
  } else if (all.length) {
    pill.classList.add(live === all.length ? "ok" : live ? "busy" : "err");
    status.textContent = t("status.live", { live, n: all.length });
  } else {
    status.textContent = t("status.devices", { n: 0 });
  }
  $<HTMLDivElement>("empty").classList.toggle("show", all.length === 0);
  updateSelCount();
}

function updateSelCount() {
  $<HTMLSpanElement>("sel-count").textContent = tiles.size
    ? t("sel.count", { n: selectedSerials().length, total: tiles.size })
    : "";
  updateActionBar();
}

/** Teléfonos a los que van las acciones de la barra: los seleccionados o, si no hay, el último
 *  que se tocó. */
function actionTargets(): string[] {
  const sel = selectedSerials();
  if (sel.length) return sel;
  return focused && tiles.has(focused) ? [focused] : [];
}

/** La barra de abajo dice siempre a qué teléfonos van las acciones. */
function updateActionBar() {
  const bar = document.getElementById("actionbar");
  if (!bar) return;
  const targets = actionTargets();
  const name = $("ab-name");
  const k = $("ab-k");
  bar.classList.toggle("no-target", targets.length === 0);
  if (!targets.length) {
    k.textContent = t("bar.none");
    name.textContent = t("bar.noneHint");
  } else {
    k.textContent = t("bar.for");
    name.textContent =
      targets.length === 1
        ? displayName(tiles.get(targets[0])!)
        : t("bar.many", { n: targets.length });
  }
  for (const b of bar.querySelectorAll<HTMLButtonElement>("button")) b.disabled = targets.length === 0;
}

function selectedSerials(): string[] {
  return [...tiles.values()].filter((x) => x.sel.checked).map((x) => x.serial);
}

function setSelected(tile: Tile, on: boolean) {
  tile.sel.checked = on;
  tile.el.classList.toggle("selected", on);
}

/** Puntero -> ratio [0,1] dentro del área visible del video (respeta object-fit: contain). */
function ratio(tile: Tile, e: MouseEvent): { rx: number; ry: number } {
  const r = tile.canvas.getBoundingClientRect();
  const w = tile.w || r.width;
  const h = tile.h || r.height;
  const scale = Math.min(r.width / w, r.height / h);
  const cw = w * scale;
  const ch = h * scale;
  const ox = r.left + (r.width - cw) / 2;
  const oy = r.top + (r.height - ch) / 2;
  return {
    rx: Math.min(1, Math.max(0, (e.clientX - ox) / cw)),
    ry: Math.min(1, Math.max(0, (e.clientY - oy) / ch)),
  };
}

// Cola de toques por teléfono: mantiene el orden DOWN → MOVE… → UP (los comandos son async y
// podrían ejecutarse desordenados) y, mientras hay un envío en curso, conserva solo el último
// MOVE. Un arrastre nunca acumula atraso.
interface TouchEv { action: number; x: number; y: number; w: number; h: number }
const touchQueues = new Map<string, { busy: boolean; queue: TouchEv[] }>();

function sendTouch(serial: string, ev: TouchEv) {
  let q = touchQueues.get(serial);
  if (!q) {
    q = { busy: false, queue: [] };
    touchQueues.set(serial, q);
  }
  const last = q.queue[q.queue.length - 1];
  if (ev.action === 2 && last?.action === 2) q.queue[q.queue.length - 1] = ev;
  else q.queue.push(ev);
  if (!q.busy) void drainTouches(serial, q);
}

async function drainTouches(serial: string, q: { busy: boolean; queue: TouchEv[] }) {
  q.busy = true;
  while (q.queue.length) {
    const ev = q.queue.shift()!;
    try {
      await invoke("inject_touch", { serial, action: ev.action, x: ev.x, y: ev.y, width: ev.w, height: ev.h });
    } catch {
      /* sesión cayendo: el evento session-ended se encarga */
    }
  }
  q.busy = false;
}

function touchTo(serial: string, action: number, rx: number, ry: number) {
  const tile = tiles.get(serial);
  if (!tile || tile.state !== "live") return;
  const x = Math.round(rx * (tile.w - 1));
  const y = Math.round(ry * (tile.h - 1));
  sendTouch(serial, { action, x, y, w: tile.w, h: tile.h });
}

/** Objetivos de un gesto o tecla: si el teléfono está seleccionado, todos los seleccionados
 *  (se controlan a la vez); si no, solo ese teléfono. */
function touchTargets(origin: string): string[] {
  const tile = tiles.get(origin);
  return tile?.sel.checked ? selectedSerials() : [origin];
}

function ensureTile(d: DeviceInfo): Tile {
  const existing = tiles.get(d.serial);
  if (existing) return existing;

  const frag = tpl.content.cloneNode(true) as DocumentFragment;
  const el = frag.querySelector(".tile") as HTMLElement;
  const canvas = frag.querySelector("canvas") as HTMLCanvasElement;
  const tile: Tile = {
    serial: d.serial,
    el,
    screen: frag.querySelector(".screen") as HTMLElement,
    canvas,
    ctx: canvas.getContext("2d")!,
    sel: frag.querySelector(".sel") as HTMLInputElement,
    msg: frag.querySelector(".overlay-msg") as HTMLSpanElement,
    w: 0,
    h: 0,
    state: "idle",
    failedAt: 0,
    adbState: d.state,
    pressing: false,
    decoding: false,
    pending: null,
    model: "",
    nameEl: null as unknown as HTMLButtonElement,
  };
  const name = frag.querySelector(".name") as HTMLButtonElement;
  tile.nameEl = name;
  tile.model = d.model ?? d.serial;
  name.textContent = displayName(tile);
  // Clic en el nombre abre la ficha; doble clic en la barra amplía el teléfono. La ficha espera
  // un instante para no abrirse en el primer clic de un doble clic.
  let detailsTimer = 0;
  name.addEventListener("click", () => {
    clearTimeout(detailsTimer);
    detailsTimer = window.setTimeout(() => void openDetails(d.serial), 250);
  });
  el.addEventListener("dblclick", (e) => {
    if ((e.target as HTMLElement).closest("canvas, input, .tile-sel, .shot, .more, .expand")) return;
    clearTimeout(detailsTimer);
    openSpotlight(d.serial);
  });
  (frag.querySelector(".expand") as HTMLButtonElement).addEventListener("click", () => openSpotlight(d.serial));
  initReorder(tile, frag.querySelector(".tile-head") as HTMLElement);
  (frag.querySelector(".more") as HTMLButtonElement).addEventListener("click", (e) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    openTileMenu(tile, r.left, r.bottom + 4);
  });
  // Captura de este teléfono (o de todos los seleccionados si está seleccionado).
  (frag.querySelector(".shot") as HTMLButtonElement).addEventListener("click", () => void takeShots(touchTargets(d.serial)));

  tile.sel.addEventListener("change", () => {
    el.classList.toggle("selected", tile.sel.checked);
    updateSelCount();
  });

  canvas.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    tile.pressing = true;
    canvas.setPointerCapture(e.pointerId);
    setFocus(d.serial);
    const { rx, ry } = ratio(tile, e);
    if (recording && tile.w) {
      const now = performance.now();
      tile.rec = {
        x: Math.round(rx * (tile.w - 1)),
        y: Math.round(ry * (tile.h - 1)),
        t: now,
        gap: recLastEnd ? Math.round(now - recLastEnd) : 0,
      };
    }
    for (const s of touchTargets(d.serial)) touchTo(s, 0, rx, ry);
  });
  canvas.addEventListener("pointermove", (e) => {
    if (!tile.pressing) return;
    const { rx, ry } = ratio(tile, e);
    for (const s of touchTargets(d.serial)) touchTo(s, 2, rx, ry);
  });
  const end = (e: PointerEvent) => {
    if (!tile.pressing) return;
    tile.pressing = false;
    const { rx, ry } = ratio(tile, e);
    if (recording && tile.rec) {
      const now = performance.now();
      const ex = Math.round(rx * (tile.w - 1));
      const ey = Math.round(ry * (tile.h - 1));
      if (tile.rec.gap > 50) recLines.push(`sleep ${tile.rec.gap}`);
      if (Math.hypot(ex - tile.rec.x, ey - tile.rec.y) < 12) {
        recLines.push(`tap ${tile.rec.x} ${tile.rec.y}`);
      } else {
        recLines.push(
          `swipe ${tile.rec.x} ${tile.rec.y} ${ex} ${ey} ${Math.max(100, Math.round(now - tile.rec.t))}`,
        );
      }
      recLastEnd = now;
      tile.rec = undefined;
    }
    for (const s of touchTargets(d.serial)) touchTo(s, 1, rx, ry);
  };
  canvas.addEventListener("pointerup", end);
  canvas.addEventListener("pointercancel", end);

  // Rueda -> scroll.
  canvas.addEventListener(
    "wheel",
    (e) => {
      if (e.ctrlKey) return; // Ctrl + rueda cambia el tamaño de las pantallas (lo maneja la grilla)
      e.preventDefault();
      const { rx, ry } = ratio(tile, e);
      const h = Math.max(-1, Math.min(1, -e.deltaX / 120));
      const v = Math.max(-1, Math.min(1, -e.deltaY / 120));
      for (const s of touchTargets(d.serial)) void scrollTo(s, rx, ry, h, v);
    },
    { passive: false },
  );

  // Clic derecho sobre la pantalla = Atrás. Con Shift, o sobre la barra, abre el menú.
  canvas.addEventListener("contextmenu", (e) => {
    if (e.shiftKey) return;
    e.preventDefault();
    e.stopPropagation();
    setFocus(d.serial);
    for (const s of touchTargets(d.serial)) void keyTo(s, AKEYCODE.BACK);
  });

  applyI18n(frag);
  el.dataset.serial = d.serial;
  placeTile(el, d.serial);
  el.style.setProperty("--i", String(tiles.size));
  tiles.set(d.serial, tile);
  setTileState(tile, "idle", t("tile.idle"));
  return tile;
}

function removeTile(serial: string) {
  const tile = tiles.get(serial);
  if (!tile) return;
  if (spotlight?.serial === serial) closeSpotlight();
  tile.el.remove();
  tiles.delete(serial);
  if (focused === serial) focused = null;
  updateStatus();
}

// ---------- Orden de los teléfonos (arrastrando su barra) ----------
function loadOrder(): string[] {
  try {
    return JSON.parse(localStorage.getItem("aguara.order") ?? "[]");
  } catch {
    return [];
  }
}
let order = loadOrder();

function saveOrder() {
  order = [...grid.querySelectorAll<HTMLElement>(".tile")].map((x) => x.dataset.serial ?? "").filter(Boolean);
  try {
    localStorage.setItem("aguara.order", JSON.stringify(order));
  } catch {
    /* sin almacenamiento: el orden dura hasta cerrar la app */
  }
}

/** Ubica una tarjeta nueva según el orden guardado; las que no están en él van al final. */
function placeTile(el: HTMLElement, serial: string) {
  const idx = order.indexOf(serial);
  if (idx >= 0) {
    for (const other of grid.querySelectorAll<HTMLElement>(".tile")) {
      const j = order.indexOf(other.dataset.serial ?? "");
      if (j < 0 || j > idx) {
        grid.insertBefore(el, other);
        return;
      }
    }
  }
  grid.appendChild(el);
}

/** Arrastrar la barra de un teléfono lo mueve entre los demás; el orden se recuerda. */
function initReorder(tile: Tile, head: HTMLElement) {
  head.addEventListener("pointerdown", (e) => {
    if (e.button !== 0 || (e.target as HTMLElement).closest(".tile-sel, .shot, .more, .expand")) return;
    const x0 = e.clientX;
    const y0 = e.clientY;
    let dragging = false;
    const move = (ev: PointerEvent) => {
      if (!dragging) {
        if (Math.hypot(ev.clientX - x0, ev.clientY - y0) < 6) return;
        dragging = true;
        tile.el.classList.add("reordering");
      }
      const over = document.elementFromPoint(ev.clientX, ev.clientY)?.closest<HTMLElement>(".tile");
      if (!over || over === tile.el) return;
      const r = over.getBoundingClientRect();
      grid.insertBefore(tile.el, ev.clientX > r.left + r.width / 2 ? over.nextSibling : over);
    };
    const onName = !!(e.target as HTMLElement).closest(".name, .name-edit");
    const up = (ev: PointerEvent) => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", up);
      if (!dragging) {
        // Clic suelto en la barra (fuera del nombre): elegir o soltar este teléfono.
        if (!onName && ev.type === "pointerup") {
          setSelected(tile, !tile.sel.checked);
          setFocus(tile.serial);
          updateSelCount();
        }
        return;
      }
      tile.el.classList.remove("reordering");
      saveOrder();
      // El clic que llega al soltar no abre la ficha.
      const swallow = (c: Event) => {
        c.stopPropagation();
        c.preventDefault();
      };
      head.addEventListener("click", swallow, { capture: true });
      setTimeout(() => head.removeEventListener("click", swallow, { capture: true }), 0);
    };
    // En la ventana entera: el puntero sale de la barra apenas empieza a arrastrar.
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", up);
  });
}

// ---------- Vista en grande de un teléfono ----------
let spotlight: { serial: string; home: Comment } | null = null;

/** Lleva la pantalla del teléfono al centro, en grande. Se sigue manejando igual. */
function openSpotlight(serial: string) {
  closeSpotlight();
  const tile = tiles.get(serial);
  if (!tile) return;
  const home = document.createComment("pantalla");
  tile.screen.replaceWith(home);
  $("spot-stage").appendChild(tile.screen);
  $("spot-name").textContent = displayName(tile);
  $("spotlight").classList.add("open");
  $("spotlight").setAttribute("aria-hidden", "false");
  spotlight = { serial, home };
  setFocus(serial);
}

function closeSpotlight() {
  if (!spotlight) return;
  const tile = tiles.get(spotlight.serial);
  if (tile) spotlight.home.replaceWith(tile.screen);
  else spotlight.home.remove();
  $("spotlight").classList.remove("open");
  $("spotlight").setAttribute("aria-hidden", "true");
  spotlight = null;
}

$("spot-close").addEventListener("click", closeSpotlight);
$("spotlight").addEventListener("click", (e) => {
  if (!(e.target as HTMLElement).closest(".screen, .spot-bar")) closeSpotlight();
});

// ---------- Tamaño de las pantallas: Ctrl + rueda ----------
function setTileWidth(w: number) {
  settings.tileWidth = Math.max(140, Math.min(440, Math.round(w / 10) * 10));
  document.documentElement.style.setProperty("--tile-w", `${settings.tileWidth}px`);
  $<HTMLInputElement>("tile-width").value = String(settings.tileWidth);
  saveSettings();
}
grid.addEventListener(
  "wheel",
  (e) => {
    if (!e.ctrlKey) return;
    e.preventDefault();
    setTileWidth(settings.tileWidth + (e.deltaY < 0 ? 20 : -20));
  },
  { passive: false },
);

// ---------- Botón de pánico ----------
const panicOpen = () => $("panic-dialog").classList.contains("open");

function closePanic() {
  $("panic-dialog").classList.remove("open");
  $("panic-dialog").setAttribute("aria-hidden", "true");
}

function connectedSerials(): string[] {
  return [...tiles.values()].filter((x) => x.adbState === "device").map((x) => x.serial);
}

$("panic").addEventListener("click", () => {
  const n = connectedSerials().length;
  if (!n) {
    toast(t("panic.none"), "err");
    return;
  }
  $("panic-body").textContent = t("panic.body", { n });
  $("panic-dialog").classList.add("open");
  $("panic-dialog").setAttribute("aria-hidden", "false");
  $("panic-cancel").focus();
});
$("panic-cancel").addEventListener("click", closePanic);
$("panic-dialog").addEventListener("click", (e) => {
  if (e.target === $("panic-dialog")) closePanic();
});

// Mantener apretado 2 s para confirmar: un clic suelto no alcanza.
{
  const hold = $<HTMLButtonElement>("panic-hold");
  let timer = 0;
  const cancel = () => {
    clearTimeout(timer);
    hold.classList.remove("holding");
  };
  hold.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    hold.classList.add("holding");
    timer = window.setTimeout(() => {
      hold.classList.remove("holding");
      closePanic();
      void runPanic();
    }, 2000);
  });
  for (const ev of ["pointerup", "pointerleave", "pointercancel"]) hold.addEventListener(ev, cancel);
}

async function runPanic() {
  const serials = connectedSerials();
  toast(t("panic.running", { n: serials.length }), "", 4000);
  const results = await Promise.allSettled(serials.map((serial) => invoke<number>("panic_wipe", { serial })));
  const ok = results.filter((r) => r.status === "fulfilled").length;
  toast(t("panic.done", { ok, n: serials.length }), ok === serials.length ? "ok" : "err", 8000);
}

function setFocus(serial: string) {
  focused = serial;
  for (const [s, x] of tiles) x.el.classList.toggle("focused", s === serial);
  updateActionBar();
}

// ---------- Selección por recuadro (clic izquierdo + arrastrar, como en el escritorio) ----------
// Arrastrar con clic izquierdo desde un espacio vacío (o el marco de una tarjeta) dibuja un
// recuadro y selecciona los teléfonos que toca; sobre la PANTALLA de un teléfono, arrastrar
// sigue siendo deslizar el dedo. Clic suelto en el marco = seleccionar solo ese; Ctrl/Shift
// suma o quita. Clic en un hueco = deseleccionar todo. Clic derecho = menú de opciones.
const DRAG_THRESHOLD = 4;

interface Marquee {
  pointerId: number;
  x0: number;
  y0: number;
  /** Posición del contenido al empezar (el recuadro sigue al contenido al desplazar). */
  scroll0: number;
  x: number;
  y: number;
  dragging: boolean;
  additive: boolean;
  base: Set<string>;
  /** Tarjeta bajo el puntero al empezar (clic suelto en su marco = seleccionarla). */
  origin: string | null;
  box: HTMLDivElement | null;
  raf: number;
}
let marquee: Marquee | null = null;

function tileAt(target: EventTarget | null): Tile | null {
  for (const tile of tiles.values()) if (tile.el.contains(target as Node)) return tile;
  return null;
}

/** El recuadro no arranca sobre la pantalla del teléfono ni sobre controles. */
function startsMarquee(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  return !!el && !el.closest("canvas, button, input, label, select, textarea, .tile-head");
}

function marqueeRect(m: Marquee) {
  // Coords de pantalla del punto inicial, corregidas por el scroll ocurrido desde entonces.
  const y0 = m.y0 - (grid.scrollTop - m.scroll0);
  return {
    left: Math.min(m.x0, m.x),
    top: Math.min(y0, m.y),
    right: Math.max(m.x0, m.x),
    bottom: Math.max(y0, m.y),
  };
}

function updateMarquee(m: Marquee) {
  const r = marqueeRect(m);
  if (!m.box) {
    m.box = document.createElement("div");
    m.box.className = "marquee";
    document.body.appendChild(m.box);
    grid.classList.add("marquee-active");
  }
  Object.assign(m.box.style, {
    left: `${r.left}px`,
    top: `${r.top}px`,
    width: `${r.right - r.left}px`,
    height: `${r.bottom - r.top}px`,
  });
  for (const tile of tiles.values()) {
    const b = tile.el.getBoundingClientRect();
    const hit = b.left < r.right && b.right > r.left && b.top < r.bottom && b.bottom > r.top;
    setSelected(tile, hit || (m.additive && m.base.has(tile.serial)));
  }
  updateSelCount();
}

/** Desplaza la grilla si el puntero está cerca del borde durante el arrastre. */
function autoScroll() {
  const m = marquee;
  if (!m || !m.dragging) return;
  const r = grid.getBoundingClientRect();
  const edge = 40;
  let dy = 0;
  if (m.y < r.top + edge) dy = -Math.ceil((r.top + edge - m.y) / 4);
  else if (m.y > r.bottom - edge) dy = Math.ceil((m.y - (r.bottom - edge)) / 4);
  if (dy) {
    grid.scrollTop += dy;
    updateMarquee(m);
  }
  m.raf = requestAnimationFrame(autoScroll);
}

function endMarquee() {
  if (!marquee) return;
  cancelAnimationFrame(marquee.raf);
  marquee.box?.remove();
  grid.classList.remove("marquee-active");
  marquee = null;
}

grid.addEventListener("pointerdown", (e) => {
  if (e.button !== 0 || !startsMarquee(e.target)) return;
  e.preventDefault();
  closeMenu();
  grid.setPointerCapture(e.pointerId);
  marquee = {
    pointerId: e.pointerId,
    x0: e.clientX,
    y0: e.clientY,
    scroll0: grid.scrollTop,
    x: e.clientX,
    y: e.clientY,
    dragging: false,
    additive: e.ctrlKey || e.shiftKey || e.metaKey,
    base: new Set(selectedSerials()),
    origin: tileAt(e.target)?.serial ?? null,
    box: null,
    raf: 0,
  };
});

grid.addEventListener("pointermove", (e) => {
  const m = marquee;
  if (!m || e.pointerId !== m.pointerId) return;
  m.x = e.clientX;
  m.y = e.clientY;
  if (!m.dragging && Math.hypot(m.x - m.x0, m.y - m.y0) < DRAG_THRESHOLD) return;
  if (!m.dragging) {
    m.dragging = true;
    m.raf = requestAnimationFrame(autoScroll);
  }
  updateMarquee(m);
});

grid.addEventListener("pointerup", (e) => {
  const m = marquee;
  if (!m || e.pointerId !== m.pointerId) return;
  if (!m.dragging) {
    // Clic suelto: en el marco de una tarjeta la selecciona (Ctrl/Shift: suma o quita);
    // en un hueco deselecciona todo (salvo con Ctrl/Shift).
    const origin = m.origin ? tiles.get(m.origin) : null;
    if (origin) {
      if (m.additive) setSelected(origin, !origin.sel.checked);
      else for (const tile of tiles.values()) setSelected(tile, tile === origin);
      setFocus(origin.serial);
    } else if (!m.additive) {
      for (const tile of tiles.values()) setSelected(tile, false);
    }
    updateSelCount();
  }
  endMarquee();
});
grid.addEventListener("pointercancel", endMarquee);
grid.addEventListener("lostpointercapture", endMarquee);

// ---------- Menú de opciones (clic derecho) ----------
// Sobre un teléfono: opciones para ese teléfono, o para toda la selección si el teléfono es
// parte de una selección de varios. Sobre un hueco: seleccionar todos / ninguno.
const QUICK_APPS = ["com.instagram.android", "com.zhiliaoapp.musically", "com.facebook.katana", "com.whatsapp", "com.twitter.android", "com.instagram.barcelona", "com.google.android.youtube"];

let menuEl: HTMLDivElement | null = null;

// ---------- Facebook: aceptar solicitudes de amistad ----------
// ---------- Tareas automáticas en curso (con botón para detenerlas) ----------
const runningTasks = new Map<string, number>();

function updateTasksButton() {
  const n = runningTasks.size;
  const btn = $<HTMLButtonElement>("stop-tasks");
  btn.hidden = n === 0;
  btn.disabled = false;
  $("stop-tasks-label").textContent = t("tasks.stop", { n });
}

/** Marca el teléfono como ocupado mientras corre la tarea y lo libera al terminar. */
async function runTask<T>(serial: string, task: () => Promise<T>): Promise<T> {
  runningTasks.set(serial, (runningTasks.get(serial) ?? 0) + 1);
  tiles.get(serial)?.el.classList.add("task-running");
  updateTasksButton();
  try {
    return await task();
  } finally {
    const left = (runningTasks.get(serial) ?? 1) - 1;
    if (left > 0) runningTasks.set(serial, left);
    else {
      runningTasks.delete(serial);
      tiles.get(serial)?.el.classList.remove("task-running");
    }
    updateTasksButton();
  }
}

function stopTasks(serials: string[] = []) {
  void invoke("stop_tasks", { serials });
  $<HTMLButtonElement>("stop-tasks").disabled = true;
  toast(t("tasks.stopping"), "", 3000);
}
$("stop-tasks").addEventListener("click", () => stopTasks());

async function acceptFriendRequests(serials: string[]) {
  const results = await Promise.allSettled(
    serials.map((s) => runTask(s, () => invoke<number>("accept_friend_requests", { serial: s, max: 20 }))),
  );
  const lines = results.map((r, i) => {
    const tile = tiles.get(serials[i]);
    const name = tile ? displayName(tile) : serials[i];
    return r.status === "fulfilled"
      ? t(r.value === 0 ? "fb.none" : "fb.accepted", { name, n: r.value })
      : t("fb.failed", { name, e: String(r.reason) });
  });
  toast(lines.join("\n"), results.every((r) => r.status === "fulfilled") ? "ok" : "err");
}

// ---------- Audio del teléfono en la PC ----------
const audioOn = new Set<string>();

function markAudio(serial: string, on: boolean) {
  if (on) audioOn.add(serial);
  else audioOn.delete(serial);
  tiles.get(serial)?.el.classList.toggle("audio-on", on);
}

async function toggleAudio(serial: string, on: boolean) {
  const tile = tiles.get(serial);
  try {
    if (on) {
      tile?.el.classList.add("audio-starting");
      await invoke("start_audio", { serial });
    } else {
      await invoke("stop_audio", { serial });
    }
    markAudio(serial, on);
  } catch (e) {
    toast(t("audio.error", { e: String(e) }), "err");
  } finally {
    tile?.el.classList.remove("audio-starting");
  }
}

// Si el teléfono se desconecta, ffplay termina: reflejarlo en la UI.
setInterval(async () => {
  if (!audioOn.size) return;
  const live = new Set(await invoke<string[]>("list_audio").catch(() => [] as string[]));
  for (const s of [...audioOn]) if (!live.has(s)) markAudio(s, false);
}, 4000);

function closeMenu() {
  menuEl?.remove();
  menuEl = null;
}

type MenuItem =
  | { label: string; run: () => unknown; danger?: boolean }
  | { sep: true }
  | { title: string }
  | { apps: { label: string; color: string; run: () => unknown }[] };

function showMenu(x: number, y: number, items: MenuItem[]) {
  closeMenu();
  const menu = document.createElement("div");
  menu.className = "ctx-menu";
  menu.setAttribute("role", "menu");
  for (const it of items) {
    if ("sep" in it) {
      menu.appendChild(el("div", "ctx-sep"));
    } else if ("title" in it) {
      menu.appendChild(el("div", "ctx-title", it.title));
    } else if ("apps" in it) {
      const row = el("div", "ctx-apps");
      for (const a of it.apps) {
        const b = el("button", "ctx-app", a.label.slice(0, 1));
        b.style.background = a.color;
        b.title = t("dv.open", { app: a.label });
        b.addEventListener("click", () => {
          closeMenu();
          void a.run();
        });
        row.appendChild(b);
      }
      menu.appendChild(row);
    } else {
      const b = el("button", `ctx-item${it.danger ? " danger" : ""}`, it.label);
      b.setAttribute("role", "menuitem");
      b.addEventListener("click", () => {
        closeMenu();
        void it.run();
      });
      menu.appendChild(b);
    }
  }
  document.body.appendChild(menu);
  // Mantener dentro de la ventana.
  const r = menu.getBoundingClientRect();
  menu.style.left = `${Math.min(x, window.innerWidth - r.width - 8)}px`;
  menu.style.top = `${Math.min(y, window.innerHeight - r.height - 8)}px`;
  menuEl = menu;
  (menu.querySelector("button") as HTMLButtonElement | null)?.focus({ preventScroll: true });
}

async function forEachSerial(serials: string[], fn: (s: string) => Promise<unknown>) {
  await Promise.allSettled(serials.map((s) => fn(s)));
}

function openTileMenu(tile: Tile, x: number, y: number) {
  const sel = selectedSerials();
  const group = tile.sel.checked && sel.length > 1;
  const targets = group ? sel : [tile.serial];
  const title = group ? t("menu.nPhones", { n: targets.length }) : displayName(tile);
  const keys = (code: number) => () => forEachSerial(targets, (s) => keyTo(s, code));
  const apps = QUICK_APPS.map((pkg) => SOCIAL_APPS.find((a) => a.pkg === pkg)!).map((a) => ({
    label: a.label,
    color: a.color,
    run: () => forEachSerial(targets, (s) => invoke("launch_app", { serial: s, package: a.pkg })),
  }));
  showMenu(x, y, [
    { title },
    ...(targets.some((s) => runningTasks.has(s))
      ? [{ label: t("tasks.stopHere"), run: () => stopTasks(targets) }, { sep: true as const }]
      : []),
    { label: t("menu.rename"), run: () => renameTile(tile) },
    { label: t("menu.details"), run: () => openDetails(tile.serial) },
    { sep: true },
    { label: t("nav.back"), run: keys(4) },
    { label: t("nav.home"), run: keys(3) },
    { label: t("nav.recents"), run: keys(187) },
    { label: t("act.volUp"), run: keys(24) },
    { label: t("act.volDown"), run: keys(25) },
    { label: t("nav.unlock"), run: () => unlockScreens(targets) },
    { label: t("dv.power"), run: keys(26) },
    { label: t("menu.paste"), run: () => pastePcClipboard(targets) },
    { label: t("menu.openUrl"), run: () => openCopiedUrl(targets) },
    { sep: true },
    { title: t("menu.openApp") },
    { apps },
    { sep: true },
    { label: t("menu.fbAccept"), run: () => acceptFriendRequests(targets) },
    { sep: true },
    audioOn.has(tile.serial)
      ? { label: t("menu.audioOff"), run: () => toggleAudio(tile.serial, false) }
      : { label: t("menu.audioOn"), run: () => toggleAudio(tile.serial, true) },
    { label: t("dv.shot"), run: () => takeShots(targets) },
    { label: t("menu.shotsDir"), run: () => invoke("open_shots_dir") },
    {
      label: t("dv.restartStream"),
      run: () =>
        forEachSerial(targets, async (s) => {
          await invoke("stop_mirror", { serial: s }).catch(() => {});
          const tl = tiles.get(s);
          if (tl) setTileState(tl, "idle", t("tile.idle"));
          await startOne(s);
        }),
    },
    { sep: true },
    {
      label: tile.sel.checked ? t("menu.unselect") : t("menu.select"),
      run: () => {
        setSelected(tile, !tile.sel.checked);
        updateSelCount();
      },
    },
  ]);
}

grid.addEventListener("contextmenu", (e) => {
  e.preventDefault();
  endMarquee();
  const tile = tileAt(e.target);
  if (tile) {
    setFocus(tile.serial);
    openTileMenu(tile, e.clientX, e.clientY);
    return;
  }
  showMenu(e.clientX, e.clientY, [
    { label: t("btn.selectAllLong"), run: () => $("select-all").click() },
    { label: t("btn.selectNoneLong"), run: () => $("select-none").click() },
    { sep: true },
    { label: t("btn.search"), run: () => refresh() },
  ]);
});

// Cerrar el menú: clic fuera, Escape, scroll o cambio de tamaño.
window.addEventListener("pointerdown", (e) => {
  if (menuEl && !menuEl.contains(e.target as Node)) closeMenu();
}, true);
window.addEventListener(
  "keydown",
  (e) => {
    if (e.key === "Escape" && menuEl) {
      e.stopImmediatePropagation();
      closeMenu();
    }
  },
  true,
);
grid.addEventListener("scroll", closeMenu);
window.addEventListener("resize", closeMenu);
window.addEventListener("blur", closeMenu);

// ---------- Nombres propios por teléfono ----------
function loadAliases(): Record<string, string> {
  try {
    return JSON.parse(localStorage.getItem("panda.aliases") ?? "{}");
  } catch {
    return {};
  }
}
const aliases = loadAliases();
function saveAlias(serial: string, alias: string) {
  if (alias) aliases[serial] = alias;
  else delete aliases[serial];
  try {
    localStorage.setItem("panda.aliases", JSON.stringify(aliases));
  } catch {
    /* almacenamiento no disponible: queda en memoria */
  }
}

/** Cambia el nombre del teléfono ahí mismo, en su barra. Vacío = vuelve al número o modelo. */
function renameTile(tile: Tile) {
  const input = document.createElement("input");
  input.type = "text";
  input.className = "name-edit";
  input.value = aliases[tile.serial] ?? "";
  input.placeholder = defaultName(tile);
  let done = false;
  const finish = (save: boolean) => {
    if (done) return;
    done = true;
    if (save) saveAlias(tile.serial, input.value.trim());
    input.replaceWith(tile.nameEl);
    refreshNames();
  };
  input.addEventListener("keydown", (e) => {
    e.stopPropagation();
    if (e.key === "Enter") finish(true);
    else if (e.key === "Escape") finish(false);
  });
  input.addEventListener("blur", () => finish(true));
  // Que arrastrar o hacer clic en el campo no mueva la tarjeta ni abra la ficha.
  input.addEventListener("pointerdown", (e) => e.stopPropagation());
  tile.nameEl.replaceWith(input);
  input.focus();
  input.select();
}

// ---------- Nombre visible por cuenta ----------
// Facebook guarda sus cuentas en Android con un ID numérico; el nombre que el usuario le asigna
// es el que se busca en el selector de cuentas de la app para entrar con un clic.
function loadAccountNames(): Record<string, string> {
  try {
    return JSON.parse(localStorage.getItem("panda.accountNames") ?? "{}");
  } catch {
    return {};
  }
}
const accountNames = loadAccountNames();
const accountKey = (serial: string, a: Account) => `${serial}|${a.kind}|${a.name}`;
function saveAccountName(serial: string, a: Account, name: string) {
  const k = accountKey(serial, a);
  if (name && name !== a.name) accountNames[k] = name;
  else delete accountNames[k];
  try {
    localStorage.setItem("panda.accountNames", JSON.stringify(accountNames));
  } catch {
    /* almacenamiento no disponible: queda en memoria */
  }
}
function displayName(tile: Tile): string {
  return aliases[tile.serial] || defaultName(tile);
}

/** Nombre del teléfono cuando no tiene uno propio: su número fijo o su modelo. */
function defaultName(tile: Tile): string {
  return settings.naming === "number" ? t("tile.numbered", { n: phoneNumber(tile.serial) }) : tile.model;
}

// ---------- Número fijo de cada teléfono ----------
// El número sigue al teléfono (no a su lugar en la grilla), así coincide con la etiqueta que se
// le pegue al aparato. Un teléfono nuevo toma el primer número libre.
function loadNumbers(): Record<string, number> {
  try {
    return JSON.parse(localStorage.getItem("aguara.numbers") ?? "{}");
  } catch {
    return {};
  }
}
let phoneNumbers = loadNumbers();

function saveNumbers() {
  try {
    localStorage.setItem("aguara.numbers", JSON.stringify(phoneNumbers));
  } catch {
    /* sin almacenamiento: los números duran hasta cerrar la app */
  }
}

function phoneNumber(serial: string): number {
  if (!phoneNumbers[serial]) {
    const used = new Set(Object.values(phoneNumbers));
    let n = 1;
    while (used.has(n)) n++;
    phoneNumbers[serial] = n;
    saveNumbers();
  }
  return phoneNumbers[serial];
}

/** Vuelve a numerar 1, 2, 3… siguiendo el orden actual de la grilla. */
function renumberByOrder() {
  phoneNumbers = {};
  let n = 1;
  for (const el of grid.querySelectorAll<HTMLElement>(".tile")) {
    if (el.dataset.serial) phoneNumbers[el.dataset.serial] = n++;
  }
  saveNumbers();
  refreshNames();
}

function refreshNames() {
  for (const tile of tiles.values()) tile.nameEl.textContent = displayName(tile);
  if (spotlight) {
    const tile = tiles.get(spotlight.serial);
    if (tile) $("spot-name").textContent = displayName(tile);
  }
}

/** Ruta de una captura dentro de Imágenes\Aguará: una carpeta por día y el nombre del
 *  teléfono con la hora, p. ej. `2026-10-29/Telefono 3_14-05-12.png`. */
function shotPath(serial: string): string {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  const day = `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
  const time = `${p(d.getHours())}-${p(d.getMinutes())}-${p(d.getSeconds())}`;
  const tile = tiles.get(serial);
  const name = (tile ? displayName(tile) : serial).replace(/[\\/:*?"<>|.]+/g, "_").trim() || serial;
  return `${day}/${name}_${time}.png`;
}

/** Captura los teléfonos y marca cada tarjeta un instante como confirmación. */
function takeShots(serials: string[]) {
  return forEachSerial(serials, async (s) => {
    await invoke<string>("screenshot", { serial: s, out: shotPath(s) });
    const el = tiles.get(s)?.el;
    el?.classList.add("shot-flash");
    setTimeout(() => el?.classList.remove("shot-flash"), 400);
  });
}

// ---------- Ficha del teléfono ----------
interface Account { kind: string; name: string }
interface DeviceDetails {
  brand: string;
  model: string;
  android: string;
  battery_level: number | null;
  charging: boolean;
  storage_total_kb: number | null;
  storage_free_kb: number | null;
  ram_total_kb: number | null;
  ram_available_kb: number | null;
  screen_on: boolean | null;
  ip: string | null;
  accounts: Account[];
  packages: string[];
}

/** Apps sociales conocidas: paquete, nombre, tipos de cuenta de Android y color de marca. */
const SOCIAL_APPS: { pkg: string; label: string; kinds: string[]; color: string }[] = [
  { pkg: "com.instagram.android", label: "Instagram", kinds: ["www.instagram.com", "com.instagram.android"], color: "#d62976" },
  { pkg: "com.instagram.barcelona", label: "Threads", kinds: ["www.instagram.barcelona"], color: "#3a3a3a" },
  { pkg: "com.facebook.katana", label: "Facebook", kinds: ["com.facebook.auth.login"], color: "#1877f2" },
  { pkg: "com.facebook.lite", label: "Facebook Lite", kinds: ["com.facebook.lite"], color: "#1877f2" },
  { pkg: "com.facebook.orca", label: "Messenger", kinds: ["com.facebook.messenger"], color: "#a033ff" },
  { pkg: "com.zhiliaoapp.musically", label: "TikTok", kinds: ["com.zhiliaoapp.musically", "com.zhiliaoapp.account"], color: "#111111" },
  { pkg: "com.ss.android.ugc.trill", label: "TikTok", kinds: ["com.ss.android.ugc.trill"], color: "#111111" },
  { pkg: "com.twitter.android", label: "X", kinds: ["com.twitter.android.auth.login"], color: "#222222" },
  { pkg: "com.whatsapp", label: "WhatsApp", kinds: ["com.whatsapp"], color: "#25d366" },
  { pkg: "com.whatsapp.w4b", label: "WhatsApp Business", kinds: ["com.whatsapp.w4b"], color: "#128c7e" },
  { pkg: "com.google.android.youtube", label: "YouTube", kinds: [], color: "#ff0000" },
  { pkg: "org.telegram.messenger", label: "Telegram", kinds: ["org.telegram.messenger"], color: "#229ed9" },
  { pkg: "com.linkedin.android", label: "LinkedIn", kinds: ["com.linkedin.android"], color: "#0a66c2" },
  { pkg: "com.pinterest", label: "Pinterest", kinds: ["com.pinterest"], color: "#e60023" },
  { pkg: "com.snapchat.android", label: "Snapchat", kinds: ["com.snapchat.android"], color: "#c9b800" },
  { pkg: "com.android.chrome", label: "Chrome", kinds: [], color: "#4285f4" },
];
const OTHER_KINDS: Record<string, { label: string; color: string }> = {
  "com.google": { label: "Google", color: "#4285f4" },
  "com.google.android.apps.tachyon": { label: "Google Meet", color: "#00897b" },
  "com.xiaomi": { label: "Xiaomi", color: "#ff6900" },
  "com.osp.app.signin": { label: "Samsung", color: "#1428a0" },
  "com.microsoft.workaccount": { label: "Microsoft", color: "#0078d4" },
};

function kindInfo(kind: string): { label: string; color: string } {
  const app = SOCIAL_APPS.find((a) => a.kinds.includes(kind));
  if (app) return { label: app.label, color: app.color };
  return OTHER_KINDS[kind] ?? { label: kind, color: "#4a5268" };
}

let drawerSerial: string | null = null;
let rebootArmed = 0;

function fmtGB(kb: number): string {
  const gb = kb / 1024 / 1024;
  return gb >= 10 ? `${gb.toFixed(0)} GB` : `${gb.toFixed(1)} GB`;
}

function el<K extends keyof HTMLElementTagNameMap>(tag: K, cls = "", text = ""): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text) e.textContent = text;
  return e;
}

function stat(k: string, v: string, ratio?: number, level: "" | "warn" | "bad" = "") {
  const box = el("div", `stat ${level}`);
  box.append(el("div", "k", k), el("div", "v", v));
  if (ratio !== undefined) {
    const bar = el("div", "bar");
    const fill = el("span");
    fill.style.width = `${Math.round(Math.max(0, Math.min(1, ratio)) * 100)}%`;
    bar.appendChild(fill);
    box.appendChild(bar);
  }
  return box;
}

function renderDetails(serial: string, d: DeviceDetails) {
  const tile = tiles.get(serial);
  if (tile && d.model) {
    tile.model = `${d.brand} ${d.model}`.trim();
    tile.nameEl.textContent = displayName(tile);
    $<HTMLInputElement>("dv-alias").placeholder = tile.model;
  }
  $<HTMLDivElement>("dv-sub").textContent = `${d.brand} ${d.model} · Android ${d.android} · ${serial}`;

  const stats = $<HTMLDivElement>("dv-stats");
  stats.innerHTML = "";
  if (d.battery_level !== null) {
    const b = d.battery_level;
    stats.appendChild(
      stat(t("dv.battery"), `${b}%${d.charging ? ` · ${t("dv.charging")}` : ""}`, b / 100, b < 15 ? "bad" : b < 30 ? "warn" : ""),
    );
  }
  if (d.screen_on !== null) {
    stats.appendChild(stat(t("dv.screen"), d.screen_on ? t("dv.screenOn") : t("dv.screenOff")));
  }
  if (d.storage_free_kb !== null && d.storage_total_kb) {
    const r = d.storage_free_kb / d.storage_total_kb;
    stats.appendChild(
      stat(t("dv.storage"), `${fmtGB(d.storage_free_kb)} ${t("dv.of")} ${fmtGB(d.storage_total_kb)}`, r, r < 0.05 ? "bad" : r < 0.15 ? "warn" : ""),
    );
  }
  if (d.ram_available_kb !== null && d.ram_total_kb) {
    const r = d.ram_available_kb / d.ram_total_kb;
    stats.appendChild(
      stat(t("dv.ram"), `${fmtGB(d.ram_available_kb)} ${t("dv.of")} ${fmtGB(d.ram_total_kb)}`, r, r < 0.1 ? "bad" : r < 0.2 ? "warn" : ""),
    );
  }
  stats.appendChild(stat(t("dv.ip"), d.ip ?? t("dv.noIp")));
  stats.appendChild(stat(t("dv.android"), d.android || "—"));

  const installed = new Set(d.packages);

  // Cuentas agrupadas por red.
  const accBox = $<HTMLDivElement>("dv-accounts");
  accBox.innerHTML = "";
  $<HTMLSpanElement>("dv-acc-count").textContent = d.accounts.length ? `(${d.accounts.length})` : "";
  const groups = new Map<string, { color: string; accounts: Account[] }>();
  for (const a of d.accounts) {
    const info = kindInfo(a.kind);
    const g = groups.get(info.label) ?? { color: info.color, accounts: [] };
    g.accounts.push(a);
    groups.set(info.label, g);
  }
  if (!groups.size) accBox.appendChild(el("p", "hint", t("dv.noAccounts")));
  for (const [label, g] of groups) {
    const box = el("div", "acc-group");
    const head = el("div", "acc-group-head");
    const badge = el("span", "acc-badge", label.slice(0, 1).toUpperCase());
    badge.style.background = g.color;
    head.append(badge, el("span", "", label), el("span", "n", String(g.accounts.length)));
    box.appendChild(head);
    const canEnter = SOCIAL_APPS.some((x) => x.kinds.includes(g.accounts[0].kind) && installed.has(x.pkg));
    for (const a of g.accounts) {
      const row = el("div", "acc-name");
      // Clic en la cuenta = abrir la red social en esa cuenta.
      const shown = accountNames[accountKey(serial, a)];
      const enter = el("button", "acc-enter", shown ?? a.name);
      enter.title = `${t("dv.enterHelp", { app: label })}${shown ? ` · ${a.name}` : ""}`;
      enter.disabled = !canEnter;
      enter.addEventListener("click", () => void enterAccount(serial, a, label, enter));
      const rename = el("button", "copy icon");
      rename.innerHTML = `<svg class="ic"><use href="#i-edit" /></svg>`;
      rename.title = t("dv.renameAcc");
      rename.addEventListener("click", () => editAccountName(serial, a, enter));
      const copy = el("button", "copy", t("dv.copy"));
      copy.addEventListener("click", () => void copyText(a.name));
      row.append(enter, rename, copy);
      box.appendChild(row);
    }
    accBox.appendChild(box);
  }

  // Apps sociales instaladas, con botón para abrirlas.
  const apps = $<HTMLDivElement>("dv-apps");
  apps.innerHTML = "";
  const present = SOCIAL_APPS.filter((a) => installed.has(a.pkg));
  if (!present.length) apps.appendChild(el("p", "hint", t("dv.noApps")));
  for (const a of present) {
    const b = el("button", "app-btn");
    const dot = el("span", "acc-badge", a.label.slice(0, 1));
    dot.style.background = a.color;
    b.append(dot, el("span", "", a.label));
    b.title = t("dv.open", { app: a.label });
    b.addEventListener("click", async () => {
      try {
        await invoke("launch_app", { serial, package: a.pkg });
        feedback("dv-feedback", t("dv.opened", { app: a.label }), "ok");
      } catch (e) {
        feedback("dv-feedback", String(e), "err");
      }
    });
    apps.appendChild(b);
  }
}

/** Abre la red social de la cuenta en el teléfono y la elige en el selector de cuentas. */
async function enterAccount(serial: string, a: Account, label: string, btn: HTMLButtonElement) {
  btn.classList.add("busy");
  feedback("dv-feedback", t("dv.entering", { app: label }), "ok");
  try {
    const name = accountNames[accountKey(serial, a)] ?? a.name;
    const r = await runTask(serial, () => invoke<string>("switch_account", { serial, kind: a.kind, name }));
    feedback("dv-feedback", r === "switched" ? t("dv.entered", { app: label }) : t("dv.openedPick", { app: label }), "ok");
  } catch (e) {
    feedback("dv-feedback", t("dv.error", { e: String(e) }), "err");
  } finally {
    btn.classList.remove("busy");
  }
}

/** Cambia el botón de la cuenta por un campo para escribir el nombre tal como lo muestra la app. */
function editAccountName(serial: string, a: Account, btn: HTMLButtonElement) {
  const input = el("input", "acc-rename");
  input.value = accountNames[accountKey(serial, a)] ?? "";
  input.placeholder = t("dv.renamePh");
  let done = false;
  const finish = (save: boolean) => {
    if (done) return;
    done = true;
    if (save) saveAccountName(serial, a, input.value.trim());
    const shown = accountNames[accountKey(serial, a)];
    btn.textContent = shown ?? a.name;
    btn.title = `${t("dv.enterHelp", { app: kindInfo(a.kind).label })}${shown ? ` · ${a.name}` : ""}`;
    input.replaceWith(btn);
  };
  input.addEventListener("keydown", (e) => {
    e.stopPropagation();
    if (e.key === "Enter") finish(true);
    else if (e.key === "Escape") finish(false);
  });
  input.addEventListener("blur", () => finish(true));
  btn.replaceWith(input);
  input.focus();
}

async function loadDetails(serial: string) {
  const refreshBtn = $<HTMLButtonElement>("dv-refresh");
  refreshBtn.classList.add("spinning");
  for (const id of ["dv-stats", "dv-accounts", "dv-apps"]) {
    $<HTMLDivElement>(id).innerHTML = '<div class="skeleton"></div>';
  }
  try {
    const d = await invoke<DeviceDetails>("device_details", { serial });
    if (drawerSerial === serial) renderDetails(serial, d);
  } catch (e) {
    if (drawerSerial === serial) {
      for (const id of ["dv-stats", "dv-accounts", "dv-apps"]) $<HTMLDivElement>(id).innerHTML = "";
      feedback("dv-feedback", t("dv.error", { e: String(e) }), "err");
    }
  } finally {
    refreshBtn.classList.remove("spinning");
  }
}

async function openDetails(serial: string) {
  const tile = tiles.get(serial);
  if (!tile) return;
  drawerSerial = serial;
  rebootArmed = 0;
  resetRebootBtn();
  setFocus(serial);
  const alias = $<HTMLInputElement>("dv-alias");
  alias.value = aliases[serial] ?? "";
  alias.placeholder = defaultName(tile) || t("dv.aliasPh");
  $<HTMLDivElement>("dv-sub").textContent = serial;
  feedback("dv-feedback", "");
  $("drawer").classList.add("open");
  $("drawer").setAttribute("aria-hidden", "false");
  $("drawer-backdrop").classList.add("open");
  await loadDetails(serial);
}

function closeDetails() {
  drawerSerial = null;
  $("drawer").classList.remove("open");
  $("drawer").setAttribute("aria-hidden", "true");
  $("drawer-backdrop").classList.remove("open");
}

function resetRebootBtn() {
  const b = $<HTMLButtonElement>("dv-reboot");
  b.classList.remove("confirm");
  b.textContent = t("dv.reboot");
}

async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text);
    feedback("dv-feedback", t("dv.copied"), "ok");
  } catch {
    /* portapapeles no disponible */
  }
}

function initDrawer() {
  $("dv-close").addEventListener("click", closeDetails);
  $("drawer-backdrop").addEventListener("click", closeDetails);
  // Escape cierra la ficha (y no llega al teléfono enfocado como "Atrás").
  window.addEventListener(
    "keydown",
    (e) => {
      if (e.key === "Escape" && drawerSerial) {
        e.stopImmediatePropagation();
        closeDetails();
      }
    },
    true,
  );
  $("dv-refresh").addEventListener("click", () => {
    if (drawerSerial) void loadDetails(drawerSerial);
  });

  const alias = $<HTMLInputElement>("dv-alias");
  const commitAlias = () => {
    if (!drawerSerial) return;
    saveAlias(drawerSerial, alias.value.trim());
    const tile = tiles.get(drawerSerial);
    if (tile) tile.nameEl.textContent = displayName(tile);
  };
  alias.addEventListener("change", commitAlias);
  alias.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      commitAlias();
      alias.blur();
    }
  });

  $("dv-acc-settings").addEventListener("click", async () => {
    if (!drawerSerial) return;
    try {
      await invoke("open_account_settings", { serial: drawerSerial });
    } catch (e) {
      feedback("dv-feedback", String(e), "err");
    }
  });
  $("dv-shot").addEventListener("click", async () => {
    if (!drawerSerial) return;
    try {
      const p = await invoke<string>("screenshot", { serial: drawerSerial, out: shotPath(drawerSerial) });
      feedback("dv-feedback", t("dv.shotSaved", { p }), "ok");
    } catch (e) {
      feedback("dv-feedback", String(e), "err");
    }
  });
  $("dv-power").addEventListener("click", () => {
    if (drawerSerial) void keyTo(drawerSerial, 26);
  });
  $("dv-restart-stream").addEventListener("click", async () => {
    const serial = drawerSerial;
    if (!serial) return;
    await invoke("stop_mirror", { serial }).catch(() => {});
    const tile = tiles.get(serial);
    if (tile) setTileState(tile, "idle", t("tile.idle"));
    await startOne(serial);
  });
  $("dv-copy-serial").addEventListener("click", () => {
    if (drawerSerial) void copyText(drawerSerial);
  });
  // Reiniciar: doble confirmación (el primer clic arma el botón por 4 s).
  $("dv-reboot").addEventListener("click", async () => {
    const serial = drawerSerial;
    if (!serial) return;
    const b = $<HTMLButtonElement>("dv-reboot");
    if (Date.now() - rebootArmed > 4000) {
      rebootArmed = Date.now();
      b.classList.add("confirm");
      b.textContent = t("dv.rebootConfirm");
      setTimeout(() => {
        if (rebootArmed && Date.now() - rebootArmed >= 4000) resetRebootBtn();
      }, 4100);
      return;
    }
    rebootArmed = 0;
    resetRebootBtn();
    try {
      await invoke("reboot_device", { serial });
      feedback("dv-feedback", t("dv.rebooting"), "ok");
    } catch (e) {
      feedback("dv-feedback", String(e), "err");
    }
  });
}

// ---------- Render de frames ----------
// Si llega un frame mientras se decodifica el anterior, se guarda solo el último: nunca se
// acumula retraso aunque la UI vaya más lenta que el stream.
async function drawFrame(tile: Tile, f: VideoFrame) {
  tile.decoding = true;
  try {
    const blob = await (await fetch(`data:image/jpeg;base64,${f.jpeg}`)).blob();
    const bmp = await createImageBitmap(blob);
    if (tile.canvas.width !== bmp.width || tile.canvas.height !== bmp.height) {
      tile.canvas.width = bmp.width;
      tile.canvas.height = bmp.height;
      tile.screen.style.aspectRatio = `${bmp.width} / ${bmp.height}`;
    }
    tile.ctx.drawImage(bmp, 0, 0);
    bmp.close();
    tile.w = f.width;
    tile.h = f.height;
    if (tile.state !== "live") setTileState(tile, "live");
  } catch {
    /* frame corrupto: se ignora */
  } finally {
    tile.decoding = false;
  }
  const next = tile.pending;
  tile.pending = null;
  if (next) void drawFrame(tile, next);
}

listen<VideoFrame>("video-frame", (ev) => {
  const tile = tiles.get(ev.payload.serial);
  if (!tile) return;
  if (tile.decoding) tile.pending = ev.payload;
  else void drawFrame(tile, ev.payload);
});

// Caída de stream no solicitada: marcar y dejar que el sondeo lo reintente.
listen<string>("session-ended", (ev) => {
  const tile = tiles.get(ev.payload);
  void invoke("stop_mirror", { serial: ev.payload }).catch(() => {});
  if (tile) {
    tile.failedAt = 0;
    setTileState(tile, "idle", t("tile.disconnected"));
  }
});

// ---------- Ciclo de vida de streams ----------
async function startOne(serial: string) {
  const tile = tiles.get(serial);
  if (!tile || tile.state === "connecting" || tile.state === "live") return;
  setTileState(tile, "connecting", t("tile.connecting"));
  try {
    await invoke("start_mirror", {
      serial,
      maxSize: settings.maxSize,
      maxFps: settings.fps,
      bitRate: Math.round(settings.bitrate * 1_000_000),
      jpegQuality: jpegQ(settings.quality),
    });
    // El estado pasa a "live" con el primer frame (ver drawFrame).
  } catch (e) {
    tile.failedAt = Date.now();
    setTileState(tile, "error", String(e));
  }
}

async function startAll() {
  await refresh();
  await Promise.allSettled(
    [...tiles.values()].filter((x) => x.adbState === "device").map((x) => startOne(x.serial)),
  );
}

async function stopAll() {
  for (const serial of await invoke<string[]>("list_active")) {
    await invoke("stop_mirror", { serial });
  }
  for (const tile of tiles.values()) setTileState(tile, "idle", t("tile.idle"));
  $<HTMLSpanElement>("status").textContent = t("status.stopped");
}

/** Sincroniza tiles con `adb devices`. Agrega nuevos, quita desconectados. */
/** Lecturas seguidas en las que cada teléfono no apareció. */
const missing = new Map<string, number>();

async function refresh(): Promise<DeviceInfo[]> {
  let devices: DeviceInfo[];
  try {
    devices = await invoke<DeviceInfo[]>("list_devices");
  } catch (e) {
    const pill = $<HTMLDivElement>("status-pill");
    pill.classList.remove("ok", "busy");
    pill.classList.add("err");
    $<HTMLSpanElement>("status").textContent = String(e);
    return [];
  }
  const present = new Set(devices.map((d) => d.serial));
  for (const serial of [...tiles.keys()]) {
    if (present.has(serial)) {
      missing.delete(serial);
      continue;
    }
    // Un teléfono que falta en una sola lectura suele ser un corte breve de USB o de adb: se
    // quita recién cuando falta en varias seguidas (~9 s).
    const n = (missing.get(serial) ?? 0) + 1;
    missing.set(serial, n);
    if (n >= 3) {
      missing.delete(serial);
      void invoke("stop_mirror", { serial }).catch(() => {});
      removeTile(serial);
    }
  }
  for (const d of devices) {
    const tile = ensureTile(d);
    tile.adbState = d.state;
    if (d.state === "unauthorized") setTileState(tile, "error", t("tile.unauthorized"));
    else if (d.state !== "device") setTileState(tile, "error", t("tile.offline"));
    else if (tile.state === "error" && !tile.failedAt) setTileState(tile, "idle", t("tile.idle"));
  }
  updateStatus();
  return devices;
}

/** Sondeo: detecta teléfonos nuevos y, con autoarranque, inicia o reintenta su video. */
async function poll() {
  await refresh();
  if (!settings.autoStart) return;
  const now = Date.now();
  for (const tile of tiles.values()) {
    if (tile.adbState !== "device") continue;
    const retry = tile.state === "error" && tile.failedAt && now - tile.failedAt > 10_000;
    if (tile.state === "idle" || retry) void startOne(tile.serial);
  }
}

// ---------- Teclado -> teléfono enfocado ----------
function isTypingInApp(e: KeyboardEvent): boolean {
  const el = e.target as HTMLElement | null;
  return !!el && (el.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(el.tagName));
}

// Ctrl+V: pega el portapapeles de la PC en el teléfono enfocado (y en los seleccionados).
// El evento `paste` trae el texto sin pedir permisos; si no llega, se lee el portapapeles
// desde Rust.
let pasteHandled = false;
window.addEventListener("paste", (e) => {
  if (!focused || drawerSerial || isTypingInApp(e as unknown as KeyboardEvent)) return;
  const text = e.clipboardData?.getData("text") ?? "";
  pasteHandled = true;
  e.preventDefault();
  if (text) pasteTo(touchTargets(focused), text);
});

function pasteTo(serials: string[], text: string) {
  for (const s of serials) void invoke("paste_text", { serial: s, text }).catch(() => {});
}

async function pastePcClipboard(serials: string[]) {
  try {
    const text = await invoke<string>("read_pc_clipboard");
    if (text) pasteTo(serials, text);
  } catch {
    /* sin portapapeles de texto */
  }
}

window.addEventListener("keydown", (e) => {
  if (panicOpen()) {
    if (e.key === "Escape") closePanic();
    return;
  }
  if (spotlight && e.key === "Escape") {
    e.preventDefault();
    closeSpotlight();
    return;
  }
  // Ctrl + / Ctrl - : tamaño de las pantallas.
  if ((e.ctrlKey || e.metaKey) && !isTypingInApp(e) && ["+", "=", "-"].includes(e.key)) {
    e.preventDefault();
    setTileWidth(settings.tileWidth + (e.key === "-" ? -20 : 20));
    return;
  }
  // Con la ficha abierta las teclas son de la ficha (Escape la cierra), no del teléfono.
  if (!focused || drawerSerial || isTypingInApp(e)) return;
  if (e.key === "F2") {
    e.preventDefault();
    const tile = tiles.get(focused);
    if (tile) renameTile(tile);
    return;
  }
  const targets = touchTargets(focused);
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "v" && !e.altKey) {
    pasteHandled = false;
    setTimeout(() => {
      if (!pasteHandled) void pastePcClipboard(targets);
    }, 120);
    return;
  }
  if (e.ctrlKey || e.metaKey || e.altKey) return;
  const key = (code: number) => {
    e.preventDefault();
    for (const s of targets) void keyTo(s, code);
  };
  if (e.key in KEYMAP) return key(KEYMAP[e.key]);
  if (e.key.length === 1) {
    e.preventDefault();
    for (const s of targets) void invoke("inject_text", { serial: s, text: e.key }).catch(() => {});
  }
});

async function keyTo(serial: string, keycode: number) {
  try {
    await invoke("inject_key", { serial, keycode, action: 0, metaState: 0 });
    await invoke("inject_key", { serial, keycode, action: 1, metaState: 0 });
  } catch {
    /* sin sesión activa */
  }
}

async function scrollTo(serial: string, rx: number, ry: number, h: number, v: number) {
  const tile = tiles.get(serial);
  if (!tile || tile.state !== "live") return;
  const x = Math.round(rx * (tile.w - 1));
  const y = Math.round(ry * (tile.h - 1));
  await invoke("inject_scroll", { serial, x, y, width: tile.w, height: tile.h, hScroll: h, vScroll: v }).catch(
    () => {},
  );
}

// ---------- Motor de scripts ----------
type Op =
  | { k: "tap"; x: number; y: number }
  | { k: "swipe"; x1: number; y1: number; x2: number; y2: number; ms: number }
  | { k: "text"; s: string }
  | { k: "key"; code: number }
  | { k: "sleep"; ms: number }
  | { k: "tapimage"; path: string; thr: number }
  | { k: "waitimage"; path: string; timeout: number; thr: number }
  | { k: "loop"; n: number; body: Op[] };

interface MatchResult { x: number; y: number; score: number; }

const KEY_NAMES: Record<string, number> = {
  BACK: 4, HOME: 3, RECENTS: 187, ENTER: 66, DEL: 67, TAB: 61,
};

function parseScript(src: string): Op[] {
  const lines = src.split(/\r?\n/).map((l) => l.trim());
  let i = 0;
  const num = (v: string | undefined, what: string): number => {
    const n = Number(v);
    if (v === undefined || !Number.isFinite(n)) throw new Error(`línea ${i}: falta ${what}`);
    return n;
  };
  const block = (top: boolean): Op[] => {
    const ops: Op[] = [];
    while (i < lines.length) {
      const line = lines[i++];
      if (!line || line.startsWith("//") || line.startsWith("#")) continue;
      const [cmd, ...rest] = line.split(/\s+/);
      switch (cmd.toLowerCase()) {
        case "end":
          if (top) throw new Error(`línea ${i}: 'end' sin 'loop'`);
          return ops;
        case "loop":
          ops.push({ k: "loop", n: Math.max(0, num(rest[0], "N")), body: block(false) });
          break;
        case "tap":
          ops.push({ k: "tap", x: num(rest[0], "x"), y: num(rest[1], "y") });
          break;
        case "swipe":
          ops.push({
            k: "swipe",
            x1: num(rest[0], "x1"), y1: num(rest[1], "y1"),
            x2: num(rest[2], "x2"), y2: num(rest[3], "y2"),
            ms: rest[4] ? num(rest[4], "ms") : 300,
          });
          break;
        case "text":
          ops.push({ k: "text", s: line.replace(/^text\s+/i, "") });
          break;
        case "key": {
          const code = KEY_NAMES[rest[0]?.toUpperCase()];
          if (code === undefined) throw new Error(`línea ${i}: tecla desconocida «${rest[0] ?? ""}»`);
          ops.push({ k: "key", code });
          break;
        }
        case "sleep":
          ops.push({ k: "sleep", ms: num(rest[0], "ms") });
          break;
        case "tapimage":
          if (!rest[0]) throw new Error(`línea ${i}: falta la ruta de la imagen`);
          ops.push({ k: "tapimage", path: rest[0], thr: rest[1] ? num(rest[1], "umbral") : 0.8 });
          break;
        case "waitimage":
          if (!rest[0]) throw new Error(`línea ${i}: falta la ruta de la imagen`);
          ops.push({
            k: "waitimage",
            path: rest[0],
            timeout: rest[1] ? num(rest[1], "ms") : 10000,
            thr: rest[2] ? num(rest[2], "umbral") : 0.8,
          });
          break;
        default:
          throw new Error(`línea ${i}: comando desconocido «${cmd}»`);
      }
    }
    if (!top) throw new Error("falta 'end'");
    return ops;
  };
  return block(true);
}

let scriptCancel = false;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function tapAbs(serial: string, x: number, y: number) {
  const tile = tiles.get(serial);
  if (!tile || !tile.w || !tile.h) return;
  const cx = Math.max(0, Math.min(tile.w - 1, Math.round(x)));
  const cy = Math.max(0, Math.min(tile.h - 1, Math.round(y)));
  await invoke("inject_touch", { serial, action: 0, x: cx, y: cy, width: tile.w, height: tile.h });
  await invoke("inject_touch", { serial, action: 1, x: cx, y: cy, width: tile.w, height: tile.h });
}

async function swipeAbs(serial: string, x1: number, y1: number, x2: number, y2: number, ms: number) {
  const tile = tiles.get(serial);
  if (!tile || !tile.w || !tile.h) return;
  const steps = Math.max(2, Math.round(ms / 16));
  const send = (a: number, x: number, y: number) =>
    invoke("inject_touch", {
      serial, action: a,
      x: Math.max(0, Math.min(tile.w - 1, Math.round(x))),
      y: Math.max(0, Math.min(tile.h - 1, Math.round(y))),
      width: tile.w, height: tile.h,
    });
  await send(0, x1, y1);
  for (let s = 1; s <= steps; s++) {
    if (scriptCancel) break;
    const f = s / steps;
    await send(2, x1 + (x2 - x1) * f, y1 + (y2 - y1) * f);
    await sleep(ms / steps);
  }
  await send(1, x2, y2);
}

async function runOps(serial: string, ops: Op[]) {
  for (const op of ops) {
    if (scriptCancel) return;
    switch (op.k) {
      case "tap": await tapAbs(serial, op.x, op.y); break;
      case "swipe": await swipeAbs(serial, op.x1, op.y1, op.x2, op.y2, op.ms); break;
      case "text": await invoke("inject_text", { serial, text: op.s }); break;
      case "key": await keyTo(serial, op.code); break;
      case "sleep": await sleep(op.ms); break;
      case "tapimage": {
        const r = await invoke<MatchResult | null>("find_template", { serial, path: op.path, threshold: op.thr });
        if (r) await tapAbs(serial, r.x, r.y);
        break;
      }
      case "waitimage": {
        const deadline = Date.now() + op.timeout;
        let found = false;
        while (Date.now() < deadline && !scriptCancel) {
          const r = await invoke<MatchResult | null>("find_template", { serial, path: op.path, threshold: op.thr });
          if (r) { found = true; break; }
          await sleep(500);
        }
        if (!found && !scriptCancel) throw new Error(`waitimage: no apareció ${op.path}`);
        break;
      }
      case "loop":
        for (let n = 0; n < op.n; n++) {
          if (scriptCancel) return;
          await runOps(serial, op.body);
        }
        break;
    }
  }
}

/** Aviso breve abajo a la derecha; desaparece solo. */
function toast(msg: string, kind: "" | "ok" | "err" = "", ms = 5000) {
  const box = document.getElementById("toasts");
  if (!box) return;
  const el = document.createElement("div");
  el.className = `toast ${kind}`;
  el.textContent = msg;
  box.appendChild(el);
  setTimeout(() => {
    el.classList.add("out");
    setTimeout(() => el.remove(), 200);
  }, ms);
}

function feedback(id: string, msg: string, kind: "" | "ok" | "err" = "") {
  const el = $<HTMLDivElement>(id);
  el.textContent = msg;
  el.className = `feedback ${kind}`;
}

async function runScript() {
  let ops: Op[];
  try {
    ops = parseScript($<HTMLTextAreaElement>("script").value);
  } catch (e) {
    feedback("script-status", t("script.syntax", { e: (e as Error).message }), "err");
    return;
  }
  const sel = selectedSerials();
  const targets = sel.length ? sel : focused ? [focused] : [];
  if (!targets.length) {
    feedback("script-status", t("script.noTargets"), "err");
    return;
  }
  scriptCancel = false;
  $<HTMLButtonElement>("script-run").disabled = true;
  $<HTMLButtonElement>("script-stop").disabled = false;
  feedback("script-status", t("script.running", { n: targets.length }));
  try {
    await Promise.all(targets.map((s) => runOps(s, ops)));
    feedback("script-status", scriptCancel ? t("script.stopped") : t("script.done"), scriptCancel ? "" : "ok");
  } catch (e) {
    feedback("script-status", String(e), "err");
  } finally {
    $<HTMLButtonElement>("script-run").disabled = false;
    $<HTMLButtonElement>("script-stop").disabled = true;
  }
}

function toggleRecording() {
  recording = !recording;
  const btn = $<HTMLButtonElement>("rec");
  const label = $<HTMLSpanElement>("rec-label");
  btn.classList.toggle("recording", recording);
  if (recording) {
    recLines.length = 0;
    recLastEnd = 0;
    label.dataset.i18n = "script.recording";
    feedback("script-status", t("script.recHint"));
  } else {
    label.dataset.i18n = "script.rec";
    if (recLines.length) $<HTMLTextAreaElement>("script").value = recLines.join("\n");
    feedback("script-status", "");
  }
  label.textContent = t(label.dataset.i18n);
}

// ---------- Envío a seleccionados ----------
/** Abre el link en los teléfonos: Android lo lleva a la app que corresponde (post de
 *  Instagram, video de TikTok…) o al navegador. */
function openUrl(serials: string[], url: string) {
  return Promise.allSettled(serials.map((serial) => invoke("open_url", { serial, url })));
}

/** Abre el link que está copiado en la PC. */
async function openCopiedUrl(serials: string[]) {
  const url = (await invoke<string>("read_pc_clipboard").catch(() => "")).trim();
  if (!/^https?:\/\/\S+$/i.test(url)) {
    toast(t("url.noCopied"), "err");
    return;
  }
  await openUrl(serials, url);
}

const urlInput = $<HTMLInputElement>("dist-url");
const openUrlField = () => {
  const url = urlInput.value.trim();
  if (!url) return;
  // Seleccionados; si no hay, el teléfono con el que se está trabajando.
  if (!selectedSerials().length && focused) {
    void openUrl([focused], url).then(() => feedback("dist-status", t("url.opened", { n: 1 }), "ok"));
    return;
  }
  void forSelected((serial) => invoke("open_url", { serial, url }), "send.lblUrl");
};
$("dist-open-url").addEventListener("click", openUrlField);
urlInput.addEventListener("keydown", (e) => {
  if (e.key === "Enter") openUrlField();
});

async function forSelected(action: (serial: string) => Promise<unknown>, labelKey: string) {
  const sel = selectedSerials();
  if (!sel.length) {
    feedback("dist-status", t("send.noSel"), "err");
    return;
  }
  const results = await Promise.allSettled(sel.map((s) => action(s)));
  const ok = results.filter((r) => r.status === "fulfilled").length;
  const firstErr = results.find((r): r is PromiseRejectedResult => r.status === "rejected");
  const msg = t("send.result", { label: t(labelKey), ok, n: sel.length });
  feedback("dist-status", firstErr ? `${msg} — ${firstErr.reason}` : msg, ok === sel.length ? "ok" : "err");
}

// ---------- Grupos ----------
type Groups = Record<string, string[]>;
function loadGroups(): Groups {
  try {
    return JSON.parse(localStorage.getItem("panda.groups") ?? "{}");
  } catch {
    return {};
  }
}
function saveGroups(g: Groups) {
  try {
    localStorage.setItem("panda.groups", JSON.stringify(g));
  } catch {
    /* almacenamiento no disponible */
  }
}
function renderGroups() {
  const list = $<HTMLDivElement>("group-list");
  list.innerHTML = "";
  const groups = loadGroups();
  const names = Object.keys(groups);
  if (!names.length) {
    const p = document.createElement("p");
    p.className = "hint";
    p.textContent = t("groups.none");
    list.appendChild(p);
    return;
  }
  for (const name of names) {
    const chip = document.createElement("div");
    chip.className = "chip";
    chip.title = groups[name].join("\n");
    const label = document.createElement("span");
    label.textContent = name;
    const count = document.createElement("span");
    count.className = "count";
    count.textContent = String(groups[name].length);
    const x = document.createElement("button");
    x.className = "x";
    x.textContent = "×";
    x.title = t("groups.delete");
    x.addEventListener("click", (e) => {
      e.stopPropagation();
      const g = loadGroups();
      delete g[name];
      saveGroups(g);
      renderGroups();
    });
    chip.append(label, count, x);
    chip.addEventListener("click", () => {
      const members = new Set(loadGroups()[name] ?? []);
      for (const tile of tiles.values()) setSelected(tile, members.has(tile.serial));
      updateSelCount();
    });
    list.appendChild(chip);
  }
}

// ---------- Ajustes ----------
function initSettingsUI() {
  const bind = (
    id: string,
    outId: string,
    get: () => number,
    set: (v: number) => void,
    fmt: (v: number) => string,
  ) => {
    const input = $<HTMLInputElement>(id);
    const out = $<HTMLOutputElement>(outId);
    input.value = String(get());
    out.textContent = fmt(get());
    input.addEventListener("input", () => {
      set(Number(input.value));
      out.textContent = fmt(get());
      saveSettings();
    });
  };
  bind("set-size", "out-size", () => settings.maxSize, (v) => (settings.maxSize = v), (v) => `${v} px`);
  bind("set-fps", "out-fps", () => settings.fps, (v) => (settings.fps = v), (v) => `${v}`);
  bind("set-bitrate", "out-bitrate", () => settings.bitrate, (v) => (settings.bitrate = v), (v) => `${v} Mbps`);
  bind("set-quality", "out-quality", () => settings.quality, (v) => (settings.quality = v), (v) => `${v}/10`);

  const auto = $<HTMLInputElement>("set-autostart");
  auto.checked = settings.autoStart;
  auto.addEventListener("change", () => {
    settings.autoStart = auto.checked;
    saveSettings();
  });

  const namingSel = $<HTMLSelectElement>("set-naming");
  namingSel.value = settings.naming;
  namingSel.addEventListener("change", () => {
    settings.naming = namingSel.value === "model" ? "model" : "number";
    saveSettings();
    refreshNames();
  });
  $("renumber").addEventListener("click", () => {
    renumberByOrder();
    toast(t("settings.renumbered"), "ok");
  });

  const langSel = $<HTMLSelectElement>("set-lang");
  for (const l of LANGUAGES) {
    const opt = document.createElement("option");
    opt.value = l;
    opt.textContent = LANGUAGE_NAMES[l] ?? l;
    langSel.appendChild(opt);
  }
  langSel.value = getLang();
  langSel.addEventListener("change", () => {
    setLang(langSel.value);
    settings.lang = langSel.value;
    saveSettings();
    applyI18n();
    renderGroups();
    updateStatus();
    refreshNames();
    for (const tile of tiles.values()) {
      if (tile.state === "idle") tile.msg.textContent = t("tile.idle");
      if (tile.state === "connecting") tile.msg.textContent = t("tile.connecting");
    }
  });

  const tileW = $<HTMLInputElement>("tile-width");
  const applyTileW = () => document.documentElement.style.setProperty("--tile-w", `${settings.tileWidth}px`);
  tileW.value = String(settings.tileWidth);
  applyTileW();
  tileW.addEventListener("input", () => {
    settings.tileWidth = Number(tileW.value);
    applyTileW();
    saveSettings();
  });

  $("restart-streams").addEventListener("click", async () => {
    await stopAll();
    await startAll();
  });
}

// ---------- Wiring ----------
/** Barra de íconos de la derecha: abre el panel en una sección; tocar la misma lo cierra.
 *  Con el panel cerrado los teléfonos usan todo el ancho. */
function initRail() {
  const layout = document.querySelector<HTMLElement>(".layout")!;
  const rails = [...document.querySelectorAll<HTMLButtonElement>(".rail-btn")];
  const open = (tab: string | null) => {
    if (tab === "phones") tab = null;
    layout.classList.toggle("panel-closed", !tab);
    for (const r of rails) r.classList.toggle("active", r.dataset.rail === (tab ?? "phones"));
    if (tab) {
      document.querySelector<HTMLButtonElement>(`.tab[data-tab="${tab}"]`)?.click();
      $("panel-title").textContent = t(`tab.${tab}`);
    }
    try {
      localStorage.setItem("aguara.panel", tab ?? "");
    } catch {
      /* sin almacenamiento: no se recuerda */
    }
  };
  for (const r of rails) {
    r.addEventListener("click", () => open(r.classList.contains("active") ? null : r.dataset.rail!));
    r.title = r.textContent?.trim() ?? "";
  }
  $("panel-close").addEventListener("click", () => open(null));
  let saved: string | null = null;
  try {
    saved = localStorage.getItem("aguara.panel");
  } catch {
    /* sin almacenamiento */
  }
  open(saved || null);
}

function initTabs() {
  for (const tab of document.querySelectorAll<HTMLButtonElement>(".tab")) {
    tab.addEventListener("click", () => {
      for (const t2 of document.querySelectorAll(".tab")) t2.classList.toggle("active", t2 === tab);
      for (const pane of document.querySelectorAll<HTMLElement>(".pane")) {
        pane.classList.toggle("active", pane.dataset.pane === tab.dataset.tab);
      }
    });
  }
}

$("refresh").addEventListener("click", () => {
  void refresh();
  void checkForeignAdb();
});

// ---------- Otro adb abierto que se queda con teléfonos ----------
// Cada teléfono lo puede tomar un solo server de adb. Si quedó otro abierto (de otra app o de una
// sesión vieja), esos teléfonos no aparecen: se avisa y se ofrece cerrarlo.
interface ForeignAdb {
  port: number;
  devices: number;
}
let foreignPorts: number[] = [];

async function checkForeignAdb() {
  const found = (await invoke<ForeignAdb[]>("foreign_adb").catch(() => [])).filter((f) => f.devices > 0);
  foreignPorts = found.map((f) => f.port);
  const banner = $("adb-warning");
  banner.hidden = !found.length;
  if (found.length) {
    const n = found.reduce((sum, f) => sum + f.devices, 0);
    $("adb-warning-text").textContent = t("adb.warning", { port: found.map((f) => f.port).join(", "), n });
  }
}

$("adb-free").addEventListener("click", async () => {
  try {
    await invoke("free_adb", { ports: foreignPorts });
    $("adb-warning").hidden = true;
    toast(t("adb.freed"), "ok");
    setTimeout(() => void refresh(), 2500);
  } catch (e) {
    toast(String(e), "err");
  }
});

void checkForeignAdb();
setInterval(() => void checkForeignAdb(), 15000);
$("empty-refresh").addEventListener("click", () => void refresh());
$("start-all").addEventListener("click", () => void startAll());
$("stop-all").addEventListener("click", () => void stopAll());
$("select-all").addEventListener("click", () => {
  for (const tile of tiles.values()) setSelected(tile, true);
  updateSelCount();
});
$("select-none").addEventListener("click", () => {
  for (const tile of tiles.values()) setSelected(tile, false);
  updateSelCount();
});
/** Enciende la pantalla y saca el bloqueo (sin depender de dónde arranca el deslizamiento). */
function unlockScreens(serials: string[]) {
  return forEachSerial(serials, (s) => invoke("unlock_screen", { serial: s }));
}
// ---------- Barra de acciones (abajo) ----------
$("dock-unlock").addEventListener("click", () => void unlockScreens(actionTargets()));
for (const btn of document.querySelectorAll<HTMLButtonElement>("#actionbar [data-key]")) {
  btn.addEventListener("click", () => {
    const keycode = Number(btn.dataset.key);
    for (const s of actionTargets()) void keyTo(s, keycode);
  });
}
const below = (id: string) => {
  const r = $(id).getBoundingClientRect();
  return { x: r.left, y: r.top };
};
$("act-apps").addEventListener("click", () => {
  const targets = actionTargets();
  const { x, y } = below("act-apps");
  showMenu(x, y - 8, [
    { title: t("act.apps") },
    ...QUICK_APPS.map((pkg) => SOCIAL_APPS.find((a) => a.pkg === pkg)!).map((a) => ({
      label: a.label,
      run: () => forEachSerial(targets, (s) => invoke("launch_app", { serial: s, package: a.pkg })),
    })),
  ]);
  // El menú se abre hacia arriba, sobre la barra.
  if (menuEl) menuEl.style.top = `${Math.max(8, y - menuEl.getBoundingClientRect().height - 8)}px`;
});
$("act-link").addEventListener("click", () => void openCopiedUrl(actionTargets()));
$("act-accept").addEventListener("click", () => void acceptFriendRequests(actionTargets()));
$("act-shot").addEventListener("click", () => void takeShots(actionTargets()));
$("act-more").addEventListener("click", () => {
  const targets = actionTargets();
  const tile = tiles.get(targets[0]);
  if (!tile) return;
  const { x, y } = below("act-more");
  openTileMenu(tile, x, y);
  if (menuEl) {
    const r = menuEl.getBoundingClientRect();
    menuEl.style.top = `${Math.max(8, y - r.height - 8)}px`;
    menuEl.style.left = `${Math.min(x, window.innerWidth - r.width - 8)}px`;
  }
});

$("script-run").addEventListener("click", () => void runScript());
$("script-stop").addEventListener("click", () => {
  scriptCancel = true;
});
$("rec").addEventListener("click", toggleRecording);

$("dist-send-text").addEventListener("click", () => {
  const text = $<HTMLInputElement>("dist-text").value;
  if (text) void forSelected((serial) => invoke("paste_text", { serial, text }), "send.lblText");
});
$("dist-install").addEventListener("click", () => {
  const path = $<HTMLInputElement>("dist-apk").value.trim();
  if (path) void forSelected((serial) => invoke("install_apk", { serial, path }), "send.lblApk");
});
$("dist-push").addEventListener("click", () => {
  const local = $<HTMLInputElement>("dist-file").value.trim();
  if (!local) return;
  const name = local.split(/[\\/]/).pop() || "file";
  const remote = `/sdcard/Download/${name}`;
  void forSelected((serial) => invoke("push_file", { serial, local, remote }), "send.lblPush");
});
$("dist-screenshot").addEventListener("click", () => {
  void forSelected(
    (serial) => invoke<string>("screenshot", { serial, out: shotPath(serial) }),
    "send.lblShot",
  );
});

$("group-save").addEventListener("click", () => {
  const input = $<HTMLInputElement>("group-name");
  const name = input.value.trim();
  if (!name) return;
  const g = loadGroups();
  g[name] = selectedSerials();
  saveGroups(g);
  input.value = "";
  renderGroups();
  feedback("group-status", t("groups.saved", { name, n: g[name].length }), "ok");
});

initTabs();
initRail();
void getVersion()
  .then((v) => ($("app-version").textContent = t("settings.version", { v })))
  .catch(() => {});
initDrawer();
initSettingsUI();
applyI18n();
renderGroups();
updateStatus();
// ---------- Arranque: logo palpitando y esqueletos hasta la primera búsqueda ----------
// El logo se ve al menos un instante (sin parpadeo) y como mucho unos segundos: si la primera
// búsqueda tarda (por ejemplo, la descarga de adb), quedan los esqueletos en la grilla.
const bootStart = performance.now();
function hideSplash() {
  const splash = document.getElementById("splash");
  if (!splash || splash.classList.contains("out")) return;
  splash.classList.add("out");
  setTimeout(() => splash.remove(), 400);
}
function showSkeletons(n: number) {
  for (let i = 0; i < n; i++) {
    const sk = document.createElement("div");
    sk.className = "skel-tile";
    sk.style.setProperty("--i", String(i));
    sk.innerHTML = '<div class="skel-head"><span class="skel-line"></span></div><div class="skel-screen"></div>';
    grid.appendChild(sk);
  }
}
showSkeletons(4);
$<HTMLSpanElement>("status").textContent = t("status.searching");
$("status-pill").classList.add("busy");
setTimeout(hideSplash, 2500);
void poll().finally(() => {
  for (const sk of grid.querySelectorAll(".skel-tile")) sk.remove();
  setTimeout(hideSplash, Math.max(0, 900 - (performance.now() - bootStart)));
});
setInterval(() => void poll(), 3000);
