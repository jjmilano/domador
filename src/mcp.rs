use std::io::{self, BufRead, Write};

use serde_json::{json, Value};

const DEFAULT_PORT: u16 = 8787;

pub fn run(port: u16) -> Result<(), String> {
    let port = if port == 0 {
        std::env::var("DOMADOR_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_PORT)
    } else {
        port
    };
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(err) => {
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": {"code": -32700, "message": format!("JSON inválido: {err}")}
                });
                let _ = writeln!(stdout, "{response}");
                let _ = stdout.flush();
                continue;
            }
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(|v| v.as_str()).unwrap_or("");
        if method.starts_with("notifications/") {
            continue;
        }
        let response = match method {
            "initialize" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "domador", "version": crate::model::VERSION}
                }
            }),
            "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            "tools/list" => json!({"jsonrpc": "2.0", "id": id, "result": {"tools": tools()}}),
            "tools/call" => {
                let name = request
                    .pointer("/params/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or(json!({}));
                match call_tool(port, name, &args) {
                    Ok(value) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{"type": "text", "text": serde_json::to_string_pretty(&value).unwrap_or_default()}],
                            "isError": false
                        }
                    }),
                    Err(err) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{"type": "text", "text": err}],
                            "isError": true
                        }
                    }),
                }
            }
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": format!("método desconocido: {method}")}
            }),
        };
        let _ = writeln!(stdout, "{response}");
        let _ = stdout.flush();
    }
    Ok(())
}

fn tools() -> Value {
    json!([
        {
            "name": "domador_status",
            "description": "Estado general de Domador: perfil activo, plan de energía, CPU/RAM y procesos top.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "domador_processes",
            "description": "Lista procesos con CPU, RAM, prioridad y afinidad.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {"type": "number", "description": "Máximo de procesos (default 20)"},
                    "filter": {"type": "string", "description": "Texto a buscar en nombre o ruta"}
                }
            }
        },
        {
            "name": "domador_set_priority",
            "description": "Cambia la prioridad de un proceso.",
            "inputSchema": {
                "type": "object",
                "required": ["pid", "priority"],
                "properties": {
                    "pid": {"type": "number"},
                    "priority": {"type": "string", "enum": ["idle", "below_normal", "normal", "above_normal", "high", "realtime"]}
                }
            }
        },
        {
            "name": "domador_set_affinity",
            "description": "Fija los núcleos de CPU de un proceso.",
            "inputSchema": {
                "type": "object",
                "required": ["pid", "cpus"],
                "properties": {
                    "pid": {"type": "number"},
                    "cpus": {"type": "array", "items": {"type": "number"}}
                }
            }
        },
        {
            "name": "domador_set_eco",
            "description": "Activa o desactiva el modo eficiencia (EcoQoS).",
            "inputSchema": {
                "type": "object",
                "required": ["pid", "enabled"],
                "properties": {"pid": {"type": "number"}, "enabled": {"type": "boolean"}}
            }
        },
        {
            "name": "domador_profile",
            "description": "Activa un perfil: full_power, gaming, ai o balanced.",
            "inputSchema": {
                "type": "object",
                "required": ["name"],
                "properties": {
                    "name": {"type": "string", "enum": ["full_power", "gaming", "ai", "balanced"]},
                    "target": {"type": "string", "description": "Ejecutable del juego. Obligatorio para gaming; \"auto\" usa la última app en primer plano que no sea Domador"}
                }
            }
        },
        {
            "name": "domador_add_rule",
            "description": "Crea una regla persistente por ejecutable.",
            "inputSchema": {
                "type": "object",
                "required": ["pattern"],
                "properties": {
                    "pattern": {"type": "string"},
                    "priority": {"type": "string"},
                    "affinity": {"type": "array", "items": {"type": "number"}},
                    "eco": {"type": "boolean"}
                }
            }
        },
        {
            "name": "domador_restore",
            "description": "Restaura los valores originales de un proceso o de todos.",
            "inputSchema": {
                "type": "object",
                "properties": {"pid": {"type": "number"}, "all": {"type": "boolean"}}
            }
        },
        {
            "name": "domador_power_plans",
            "description": "Lista los planes de energía y el activo.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "domador_mem_info",
            "description": "Presión de memoria (commit, RAM libre, nivel de MemGuard), composición y los procesos que más memoria privada usan.",
            "inputSchema": {
                "type": "object",
                "properties": {"top": {"type": "number", "description": "Cuántos procesos listar (default 10)"}}
            }
        },
        {
            "name": "domador_trim_idle",
            "description": "Alivio conservador: recorta el working set de hasta 10 procesos de fondo sin ventana y con CPU baja. No cierra nada. Devuelve bytes liberados.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "domador_families",
            "description": "Agrupa Chrome/Edge, WebView2 y Electron: cuántos procesos, CPU y memoria suman. Con family+action aplica en grupo: trim, eco (enabled), priority (cls) o restore.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "family": {"type": "string", "description": "Clave de familia, ej. chromium, webview2, electron"},
                    "action": {"type": "string", "enum": ["trim", "eco", "priority", "restore"]},
                    "enabled": {"type": "boolean"},
                    "cls": {"type": "string"}
                }
            }
        },
        {
            "name": "domador_log",
            "description": "Últimos eventos del motor (ProBalanza, reglas, perfiles).",
            "inputSchema": {
                "type": "object",
                "properties": {"limit": {"type": "number"}}
            }
        }
    ])
}

fn call_tool(port: u16, name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "domador_status" => {
            let state = get(port, "/api/state")?;
            let processes = state
                .get("processes")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let top: Vec<Value> = processes
                .iter()
                .take(10)
                .map(|p| {
                    json!({
                        "pid": p.get("pid"),
                        "name": p.get("name"),
                        "cpu": p.get("cpu"),
                        "ram_mb": p.get("rss").and_then(|v| v.as_u64()).map(|b| b / 1048576),
                        "priority": p.get("prio_label"),
                        "eco": p.get("eco"),
                    })
                })
                .collect();
            Ok(json!({
                "profile": state.get("profile"),
                "plan": state.pointer("/power/active_name"),
                "ac_line": state.pointer("/power/ac_line"),
                "counts": state.get("counts"),
                "top": top,
            }))
        }
        "domador_processes" => {
            let state = get(port, "/api/state")?;
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(20)
                .min(200) as usize;
            let filter = args
                .get("filter")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let processes = state
                .get("processes")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let rows: Vec<Value> = processes
                .into_iter()
                .filter(|p| {
                    filter.is_empty()
                        || p.get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&filter)
                        || p.get("exe")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&filter)
                })
                .take(limit)
                .map(|p| {
                    json!({
                        "pid": p.get("pid"),
                        "name": p.get("name"),
                        "exe": p.get("exe"),
                        "cpu": p.get("cpu"),
                        "ram_mb": p.get("rss").and_then(|v| v.as_u64()).map(|b| b / 1048576),
                        "priority": p.get("prio_label"),
                        "affinity": p.get("affinity_label"),
                        "eco": p.get("eco"),
                    })
                })
                .collect();
            Ok(json!({"count": rows.len(), "processes": rows}))
        }
        "domador_set_priority" => {
            let pid = pid_arg(args)?;
            let priority = str_arg(args, "priority")?;
            post(port, "/api/priority", json!({"pid": pid, "cls": priority}))
        }
        "domador_set_affinity" => {
            let pid = pid_arg(args)?;
            let cpus = args
                .get("cpus")
                .and_then(|v| v.as_array())
                .ok_or("Falta cpus")?;
            post(port, "/api/affinity", json!({"pid": pid, "cpus": cpus}))
        }
        "domador_set_eco" => {
            let pid = pid_arg(args)?;
            let enabled = args
                .get("enabled")
                .and_then(|v| v.as_bool())
                .ok_or("Falta enabled")?;
            post(port, "/api/eco", json!({"pid": pid, "enabled": enabled}))
        }
        "domador_profile" => {
            let name = str_arg(args, "name")?;
            post(
                port,
                "/api/profile",
                json!({"name": name, "target": args.get("target")}),
            )
        }
        "domador_add_rule" => {
            let rule = json!({
                "pattern": str_arg(args, "pattern")?,
                "priority": args.get("priority"),
                "affinity": args.get("affinity"),
                "eco": args.get("eco"),
            });
            post(port, "/api/rules", json!({"op": "create", "rule": rule}))
        }
        "domador_restore" => {
            if args.get("all").and_then(|v| v.as_bool()).unwrap_or(false) {
                post(port, "/api/restore-all", json!({}))
            } else {
                let pid = pid_arg(args)?;
                post(port, "/api/restore", json!({"pid": pid}))
            }
        }
        "domador_power_plans" => {
            let state = get(port, "/api/state")?;
            Ok(state.get("power").cloned().unwrap_or(json!({})))
        }
        "domador_mem_info" => {
            let top = args.get("top").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let mut mem = get(port, "/api/mem")?;
            if let Some(list) = mem.get_mut("top").and_then(|v| v.as_array_mut()) {
                list.truncate(top);
            }
            Ok(mem)
        }
        "domador_trim_idle" => post(port, "/api/trim-idle", json!({})),
        "domador_families" => match args.get("action").and_then(|v| v.as_str()) {
            Some(action) => {
                let family = str_arg(args, "family")?;
                let mut body = json!({"family": family, "action": action});
                for key in ["enabled", "cls"] {
                    if let Some(value) = args.get(key) {
                        body[key] = value.clone();
                    }
                }
                post(port, "/api/family", body)
            }
            None => get(port, "/api/families"),
        },
        "domador_log" => {
            let state = get(port, "/api/state")?;
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(30) as usize;
            let log = state
                .get("log")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let start = log.len().saturating_sub(limit);
            Ok(json!({"log": &log[start..]}))
        }
        other => Err(format!("herramienta desconocida: {other}")),
    }
}

fn pid_arg(args: &Value) -> Result<u64, String> {
    args.get("pid")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "Falta pid".to_string())
}

fn str_arg(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("Falta {key}"))
}

fn base(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

fn get(port: u16, path: &str) -> Result<Value, String> {
    let response = ureq::get(&format!("{}{path}", base(port)))
        .timeout(std::time::Duration::from_secs(5))
        .call()
        .map_err(|err| format!("no pude conectar con Domador en el puerto {port}: {err}"))?;
    response
        .into_json()
        .map_err(|err| format!("respuesta inválida: {err}"))
}

fn post(port: u16, path: &str, body: Value) -> Result<Value, String> {
    let result = ureq::post(&format!("{}{path}", base(port)))
        .timeout(std::time::Duration::from_secs(5))
        .set("X-Domador", "1")
        .set("Content-Type", "application/json")
        .send_json(body);
    match result {
        Ok(response) => response
            .into_json()
            .map_err(|err| format!("respuesta inválida: {err}")),
        Err(ureq::Error::Status(code, response)) => {
            let text = response.into_string().unwrap_or_default();
            Err(format!("HTTP {code}: {text}"))
        }
        Err(err) => Err(format!("no pude conectar con Domador: {err}")),
    }
}
