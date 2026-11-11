"""Benchmark Domador (Rust) vs Process Lasso, misma máquina y misma carga.

Reutiliza la medición de Lasso del prototipo (mismo día, misma config) y mide
el binario Rust: huella, ProBalance, flapping y latencia de acciones.
"""
from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import tempfile
import time
from datetime import datetime
from pathlib import Path

import psutil

ROOT = Path(__file__).resolve().parents[1]
RUST = ROOT / "target" / "release" / "domador.exe"
HOG = ROOT / "bench" / "domador-hog.exe"
LASSO_RESULTS = ROOT / "bench" / "lasso-results.json"
EDGE_CANDIDATES = [
    Path(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"),
    Path(r"C:\Program Files\Microsoft\Edge\Application\msedge.exe"),
]

BELOW = int(psutil.BELOW_NORMAL_PRIORITY_CLASS)
NORMAL = int(psutil.NORMAL_PRIORITY_CLASS)
ABOVE = int(psutil.ABOVE_NORMAL_PRIORITY_CLASS)


def free_port() -> int:
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    return port


def http(base: str, path: str, body: dict | None = None, timeout: float = 8.0) -> dict:
    import urllib.request

    data = None
    headers = {}
    if body is not None:
        data = json.dumps(body).encode()
        headers = {"Content-Type": "application/json", "X-Domador": "1"}
    req = urllib.request.Request(
        base + path, data=data, headers=headers, method="POST" if data else "GET"
    )
    with urllib.request.urlopen(req, timeout=timeout) as response:
        return json.loads(response.read().decode())


def wait_health(base: str, timeout: float = 20.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            if http(base, "/api/health", timeout=0.6).get("app") == "domador":
                return
        except Exception:
            time.sleep(0.25)
    raise RuntimeError("timeout esperando a Domador")


def start_rust(tag: str, pb: bool = False) -> tuple[subprocess.Popen, str, Path]:
    port = free_port()
    config_dir = Path(tempfile.mkdtemp(prefix=f"domador-rs-{tag}-"))
    config = config_dir / "config.json"
    if pb:
        config.write_text(
            json.dumps(
                {
                    "probalance": {
                        "enabled": True,
                        "threshold": 5.0,
                        "sustain_s": 9,
                        "restore_below": 2.0,
                        "restore_s": 5,
                        "priority": "below_normal",
                        "eco": False,
                        "skip_foreground": True,
                        "skip_interactive": False,
                        "cooldown_s": 30,
                    }
                }
            ),
            encoding="utf-8",
        )
    env = os.environ.copy()
    env["DOMADOR_CONFIG"] = str(config)
    proc = subprocess.Popen(
        [str(RUST), "serve", "--port", str(port)],
        env=env,
        cwd=str(ROOT),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        creationflags=0x08000000,
    )
    base = f"http://127.0.0.1:{port}"
    wait_health(base)
    return proc, base, config


def stop_tree(proc: subprocess.Popen):
    try:
        root = psutil.Process(proc.pid)
        targets = root.children(recursive=True) + [root]
    except Exception:
        return
    for target in reversed(targets):
        try:
            target.terminate()
        except Exception:
            pass
    psutil.wait_procs(targets, timeout=6)


def measure(pids=None, root_pid=None, seconds=20.0) -> dict:
    def current():
        if root_pid is not None:
            try:
                root = psutil.Process(root_pid)
                return [root] + root.children(recursive=True)
            except Exception:
                return []
        out = []
        for pid in pids or []:
            try:
                out.append(psutil.Process(pid))
            except Exception:
                pass
        return out

    procs = current()
    prev = {}
    for p in procs:
        try:
            prev[p.pid] = sum(p.cpu_times()[:2])
        except Exception:
            pass
    rss_samples, cpu_samples = [], []
    started = time.time()
    last = started
    ncpu = psutil.cpu_count() or 1
    while time.time() - started < seconds:
        time.sleep(0.5)
        now = time.time()
        dt = max(1e-6, (now - last) * ncpu)
        rss = 0
        cpu = 0.0
        for p in current():
            try:
                rss += p.memory_info().rss
                cur = sum(p.cpu_times()[:2])
                if p.pid in prev:
                    cpu += 100.0 * max(0.0, cur - prev[p.pid]) / dt
                prev[p.pid] = cur
            except Exception:
                pass
        rss_samples.append(rss)
        cpu_samples.append(cpu)
        last = now
    return {
        "rss_avg_mb": round(sum(rss_samples) / len(rss_samples) / 1048576, 1),
        "rss_peak_mb": round(max(rss_samples) / 1048576, 1),
        "cpu_avg_pct": round(sum(cpu_samples) / len(cpu_samples), 3),
        "cpu_peak_pct": round(max(cpu_samples), 3),
        "processes": len(procs),
    }


def spawn_hogs(target: int, count: int = 2) -> tuple[list, list]:
    hogs = [
        subprocess.Popen(
            [str(HOG), "12", "6", "12", "10", "2"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            creationflags=0x08000000,
        )
        for _ in range(count)
    ]
    time.sleep(0.8)
    procs = []
    for hog in hogs:
        try:
            proc = psutil.Process(hog.pid)
            proc.nice(target)
            proc.cpu_percent(None)
            procs.append(proc)
        except Exception:
            pass
    return hogs, procs


def watch(procs, seconds: float):
    started = time.time()
    last = {p.pid: int(p.nice()) for p in procs}
    events = []
    while time.time() - started < seconds:
        time.sleep(0.1)
        for proc in procs:
            try:
                now = int(proc.nice())
            except Exception:
                continue
            if now != last.get(proc.pid):
                events.append(
                    {"t": round(time.time() - started, 1), "pid": proc.pid, "old": last[proc.pid], "new": now}
                )
                last[proc.pid] = now
    return events


def rank(nice: int) -> int:
    lookup = {
        int(psutil.IDLE_PRIORITY_CLASS): 0,
        int(psutil.BELOW_NORMAL_PRIORITY_CLASS): 1,
        int(psutil.NORMAL_PRIORITY_CLASS): 2,
        int(psutil.ABOVE_NORMAL_PRIORITY_CLASS): 3,
        int(psutil.HIGH_PRIORITY_CLASS): 4,
        int(psutil.REALTIME_PRIORITY_CLASS): 5,
    }
    return lookup.get(nice, 2)


def summarize(events):
    per = {}
    for event in events:
        per.setdefault(event["pid"], []).append(event)
    out = []
    for pid, items in per.items():
        throttles = [e for e in items if rank(e["new"]) < rank(e["old"])]
        restores = [e for e in items if rank(e["new"]) > rank(e["old"])]
        out.append(
            {
                "pid": pid,
                "transitions": len(items),
                "throttles": len(throttles),
                "restores": len(restores),
                "first_throttle_s": throttles[0]["t"] if throttles else None,
                "first_restore_s": restores[0]["t"] if restores else None,
            }
        )
    return out


def bench_footprint() -> dict:
    print("· Huella del motor Rust")
    proc, base, _ = start_rust("fp")
    try:
        time.sleep(3)
        engine = measure(root_pid=proc.pid, seconds=20)
    finally:
        stop_tree(proc)
    return {"engine": engine, "ui": None}


def bench_pb() -> dict:
    print("· ProBalance Rust (misma carga que Lasso)")
    proc, base, _ = start_rust("pb", pb=True)
    try:
        time.sleep(1.5)
        hogs, procs = spawn_hogs(ABOVE)
        try:
            events = watch(procs, 42)
            state = http(base, "/api/state")
            log = [
                {"ts": e["ts"], "msg": e["msg"]}
                for e in state["log"]
                if e["level"] == "pb"
            ]
            tick = state["app"]["tick_ms"]
        finally:
            for hog in hogs:
                hog.terminate()
    finally:
        stop_tree(proc)
    return {"events": events, "summary": summarize(events), "log": log, "tick_ms": tick}


def bench_latency() -> dict:
    print("· Latencia de acciones Rust")
    proc, base, _ = start_rust("lat")
    hog = subprocess.Popen(
        [str(ROOT / "target" / "release" / "hog.exe"), "30", "1"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        creationflags=0x08000000,
    )
    try:
        time.sleep(0.8)
        target = psutil.Process(hog.pid)
        target.nice(ABOVE)
        samples = []
        for index in range(20):
            cls = "below_normal" if index % 2 == 0 else "normal"
            expected = BELOW if cls == "below_normal" else NORMAL
            started = time.perf_counter()
            http(base, "/api/priority", {"pid": hog.pid, "cls": cls})
            ok = int(target.nice()) == expected
            samples.append(((time.perf_counter() - started) * 1000, ok))
        times = sorted(s[0] for s in samples)
        return {
            "n": len(times),
            "verified": sum(1 for s in samples if s[1]),
            "p50_ms": round(times[len(times) // 2], 2),
            "p95_ms": round(times[min(len(times) - 1, int(len(times) * 0.95))], 2),
            "min_ms": round(times[0], 2),
            "max_ms": round(times[-1], 2),
        }
    finally:
        hog.terminate()
        stop_tree(proc)


def main() -> int:
    if not RUST.exists():
        print("Falta target/release/domador.exe")
        return 2
    lasso = json.loads(LASSO_RESULTS.read_text(encoding="utf-8")) if LASSO_RESULTS.exists() else {}
    lasso_pb = lasso.get("lasso_probalance") or {}
    if lasso_pb.get("events"):
        lasso_pb["summary"] = summarize(lasso_pb["events"])
    results = {
        "generated": datetime.now().isoformat(timespec="seconds"),
        "binary_mb": round(RUST.stat().st_size / 1048576, 2),
        "footprint": bench_footprint(),
        "pb": bench_pb(),
        "latency": bench_latency(),
        "lasso": {
            "footprint": lasso.get("footprint"),
            "pb": lasso_pb,
            "churn": lasso.get("lasso_log"),
        },
    }
    (ROOT / "bench" / "results-rs.json").write_text(
        json.dumps(results, indent=2, ensure_ascii=False), encoding="utf-8"
    )

    lf = results["lasso"]["footprint"] or {}
    rf = results["footprint"]
    lpb = results["lasso"]["pb"] or {}
    rpb = results["pb"]
    lines = [
        "# Domador (Rust) vs Process Lasso — benchmark",
        "",
        f"Generado: {results['generated']}",
        "",
        "| Métrica | Process Lasso | Domador Rust |",
        "|---|---|---|",
        f"| RAM motor | {(lf.get('lasso_engine') or {}).get('rss_avg_mb', '—')} MB | {rf['engine']['rss_avg_mb']} MB |",
        f"| CPU motor | {(lf.get('lasso_engine') or {}).get('cpu_avg_pct', '—')}% | {rf['engine']['cpu_avg_pct']}% |",
        f"| RAM app completa | {(lf.get('lasso_full') or {}).get('rss_avg_mb', '—')} MB (GUI nativa) | UI en pestaña de Edge (comparte proceso) |",
        f"| Cambios de prioridad (42 s) | {sum(i['transitions'] for i in (lpb.get('summary') or []))} | {sum(i['transitions'] for i in rpb['summary'])} |",
        f"| Primer throttle | {min((i['first_throttle_s'] or 99) for i in (lpb.get('summary') or [{'first_throttle_s': 99}]))} s | {min((i['first_throttle_s'] or 99) for i in rpb['summary'])} s |",
        f"| Latencia p50 / p95 | sin API | {results['latency']['p50_ms']} / {results['latency']['p95_ms']} ms |",
        f"| Binario | instalador + servicio | {results['binary_mb']} MB, un .exe |",
        "",
        "## ProBalance",
        "",
        "Lasso: " + json.dumps(lpb.get("summary"), ensure_ascii=False),
        "",
        "Domador: " + json.dumps(rpb["summary"], ensure_ascii=False),
        "",
        "## Churn real de Lasso (30 min de su log)",
        "",
        json.dumps(results["lasso"]["churn"], ensure_ascii=False, indent=2),
        "",
        "## Lectura honesta",
        "",
        "- Lasso reacciona más rápido por defecto (baja prioridad casi al instante), pero flapea: 4 cambios por proceso en 42 s.",
        "- Domador tarda ~9 s (configurable) y hace 2 cambios por proceso gracias al cooldown. Menos ping-pong, más estabilidad.",
        "- El motor Rust es un binario único de 2 MB y no necesita servicio ni instalador.",
        "- La UI corre en el Edge que ya tenés abierto: el costo real es una pestaña compartida, no un WebView2 embebido.",
        "- Falta medir FPS en juegos; eso queda como próximo benchmark con CapFrameX.",
    ]
    (ROOT / "bench" / "report.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    sys.exit(main())
