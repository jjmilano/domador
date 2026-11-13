use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread;

use serde::Serialize;
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::engine::Engine;

type Shared = Arc<Mutex<Engine>>;

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

    thread::spawn(move || {
        for request in server.incoming_requests() {
            handle(request, engine.clone());
        }
    });

    Ok(actual)
}

const MAX_BODY: usize = 64 * 1024;

fn handle(mut request: Request, engine: Shared) {
    let method = request.method().clone();
    let path = request.url().split('?').next().unwrap_or("/").to_string();

    // Sólo aceptar Host loopback: corta DNS rebinding contra la API local.
    let host_ok = request
        .headers()
        .iter()
        .find(|header| header.field.equiv("Host"))
        .map(|header| {
            let host = header.value.as_str().trim().to_ascii_lowercase();
            host == "127.0.0.1"
                || host == "localhost"
                || host.starts_with("127.0.0.1:")
                || host.starts_with("localhost:")
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
                let snapshot = lock(&engine).snapshot();
                return reply(request, 200, &snapshot);
            }
            "/" | "/index.html" => return static_file(request, "index.html"),
            "/styles.css" => return static_file(request, "styles.css"),
            "/app.js" => return static_file(request, "app.js"),
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

    let mut body = String::new();
    let _ = request
        .as_reader()
        .take(MAX_BODY as u64)
        .read_to_string(&mut body);
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
        Err(err) => reply(request, 400, &json!({"ok": false, "error": err})),
    }
}

fn dispatch(path: &str, body: &Value, engine: &Shared) -> Result<Value, String> {
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
                .filter_map(|v| v.as_u64().map(|n| n as u32))
                .collect();
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
        "/api/trim-all" => {
            let (freed, count) = lock(engine).trim_all();
            Ok(json!({"ok": true, "freed": freed, "count": count}))
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
                other => Err(format!("operación desconocida: {other}")),
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
            lock(engine).shutdown();
            thread::spawn(|| {
                thread::sleep(std::time::Duration::from_millis(150));
                std::process::exit(0);
            });
            Ok(json!({"ok": true}))
        }
        other => Err(format!("endpoint desconocido: {other}")),
    }
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

fn static_file(request: Request, name: &str) -> () {
    let (body, content_type) = match name {
        "index.html" => (include_str!("../ui/index.html"), "text/html; charset=utf-8"),
        "styles.css" => (include_str!("../ui/styles.css"), "text/css; charset=utf-8"),
        "app.js" => (
            include_str!("../ui/app.js"),
            "application/javascript; charset=utf-8",
        ),
        _ => ("", "text/plain"),
    };
    let response = Response::from_string(body)
        .with_status_code(200)
        .with_header(Header::from_bytes("Content-Type", content_type).expect("header válido"))
        .with_header(Header::from_bytes("Cache-Control", "no-store").expect("header válido"))
        .with_header(
            Header::from_bytes("X-Content-Type-Options", "nosniff").expect("header válido"),
        );
    let _ = request.respond(response);
}
