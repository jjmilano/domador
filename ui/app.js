"use strict";

const el = (id) => document.getElementById(id);
const esc = (s) => String(s == null ? "" : s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

const PRIOS = [
  ["idle", "Idle"],
  ["below_normal", "Bajo"],
  ["normal", "Normal"],
  ["above_normal", "Alto"],
  ["high", "Muy alto"],
  ["realtime", "Tiempo real"],
];
const PRIO_LABEL = Object.fromEntries(PRIOS);
const IO_LABEL = { very_low: "Muy baja", low: "Baja", normal: "Normal", high: "Alta" };
const MEM_LABEL = { very_low: "Muy baja", low: "Baja", medium: "Media", below_normal: "Bajo normal", normal: "Normal" };

const PROFILES = [
  ["full_power", "⚡ Full Power"],
  ["gaming", "🎮 Juego"],
  ["ai", "🤖 IA"],
  ["balanced", "⚖ Equilibrado"],
];
const PROFILE_LABEL = Object.fromEntries(PROFILES);

const S = {
  data: null,
  tab: "procesos",
  query: "",
  sort: "cpu",
  onlyRules: false,
  selection: new Set(),
  modal: null,
  logClearedAt: 0,
  planSig: "",
  online: true,
  profileBusy: false,
};

/* ---------- infra ---------- */

async function api(path, body) {
  const opts = body === undefined ? {} : {
    method: "POST",
    headers: { "Content-Type": "application/json", "X-Domador": "1" },
    body: JSON.stringify(body),
  };
  const res = await fetch(path, opts);
  let data = null;
  try { data = await res.json(); } catch (e) { /* sin cuerpo */ }
  if (!res.ok || !data || data.ok === false) {
    throw new Error((data && data.error) || ("HTTP " + res.status));
  }
  return data;
}

function toast(msg, kind) {
  const node = document.createElement("div");
  node.className = "toast " + (kind || "");
  node.textContent = msg;
  el("toasts").appendChild(node);
  setTimeout(() => node.remove(), 4200);
}

const fmtMB = (b) => (b >= 1073741824 ? (b / 1073741824).toFixed(1) + " GB" : Math.round(b / 1048576) + " MB");
const hhmmss = (ts) => new Date(ts * 1000).toLocaleTimeString("es-AR", { hour12: false });
const dur = (s) => {
  s = Math.max(0, Math.floor(s));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  return h ? h + "h " + m + "m" : m + "m";
};

function compress(cpus) {
  if (!cpus || !cpus.length) return "—";
  const arr = [...cpus].map(Number).sort((a, b) => a - b);
  if (S.data && arr.length === S.data.app.ncpu) return "todos";
  const parts = [];
  let start = arr[0];
  let prev = arr[0];
  for (const v of arr.slice(1)) {
    if (v === prev + 1) { prev = v; continue; }
    parts.push(start === prev ? String(start) : start + "-" + prev);
    start = prev = v;
  }
  parts.push(start === prev ? String(start) : start + "-" + prev);
  return parts.join(",");
}

function parseCores(text, ncpu) {
  if (!text || !text.trim()) return null;
  const out = new Set();
  for (const raw of text.split(",")) {
    const part = raw.trim();
    if (!part) continue;
    const range = part.match(/^(\d+)\s*-\s*(\d+)$/);
    if (range) {
      for (let i = Number(range[1]); i <= Number(range[2]); i++) {
        if (i >= 0 && i < ncpu) out.add(i);
      }
    } else if (/^\d+$/.test(part)) {
      const v = Number(part);
      if (v >= 0 && v < ncpu) out.add(v);
    } else {
      throw new Error("Afinidad inválida: " + part);
    }
  }
  if (!out.size) throw new Error("Afinidad vacía");
  return [...out].sort((a, b) => a - b);
}

/* ---------- polling ---------- */

let polling = false;

async function poll() {
  if (polling) return;
  polling = true;
  try {
    const data = await api("/api/state");
    S.data = data;
    S.online = true;
    render();
  } catch (e) {
    S.online = false;
    el("meta").textContent = "reconectando…";
  } finally {
    polling = false;
    setTimeout(poll, 1500);
  }
}

/* ---------- render ---------- */

function render() {
  if (!S.data) return;
  renderHeader();
  renderTable();
  renderRules();
  renderLog();
  renderFooter();
}

function renderHeader() {
  const d = S.data;
  const sys = d.system;

  el("cpuVal").textContent = Math.round(sys.cpu) + "%";
  el("ramVal").textContent = Math.round(sys.ram) + "%";
  const cpuBar = el("cpuBar");
  cpuBar.style.width = Math.min(100, sys.cpu) + "%";
  cpuBar.className = sys.cpu >= 80 ? "hot" : sys.cpu >= 55 ? "warm" : "";
  const ramBar = el("ramBar");
  ramBar.style.width = Math.min(100, sys.ram) + "%";
  ramBar.className = sys.ram >= 90 ? "hot" : sys.ram >= 75 ? "warm" : "";

  const badge = el("adminBadge");
  badge.textContent = d.app.admin ? "admin" : "sin admin";
  badge.classList.toggle("admin", !!d.app.admin);
  badge.title = d.app.admin ? "Corriendo con privilegios elevados" : "Algunos procesos del sistema van a rechazar cambios";

  const toggle = el("pbToggle");
  toggle.checked = !!d.probalance.enabled;
  el("pbCount").textContent = d.counts.throttled;

  el("meta").textContent =
    d.counts.processes + " procesos · " + d.counts.throttled + " regulados · " + dur(d.app.uptime);
  el("ruleCount").textContent = d.counts.rules;

  renderCores();
  renderSpark();
  renderPlans();
  renderProfile();
  renderHealth();
}

function renderCores() {
  const cores = el("cores");
  const values = S.data.system.percpu || [];
  if (cores.childElementCount !== values.length) {
    cores.innerHTML = values.map(() => "<i></i>").join("");
  }
  const nodes = cores.children;
  for (let i = 0; i < values.length && i < nodes.length; i++) {
    const v = values[i];
    nodes[i].style.height = Math.max(2, (v / 100) * 30).toFixed(1) + "px";
    nodes[i].style.background = v >= 85 ? "#f27070" : v >= 55 ? "#f2bd4b" : v >= 20 ? "#4fb286" : "#2a3a45";
    nodes[i].title = "Núcleo " + i + ": " + Math.round(v) + "%";
  }
}

function renderSpark() {
  const hist = S.data.history || [];
  const pts = (key) => {
    if (hist.length < 2) return "";
    const slice = hist.slice(-70);
    return slice.map((p, i) => {
      const x = (i / (slice.length - 1)) * 180;
      const y = 42 - (Math.max(0, Math.min(100, p[key] || 0)) / 100) * 40;
      return x.toFixed(1) + "," + y.toFixed(1);
    }).join(" ");
  };
  el("sparkCpu").setAttribute("points", pts("cpu"));
  el("sparkRam").setAttribute("points", pts("ram"));
}

function renderPlans() {
  const power = S.data.power;
  const sig = power.plans.map((p) => p.guid + ":" + p.name).join("|") + "#" + power.active;
  if (sig === S.planSig) return;
  S.planSig = sig;
  const select = el("planSelect");
  select.innerHTML = power.plans.map((p) =>
    '<option value="' + esc(p.guid) + '"' + (p.guid === power.active ? " selected" : "") + ">" + esc(p.name) + "</option>"
  ).join("");
}

function renderProfile() {
  const prof = (S.data && S.data.profile) || {};
  const active = prof.active || "";

  document.querySelectorAll("#profileBtns .profile").forEach((btn) => {
    const on = btn.dataset.profile === active;
    btn.classList.toggle("active", on);
    btn.setAttribute("aria-pressed", on ? "true" : "false");
    btn.disabled = S.profileBusy;
  });

  const box = el("profileState");
  box.dataset.profile = active;
  if (!active) {
    box.innerHTML = '<span class="dot off"></span><span class="ptext">sin datos del perfil</span>';
    box.title = "El motor todavía no informó un perfil";
    return;
  }

  const parts = ['<span class="dot"></span>', "<b>" + esc(PROFILE_LABEL[active] || active) + "</b>"];
  if (prof.plan) parts.push('<span class="sep">·</span><span class="plan">' + esc(prof.plan) + "</span>");
  if (prof.target) parts.push('<span class="sep">·</span><span class="target mono" title="' + esc(prof.target) + '">' + esc(prof.target) + "</span>");
  if (prof.since) parts.push('<span class="since">· desde ' + hhmmss(prof.since) + "</span>");
  box.innerHTML = parts.join("");
  box.title =
    "Perfil activo: " + (PROFILE_LABEL[active] || active) +
    (prof.plan ? " · Plan de energía: " + prof.plan : "") +
    (prof.since ? " · Desde " + hhmmss(prof.since) : "");
}

function filteredProcesses() {
  const q = S.query.trim().toLowerCase();
  let list = S.data.processes.filter((p) => {
    if (q && !(p.name || "").toLowerCase().includes(q) && !(p.exe || "").toLowerCase().includes(q)) return false;
    if (S.onlyRules && !p.rule_id) return false;
    return true;
  });
  const cmp = {
    cpu: (a, b) => b.cpu - a.cpu || a.name.localeCompare(b.name),
    ram: (a, b) => b.rss - a.rss,
    name: (a, b) => a.name.localeCompare(b.name),
    pid: (a, b) => a.pid - b.pid,
  }[S.sort] || ((a, b) => b.cpu - a.cpu);
  return list.sort(cmp);
}

function renderTable() {
  const active = document.activeElement;
  if (active && active.tagName === "SELECT" && active.closest("#procBody")) return;

  const all = S.data.processes;
  const alive = new Set(all.map((p) => p.pid));
  for (const pid of [...S.selection]) {
    if (!alive.has(pid)) S.selection.delete(pid);
  }

  const list = filteredProcesses();
  const body = el("procBody");
  body.innerHTML = list.slice(0, 500).map(procRow).join("");
  el("empty").classList.toggle("hidden", list.length > 0);
  el("procCount").textContent = list.length + " de " + all.length;
  updateBatch();
}

function procRow(p) {
  const selected = S.selection.has(p.pid);
  const cpuClass = p.cpu >= 25 ? "hot" : p.cpu >= 8 ? "warm" : "";
  const tags =
    (p.rule_id ? '<span class="tag rule" title="Tiene regla">REGLA</span>' : "") +
    (p.pb ? '<span class="tag pbtag" title="Regulado por ProBalanza">PB</span>' : "");
  const opts = PRIOS.map(([v, label]) =>
    '<option value="' + v + '"' + (p.prio === v ? " selected" : "") + ">" + label + "</option>"
  ).join("");
  return (
    '<tr data-pid="' + p.pid + '" class="' + (selected ? "sel" : "") + '">' +
      '<td><input type="checkbox" class="sel"' + (selected ? " checked" : "") + "></td>" +
      '<td class="pcell"><div class="pname">' + esc(p.name) + tags + "</div>" +
        '<div class="ppath" title="' + esc(p.exe) + '">' + esc(p.exe || "—") + "</div></td>" +
      '<td class="num">' + p.pid + "</td>" +
      '<td class="num"><span class="cpubar"><i class="' + cpuClass + '" style="width:' + Math.min(100, p.cpu * 2) + '%"></i></span>' + p.cpu.toFixed(1) + "</td>" +
      '<td class="num">' + fmtMB(p.rss) + "</td>" +
      '<td><select class="select prio" data-prio="' + esc(p.prio || "") + '">' + opts + "</select></td>" +
      '<td><button class="chip" data-act="aff" title="Elegir núcleos">' + esc(p.affinity_label || "—") + "</button></td>" +
      '<td class="center"><label class="mini"><input type="checkbox" class="eco"' + (p.eco ? " checked" : "") + "><span></span></label></td>" +
      '<td class="acts">' +
        '<button data-act="rule" title="Regla para este ejecutable">⚙</button>' +
        '<button data-act="gpu" title="Forzar GPU dedicada para este ejecutable">🎮</button>' +
        '<button data-act="trim" title="Liberar RAM">💧</button>' +
        '<button data-act="restore" title="Restaurar valores originales">↺</button>' +
        '<button data-act="kill" title="Terminar proceso">✕</button>' +
      "</td>" +
    "</tr>"
  );
}

function renderRules() {
  const rules = S.data.rules;
  const list = el("rulesList");
  if (!rules.length) {
    list.innerHTML = '<div class="empty">Todavía no hay reglas. Creá una desde un proceso con ⚙ o con “Nueva regla”.</div>';
    return;
  }
  list.innerHTML = rules.map(ruleCard).join("");
}

function ruleCard(r) {
  const chips = [];
  if (r.priority) chips.push('<span class="rchip acc">Prio ' + esc(PRIO_LABEL[r.priority] || r.priority) + "</span>");
  if (r.affinity) chips.push('<span class="rchip">Afinidad ' + esc(compress(r.affinity)) + "</span>");
  if (r.eco === true) chips.push('<span class="rchip acc">Eco sí</span>');
  if (r.eco === false) chips.push('<span class="rchip">Eco no</span>');
  if (r.io) chips.push('<span class="rchip">E/S ' + esc(IO_LABEL[r.io] || r.io) + "</span>");
  if (r.mem) chips.push('<span class="rchip">Mem ' + esc(MEM_LABEL[r.mem] || r.mem) + "</span>");
  const wd = r.watchdog || {};
  if (wd.keep_alive) chips.push('<span class="rchip warn">Relanzar</span>');
  if (wd.max_instances) chips.push('<span class="rchip warn">Máx ' + wd.max_instances + "</span>");
  if (!chips.length) chips.push('<span class="rchip">Sólo coincidencia</span>');

  return (
    '<div class="rule' + (r.enabled ? "" : " off") + '" data-id="' + esc(r.id) + '">' +
      '<div class="rule-main">' +
        '<div class="rule-pat" title="' + esc(r.pattern) + '">' + esc(r.pattern) + "</div>" +
        (r.note ? '<div class="rule-note">' + esc(r.note) + "</div>" : "") +
        '<div class="rule-chips">' + chips.join("") + "</div>" +
      "</div>" +
      '<div class="rule-acts">' +
        '<button class="btn tiny" data-r="apply">Aplicar</button>' +
        '<button class="btn tiny ghost" data-r="edit">Editar</button>' +
        '<button class="btn tiny ghost" data-r="toggle">' + (r.enabled ? "Pausar" : "Activar") + "</button>" +
        '<button class="btn tiny danger" data-r="del">Borrar</button>' +
      "</div>" +
    "</div>"
  );
}

function renderLog() {
  const entries = (S.data.log || []).filter((e) => e.ts > S.logClearedAt);
  const list = el("logList");
  const atBottom = list.scrollTop + list.clientHeight >= list.scrollHeight - 24;
  list.innerHTML = entries.map((e) =>
    '<li class="' + esc(e.level) + '"><time>' + hhmmss(e.ts) + '</time><span class="lv">' + esc(e.level) + '</span><span class="msg">' + esc(e.msg) + "</span></li>"
  ).join("");
  if (atBottom) list.scrollTop = list.scrollHeight;
}

function renderFooter() {
  const d = S.data;
  el("footLeft").textContent = d.app.name + " " + d.app.version + " · " + d.app.ncpu + " núcleos · " + (d.app.admin ? "admin" : "sin admin");
  el("footLeft").title = d.app.config_path;
  el("footRight").textContent = "primer plano: " + (d.system.foreground_name || "—");
}

function renderHealth() {
  const d = S.data;
  const mem = d.system.mem || {};
  const gb = (b) => ((b || 0) / 1073741824).toFixed(1) + " GB";
  const pct = (a, b) => (b ? Math.round((a / b) * 100) : 0);
  const commitPct = pct(mem.commit_used, mem.commit_limit);
  el("memCards").innerHTML =
    '<div class="kv"><span>Disponible</span><b>' + gb(mem.available) + "</b></div>" +
    '<div class="kv"><span>Commit</span><b class="' + (commitPct >= 90 ? "hot" : commitPct >= 75 ? "warm" : "") + '">' + gb(mem.commit_used) + " / " + gb(mem.commit_limit) + "</b></div>" +
    '<div class="kv"><span>Pool paginado</span><b>' + gb(mem.pool_paged) + "</b></div>" +
    '<div class="kv"><span>Pool no paginado</span><b>' + gb(mem.pool_nonpaged) + "</b></div>" +
    '<div class="kv"><span>Cache</span><b>' + gb(mem.cache) + "</b></div>" +
    '<div class="kv"><span>Procesos / handles</span><b>' + (mem.processes || "—") + " / " + (mem.handles || "—") + "</b></div>";

  const gpu = d.gpu || {};
  el("gpuCard").innerHTML = gpu.available
    ? '<div class="kv"><span>' + esc(gpu.name) + "</span><b>" + gpu.utilization.toFixed(0) + "%</b></div>" +
      '<div class="kv"><span>VRAM</span><b>' + gb(gpu.memory_used) + " / " + gb(gpu.memory_total) + "</b></div>" +
      '<div class="kv"><span>Temperatura</span><b>' + gpu.temperature.toFixed(0) + " °C</b></div>" +
      '<div class="kv"><span>Potencia</span><b>' + (gpu.power ? gpu.power.toFixed(0) + " W" : "—") + "</b></div>" +
      '<div class="kv"><span>Estado</span><b>' + esc(gpu.pstate) + " · driver " + esc(gpu.driver) + "</b></div>"
    : '<div class="empty">nvidia-smi no disponible</div>';

  const top = [...d.processes].sort((a, b) => (b.private || b.rss) - (a.private || a.rss)).slice(0, 12);
  el("privateBody").innerHTML = top.map((p) =>
    '<tr><td><div class="pname">' + esc(p.name) + '</div><div class="ppath">' + esc(p.exe || "—") + "</div></td>" +
    '<td class="num">' + fmtMB(p.private || p.rss) + "</td>" +
    '<td class="num">' + fmtMB(p.rss) + "</td>" +
    '<td class="num">' + p.cpu.toFixed(1) + "%</td>" +
    '<td class="acts"><button data-gpu-pid="' + p.pid + '" title="Forzar GPU dedicada para este ejecutable">GPU</button></td></tr>'
  ).join("");

  const runaways = d.runaways || [];
  el("runawayList").innerHTML = runaways.length
    ? runaways.map((r) =>
        '<div class="runaway-row"><div><b>' + esc(r.name) + '</b><span>PID ' + r.pid + " · " + r.cpu.toFixed(1) + "% · " + r.minutes.toFixed(0) + ' min</span></div><button data-kill-pid="' + r.pid + '">Terminar</button></div>'
      ).join("")
    : '<div class="empty">Sin procesos colgados.</div>';
}

/* ---------- selección y lote ---------- */

function updateBatch() {
  const n = S.selection.size;
  el("batch").classList.toggle("hidden", n === 0);
  el("batchCount").textContent = n + " seleccionado" + (n === 1 ? "" : "s");
}

async function eachPid(pids, fn) {
  let ok = 0, fail = 0, lastErr = "";
  for (const pid of pids) {
    try { await fn(pid); ok++; } catch (e) { fail++; lastErr = e.message; }
  }
  return { ok, fail, lastErr };
}

function selectedPids() {
  return [...S.selection];
}

/* ---------- modales ---------- */

function openModal(opts) {
  S.modal = opts;
  el("modalTitle").textContent = opts.title || "";
  el("modalBody").innerHTML = opts.body || "";
  el("modalOk").textContent = opts.okLabel || "Aplicar";
  el("modalOk").classList.toggle("hidden", opts.hideOk === true);
  el("modal").classList.remove("hidden");
  setTimeout(() => {
    const first = el("modalBody").querySelector("input:not([type=checkbox]), select, textarea");
    if (first) first.focus();
  }, 30);
}

function closeModal() {
  S.modal = null;
  el("modal").classList.add("hidden");
  el("modalBody").innerHTML = "";
}

async function ensureMeta(pid) {
  const proc = S.data.processes.find((p) => p.pid === pid);
  if (!proc) return null;
  if (proc.affinity && proc.exe) return proc;
  try {
    const meta = await api("/api/meta", { pid });
    Object.assign(proc, meta);
    return proc;
  } catch (e) {
    return proc;
  }
}

async function affinityModal(pids) {
  if (!pids.length) return;
  if (pids.length === 1) await ensureMeta(pids[0]);
  const ncpu = S.data.app.ncpu;
  const single = pids.length === 1 ? S.data.processes.find((p) => p.pid === pids[0]) : null;
  const current = new Set(single && single.affinity ? single.affinity : Array.from({ length: ncpu }, (_, i) => i));
  const cells = Array.from({ length: ncpu }, (_, i) =>
    '<label class="core"><input type="checkbox" class="core-cb" value="' + i + '"' + (current.has(i) ? " checked" : "") + "><span>" + i + "</span></label>"
  ).join("");

  openModal({
    title: single ? "Afinidad — " + single.name : "Afinidad — " + pids.length + " procesos",
    body:
      '<div class="coregrid">' + cells + "</div>" +
      '<div class="rowgap"><button class="btn tiny" id="coresAll">Todos</button>' +
      '<button class="btn tiny" id="coresNone">Ninguno</button>' +
      '<button class="btn tiny" id="coresZero">Sólo 0</button></div>' +
      '<p class="help">Para reglas podés usar rangos: <span class="mono">0-3,8</span>.</p>',
    onOk: async () => {
      const cpus = [...document.querySelectorAll("#modalBody .core-cb:checked")].map((x) => Number(x.value));
      if (!cpus.length) throw new Error("Elegí al menos un núcleo");
      const res = await eachPid(pids, (pid) => api("/api/affinity", { pid, cpus }));
      toast("Afinidad aplicada a " + res.ok + (res.fail ? " · " + res.fail + " fallaron" : ""), res.fail ? "warn" : "ok");
    },
  });

  el("coresAll").onclick = () => document.querySelectorAll("#modalBody .core-cb").forEach((c) => { c.checked = true; });
  el("coresNone").onclick = () => document.querySelectorAll("#modalBody .core-cb").forEach((c) => { c.checked = false; });
  el("coresZero").onclick = () => document.querySelectorAll("#modalBody .core-cb").forEach((c) => { c.checked = c.value === "0"; });
}

function ruleModal(rule, proc) {
  const isNew = !rule;
  const ncpu = S.data.app.ncpu;
  const r = rule || {
    pattern: proc ? (proc.exe || proc.name) : "",
    priority: proc ? proc.prio : "",
    affinity: null,
    eco: null,
    io: null,
    mem: null,
    watchdog: { keep_alive: false, max_instances: 0 },
    note: "",
    enabled: true,
  };
  const affText = r.affinity ? compress(r.affinity) : "";
  const prioOpts = '<option value="">— no cambiar —</option>' +
    PRIOS.map(([v, l]) => '<option value="' + v + '"' + (r.priority === v ? " selected" : "") + ">" + l + "</option>").join("");
  const ecoVal = r.eco === true ? "1" : r.eco === false ? "0" : "";
  const ecoOpts = '<option value=""' + (ecoVal === "" ? " selected" : "") + '>— no cambiar —</option>' +
    '<option value="1"' + (ecoVal === "1" ? " selected" : "") + ">activar</option>" +
    '<option value="0"' + (ecoVal === "0" ? " selected" : "") + ">desactivar</option>";
  const ioOpts = '<option value="">— no cambiar —</option>' +
    Object.keys(IO_LABEL).map((k) => '<option value="' + k + '"' + (r.io === k ? " selected" : "") + ">" + IO_LABEL[k] + "</option>").join("");
  const memOpts = '<option value="">— no cambiar —</option>' +
    Object.keys(MEM_LABEL).map((k) => '<option value="' + k + '"' + (r.mem === k ? " selected" : "") + ">" + MEM_LABEL[k] + "</option>").join("");
  const wd = r.watchdog || {};

  openModal({
    title: isNew ? "Nueva regla" : "Editar regla",
    okLabel: isNew ? "Crear regla" : "Guardar",
    body:
      '<div class="field"><label>Ejecutable o nombre (admite * como comodín)</label>' +
        '<input type="text" id="rPattern" value="' + esc(r.pattern) + '" placeholder="C:\\Juegos\\juego.exe  o  chrome.exe"></div>' +
      '<p class="help">Ruta completa = sólo ese archivo. Sólo nombre = todas las instancias, desde cualquier carpeta.</p>' +
      '<div class="grid2" style="margin-top:10px">' +
        '<div class="field"><label>Prioridad</label><select id="rPrio">' + prioOpts + "</select></div>" +
        '<div class="field"><label>Afinidad de CPU</label><input type="text" id="rAff" value="' + esc(affText) + '" placeholder="0-3,8 (vacío = no cambiar)"></div>' +
      "</div>" +
      '<div class="grid3">' +
        '<div class="field"><label>Eficiencia (EcoQoS)</label><select id="rEco">' + ecoOpts + "</select></div>" +
        '<div class="field"><label>Prioridad de E/S</label><select id="rIo">' + ioOpts + "</select></div>" +
        '<div class="field"><label>Prioridad de memoria</label><select id="rMem">' + memOpts + "</select></div>" +
      "</div>" +
      '<div class="section">Watchdog</div>' +
      '<div class="checkrow"><input type="checkbox" id="rKeep"' + (wd.keep_alive ? " checked" : "") + "><span>Relanzar si se cierra (necesita ruta completa)</span></div>" +
      '<div class="field" style="max-width:220px"><label>Máximo de instancias (0 = sin límite)</label>' +
        '<input type="number" id="rMax" min="0" max="20" value="' + Number(wd.max_instances || 0) + '"></div>' +
      '<div class="field"><label>Nota</label><input type="text" id="rNote" value="' + esc(r.note || "") + '"></div>',
    onOk: async () => {
      const payload = {
        pattern: el("rPattern").value.trim(),
        priority: el("rPrio").value || null,
        affinity: parseCores(el("rAff").value, ncpu),
        eco: el("rEco").value === "" ? null : el("rEco").value === "1",
        io: el("rIo").value || null,
        mem: el("rMem").value || null,
        watchdog: { keep_alive: el("rKeep").checked, max_instances: Number(el("rMax").value) || 0 },
        note: el("rNote").value.trim(),
      };
      if (!payload.pattern) throw new Error("Falta el ejecutable o nombre");
      if (isNew) await api("/api/rules", { op: "create", rule: payload });
      else await api("/api/rules", { op: "update", id: rule.id, rule: payload });
      toast(isNew ? "Regla creada" : "Regla actualizada", "ok");
    },
  });
}

function aiToolsHtml() {
  const re = /hermes|claude|codex|opencode|gemini|cursor/i;
  const tools = ((S.data && S.data.processes) || [])
    .filter((p) => re.test(p.name || ""))
    .sort((a, b) => b.rss - a.rss)
    .slice(0, 8);

  const list = tools.length
    ? '<div class="aitools">' + tools.map((p) =>
        '<div class="aitool" title="' + esc(p.exe || p.name) + '">' +
          '<span class="ainame">' + esc(p.name) + "</span>" +
          '<span class="aiprio">' + esc(PRIO_LABEL[p.prio] || p.prio || "—") + "</span>" +
          '<span class="airam mono">' + fmtMB(p.rss) + "</span>" +
        "</div>"
      ).join("") + "</div>"
    : '<p class="help">No hay herramientas de IA corriendo ahora mismo (Hermes, Claude, Codex, OpenCode, Gemini, Cursor).</p>';

  return list +
    '<div class="ai-actions">' +
      '<button type="button" class="btn tiny ai" id="aiBoost">🤖 Potenciar IA</button>' +
      '<span class="help">Aplica el perfil IA: más prioridad y recursos para estas herramientas.</span>' +
    "</div>";
}

function settingsModal() {
  const d = S.data;
  const pb = d.probalance;
  const trim = d.trim;
  const fg = d.foreground;
  const pw = d.power;
  const planOpts = (selected) => '<option value="">—</option>' + pw.plans.map((p) =>
    '<option value="' + esc(p.guid) + '"' + (p.guid === selected ? " selected" : "") + ">" + esc(p.name) + "</option>"
  ).join("");
  const prioOpts = (selected) => PRIOS.map(([v, l]) =>
    '<option value="' + v + '"' + (v === selected ? " selected" : "") + ">" + l + "</option>"
  ).join("");
  const trimPrio = '<option value="below_normal"' + (pb.priority === "below_normal" ? " selected" : "") + ">Bajo</option>" +
    '<option value="idle"' + (pb.priority === "idle" ? " selected" : "") + ">Idle</option>";
  const runOpts = (v, label) => '<option value="' + v + '"' + (d.runaway.action === v ? " selected" : "") + ">" + label + "</option>";

  openModal({
    title: "Ajustes",
    okLabel: "Guardar",
    body:
      '<div class="section">ProBalanza</div>' +
      '<div class="checkrow"><input type="checkbox" id="sPbEnabled"' + (pb.enabled ? " checked" : "") + "><span>Activa (regula los que se comen la CPU)</span></div>" +
      '<div class="grid3">' +
        '<div class="field"><label>Umbral (%)</label><input type="number" id="sPbThreshold" min="1" max="100" value="' + pb.threshold + '"></div>' +
        '<div class="field"><label>Sostener (s)</label><input type="number" id="sPbSustain" min="1" max="60" value="' + pb.sustain_s + '"></div>' +
        '<div class="field"><label>Restaurar bajo (%)</label><input type="number" id="sPbRestoreBelow" min="0" max="100" value="' + pb.restore_below + '"></div>' +
      "</div>" +
      '<div class="grid2">' +
        '<div class="field"><label>Restaurar tras (s)</label><input type="number" id="sPbRestoreS" min="1" max="120" value="' + pb.restore_s + '"></div>' +
        '<div class="field"><label>Bajar a</label><select id="sPbPriority">' + trimPrio + "</select></div>" +
      "</div>" +
      '<div class="grid2">' +
        '<div class="field"><label>Vía rápida: umbral (%)</label><input type="number" id="sPbFastThreshold" min="0" max="100" value="' + (pb.fast_threshold ?? 25) + '"></div>' +
        '<div class="field"><label>Vía rápida: sostener (s)</label><input type="number" id="sPbFastSustain" min="0" max="60" value="' + (pb.fast_sustain_s ?? 2) + '"></div>' +
      "</div>" +
      '<div class="checkrow"><input type="checkbox" id="sPbEco"' + (pb.eco ? " checked" : "") + "><span>Activar eficiencia (EcoQoS) mientras está regulado</span></div>" +
      '<div class="checkrow"><input type="checkbox" id="sPbSkipFg"' + (pb.skip_foreground ? " checked" : "") + "><span>No tocar el proceso en primer plano</span></div>" +
      '<div class="checkrow"><input type="checkbox" id="sPbSkipInt"' + (pb.skip_interactive ? " checked" : "") + "><span>No tocar apps con ventana visible (navegador, chat, IDE)</span></div>" +
      '<div class="field" style="max-width:220px"><label>Cooldown tras restaurar (s)</label><input type="number" id="sPbCooldown" min="0" max="600" value="' + pb.cooldown_s + '"></div>' +
      '<div class="field"><label>Protegidos (uno por línea)</label><textarea id="sProtect">' + esc((pb.protect || []).join("\n")) + "</textarea></div>" +

      '<div class="section">Watchdog de procesos colgados</div>' +
      '<div class="checkrow"><input type="checkbox" id="sRunEnabled"' + (d.runaway.enabled ? " checked" : "") + "><span>Avisar cuando un proceso sin ventana queda clavado en CPU</span></div>" +
      '<div class="grid3">' +
        '<div class="field"><label>CPU sostenida (%)</label><input type="number" id="sRunCpu" min="1" max="100" value="' + d.runaway.cpu_pct + '"></div>' +
        '<div class="field"><label>Durante (min)</label><input type="number" id="sRunMinutes" min="1" max="180" value="' + d.runaway.minutes + '"></div>' +
        '<div class="field"><label>Acción</label><select id="sRunAction">' + runOpts("warn", "Avisar") + runOpts("kill", "Terminar") + "</select></div>" +
      "</div>" +

      '<div class="section">SmartTrim</div>' +
      '<div class="checkrow"><input type="checkbox" id="sTrimEnabled"' + (trim.enabled ? " checked" : "") + "><span>Liberar RAM automáticamente cuando el sistema está al límite</span></div>" +
      '<div class="grid3">' +
        '<div class="field"><label>Umbral de RAM (%)</label><input type="number" id="sTrimThreshold" min="30" max="100" value="' + trim.threshold + '"></div>' +
        '<div class="field"><label>Mínimo por proceso (MB)</label><input type="number" id="sTrimMin" min="10" max="8192" value="' + trim.min_mb + '"></div>' +
        '<div class="field"><label>Cada (s)</label><input type="number" id="sTrimInterval" min="30" max="3600" value="' + trim.interval_s + '"></div>' +
      "</div>" +

      '<div class="section">Boost de primer plano</div>' +
      '<div class="checkrow"><input type="checkbox" id="sFgEnabled"' + (fg.enabled ? " checked" : "") + "><span>Subir prioridad a la app que estás usando y devolverla al salir</span></div>" +
      '<div class="field" style="max-width:240px"><label>Prioridad mientras está en primer plano</label><select id="sFgPriority">' + prioOpts(fg.priority) + "</select></div>" +

      '<div class="section">Plan de energía automático</div>' +
      '<div class="checkrow"><input type="checkbox" id="sPwAuto"' + (pw.auto ? " checked" : "") + "><span>Cambiar de plan según un proceso</span></div>" +
      '<div class="grid2">' +
        '<div class="field"><label>Proceso disparador</label><input type="text" id="sPwTrigger" value="' + esc(pw.trigger || "") + '" placeholder="juego.exe"></div>' +
        '<div class="field"><label>Plan normal</label><select id="sPwBase">' + planOpts(pw.base_plan) + "</select></div>" +
      "</div>" +
      '<div class="field" style="max-width:280px"><label>Plan con el proceso abierto</label><select id="sPwGame">' + planOpts(pw.game_plan) + "</select></div>" +

      '<div class="section">Herramientas de IA</div>' +
      aiToolsHtml(),
    onOk: async () => {
      await api("/api/probalance", {
        enabled: el("sPbEnabled").checked,
        threshold: Number(el("sPbThreshold").value),
        sustain_s: Number(el("sPbSustain").value),
        fast_threshold: Number(el("sPbFastThreshold").value),
        fast_sustain_s: Number(el("sPbFastSustain").value),
        restore_below: Number(el("sPbRestoreBelow").value),
        restore_s: Number(el("sPbRestoreS").value),
        priority: el("sPbPriority").value,
        eco: el("sPbEco").checked,
        skip_foreground: el("sPbSkipFg").checked,
        skip_interactive: el("sPbSkipInt").checked,
        cooldown_s: Number(el("sPbCooldown").value),
        protect: el("sProtect").value.split(/[\n,]+/).map((x) => x.trim()).filter(Boolean),
      });
      await api("/api/runaway", {
        enabled: el("sRunEnabled").checked,
        cpu_pct: Number(el("sRunCpu").value),
        minutes: Number(el("sRunMinutes").value),
        action: el("sRunAction").value,
      });
      await api("/api/settings/trim", {
        enabled: el("sTrimEnabled").checked,
        threshold: Number(el("sTrimThreshold").value),
        min_mb: Number(el("sTrimMin").value),
        interval_s: Number(el("sTrimInterval").value),
      });
      await api("/api/settings/foreground", {
        enabled: el("sFgEnabled").checked,
        priority: el("sFgPriority").value,
      });
      await api("/api/power", {
        auto: el("sPwAuto").checked,
        trigger: el("sPwTrigger").value.trim(),
        base_plan: el("sPwBase").value,
        game_plan: el("sPwGame").value,
      });
      toast("Ajustes guardados", "ok");
    },
  });

  const boost = el("aiBoost");
  if (boost) {
    boost.addEventListener("click", async () => {
      boost.disabled = true;
      boost.textContent = "Aplicando…";
      try {
        await setProfile("ai");
      } finally {
        boost.disabled = false;
        boost.textContent = "🤖 Potenciar IA";
      }
    });
  }
}

/* ---------- acciones ---------- */

async function setProfile(name, target) {
  if (!PROFILE_LABEL[name] || S.profileBusy) return;
  const d = S.data || {};
  const prof = d.profile || {};

  if (name === "full_power" && prof.active !== "full_power" && d.power && d.power.ac_line === false) {
    const ok = window.confirm(
      "Full Power con batería: consume más batería y el equipo va a calentar. ¿Activarlo igual?"
    );
    if (!ok) return;
  }

  S.profileBusy = true;
  renderProfile();
  try {
    const res = await api("/api/profile", target ? { name, target } : { name });
    if (res && res.profile && S.data) S.data.profile = res.profile;
    toast("Perfil " + PROFILE_LABEL[name] + " activado", "ok");
  } catch (e) {
    toast("No se pudo cambiar el perfil: " + e.message, "err");
  } finally {
    S.profileBusy = false;
    renderProfile();
  }
}

async function doTrim(pid) {
  const res = await api("/api/trim", { pid });
  toast("RAM liberada: " + fmtMB(res.freed), "ok");
}

async function doRestore(pid) {
  await api("/api/restore", { pid });
  toast("Valores originales restaurados", "ok");
}

async function doKill(pid, name) {
  if (!window.confirm("¿Terminar " + name + " (PID " + pid + ")?")) return;
  await api("/api/kill", { pid });
  toast(name + " terminado", "warn");
}

/* ---------- eventos ---------- */

function bind() {
  document.querySelectorAll(".tab").forEach((tab) => {
    tab.addEventListener("click", () => {
      document.querySelectorAll(".tab").forEach((t) => t.classList.remove("active"));
      document.querySelectorAll(".pane").forEach((p) => p.classList.remove("active"));
      tab.classList.add("active");
      S.tab = tab.dataset.tab;
      el("tab-" + S.tab).classList.add("active");
      localStorage.setItem("domador.tab", S.tab);
    });
  });

  el("pbToggle").addEventListener("change", async (ev) => {
    try {
      await api("/api/probalance", { enabled: ev.target.checked });
      toast("ProBalanza " + (ev.target.checked ? "activada" : "desactivada"), "ok");
    } catch (e) {
      ev.target.checked = !ev.target.checked;
      toast(e.message, "err");
    }
  });

  el("planSelect").addEventListener("change", async (ev) => {
    try {
      await api("/api/plan", { guid: ev.target.value });
    } catch (e) {
      toast(e.message, "err");
    }
  });

  el("settingsBtn").addEventListener("click", () => settingsModal());

  document.querySelectorAll("#profileBtns .profile").forEach((btn) => {
    btn.addEventListener("click", () => setProfile(btn.dataset.profile));
  });

  el("search").addEventListener("input", (ev) => {
    S.query = ev.target.value;
    renderTable();
  });

  el("sortSel").addEventListener("change", (ev) => {
    S.sort = ev.target.value;
    renderTable();
  });

  el("onlyRules").addEventListener("change", (ev) => {
    S.onlyRules = ev.target.checked;
    renderTable();
  });

  el("newRule").addEventListener("click", () => ruleModal(null, null));

  el("applyAllRules").addEventListener("click", async () => {
    let ok = 0;
    let skipped = 0;
    for (const rule of S.data.rules) {
      if (!rule.enabled) { skipped++; continue; }
      try {
        await api("/api/rules", { op: "apply", id: rule.id });
        ok++;
      } catch (e) { toast(e.message, "err"); }
    }
    toast("Reglas aplicadas: " + ok + (skipped ? " · " + skipped + " pausadas" : ""), "ok");
  });

  el("clearLog").addEventListener("click", () => {
    S.logClearedAt = Date.now() / 1000;
    renderLog();
  });

  el("trimAll").addEventListener("click", async () => {
    try {
      const res = await api("/api/trim-all");
      toast("SmartTrim: " + fmtMB(res.freed) + " en " + res.count + " procesos", "ok");
    } catch (e) { toast(e.message, "err"); }
  });

  el("restoreAll").addEventListener("click", async () => {
    try {
      const res = await api("/api/restore-all");
      toast("Restaurados: " + res.restored, "ok");
    } catch (e) { toast(e.message, "err"); }
  });

  const body = el("procBody");
  body.addEventListener("change", async (ev) => {
    const tr = ev.target.closest("tr");
    if (!tr) return;
    const pid = Number(tr.dataset.pid);
    if (ev.target.matches(".sel")) {
      if (ev.target.checked) S.selection.add(pid); else S.selection.delete(pid);
      tr.classList.toggle("sel", ev.target.checked);
      updateBatch();
    } else if (ev.target.matches(".prio")) {
      const cls = ev.target.value;
      ev.target.dataset.prio = cls;
      try {
        await api("/api/priority", { pid, cls });
      } catch (e) {
        toast(e.message, "err");
      }
    } else if (ev.target.matches(".eco")) {
      try {
        await api("/api/eco", { pid, enabled: ev.target.checked });
      } catch (e) {
        ev.target.checked = !ev.target.checked;
        toast(e.message, "err");
      }
    }
  });

  body.addEventListener("click", async (ev) => {
    const btn = ev.target.closest("button[data-act]");
    if (!btn) return;
    const tr = btn.closest("tr");
    const pid = Number(tr.dataset.pid);
    const proc = S.data.processes.find((p) => p.pid === pid);
    if (!proc) return;
    const act = btn.dataset.act;
    try {
      if (act === "aff") await affinityModal([pid]);
      else if (act === "rule") {
        await ensureMeta(pid);
        const rule = proc.rule_id ? S.data.rules.find((r) => r.id === proc.rule_id) : null;
        ruleModal(rule || null, proc);
      } else if (act === "trim") await doTrim(pid);
      else if (act === "gpu") {
        const res = await api("/api/gpu-preference", { pid, mode: "high" });
        toast("GPU dedicada para " + res.exe.split("\\").pop(), "ok");
      }
      else if (act === "restore") await doRestore(pid);
      else if (act === "kill") await doKill(pid, proc.name);
    } catch (e) {
      toast(e.message, "err");
    }
  });

  el("privateBody").addEventListener("click", async (ev) => {
    const btn = ev.target.closest("button[data-gpu-pid]");
    if (!btn) return;
    try {
      const res = await api("/api/gpu-preference", { pid: Number(btn.dataset.gpuPid), mode: "high" });
      toast("GPU dedicada para " + res.exe.split("\\").pop(), "ok");
    } catch (e) {
      toast(e.message, "err");
    }
  });

  el("runawayList").addEventListener("click", async (ev) => {
    const btn = ev.target.closest("button[data-kill-pid]");
    if (!btn) return;
    const pid = Number(btn.dataset.killPid);
    const proc = S.data.processes.find((p) => p.pid === pid);
    try {
      await api("/api/kill", { pid });
      toast("Terminado " + (proc ? proc.name : "PID " + pid), "ok");
    } catch (e) {
      toast(e.message, "err");
    }
  });

  const rules = el("rulesList");
  rules.addEventListener("click", async (ev) => {
    const btn = ev.target.closest("button[data-r]");
    if (!btn) return;
    const card = btn.closest(".rule");
    const rule = S.data.rules.find((r) => r.id === card.dataset.id);
    if (!rule) return;
    try {
      if (btn.dataset.r === "apply") {
        const res = await api("/api/rules", { op: "apply", id: rule.id });
        toast("Aplicada a " + res.count + " proceso(s)", "ok");
      } else if (btn.dataset.r === "edit") {
        ruleModal(rule, null);
      } else if (btn.dataset.r === "toggle") {
        await api("/api/rules", { op: "toggle", id: rule.id });
      } else if (btn.dataset.r === "del") {
        if (!window.confirm("¿Borrar la regla «" + rule.pattern + "»?")) return;
        await api("/api/rules", { op: "delete", id: rule.id });
        toast("Regla eliminada", "warn");
      }
    } catch (e) {
      toast(e.message, "err");
    }
  });

  el("modalClose").onclick = closeModal;
  el("modalCancel").onclick = closeModal;
  el("modal").addEventListener("mousedown", (ev) => {
    if (ev.target === el("modal")) closeModal();
  });
  el("modalOk").onclick = async () => {
    if (!S.modal || !S.modal.onOk) return;
    const button = el("modalOk");
    button.disabled = true;
    try {
      await S.modal.onOk();
      closeModal();
    } catch (e) {
      toast(e.message || String(e), "err");
    } finally {
      button.disabled = false;
    }
  };

  el("batchPrio").addEventListener("change", async (ev) => {
    const cls = ev.target.value;
    ev.target.value = "";
    if (!cls) return;
    const res = await eachPid(selectedPids(), (pid) => api("/api/priority", { pid, cls }));
    toast("Prioridad aplicada a " + res.ok + (res.fail ? " · " + res.fail + " fallaron" : ""), res.fail ? "warn" : "ok");
  });
  el("batchAff").addEventListener("click", async () => affinityModal(selectedPids()));
  el("batchEco").addEventListener("click", async () => {
    const res = await eachPid(selectedPids(), (pid) => api("/api/eco", { pid, enabled: true }));
    toast("Eficiencia activada en " + res.ok + (res.fail ? " · " + res.fail + " fallaron" : ""), res.fail ? "warn" : "ok");
  });
  el("batchTrim").addEventListener("click", async () => {
    let freed = 0;
    const res = await eachPid(selectedPids(), async (pid) => {
      const r = await api("/api/trim", { pid });
      freed += r.freed;
    });
    toast("Liberados " + fmtMB(freed) + (res.fail ? " · " + res.fail + " fallaron" : ""), res.fail ? "warn" : "ok");
  });
  el("batchRestore").addEventListener("click", async () => {
    const res = await eachPid(selectedPids(), (pid) => api("/api/restore", { pid }));
    toast("Restaurados " + res.ok + (res.fail ? " · " + res.fail + " sin cambios" : ""), res.fail ? "warn" : "ok");
  });
  el("batchClear").addEventListener("click", () => {
    S.selection.clear();
    renderTable();
  });

  document.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape") {
      closeModal();
      return;
    }
    if (ev.key === "/" && !/INPUT|TEXTAREA|SELECT/.test(document.activeElement.tagName)) {
      ev.preventDefault();
      el("search").focus();
    }
  });
}

/* ---------- arranque ---------- */

const savedTab = localStorage.getItem("domador.tab");
if (savedTab) {
  const tab = document.querySelector('.tab[data-tab="' + savedTab + '"]');
  if (tab) tab.click();
}
bind();
poll();
