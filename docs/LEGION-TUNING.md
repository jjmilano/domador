# Legion 5 Pro 16ACH6H — tuning real para Domador

## Hardware exacto de esta máquina

| Dato | Valor |
|---|---|
| Modelo | Lenovo Legion 5 Pro 16ACH6H (MT 82JQ) |
| CPU | AMD Ryzen 7 5800H, 8C/16T |
| GPU | NVIDIA RTX 3070 Laptop, 8 GB |
| BIOS | GKCN65WW (2024-01-15) |
| RAM | 32 GB |
| Límite GPU actual | 140 W (default 115 W, max 140 W) |

Verificado con `Win32_ComputerSystem`, `Win32_BIOS`, `Win32_Processor`, `nvidia-smi` y `root\wmi`.

## Apps que sirven para este modelo

| App | Para qué sirve | Estado en esta máquina |
|---|---|---|
| **Legion Fan Control (LFC) v2.05** | Curva de fans real por modo de energía, RPM, histéresis, aceleración, Turbo (sólo dGPU), restaurar curva stock al salir | El modelo `5 Pro 16ACH6H` figura en la lista oficial de compatibles (BIOS GKCN). Requiere admin. Es la app específica de ventiladores. |
| **Lenovo Legion Toolkit (LLT)** | Modos de energía, Custom Mode, batería, GPU híbrida/dGPU, OSD, automatizaciones | Sirve para el resto, pero **no agrega curva de fans en Gen 6**; LLT dice que el control de fans es Gen 7+. Con BIOS GKCN46+ habilita Custom Mode. |
| **Universal x86 Tuning Utility (UXTU)** | Límites de potencia CPU (STAPM/PPT/TDC), temperatura objetivo, Adaptive TDP | Ya instalada (2.5.5). Sirve en el 5800H para **power limits**, no para undervolt. |
| **Ryzen Controller 2.5.4** | Predecesor de UXTU; límites de potencia/temperatura | Ya instalada. UXTU la reemplaza; no conviene correr las dos. |
| **MSI Afterburner 4.6.5** | Curva de voltaje/frecuencia GPU, offsets de core/memoria, monitoreo | Ya instalado. Es la vía para undervolt/OC de la RTX 3070 Laptop. |
| **Lenovo Vantage / Legion Arena** | Modos térmicos oficiales, batería, actualizaciones | Ya instalados. No dan curva de fans propia; pueden pelear por el EC/WMI con LFC o con Domador. |
| **ThrottleStop** | Undervolt/OC Intel | **No aplica**: esta máquina es AMD. |

La app que recordás para ventiladores es casi seguro **Legion Fan Control**. La de tuning de CPU es **UXTU** (o su predecesora Ryzen Controller).

## Lo que el 5800H sí y no permite

- **No** tiene Curve Optimizer/undervolt por núcleo habilitado en las APU móviles H. UXTU y Ryzen Controller no pueden bajar el voltaje; sólo limitar potencia, tiempo de boost y temperatura.
- **Sí** se pueden cambiar límites de potencia corto/largo plazo y el modo térmico; eso es lo que mueve la aguja en una notebook.
- En juegos GPU-bound, bajar el CPU a ~35–45 W puede dejar más margen térmico y de potencia para la 3070; en cargas CPU-bound baja el rendimiento. Hay que medirlo, no asumirlo.
- El “undervolt” que sí existe es el de la **GPU**, con Afterburner: curva de frecuencia/voltaje y offsets. Es la modificación mínima con mayor impacto en temperaturas y clocks sostenidos, pero depende del silicio.

## LED del botón de encendido

El LED del botón de encendido (el “home button” que ves arriba) lo maneja el firmware según el modo térmico:

- **Quiet** → azul.
- **Balance/Auto** → blanco.
- **Performance** → rojo.
- **Custom** → lo que defina el firmware/EC.

No es un color independiente que haya que setear a mano: al cambiar el modo térmico por WMI, el EC debería cambiar el LED solo. Domador va a leer el estado con `LENOVO_LIGHTING_METHOD` (`Lighting_ID = 4`, `Get_Lighting_Current_Status`) y mostrar el color esperado; si el LED no sigue al modo, va a avisar. No se escriben colores arbitrarios.

## Tweaks con impacto real

1. **Undervolt de GPU con Afterburner**: es el tweak más rentable. La 3070 Laptop ya está en 140 W máximos; no hay “más vatios” por software. Una curva de ~800–850 mV con 1700–1800 MHz y memoria +500 suele bajar temperaturas y sostener clocks; el valor exacto depende del silicio. Probar con Heaven/Time Spy y bajar si hay artifacts.
2. **Límite de CPU por perfil**: en juegos GPU-bound, 35–45 W suelen dejar más margen a la GPU; en cargas CPU-bound, 54–60 W. El 5800H no permite undervolt por núcleo.
3. **Curva de fans**: rampa temprana y agresiva sin llegar a máximo permanente; evita throttling y baja ruido frente a Full Speed constante.
4. **Modo dGPU/MUX**: para jugar, desactivar híbrido y usar sólo la 3070; requiere reinicio. Mejora algunos juegos, sacrifica batería.
5. **NVIDIA/Windows**: plan Legion Performance, “Prefer maximum performance” en el panel NVIDIA, Game Mode activo; no usar “Ultimate Performance” a ciegas.
6. **Cerrar el fondo**: ya limpiamos el arranque; Vantage, UXTU, Chrome/WebView y Electron de fondo son los que más reservan memoria.
7. **Hardware**: base/cooling pad y, si las temperaturas siguen altas, repaste. Eso suele rendir más que cualquier tweak de software.

No hay que tocar BIOS ni desactivar boost globalmente: en juegos suele empeorar los 1% lows. Medir con CapFrameX/Time Spy, misma escena y misma temperatura ambiente.

## Control adaptativo

El modo automático debe ser un supervisor con estados, no un `if` por temperatura:

- **Normal**: temperatura bajo el objetivo; fans en la curva elegida y límites stock.
- **Caliente**: supera `hot_c` durante `dwell_s`; sube fans y, si hace falta, baja el límite de CPU.
- **Crítico**: supera `critical_c`; fans a máxima y CPU limitada; aviso visible.
- Vuelve a Normal sólo si baja `hysteresis_c` por debajo del umbral durante `cooldown_s`.
- Nunca cambia el modo térmico ni la tabla de fans cada 2 segundos; eso produce flapping y desgaste.
- Cada acción se registra con temperatura, regla, valor anterior y resultado. Si no hay admin, no escribe: muestra `limited`.
- El LED del botón sigue el modo térmico; si el modo no cambia, no se fuerza un color.

## Lo que el hardware expone por WMI

En `root\wmi` existen las clases del driver Lenovo:

- `LENOVO_GAMEZONE_DATA`: `GetThermalMode`, `GetFan1Speed`, `GetFan2Speed`, `GetFanMaxSpeed`, `GetCPUTemp`, `GetGPUTemp`, `GetPowerChargeMode`, `GetSmartFanMode`, `SetThermalTableID`.
- `LENOVO_FAN_METHOD`: `Fan_Get_Table`, `Fan_Set_Table`, `Fan_Get_MaxSpeed`, `Fan_Set_MaxSpeed`, `Fan_Get_FullSpeed`, `Fan_Set_FullSpeed`, `Fan_GetCurrentFanSpeed`, `Fan_GetCurrentSensorTemperature`.
- `LENOVO_CPU_METHOD`: `CPU_Get_LongTerm_PowerLimit`, `CPU_Set_LongTerm_PowerLimit`, `CPU_Get_ShortTerm_PowerLimit`, `CPU_Set_ShortTerm_PowerLimit` (con min/max/step que informa el firmware).
- `LENOVO_OTHER_METHOD`: `GetSupportThermalMode`, `GetCustomModeAbility`, `Set_Custom_Mode_Status`, `Get_Legion_Device_Support_Feature`.

Sin admin, Windows devuelve “Access to a CIM resource was not available” para varias de estas clases. Con admin se pueden leer y escribir. No hace falta WinRing0 ni un driver de terceros para el camino WMI; LFC usa EC directo, pero el firmware también expone tablas por WMI.

## Qué agregar a Domador

### Fase 1 — lectura y modos seguros

- Pestaña **Legion** con:
  - modo térmico actual (Quiet/Balanced/Performance/Custom) y botón para cambiarlo;
  - RPM de los dos fans, temperaturas CPU/GPU;
  - límites de potencia CPU actuales, mín/máx y paso;
  - consumo/estado GPU y modo híbrido;
  - batería: conservación, carga rápida.
- Todo por WMI nativo, sin depender de Vantage/LLT/LFC.
- Si no hay admin: mostrar `sin admin` y sólo lectura.

### Fase 2 — CPU y perfiles

- Presets de potencia larga/corta: el valor exacto se lee del firmware, no se hardcodea.
- Perfiles de Domador:
  - `Full Power`: Performance + límites máximos + fans agresivos.
  - `Juego`: dGPU, CPU moderado (35–45 W), GPU máxima; medir FPS.
  - `IA`: Balanced/Quiet, CPU suficiente para tokens, fans bajos.
  - `Equilibrado`: stock.
- Aplicar y restaurar automáticamente al entrar/salir del juego.
- Medir antes/después: Cinebench R23, 3DMark Time Spy, FPS con CapFrameX, temperatura y potencia.

### Fase 3 — curva de fans nativa

- Leer la tabla stock con `Fan_Get_Table` y guardarla **antes** de escribir.
- Editar puntos por modo, con máximo de `Fan_Get_MaxSpeed`.
- Escribir sólo en Custom Mode; restaurar stock al salir, al cambiar de perfil y al apagar.
- Watchdog: si la temperatura sube por encima del umbral o el motor desaparece, volver a stock/full speed.
- Exigir admin y bloquear la escritura si están corriendo Vantage, Legion Arena, LLT o LFC: un solo dueño del EC.
- No prometer que baja temperaturas sin medir; la curva evita throttling, no enfría por arte de magia.

### Fase 4 — GPU

- Leer potencia, temperatura, clocks y límite con `nvidia-smi`/NVML.
- Para undervolt/OC real, integrar NVAPI o abrir Afterburner con un perfil; no duplicar una curva de voltaje sin validación.
- La 3070 Laptop ya está en 140 W máximos; no hay “más vatios” disponibles por software. El margen está en temperatura, curva de voltaje y memoria.

## Reglas de seguridad

1. Nada de escrituras de fan/EC sin backup de la tabla stock y restauración verificada.
2. Un solo programa controlando el EC. Si Vantage/Legion Arena/LLT/LFC está activo, Domador no escribe.
3. La curva nunca supera el máximo que informa el firmware.
4. Si el motor se cierra o crashea, el watchdog debe restaurar stock; si no se puede, avisar y no dejar la máquina en un perfil desconocido.
5. Benchmarks con la misma carga, misma temperatura ambiente y misma versión de BIOS; sin números estimados.

## Referencias

- Legion Fan Control: https://www.legionfancontrol.com/ (modelo y BIOS en la lista de compatibles).
- Lenovo Legion Toolkit: https://github.com/LenovoLegionToolkit-Team/LenovoLegionToolkit (Custom Mode; fans Gen 7+).
- UXTU: https://github.com/JamesCJ60/Universal-x86-Tuning-Utility (5800H: power limits, no undervolt).
- RyzenAdj: https://github.com/FlyGoat/RyzenAdj.
- MSI Afterburner: https://www.msi.com/Landing/afterburner.
