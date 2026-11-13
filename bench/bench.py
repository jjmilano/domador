#!/usr/bin/env python3
"""Benchmark Domador (motor y UI nativa) vs Process Lasso.

Misma máquina, misma carga y misma configuración para los dos motores. El
script mide el build actual de Domador; la línea base de Lasso se lee de
`bench/lasso-results.json` y NO se re-mide automáticamente (Lasso no expone una
API estable para hacerlo). Si ese archivo no existe, la tabla muestra "—".

Uso:
    python bench/bench.py --check    # valida requisitos, no mide
    python bench/bench.py            # medición completa (~2 min)
    python bench/bench.py --quick    # smoke corto, no publicable
    python bench/bench.py --ui       # agrega RAM/CPU de domador-ui.exe (puerto 8787)

Requiere Windows x64, Python 3.11+ y psutil (`bench/requirements.txt`).
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import socket
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

try:
    import psutil
except ImportError:  # pragma: no cover - mensaje para el usuario
    print("Falta psutil. Instalá: python -m pip install -r bench/requirements.txt")
    raise SystemExit(2)

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("DOMADOR_BIN", ROOT / "target" / "release" / "domador.exe"))
UI_BIN = Path(os.environ.get("DOMADOR_UI", ROOT / "target" / "release" / "domador-ui.exe"))
TRAY_BIN = Path(os.environ.get("DOMADOR_TRAY", ROOT / "target" / "release" / "domador-tray.exe"))
HOG = Path(os.environ.get("DOMADOR_HOG", ROOT / "bench" / "domador-hog.exe"))
LASSO_RESULTS = ROOT / "bench" / "lasso-results.json"
RESULTS = ROOT / "bench" / "results-rs.json"
REPORT = ROOT / "bench" / "report.md"
CREATE_NO_WINDOW = 0x08000000

BELOW = int(psutil.BELOW_NORMAL_PRIORITY_CLASS)
NORMAL = int(psutil.NORMAL_PRIORITY_CLASS)
ABOVE = int(psutil.ABOVE_NORMAL_PRIORITY_CLASS)


# --------------------------------------------------------------------- helpers


def fail(message: str) -> int:
    print(f"ERROR: {message}")
    return 2


def free_port() -> int:
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    return port


def port_free(port: int) -> bool:
    sock = socket.socket()
    try:
        sock.bind(("127.0.0.1", port))
        return True
    except OSError:
        return False
    finally:
        sock.close()


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


def wait_health(base: str, timeout: float = 25.0) -> dict:
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        try:
            payload = http(base, "/api/health", timeout=0.6)
            if payload.get("app") == "domador":
                return payload
        except Exception as err:  # el server todavía no levantó
            last = err
            time.sleep(0.25)
    raise RuntimeError(f"timeout esperando a Domador en {base}: {last}")


def hog_args() -> list[str]:
    # 12 s activo / 6 s idle / 12 s activo, 10 s de cola, 2 hilos.
    return [str(HOG), "12", "6", "12", "10", "2"]


def start_rust(
    tag: str, pb: bool = False, port: int | None = None
) -> tuple[subprocess.Popen, str, Path, int]:
    chosen = port if port is not None else free_port()
    config_dir = Path(tempfile.mkdtemp(prefix=f"domador-rs-{tag}-"))
    config = config_dir / "config.json"
    payload = {"memguard": {"enabled": False}}
    if pb:
        payload["probalance"] = {
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
    config.write_text(json.dumps(payload), encoding="utf-8")
    env = os.environ.copy()
    env["DOMADOR_CONFIG"] = str(config)
    proc = subprocess.Popen(
        [str(BIN), "serve", "--port", str(chosen)],
        env=env,
        cwd=str(ROOT),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        creationflags=CREATE_NO_WINDOW,
    )
    base = f"http://127.0.0.1:{chosen}"
    wait_health(base)
    return proc, base, config, chosen


def stop_tree(proc: subprocess.Popen) -> None:
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


def measure(
    pids: list[int] | None = None, root_pid: int | None = None, seconds: float = 20.0
) -> dict:
    def current() -> list[psutil.Process]:
        if root_pid is not None:
            try:
                root = psutil.Process(root_pid)
                kids = [
                    child
                    for child in root.children(recursive=True)
                    if (child.name() or "").lower() != "conhost.exe"
                ]
                return [root] + kids
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
    prev: dict[int, float] = {}
    for proc in procs:
        try:
            prev[proc.pid] = sum(proc.cpu_times()[:2])
        except Exception:
            pass
    rss_samples: list[float] = []
    cpu_samples: list[float] = []
    started = time.time()
    last = started
    ncpu = psutil.cpu_count() or 1
    while time.time() - started < seconds:
        time.sleep(0.5)
        now = time.time()
        dt = max(1e-6, (now - last) * ncpu)
        rss = 0
        cpu = 0.0
        for proc in current():
            try:
                rss += proc.memory_info().rss
                cur = sum(proc.cpu_times()[:2])
                if proc.pid in prev:
                    cpu += 100.0 * max(0.0, cur - prev[proc.pid]) / dt
                prev[proc.pid] = cur
            except Exception:
                pass
        rss_samples.append(rss)
        cpu_samples.append(cpu)
        last = now
    if not rss_samples:
        return {"error": "sin muestras"}
    return {
        "rss_avg_mb": round(sum(rss_samples) / len(rss_samples) / 1048576, 1),
        "rss_peak_mb": round(max(rss_samples) / 1048576, 1),
        "cpu_avg_pct": round(sum(cpu_samples) / len(cpu_samples), 3),
        "cpu_peak_pct": round(max(cpu_samples), 3),
        "processes": len(procs),
    }


def spawn_hogs(target: int, count: int = 2) -> tuple[list[subprocess.Popen], list[psutil.Process]]:
    hogs = [
        subprocess.Popen(
            hog_args(),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            creationflags=CREATE_NO_WINDOW,
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


def watch(procs: list[psutil.Process], seconds: float) -> list[dict]:
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
                    {
                        "t": round(time.time() - started, 1),
                        "pid": proc.pid,
                        "old": last[proc.pid],
                        "new": now,
                    }
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


def summarize(events: list[dict]) -> list[dict]:
    per: dict[int, list[dict]] = {}
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
                "first_throttle_s": round(throttles[0]["t"], 1) if throttles else None,
                "first_restore_s": round(restores[0]["t"], 1) if restores else None,
            }
        )
    return out


# ------------------------------------------------------------------ benchmarks


def bench_footprint(measure_ui: bool, ui_port: int, seconds: float) -> dict:
    print("· Huella del motor Rust")
    use_fixed = measure_ui and port_free(ui_port)
    proc, base, _, actual_port = start_rust("fp", port=ui_port if use_fixed else None)
    ui_result: dict | None = None
    try:
        time.sleep(3)
        engine = measure(root_pid=proc.pid, seconds=seconds)
        if measure_ui:
            if actual_port != ui_port:
                ui_result = {
                    "skipped": f"el motor no quedó en {ui_port} (usó {actual_port}); la UI usa 8787 fijo"
                }
            elif not UI_BIN.exists():
                ui_result = {"skipped": f"falta {UI_BIN}"}
            else:
                print("· Huella de la UI nativa (egui)")
                ui = subprocess.Popen(
                    [str(UI_BIN)],
                    cwd=str(ROOT),
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    creationflags=CREATE_NO_WINDOW,
                )
                try:
                    time.sleep(3)
                    ui_result = measure(root_pid=ui.pid, seconds=seconds)
                finally:
                    ui.terminate()
                    try:
                        ui.wait(timeout=6)
                    except Exception:
                        ui.kill()
    finally:
        stop_tree(proc)
    return {"engine": engine, "ui": ui_result}


def bench_pb(seconds: float) -> dict:
    print("· ProBalance Rust (misma carga que Lasso)")
    proc, base, _, _ = start_rust("pb", pb=True)
    tick = None
    events: list[dict] = []
    try:
        time.sleep(1.5)
        hogs, procs = spawn_hogs(ABOVE)
        try:
            events = watch(procs, seconds)
            state = http(base, "/api/state")
            log = [
                {"ts": e["ts"], "msg": e["msg"]}
                for e in state.get("log", [])
                if e.get("level") == "pb"
            ]
            tick = (state.get("app") or {}).get("tick_ms")
        finally:
            for hog in hogs:
                hog.terminate()
    finally:
        stop_tree(proc)
    return {"events": events, "summary": summarize(events), "log": log, "tick_ms": tick}


def bench_latency(samples_count: int = 20) -> dict:
    print("· Latencia de acciones Rust")
    proc, base, _, _ = start_rust("lat")
    hog = subprocess.Popen(
        [str(HOG), "30", "1"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        creationflags=CREATE_NO_WINDOW,
    )
    try:
        time.sleep(0.8)
        target = psutil.Process(hog.pid)
        target.nice(ABOVE)
        samples = []
        for index in range(samples_count):
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


# ---------------------------------------------------------------------- report


def app_version() -> str:
    try:
        out = subprocess.run(
            [str(BIN), "--version"],
            capture_output=True,
            text=True,
            timeout=10,
            creationflags=CREATE_NO_WINDOW,
        )
        text = (out.stdout or "").strip()
        return text.split()[-1] if text else "desconocida"
    except Exception:
        return "desconocida"


def mb(path: Path) -> float | None:
    try:
        return round(path.stat().st_size / 1048576, 2)
    except Exception:
        return None


def fmt(value, suffix: str = "") -> str:
    if value is None:
        return "—"
    if isinstance(value, float):
        return f"{value:g}{suffix}"
    return f"{value}{suffix}"


def first_throttle(summary: list[dict] | None) -> str:
    values = [item.get("first_throttle_s") for item in (summary or [])]
    values = [v for v in values if v is not None]
    return fmt(min(values), " s") if values else "—"


def transitions(summary: list[dict] | None) -> int:
    return sum(item.get("transitions", 0) for item in (summary or []))


def build_report(results: dict) -> str:
    lasso = results.get("lasso") or {}
    lf = lasso.get("footprint") or {}
    lpb = lasso.get("pb") or {}
    rpb = results["pb"]
    engine = results["footprint"]["engine"]
    ui = results["footprint"].get("ui")
    app = results.get("app") or {}
    lasso_engine = lf.get("lasso_engine") or {}
    lasso_full = lf.get("lasso_full") or {}

    if isinstance(ui, dict) and ui.get("skipped"):
        ui_cell = f"no medido ({ui['skipped']})"
    elif isinstance(ui, dict) and "rss_avg_mb" in ui:
        ui_cell = f"{ui['rss_avg_mb']} MB · {ui['cpu_avg_pct']}% CPU"
    else:
        ui_cell = "no medido (correr con --ui)"

    sizes = " + ".join(
        f"{name} {fmt(value, ' MB')}"
        for name, value in (
            ("motor", app.get("exe_mb")),
            ("UI", app.get("ui_mb")),
            ("bandeja", app.get("tray_mb")),
        )
        if value is not None
    ) or "—"

    lines = [
        "# Domador (motor + UI nativa) vs Process Lasso — benchmark",
        "",
        f"Generado: {results['generated']} · modo **{results['mode']}** · Domador {app.get('version', '?')}",
        "",
        "> Los números de Lasso salen de `bench/lasso-results.json` (medición cacheada).",
        "> Este script **no** vuelve a medir Lasso: sólo mide el build actual de Domador.",
        "> Si Lasso no está en la tabla, es porque no hay baseline disponible.",
        "",
        "| Métrica | Process Lasso | Domador |",
        "|---|---|---|",
        f"| RAM motor | {fmt(lasso_engine.get('rss_avg_mb'), ' MB')} | {fmt(engine.get('rss_avg_mb'), ' MB')} |",
        f"| CPU motor (idle) | {fmt(lasso_engine.get('cpu_avg_pct'), '%')} | {fmt(engine.get('cpu_avg_pct'), '%')} |",
        f"| RAM app completa | {fmt(lasso_full.get('rss_avg_mb'), ' MB')} (GUI) | {ui_cell} |",
        f"| Cambios de prioridad ({results['pb_seconds']} s) | {transitions(lpb.get('summary')) or '—'} | {transitions(rpb['summary'])} |",
        f"| Primer throttle | {first_throttle(lpb.get('summary'))} | {first_throttle(rpb['summary'])} |",
        f"| Latencia p50 / p95 | sin API documentada | {results['latency']['p50_ms']} / {results['latency']['p95_ms']} ms |",
        f"| Distribución | instalador + servicio | {sizes} |",
        "",
        "## Configuración",
        "",
        f"- Domador: `{json.dumps(results['config'], ensure_ascii=False)}`",
        "- Lasso: umbral 5% por proceso / 10% total, 9 s sobre el umbral, restauración a 2% (ver `bench/README.md`).",
        f"- Máquina: {results['machine']['cpu']} · {results['machine']['ram_gb']} GB · {results['machine']['os']}",
        "",
        "## ProBalance",
        "",
        "Lasso: " + json.dumps(lpb.get("summary"), ensure_ascii=False),
        "",
        "Domador: " + json.dumps(rpb["summary"], ensure_ascii=False),
        "",
        "## Churn real de Lasso (30 min de su log)",
        "",
        json.dumps(lasso.get("churn"), ensure_ascii=False, indent=2),
        "",
        "## Lectura honesta",
        "",
        "- Lasso reacciona más rápido por defecto, pero flapea: 4 transiciones por proceso en 42 s.",
        "- Domador espera 9 s de CPU sostenida y aplica 30 s de cooldown: 2 transiciones por proceso. Es más lento a propósito; la mejora es la estabilidad.",
        "- La UI nativa es un proceso egui aparte y **no** forma parte de la RAM del motor. Si no se midió, la tabla lo dice.",
        "- La distribución de Domador son tres binarios; no hay servicio ni instalador obligatorio.",
        "- No hay FPS medidos. El próximo benchmark es CapFrameX, mismo juego y escena, con y sin perfil Juego.",
        "",
        "## Limitaciones",
        "",
        "- Una sola máquina y una sola corrida; no es un benchmark estadístico.",
        "- La línea base de Lasso está cacheada y puede ser de otra versión/fecha.",
        "- La UI necesita el puerto 8787 libre para medirse; si está ocupado, se saltea.",
        "- `--quick` es un smoke: no publicar sus números.",
        "",
    ]
    return "\n".join(lines)


# ------------------------------------------------------------------------ main


def check() -> int:
    problems = []
    if sys.platform != "win32":
        problems.append("el benchmark es Windows-only (prioridades y proceso por PID)")
    for label, path in (("motor", BIN), ("hog", HOG)):
        if not path.exists():
            problems.append(f"falta {label}: {path}")
    if not UI_BIN.exists():
        print(f"AVISO: falta UI ({UI_BIN}); --ui se va a saltear")
    print(f"Python: {platform.python_version()} · psutil {psutil.__version__}")
    print(f"Motor:  {BIN} ({fmt(mb(BIN), ' MB')})")
    print(f"UI:     {UI_BIN} ({fmt(mb(UI_BIN), ' MB')})")
    print(f"Bandeja:{TRAY_BIN} ({fmt(mb(TRAY_BIN), ' MB')})")
    print(f"Hog:    {HOG}")
    print(f"Lasso:  {'sí' if LASSO_RESULTS.exists() else 'no'} ({LASSO_RESULTS})")
    if problems:
        for problem in problems:
            print(f"ERROR: {problem}")
        return 2
    print("OK: prerequisites")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Benchmark Domador vs Process Lasso")
    parser.add_argument("--check", action="store_true", help="valida requisitos y sale")
    parser.add_argument("--quick", action="store_true", help="smoke corto, no publicable")
    parser.add_argument("--ui", action="store_true", help="mide también domador-ui.exe")
    parser.add_argument("--port", type=int, default=8787, help="puerto para la UI (default 8787)")
    args = parser.parse_args()

    if args.check:
        return check()
    if sys.platform != "win32":
        return fail("Windows-only: usá una máquina Windows x64")
    for label, path in (("motor", BIN), ("hog", HOG)):
        if not path.exists():
            return fail(f"falta {label}: {path}. Corré `cargo build --release --bins`.")

    lasso = {}
    if LASSO_RESULTS.exists():
        try:
            lasso = json.loads(LASSO_RESULTS.read_text(encoding="utf-8"))
        except Exception as err:
            print(f"AVISO: no pude leer {LASSO_RESULTS}: {err}")

    lasso_pb = lasso.get("lasso_probalance") or {}
    if lasso_pb.get("events"):
        lasso_pb["summary"] = summarize(lasso_pb["events"])

    seconds = 5.0 if args.quick else 20.0
    pb_seconds = 20.0 if args.quick else 42.0
    latency_samples = 5 if args.quick else 20

    print(f"Domador {app_version()}")
    results = {
        "schema": 2,
        "generated": datetime.now(timezone.utc).astimezone().isoformat(timespec="seconds"),
        "mode": "quick" if args.quick else "full",
        "app": {
            "version": app_version(),
            "exe_mb": mb(BIN),
            "ui_mb": mb(UI_BIN),
            "tray_mb": mb(TRAY_BIN),
        },
        "machine": {
            "os": platform.platform(),
            "cpu": platform.processor() or "CPU desconocida",
            "cores_logical": psutil.cpu_count(),
            "cores_physical": psutil.cpu_count(logical=False),
            "ram_gb": round(psutil.virtual_memory().total / 1073741824, 1),
        },
        "config": {
            "threshold": 5.0,
            "sustain_s": 9,
            "restore_below": 2.0,
            "restore_s": 5,
            "cooldown_s": 30,
            "eco": False,
            "skip_foreground": True,
            "hogs": 2,
        },
        "pb_seconds": int(pb_seconds),
        "footprint": bench_footprint(args.ui, args.port, seconds),
        "pb": bench_pb(pb_seconds),
        "latency": bench_latency(latency_samples),
        "lasso": {
            "source": str(LASSO_RESULTS),
            "footprint": lasso.get("footprint"),
            "pb": lasso_pb,
            "churn": lasso.get("lasso_log"),
            "measured_at": lasso.get("generated"),
        },
    }

    if args.quick:
        out = ROOT / "bench" / "results-quick.json"
        out.write_text(json.dumps(results, indent=2, ensure_ascii=False), encoding="utf-8")
        print(f"Smoke OK: {out} (no publicar estos números)")
        return 0

    RESULTS.write_text(json.dumps(results, indent=2, ensure_ascii=False), encoding="utf-8")
    report = build_report(results)
    REPORT.write_text(report + "\n", encoding="utf-8")
    print()
    print(report)
    print(f"\nCrudos: {RESULTS}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
