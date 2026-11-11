# Benchmark: Domador vs Process Lasso

Este directorio tiene todo lo necesario para reproducir la comparación. Los números crudos están en `results-rs.json` y `lasso-results.json`; el resumen legible, en `report.md`.

## Equipo

- AMD Ryzen 7 5800H (8 núcleos, 16 hilos)
- 31,9 GB de RAM
- NVIDIA GeForce RTX 3070 Laptop GPU
- Windows 11 Home Insider Preview 10.0.26220
- Process Lasso 18.4.0.48, configuración vigente de la máquina
- Domador 0.4.1 (release, un `.exe`)

## Carga

Dos procesos `bench/domador-hog.exe` con 2 hilos cada uno y fases 12 s activo / 6 s idle / 12 s activo. Se lanzan con prioridad **Above Normal** para que Lasso no los toque con su regla de prioridad por defecto y para que ambos motores vean la misma carga.

## Qué se mide

| Métrica | Método |
|---|---|
| RAM y CPU del motor | `domador serve` sin UI, 20 s de muestreo cada 0,5 s, sumando el árbol del proceso |
| Cambios de prioridad | lectura de la prioridad de cada hog cada 100 ms durante 42 s; se cuentan transiciones, no muestras |
| Primer throttle | primer cambio de Normal/Above a Below/Idle |
| Latencia | 20 `POST /api/priority` alternando `below_normal` y `normal`, verificando el valor real con psutil |
| Churn de Lasso | 30 minutos de su propio log, contando eventos de ProBalance |
| Huella de la UI | no se suma a la del motor: la UI de Domador es una pestaña del Edge existente |

## Configuración de ProBalance

**Lasso:** umbral 5% por proceso / 10% total, 9 s por encima del umbral, restauración a 2% con mínimo 4,2 s, según su `prolasso.ini`.

**Domador:** `threshold=5`, `sustain_s=9`, `restore_below=2`, `restore_s=5`, `cooldown_s=30`, `eco=false`, `skip_foreground=true`, `skip_interactive=false`, protegidos por defecto.

## Lectura correcta

- Lasso baja la prioridad casi al instante. Por eso su “primer throttle” es 0,1 s, pero también por eso hace **4 transiciones por proceso** en la ventana: baja, restaura, vuelve a bajar y vuelve a restaurar. No es un ProBalance con histéresis; es una reacción agresiva que después corrige.
- Domador espera 9 s de CPU sostenida y aplica un cooldown de 30 s. Hace **2 transiciones por proceso**. La reacción es más lenta a propósito; la estabilidad es la mejora.
- La vía rápida cambia eso para los casos pesados: un proceso que se come 25% o más durante 2 s se regula en ~1,8 s. El hog de 2 hilos de esta prueba no llega a ese umbral (12,5%), así que usa la vía lenta.
- El motor de Lasso pesa 20,0 MB y consume 0,19% de CPU. El de Domador, 17,3 MB y 0,12%, gracias a un único snapshot de `NtQuerySystemInformation` por tick en lugar de recorrer los ~500 procesos con `sysinfo`.
- La UI de Domador no es un WebView2 embebido: es una pestaña de Edge. Sumar los procesos de Edge al motor sería medir el navegador, no la app.
- No hay FPS todavía. El próximo benchmark es con CapFrameX, mismo juego y misma escena, con y sin perfil Juego.

## Reproducir

```bash
cargo build --release
python bench/bench.py
```

`bench.py` requiere Python 3.11+ y `psutil`; usa `bench/domador-hog.exe` y `bench/lasso-results.json`, así que no necesita tener Lasso instalado para recalcular la tabla, sólo para volver a medirlo.
