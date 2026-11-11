# Contribuir a Domador

Gracias por el interés. Esto es corto a propósito.

## Antes de escribir código

- Para bugs: abrí un issue con la versión de Windows, la de Domador, qué esperabas y qué pasó. Si podés, el log de actividad del motor (`/api/state` → `log`) o una captura de la pestaña Actividad.
- Para features: abrí un issue primero y contá el caso de uso. Domador tiene una línea editorial clara — bajo consumo, sin telemetría, sin dependencias pesadas — y no todo entra.

## Entorno

- Windows 10/11 x64.
- Rust stable (`rustup default stable`). El proyecto usa edition 2021.
- `cargo fmt` y `cargo clippy` antes de mandar el PR.

```bash
cargo build            # debug
cargo build --release  # binario final
cargo test
cargo fmt --all
cargo clippy --all-targets -- -D warnings
```

## Reglas del código

- **`unsafe` sólo en `src/win.rs`.** El resto del código trabaja con abstracciones seguras. Si necesitás una llamada nueva a Win32, va ahí con su wrapper.
- **Todo handle se cierra.** Usá guards; nada de `CloseHandle` suelto en un camino de error.
- **Errores con contexto**: `Result<T, String>` con mensaje en español, sin `unwrap()` en rutas de runtime (tests sí).
- **Nada de telemetría ni red saliente.** El server escucha en loopback; no se agregan llamadas a internet.
- **Cambios de proceso reversibles**: si tocás prioridad, afinidad o EcoQoS, guardá el valor original para poder restaurarlo.
- **La UI no se compila**: HTML/CSS/JS plano en `ui/`. Sin frameworks, sin bundler, sin `node_modules`.

## Commits y PRs

- Commits chicos, en español o inglés, con el qué y el porqué: `engine: agregar cooldown tras restaurar prioridad`.
- Un PR = un tema. Si tocaste el motor y de paso reacomodaste la UI, separalo.
- El PR tiene que pasar CI (build release, tests, clippy informativo).
- Describí cómo lo probaste. "Compila" no es una prueba: si cambiaste el motor, contá en qué proceso lo viste funcionar.
- Si el cambio afecta el comportamiento por defecto (ProBalance, perfiles), actualizá el README en el mismo PR.

## Benchmark

No se aceptan números de benchmark sin el script y la metodología en el PR. Se corre en la misma máquina, alternando A/B, y se reporta mediana y 1% low. Los resultados dudosos se revierten.

## Licencia

Al mandar un PR aceptás que tu aporte se publique bajo la licencia MIT del proyecto.
