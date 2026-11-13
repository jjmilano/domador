# Domador (Rust) vs Process Lasso — benchmark

> **Reporte histórico.** Los números del motor son de la corrida del **2026-11-11** con el motor **v0.4.2** (misma base de sampler que v0.6.x). La UI nativa y FPS **no** se midieron en esa corrida. El `bench.py` actual genera este mismo archivo con la versión y fecha de la corrida nueva; hasta entonces, esto es la línea base publicada.

Generado: 2026-11-11T17:13:15

| Métrica | Process Lasso | Domador (motor) |
|---|---|---|
| RAM motor | 20,0 MB | 17,3 MB |
| CPU motor | 0,19% | 0,115% |
| RAM app completa | 64,8 MB (GUI nativa) | **no medido** (UI egui en proceso aparte) |
| Cambios de prioridad (42 s) | 8 | 4 |
| Primer throttle | 0,1 s | 8,8 s |
| Latencia p50 / p95 | sin API documentada | 2,37 / 25,42 ms |
| Distribución (v0.6.2) | instalador + servicio | 3 binarios: motor 2,6 MB + UI 5,5 MB + bandeja 0,6 MB |

La fila de distribución se actualizó al build v0.6.2; no formaba parte de la medición original de v0.4.2.

## ProBalance

Lasso: `[{"pid": 752, "transitions": 4, "throttles": 2, "restores": 2, "first_throttle_s": 0.1, "first_restore_s": 12.5}, {"pid": 59264, "transitions": 4, "throttles": 2, "restores": 2, "first_throttle_s": 0.1, "first_restore_s": 12.5}]`

Domador: `[{"pid": 32336, "transitions": 2, "throttles": 1, "restores": 1, "first_throttle_s": 8.8, "first_restore_s": 15.8}, {"pid": 24840, "transitions": 2, "throttles": 1, "restores": 1, "first_throttle_s": 8.8, "first_restore_s": 15.8}]`

## Churn real de Lasso (30 min de su log)

```json
{
  "window_min": 30,
  "probalance_events": 140,
  "lowered": 35,
  "restored": 32,
  "distinct_processes": 5,
  "lowered_per_min": 1.17,
  "top_processes": [
    { "name": "chrome.exe", "lowered": 26 },
    { "name": "msedgewebview2.exe", "lowered": 5 },
    { "name": "powershell.exe", "lowered": 2 },
    { "name": "python3.13.exe", "lowered": 1 },
    { "name": "chatgpt.exe", "lowered": 1 }
  ]
}
```

## Lectura honesta

- Lasso reacciona más rápido por defecto (baja prioridad casi al instante), pero flapea: 4 cambios por proceso en 42 s.
- Domador tarda ~9 s (configurable) y hace 2 cambios por proceso gracias al cooldown. Menos ping-pong, más estabilidad.
- El motor Rust pesa 17,3 MB y no necesita servicio ni instalador obligatorio.
- La UI nativa es una ventana egui en un **proceso aparte**; no se suma a la RAM del motor ni se disfraza de pestaña de navegador. Su huella queda pendiente de medición (`bench.py --ui`).
- La vía rápida de ProBalance (25%/2 s) no se midió en esta corrida: el hog de 2 hilos llega a 12,5% y usa la vía lenta. No se cita un tiempo hasta archivarla.
- Falta medir FPS en juegos; eso queda como próximo benchmark con CapFrameX.

## Limitaciones

- Una sola máquina y una sola corrida; no es un benchmark estadístico.
- La línea base de Lasso está cacheada en `lasso-results.json`; no se re-mide automáticamente.
- No hay números de UI ni de FPS en este reporte.
