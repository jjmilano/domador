# Benchmark: Domador vs Process Lasso

Este directorio tiene el script, la carga de prueba y los resultados crudos. El resumen legible se genera en `report.md`; los números crudos, en `results-rs.json`.

## Archivos

| Archivo | Qué es |
|---|---|
| `bench.py` | Script de medición y generación del reporte |
| `requirements.txt` | `psutil` (única dependencia Python) |
| `domador-hog.exe` | Carga de prueba precompilada (2 hilos, fases 12/6/12 s) |
| `hog.cs` | Fuente del hog |
| `lasso-results.json` | Línea base de Lasso, **cacheada** de la medición original |
| `results-rs.json` | Corrida histórica del motor v0.4.2; se sobrescribe con la próxima corrida completa |
| `results-quick.json` | Smoke de `--quick`; no publicar |
| `report.md` | Tabla y lectura honesta generadas por `bench.py` |

## Equipo de la línea base

- AMD Ryzen 7 5800H (8 núcleos, 16 hilos)
- 31,9 GB de RAM
- NVIDIA GeForce RTX 3070 Laptop GPU
- Windows 11 Home Insider Preview 10.0.26220
- Process Lasso 18.4.0.48, configuración vigente de la máquina
- Domador: motor v0.4.2 para los números históricos; el script actual mide el build que encuentre y registra su versión

## Carga

Dos procesos `bench/domador-hog.exe` con 2 hilos cada uno y fases 12 s activo / 6 s idle / 12 s activo. Se lanzan con prioridad **Above Normal** para que Lasso no los toque con su regla por defecto y para que ambos motores vean la misma carga.

## Qué se mide

| Métrica | Método |
|---|---|
| RAM y CPU del motor | `domador serve` sin UI, muestreo cada 0,5 s durante 20 s, sumando el árbol del proceso |
| RAM y CPU de la UI | `domador-ui.exe` conectada al motor en 8787, mismo muestreo; **sólo con `--ui`** |
| Cambios de prioridad | lectura de la prioridad de cada hog cada 100 ms durante 42 s; se cuentan transiciones, no muestras |
| Primer throttle | primer cambio de Normal/Above a Below/Idle |
| Latencia | 20 `POST /api/priority` alternando `below_normal` y `normal`, verificando el valor real con psutil |
| Churn de Lasso | 30 minutos de su propio log, contando eventos de ProBalance (del JSON cacheado) |

## Configuración de ProBalance

**Lasso:** umbral 5% por proceso / 10% total, 9 s por encima del umbral, restauración a 2% con mínimo 4,2 s, según su `prolasso.ini`.

**Domador:** `threshold=5`, `sustain_s=9`, `restore_below=2`, `restore_s=5`, `cooldown_s=30`, `eco=false`, `skip_foreground=true`, `skip_interactive=false`, protegidos por defecto.

`bench.py` escribe esa configuración en un `config.json` temporal y arranca `domador serve` con `DOMADOR_CONFIG` apuntando ahí. No toca tu configuración real.

## Reproducir

```bash
cargo build --release --bins
python -m pip install -r bench/requirements.txt

python bench/bench.py --check    # valida binarios, hog, psutil y baseline
python bench/bench.py            # corrida completa (~2 min)
python bench/bench.py --quick    # smoke corto; no publicar
python bench/bench.py --ui       # agrega la UI nativa (requiere 8787 libre)
```

Variables opcionales: `DOMADOR_BIN`, `DOMADOR_UI`, `DOMADOR_TRAY`, `DOMADOR_HOG`.

La UI nativa usa el puerto **8787 fijo**. Si está ocupado, `--ui` se saltea y lo deja escrito en el JSON; no inventa la medición.

## Lectura correcta

- Lasso baja la prioridad casi al instante. Por eso su "primer throttle" es 0,1 s, pero también por eso hace **4 transiciones por proceso** en la ventana: baja, restaura, vuelve a bajar y vuelve a restaurar. No es un ProBalance con histéresis; es una reacción agresiva que después corrige.
- Domador espera 9 s de CPU sostenida y aplica un cooldown de 30 s. Hace **2 transiciones por proceso**. La reacción es más lenta a propósito; la estabilidad es la mejora.
- La vía rápida cambia eso para los casos pesados: un proceso que se come 25% o más durante 2 s se regula antes del sustain lento. El hog de 2 hilos de esta prueba no llega a ese umbral (12,5%), así que usa la vía lenta. **No hay un tiempo de vía rápida archivado en `results-rs.json`**; no se cita hasta medirlo.
- El motor de Lasso pesa 20,0 MB y consume 0,19% de CPU. El de Domador, 17,3 MB y 0,12%, gracias a un único snapshot de `NtQuerySystemInformation` por tick en lugar de recorrer los ~500 procesos con `sysinfo`.
- La UI de Domador es una ventana egui/eframe en un **proceso aparte**, no una pestaña de Edge. Sumarla al motor mezcla dos cosas distintas: se mide aparte y se informa aparte.
- No hay FPS todavía. El próximo benchmark es con CapFrameX, mismo juego y misma escena, con y sin perfil Juego.

## Limitaciones

- **Lasso no se automatiza.** `bench.py` lee `lasso-results.json`; no lanza Lasso ni parsea su config. Para una comparación nueva hay que medirlo a mano con la misma carga y configuración, y actualizar el JSON.
- **Una máquina, una corrida.** No hay intervalos de confianza ni repeticiones; los números son indicativos, no una prueba estadística.
- **La RAM "app completa" no es comparable uno a uno**: Lasso incluye una GUI Win32/WPF; Domador separa motor y UI egui.
- **`--quick` no es publicable**: acorta las ventanas y sirve sólo para verificar que el script corre.
- **El puerto de la UI es fijo** (8787) en v0.6.2; si está ocupado, no se mide.
