use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::engine::Engine;
use crate::win::{self, AdminTaskStatus};

type Shared = Arc<Mutex<Engine>>;

/// Pedidos simultáneos máximos. Por encima de esto se atiende en el hilo del
/// accept loop: la API no se cae por una ráfaga, pero tampoco crea threads sin
/// límite.
const MAX_INFLIGHT: usize = 8;

pub fn serve(engine: Shared, port: u16) -> Result<u16, String> {
    let mut chosen = port;
    let server = loop {
        match Server::http(("127.0.0.1", chosen)) {
            Ok(server) => break server,
            Err(err) => {
                if port != 0 && chosen < port.saturating_add(10) {
                    chosen += 1;
                    continue;
                }
                return Err(format!("no pude abrir el puerto {chosen}: {err}"));
            }
        }
    };
    let actual = server
        .server_addr()
        .to_ip()
        .map(|addr| addr.port())
        .unwrap_or(chosen);

    let inflight = Arc::new(AtomicUsize::new(0));
    thread::spawn(move || {
        for request in server.incoming_requests() {
            let engine = engine.clone();
            let slot = Inflight::acquire(&inflight);
            if inflight.load(Ordering::SeqCst) > MAX_INFLIGHT {
                // Sin cupo: se atiende acá mismo (más lento, pero sin crecer).
                handle(request, engine);
                drop(slot);
                continue;
            }
            thread::spawn(move || {
                let _slot = slot;
                handle(request, engine);
            });
        }
    });

    Ok(actual)
}

/// Cupo de requests concurrentes; se libera al terminar el handler, incluso si
/// entra en pánico.
struct Inflight(Arc<AtomicUsize>);

impl Inflight {
    fn acquire(counter: &Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(counter.clone())
    }
}

impl Drop for Inflight {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

const MAX_BODY: usize = 64 * 1024;

/// Error con status HTTP explícito: no todo error de cliente es 400. Un
/// endpoint que no existe es 404 y un método equivocado es 405.
struct ApiError {
    code: u16,
    message: String,
    /// Sólo lo usa Legion: distingue "la máquina no expone WMI" de un error
    /// genérico, sin inventar un 500 para una notebook que no es Legion.
    available: Option<bool>,
}

impl ApiError {
    fn new(code: u16, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            available: None,
        }
    }

    fn with_available(mut self, available: bool) -> Self {
        self.available = Some(available);
        self
    }

    fn bad(message: impl Into<String>) -> Self {
        Self::new(400, message)
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self::new(403, message)
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::new(404, message)
    }

    /// 410: el recurso existía y dejó de existir durante el pedido. Distinto de
    /// 404 para que la UI pueda reintentar con otro PID sin decir "no existe".
    fn gone(message: impl Into<String>) -> Self {
        Self::new(410, message)
    }
}

impl From<String> for ApiError {
    fn from(message: String) -> Self {
        Self::bad(message)
    }
}

impl From<&str> for ApiError {
    fn from(message: &str) -> Self {
        Self::bad(message)
    }
}

fn handle(mut request: Request, engine: Shared) {
    let method = request.method().clone();
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or("/").to_string();

    // Sólo aceptar Host loopback: corta DNS rebinding contra la API local.
    let host_ok = request
        .headers()
        .iter()
        .find(|header| header.field.equiv("Host"))
        .map(|header| {
            let host = header.value.as_str().trim().to_ascii_lowercase();
            host == "127.0.0.1"
                || host == "localhost"
                || host == "[::1]"
                || host.starts_with("127.0.0.1:")
                || host.starts_with("localhost:")
                || host.starts_with("[::1]:")
        })
        .unwrap_or(false);
    if !host_ok {
        return reply(
            request,
            403,
            &json!({"ok": false, "error": "Host no permitido"}),
        );
    }

    if method == Method::Get {
        match path.as_str() {
            "/api/health" => {
                return reply(
                    request,
                    200,
                    &json!({"ok": true, "app": "domador", "version": crate::model::VERSION}),
                )
            }
            "/api/state" => {
                let snapshot = lock(&engine).state_json();
                return reply(request, 200, &snapshot);
            }
            "/api/mem" => {
                let report = lock(&engine).mem_report();
                return reply(request, 200, &report);
            }
            "/api/families" => {
                let families = lock(&engine).families();
                return reply(request, 200, &json!({"ok": true, "families": families}));
            }
            // Estado Legion: siempre 200, incluso sin admin o sin WMI. El
            // contrato vive en docs/LEGION-API.md.
            "/api/legion" => {
                crate::telemetry::ensure_started();
                let mut status = crate::legion::status();
                if status
                    .pointer("/temps/cpu")
                    .map(|v| v.is_null())
                    .unwrap_or(true)
                {
                    if let Some(temp) = crate::cputemp::read() {
                        status["temps"]["cpu"] = json!((temp * 10.0).round() / 10.0);
                    }
                }
                status["cpu_temp"] = crate::cputemp::source();
                return reply(request, 200, &status);
            }
            // Historial de telemetría (ring de 24 h). Nunca 500: si WMI no
            // está, responde 200 con samples vacíos o con nulls.
            "/api/legion/probe" => {
                return reply(request, 200, &crate::legion::probe());
            }
            "/api/legion/history" => {
                return match crate::telemetry::history(&url) {
                    Ok(value) => reply(request, 200, &value),
                    Err(message) => reply(request, 400, &json!({"ok": false, "error": message})),
                }
            }
            "/api/legion/export.csv" => {
                return match crate::telemetry::export_csv(&url) {
                    Ok(csv) => csv_file(request, csv, "domador-telemetry.csv"),
                    Err(message) => reply(request, 400, &json!({"ok": false, "error": message})),
                }
            }
            // Control adaptativo: la lectura no exige admin; POST valida y
            // nunca escribe en el EC sin admin/WMI.
            "/api/legion/adaptive" => {
                crate::telemetry::ensure_started();
                return reply(request, 200, &crate::adaptive::status());
            }
            "/api/admin-task" => {
                let admin = win::is_admin();
                let status = cached_admin_status();
                let job = job_snapshot();
                return reply(
                    request,
                    200,
                    &json!({
                        "ok": true,
                        "admin": admin,
                        "task": status.to_json(),
                        "job": job,
                    }),
                );
            }
            // Ficha de un solo proceso, bajo demanda: la UI la pide al abrir el
            // modal, nunca la incluye el snapshot.
            "/api/process" => {
                return match process_detail_response(&url, &engine) {
                    Ok(value) => reply(request, 200, &value),
                    Err(err) => reply(
                        request,
                        err.code,
                        &json!({"ok": false, "error": err.message}),
                    ),
                }
            }
            "/favicon.ico" => {
                return binary_file(
                    request,
                    include_bytes!("../assets/domador.ico"),
                    "image/x-icon",
                )
            }
            "/icon-32.png" => {
                return binary_file(
                    request,
                    include_bytes!("../assets/icon-32.png"),
                    "image/png",
                )
            }
            "/icon-64.png" => {
                return binary_file(
                    request,
                    include_bytes!("../assets/icon-64.png"),
                    "image/png",
                )
            }
            "/icon-256.png" => {
                return binary_file(
                    request,
                    include_bytes!("../assets/icon-256.png"),
                    "image/png",
                )
            }
            _ => {
                return reply(
                    request,
                    404,
                    &json!({"ok": false, "error": "no encontrado"}),
                )
            }
        }
    }

    if method != Method::Post {
        return reply(
            request,
            405,
            &json!({"ok": false, "error": "método no permitido"}),
        );
    }

    // POST sobre un endpoint que sólo lee: 405 antes de pedir el encabezado,
    // que es lo que corresponde semánticamente.
    if is_get_only(&path) {
        return reply(
            request,
            405,
            &json!({"ok": false, "error": "método no permitido"}),
        );
    }

    let has_header = request
        .headers()
        .iter()
        .any(|header| header.field.equiv("X-Domador") && header.value.as_str().trim() == "1");
    if !has_header {
        return reply(
            request,
            403,
            &json!({"ok": false, "error": "falta el encabezado X-Domador"}),
        );
    }

    if request
        .body_length()
        .map(|length| length > MAX_BODY)
        .unwrap_or(false)
    {
        return reply(
            request,
            413,
            &json!({"ok": false, "error": "cuerpo demasiado grande"}),
        );
    }

    // Tope también para cuerpos sin Content-Length (chunked): se lee un byte de
    // más y si sobra es 413, en vez de truncar y devolver un JSON roto.
    let mut bytes = Vec::new();
    let _ = request
        .as_reader()
        .take((MAX_BODY + 1) as u64)
        .read_to_end(&mut bytes);
    if bytes.len() > MAX_BODY {
        return reply(
            request,
            413,
            &json!({"ok": false, "error": "cuerpo demasiado grande"}),
        );
    }
    let body = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => {
            return reply(
                request,
                400,
                &json!({"ok": false, "error": "el cuerpo no es UTF-8 válido"}),
            )
        }
    };
    let payload: Value = if body.trim().is_empty() {
        json!({})
    } else {
        match serde_json::from_str(&body) {
            Ok(value) => value,
            Err(_) => {
                return reply(
                    request,
                    400,
                    &json!({"ok": false, "error": "JSON inválido"}),
                )
            }
        }
    };

    let result = dispatch(&path, &payload, &engine);
    match result {
        Ok(value) => reply(request, 200, &value),
        Err(err) => {
            let mut body = json!({"ok": false, "error": err.message});
            if let (Some(available), Some(map)) = (err.available, body.as_object_mut()) {
                map.insert("available".into(), json!(available));
            }
            reply(request, err.code, &body)
        }
    }
}

fn is_get_only(path: &str) -> bool {
    matches!(
        path,
        "/api/health"
            | "/api/state"
            | "/api/mem"
            | "/api/families"
            | "/api/legion"
            | "/api/legion/probe"
            | "/api/legion/history"
            | "/api/legion/export.csv"
            | "/api/process"
            | "/favicon.ico"
            | "/icon-32.png"
            | "/icon-64.png"
            | "/icon-256.png"
    )
}

fn dispatch(path: &str, body: &Value, engine: &Shared) -> Result<Value, ApiError> {
    match path {
        "/api/priority" => {
            let pid = pid_of(body)?;
            let cls = str_of(body, "cls")?;
            lock(engine).set_priority(pid, &cls)?;
            Ok(json!({"ok": true}))
        }
        "/api/affinity" => {
            let pid = pid_of(body)?;
            let cpus: Vec<u32> = body
                .get("cpus")
                .and_then(|v| v.as_array())
                .ok_or("Falta cpus")?
                .iter()
                .map(|v| {
                    v.as_u64()
                        .and_then(|n| u32::try_from(n).ok())
                        .ok_or("cpus debe ser una lista de enteros")
                })
                .collect::<Result<_, _>>()?;
            lock(engine).set_affinity(pid, &cpus)?;
            Ok(json!({"ok": true, "cpus": cpus}))
        }
        "/api/eco" => {
            let pid = pid_of(body)?;
            let enabled = body
                .get("enabled")
                .and_then(|v| v.as_bool())
                .ok_or("Falta enabled")?;
            lock(engine).set_eco(pid, enabled)?;
            Ok(json!({"ok": true, "enabled": enabled}))
        }
        "/api/io" => {
            let pid = pid_of(body)?;
            let level = str_of(body, "level")?;
            lock(engine).set_io(pid, &level)?;
            Ok(json!({"ok": true}))
        }
        "/api/memory-priority" => {
            let pid = pid_of(body)?;
            let level = str_of(body, "level")?;
            lock(engine).set_memory_priority(pid, &level)?;
            Ok(json!({"ok": true}))
        }
        "/api/restore" => {
            let pid = pid_of(body)?;
            lock(engine).restore(pid)?;
            Ok(json!({"ok": true}))
        }
        "/api/restore-all" => {
            let restored = lock(engine).restore_all();
            Ok(json!({"ok": true, "restored": restored}))
        }
        "/api/trim" => {
            let pid = pid_of(body)?;
            let freed = lock(engine).trim(pid)?;
            Ok(json!({"ok": true, "freed": freed}))
        }
        "/api/gpu-preference" => {
            let pid = pid_of(body)?;
            let mode = str_of(body, "mode")?;
            let exe = crate::win::process_exe(pid)
                .ok_or_else(|| "no pude resolver el ejecutable".to_string())?;
            crate::gpu::set_preference(&exe, &mode)?;
            Ok(json!({"ok": true, "exe": exe, "mode": mode}))
        }
        "/api/runaway" => {
            let cfg = lock(engine).set_runaway(body)?;
            Ok(json!({"ok": true, "runaway": cfg}))
        }
        // Los trims eligen candidatos con el lock y liberan memoria sin él:
        // antes un alivio de ~450 ms congelaba /api/state (QA A6).
        "/api/trim-all" => {
            let targets = lock(engine).trim_all_targets();
            let (freed, count) = crate::engine::trim_pids(&targets);
            lock(engine).record_trim("all", freed, count);
            Ok(json!({"ok": true, "freed": freed, "count": count}))
        }
        "/api/trim-idle" => {
            let targets = lock(engine).background_targets();
            let (freed, count) = crate::engine::trim_pids(&targets);
            lock(engine).record_trim("idle", freed, count);
            Ok(json!({"ok": true, "freed": freed, "count": count}))
        }
        "/api/mem/relief" => {
            let targets = lock(engine).background_targets();
            let (freed, count) = crate::engine::trim_pids(&targets);
            lock(engine).record_trim("relief", freed, count);
            Ok(json!({"ok": true, "freed": freed, "count": count}))
        }
        "/api/memguard" => {
            let cfg = lock(engine).set_memguard(body)?;
            Ok(json!({"ok": true, "memguard": cfg}))
        }
        "/api/family" | "/api/family-action" => {
            let family = str_of(body, "family")?;
            let action = body
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("trim")
                .to_string();
            Ok(lock(engine).family_action(&family, &action, body)?)
        }
        "/api/kill" => {
            let pid = pid_of(body)?;
            lock(engine).kill(pid)?;
            Ok(json!({"ok": true}))
        }
        "/api/meta" => {
            let pid = pid_of(body)?;
            let meta = lock(engine).process_meta(pid)?;
            Ok(
                json!({"ok": true, "pid": meta.pid, "ct": meta.ct, "nice": meta.nice,
                       "prio": meta.prio, "prio_label": meta.prio_label,
                       "affinity": meta.affinity, "affinity_label": meta.affinity_label,
                       "eco": meta.eco, "exe": meta.exe}),
            )
        }
        "/api/rules" => {
            let op = body.get("op").and_then(|v| v.as_str()).unwrap_or("create");
            match op {
                "create" => {
                    let rule = lock(engine).add_rule(body.get("rule").unwrap_or(&json!({})))?;
                    Ok(json!({"ok": true, "rule": rule}))
                }
                "update" => {
                    let id = str_of(body, "id")?;
                    let rule =
                        lock(engine).update_rule(&id, body.get("rule").unwrap_or(&json!({})))?;
                    Ok(json!({"ok": true, "rule": rule}))
                }
                "delete" => {
                    let id = str_of(body, "id")?;
                    lock(engine).delete_rule(&id)?;
                    Ok(json!({"ok": true}))
                }
                "toggle" => {
                    let id = str_of(body, "id")?;
                    let enabled = body.get("enabled").and_then(|v| v.as_bool());
                    let rule = lock(engine).toggle_rule(&id, enabled)?;
                    Ok(json!({"ok": true, "rule": rule}))
                }
                "apply" => {
                    let id = str_of(body, "id")?;
                    let count = lock(engine).apply_rule_now(&id)?;
                    Ok(json!({"ok": true, "count": count}))
                }
                other => Err(ApiError::bad(format!("operación desconocida: {other}"))),
            }
        }
        "/api/probalance" => {
            let cfg = lock(engine).set_probalance(body)?;
            Ok(json!({"ok": true, "probalance": cfg}))
        }
        "/api/settings/trim" => {
            let cfg = lock(engine).set_trim(body)?;
            Ok(json!({"ok": true, "trim": cfg}))
        }
        "/api/settings/foreground" => {
            let cfg = lock(engine).set_foreground(body)?;
            Ok(json!({"ok": true, "foreground": cfg}))
        }
        "/api/power" => {
            let cfg = lock(engine).set_power(body)?;
            Ok(json!({"ok": true, "power": cfg}))
        }
        "/api/plan" => {
            let guid = str_of(body, "guid")?;
            lock(engine).set_plan(&guid)?;
            Ok(json!({"ok": true}))
        }
        "/api/profile" => {
            let name = str_of(body, "name")?;
            let target = body
                .get("target")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let profile = lock(engine).set_profile(&name, target)?;
            Ok(json!({"ok": true, "profile": profile}))
        }
        "/api/shutdown" => {
            // `close_all`: para actualizar sin pasos manuales. El motor corre
            // elevado, así que puede cerrar la UI y la bandeja elevadas (un
            // proceso sin admin no puede).
            let close_all = body
                .get("close_all")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if close_all {
                let own = std::process::id();
                let targets: Vec<u32> = lock(engine)
                    .rows
                    .iter()
                    .filter(|r| r.pid != own)
                    .filter(|r| {
                        matches!(
                            r.name.to_ascii_lowercase().as_str(),
                            "domador-ui.exe" | "domador-tray.exe"
                        )
                    })
                    .map(|r| r.pid)
                    .collect();
                for pid in targets {
                    let _ = crate::win::terminate_process(pid);
                }
            }
            lock(engine).shutdown();
            thread::spawn(|| {
                thread::sleep(std::time::Duration::from_millis(150));
                std::process::exit(0);
            });
            Ok(json!({"ok": true}))
        }
        // Legion: cada escritura exige admin y hace backup antes de tocar el EC.
        "/api/legion/thermal" => crate::legion::set_thermal(body).map_err(legion_error),
        "/api/legion/fans" => crate::legion::set_fans(body).map_err(legion_error),
        "/api/legion/cpu" => crate::legion::set_cpu(body).map_err(legion_error),
        "/api/legion/chief" => crate::legion::set_chief(body).map_err(legion_error),
        "/api/legion/restore" => crate::legion::restore().map_err(legion_error),
        // Control adaptativo: configurar/armar/apagar/restaurar. No es GET-only
        // porque acepta POST; la lectura va por el match de GET.
        "/api/legion/adaptive" => crate::adaptive::update(body).map_err(legion_error),
        // La creación de la tarea corre en segundo plano: PowerShell puede
        // tardar y antes bloqueaba el accept loop completo.
        "/api/admin-task" => start_admin_job(engine),
        other => Err(ApiError::not_found(format!(
            "endpoint desconocido: {other}"
        ))),
    }
}

/// Traduce el error de Legion a la API: 403 sin admin, 503 con
/// `available:false` cuando el problema es que no hay WMI.
fn legion_error(err: crate::legion::Error) -> ApiError {
    let mut api = ApiError::new(err.status, err.message);
    if err.status == 503 {
        api = api.with_available(false);
    }
    api
}

// ------------------------------------------------------ ficha de un proceso

/// Tope de vida de una ficha cacheada. Abrir y cerrar el modal repite el mismo
/// PID y `WinVerifyTrust` no es gratis; 20 s es suficiente para que se sienta
/// instantáneo sin mostrar datos viejos.
const DETAIL_TTL: Duration = Duration::from_secs(20);
const DETAIL_CACHE_MAX: usize = 64;

struct CachedDetail {
    stored_at: Instant,
    /// Fecha de creación del proceso: si cambia, el PID se recicló y la ficha
    /// vieja no sirve.
    start_time: f64,
    payload: Value,
}

fn detail_cache() -> &'static Mutex<Vec<(u32, CachedDetail)>> {
    static CACHE: OnceLock<Mutex<Vec<(u32, CachedDetail)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

fn detail_lookup(pid: u32, start_time: f64) -> Option<Value> {
    if start_time <= 0.0 {
        return None;
    }
    let cache = detail_cache();
    let mut guard = cache.lock().unwrap_or_else(|poison| poison.into_inner());
    guard.retain(|(_, entry)| entry.stored_at.elapsed() < DETAIL_TTL);
    let entry = guard.iter_mut().find(|(cached_pid, entry)| {
        *cached_pid == pid && (entry.start_time - start_time).abs() < 1.0
    })?;
    let mut payload = entry.1.payload.clone();
    if let Some(map) = payload.as_object_mut() {
        map.insert("cached".into(), json!(true));
    }
    Some(payload)
}

fn detail_store(pid: u32, start_time: f64, payload: &Value) {
    if start_time <= 0.0 {
        return;
    }
    let cache = detail_cache();
    let mut guard = cache.lock().unwrap_or_else(|poison| poison.into_inner());
    guard
        .retain(|(cached_pid, entry)| *cached_pid != pid && entry.stored_at.elapsed() < DETAIL_TTL);
    if guard.len() >= DETAIL_CACHE_MAX {
        guard.remove(0);
    }
    guard.push((
        pid,
        CachedDetail {
            stored_at: Instant::now(),
            start_time,
            payload: payload.clone(),
        },
    ));
}

/// GET /api/process?pid=N.
///
/// El trabajo pesado corre sin el lock del motor: sólo se toma un instante para
/// leer el nombre y la fecha de creación del último tick. Así una consulta
/// lenta (WinVerifyTrust, PowerShell de respaldo) no frena el sampler ni el
/// `/api/state` que la UI pide cada segundo.
fn process_detail_response(url: &str, engine: &Shared) -> Result<Value, ApiError> {
    let raw = query_param(url, "pid").ok_or_else(|| ApiError::bad("Falta pid"))?;
    let pid: u32 = raw
        .trim()
        .parse()
        .map_err(|_| ApiError::bad(format!("pid inválido: {raw}")))?;

    let seed = {
        let guard = lock(engine);
        guard.process_seed(pid)
    };
    let (seed_name, seed_start) = seed.unwrap_or_default();

    // La ficha cacheada se sirve sólo si el PID sigue vivo: un proceso que se
    // cerró no debe seguir respondiendo 200 durante la ventana del cache. El
    // chequeo va sólo en el camino de cache, para no pagarlo en cada consulta.
    if let Some(cached) = detail_lookup(pid, seed_start) {
        if win::process_exists(pid) {
            return Ok(cached);
        }
    }

    let started = Instant::now();
    let detail = win::process_detail(pid, &seed_name, seed_start).map_err(|err| match err {
        win::DetailError::NotFound => ApiError::not_found(format!("no encontré el proceso {pid}")),
        win::DetailError::Vanished => {
            ApiError::gone(format!("el proceso {pid} terminó durante la consulta"))
        }
    })?;
    let elapsed_ms = (started.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0;

    // `description` es siempre la del fabricante (FileDescription). Cuando no
    // hay, `summary` lleva la explicación en español de los procesos conocidos:
    // la UI ya tolera esa clave y así el modal no queda vacío en svchost.
    let summary = if detail.description.is_none() {
        detail.known_description
    } else {
        None
    };

    let payload = json!({
        "ok": true,
        "pid": detail.pid,
        "name": detail.name,
        "path": detail.path,
        "description": detail.description,
        "summary": summary,
        "known_description": detail.known_description,
        "company": detail.company,
        "product": detail.product,
        "version": detail.version,
        "parent_pid": detail.parent_pid,
        "parent": detail.parent,
        "start_time": detail.start_time,
        "window_title": detail.window_title,
        "command_line": detail.command_line,
        "signature": detail.signature,
        "signature_kind": detail.signature_kind,
        "trusted": detail.trusted,
        "signed": detail.signed,
        // `true` cuando el proceso no se dejó abrir: la ficha trae sólo lo que
        // Toolhelp y el snapshot permiten (nombre, padre, fecha, descripción).
        "limited": !detail.opened,
        "cached": false,
        "elapsed_ms": elapsed_ms,
    });
    if let Some(start_time) = detail.start_time {
        detail_store(pid, start_time, &payload);
    }
    Ok(payload)
}

/// Primer valor de un parámetro de query. Sólo se decodifica `%XX` y `+`, que
/// es lo único que puede aparecer en un `pid` numérico.
pub(crate) fn query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        if name == key {
            return Some(percent_decode(value));
        }
    }
    None
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let high = (bytes[index + 1] as char).to_digit(16);
                let low = (bytes[index + 2] as char).to_digit(16);
                match (high, low) {
                    (Some(high), Some(low)) => {
                        out.push((high * 16 + low) as u8);
                        index += 3;
                    }
                    _ => {
                        out.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

// ------------------------------------------------------------ tarea admin

#[derive(Clone, Default)]
struct AdminJob {
    running: bool,
    started_at: f64,
    finished_at: f64,
    ok: bool,
    detail: String,
    error: Option<String>,
    verified: Option<Value>,
}

fn job_lock() -> std::sync::MutexGuard<'static, AdminJob> {
    static JOB: OnceLock<Mutex<AdminJob>> = OnceLock::new();
    JOB.get_or_init(|| {
        Mutex::new(AdminJob {
            detail: "sin ejecuciones".into(),
            ..AdminJob::default()
        })
    })
    .lock()
    .unwrap_or_else(|poison| poison.into_inner())
}

fn job_snapshot() -> Value {
    let job = job_lock();
    json!({
        "running": job.running,
        "ok": job.ok,
        "started_at": job.started_at,
        "finished_at": job.finished_at,
        "detail": job.detail,
        "error": job.error,
        "verified": job.verified,
    })
}

/// Estado real de la tarea programada, cacheado unos segundos para no lanzar
/// PowerShell en cada sondeo de la UI.
fn admin_status_cache() -> &'static Mutex<Option<(Instant, AdminTaskStatus)>> {
    static CACHE: OnceLock<Mutex<Option<(Instant, AdminTaskStatus)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

fn cached_admin_status() -> AdminTaskStatus {
    let cache = admin_status_cache();
    let mut guard = cache.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some((at, status)) = guard.as_ref() {
        if at.elapsed() < Duration::from_secs(30) {
            return status.clone();
        }
    }
    let status = win::admin_task_status();
    *guard = Some((Instant::now(), status.clone()));
    status
}

fn remember_admin_status(status: &AdminTaskStatus) {
    let cache = admin_status_cache();
    let mut guard = cache.lock().unwrap_or_else(|poison| poison.into_inner());
    *guard = Some((Instant::now(), status.clone()));
}

fn start_admin_job(engine: &Shared) -> Result<Value, ApiError> {
    if !win::is_admin() {
        return Err(ApiError::forbidden(
            "necesito correr como administrador para crear la tarea",
        ));
    }
    // Se ejecuta la copia embebida del instalador: no depende de que el .ps1
    // junto al ejecutable esté actualizado ni de una ruta resuelta a mano.
    let script_label = win::find_install_script()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "install-admin.ps1 (embebido)".to_string());
    {
        let mut job = job_lock();
        if job.running {
            return Ok(json!({
                "ok": true,
                "started": false,
                "running": true,
                "detail": job.detail,
                "script": script_label,
            }));
        }
        job.running = true;
        job.started_at = now_epoch();
        job.finished_at = 0.0;
        job.ok = false;
        job.detail = "creando tarea…".into();
        job.error = None;
        job.verified = None;
    }

    let engine = engine.clone();
    thread::spawn(move || {
        let outcome = win::run_embedded_install_script();
        let (script_ok, detail, script_error) = match outcome {
            Ok(output) => {
                let detail = if output.stdout.trim().is_empty() {
                    output.stderr.trim().to_string()
                } else {
                    output.stdout.trim().to_string()
                };
                if output.timed_out {
                    (
                        false,
                        detail,
                        Some("el instalador tardó más de 120 s".to_string()),
                    )
                } else if output.success() {
                    (true, detail, None)
                } else {
                    let from_stdout = output
                        .stdout
                        .lines()
                        .map(str::trim)
                        .find(|line| line.starts_with("ERROR:"))
                        .map(|line| line.to_string());
                    let stderr = win::clean_diagnostic(&output.stderr);
                    let error = from_stdout
                        .or(if stderr.is_empty() {
                            None
                        } else {
                            Some(stderr)
                        })
                        .unwrap_or_else(|| {
                            format!(
                                "el instalador salió con código {}",
                                output.code.unwrap_or(-1)
                            )
                        });
                    (false, detail, Some(error))
                }
            }
            Err(err) => (false, String::new(), Some(err)),
        };

        // Verificación independiente: el script puede decir OK y la tarea no
        // estar elevada, o al revés.
        let verified = win::admin_task_status();
        remember_admin_status(&verified);
        let verified_ok = verified.elevated();
        let ok = script_ok && verified_ok;
        let error = script_error.or_else(|| {
            if verified_ok {
                return None;
            }
            Some(if verified.installed {
                format!(
                    "la tarea existe pero RunLevel={} (se esperaba HighestAvailable)",
                    if verified.run_level.is_empty() {
                        "desconocido"
                    } else {
                        &verified.run_level
                    }
                )
            } else {
                "la tarea no quedó registrada".to_string()
            })
        });

        {
            let mut guard = lock(&engine);
            if ok {
                guard.log(
                    "info",
                    format!(
                        "tarea admin verificada · state={} · runlevel={}",
                        verified.state, verified.run_level
                    ),
                );
            } else {
                let why = error.clone().unwrap_or_else(|| "desconocido".into());
                guard.log("warn", format!("tarea admin sin verificar: {why}"));
            }
        }

        let mut job = job_lock();
        job.running = false;
        job.finished_at = now_epoch();
        job.ok = ok;
        job.detail = detail;
        job.error = error;
        job.verified = Some(verified.to_json());
    });

    Ok(json!({
        "ok": true,
        "started": true,
        "running": true,
        "detail": "creando tarea…",
        "script": script_label,
    }))
}

fn now_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn lock(engine: &Shared) -> std::sync::MutexGuard<'_, Engine> {
    engine.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn pid_of(body: &Value) -> Result<u32, String> {
    body.get("pid")
        .and_then(|v| v.as_u64())
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| "Falta pid".to_string())
}

fn str_of(body: &Value, key: &str) -> Result<String, String> {
    body.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("Falta {key}"))
}

fn reply<T: Serialize>(request: Request, code: u16, body: &T) {
    let text = serde_json::to_string(body).unwrap_or_else(|_| "{}".into());
    let response = Response::from_string(text)
        .with_status_code(code)
        .with_header(
            Header::from_bytes("Content-Type", "application/json; charset=utf-8")
                .expect("header válido"),
        )
        .with_header(Header::from_bytes("Cache-Control", "no-store").expect("header válido"));
    let _ = request.respond(response);
}

fn binary_file(request: Request, body: &'static [u8], content_type: &str) -> () {
    let response = Response::from_data(body)
        .with_status_code(200)
        .with_header(Header::from_bytes("Content-Type", content_type).expect("header válido"))
        .with_header(Header::from_bytes("Cache-Control", "max-age=86400").expect("header válido"))
        .with_header(
            Header::from_bytes("X-Content-Type-Options", "nosniff").expect("header válido"),
        );
    let _ = request.respond(response);
}

/// CSV de telemetría: texto plano que el navegador/UI descarga tal cual.
fn csv_file(request: Request, body: String, filename: &str) {
    let response = Response::from_data(body.into_bytes())
        .with_status_code(200)
        .with_header(
            Header::from_bytes("Content-Type", "text/csv; charset=utf-8").expect("header válido"),
        )
        .with_header(
            Header::from_bytes(
                "Content-Disposition",
                format!("attachment; filename=\"{filename}\""),
            )
            .expect("header válido"),
        )
        .with_header(Header::from_bytes("Cache-Control", "no-store").expect("header válido"))
        .with_header(
            Header::from_bytes("X-Content-Type-Options", "nosniff").expect("header válido"),
        );
    let _ = request.respond(response);
}
