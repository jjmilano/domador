# Domador (Rust) vs Process Lasso — benchmark

Generado: 2026-11-11T16:32:27

| Métrica | Process Lasso | Domador Rust |
|---|---|---|
| RAM motor | 20.0 MB | 29.7 MB |
| CPU motor | 0.19% | 0.431% |
| RAM app completa | 64.8 MB (GUI nativa) | UI en pestaña de Edge (comparte proceso) |
| Cambios de prioridad (42 s) | 8 | 4 |
| Primer throttle | 0,1 s | 8,8 s |
| Latencia p50 / p95 | sin API | 1.19 / 21.54 ms |
| Binario | instalador + servicio | 2.11 MB, un .exe |

## ProBalance

Lasso: [{"pid": 752, "transitions": 4, "throttles": 2, "restores": 2, "first_throttle_s": 0.1006474494934082, "first_restore_s": 12.451271057128906}, {"pid": 59264, "transitions": 4, "throttles": 2, "restores": 2, "first_throttle_s": 0.10067367553710938, "first_restore_s": 12.45128345489502}]

Domador: [{"pid": 40416, "transitions": 2, "throttles": 1, "restores": 1, "first_throttle_s": 8.8, "first_restore_s": 16.2}, {"pid": 1276, "transitions": 2, "throttles": 1, "restores": 1, "first_throttle_s": 8.8, "first_restore_s": 16.2}]

## Churn real de Lasso (30 min de su log)

{
  "window_min": 30,
  "probalance_events": 140,
  "lowered": 35,
  "restored": 32,
  "distinct_processes": 5,
  "lowered_per_min": 1.17,
  "top_processes": [
    {
      "name": "chrome.exe",
      "lowered": 26
    },
    {
      "name": "msedgewebview2.exe",
      "lowered": 5
    },
    {
      "name": "powershell.exe",
      "lowered": 2
    },
    {
      "name": "python3.13.exe",
      "lowered": 1
    },
    {
      "name": "chatgpt.exe",
      "lowered": 1
    }
  ]
}

## Lectura honesta

- Lasso reacciona más rápido por defecto (baja prioridad casi al instante), pero flapea: 4 cambios por proceso en 42 s.
- Domador tarda ~9 s (configurable) y hace 2 cambios por proceso gracias al cooldown. Menos ping-pong, más estabilidad.
- El motor Rust es un binario único de 2 MB y no necesita servicio ni instalador.
- La UI corre en el Edge que ya tenés abierto: el costo real es una pestaña compartida, no un WebView2 embebido.
- Falta medir FPS en juegos; eso queda como próximo benchmark con CapFrameX.
