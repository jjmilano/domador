use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use domador::{api, config, engine, mcp, model, win};
use serde_json::{json, Value};

type Shared = Arc<Mutex<domador::engine::Engine>>;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map(|s| s.as_str()).unwrap_or("run");
    let port = port_arg(&args, 8787);

    let result = match command {
        "run" => run_server(port, !args.iter().any(|a| a == "--no-open")),
        "serve" => run_server(port, false),
        "mcp" => {
            let mcp_port = flag(&args, "--port")
                .and_then(|v| v.parse().ok())
                .or_else(|| {
                    std::env::var("DOMADOR_PORT")
                        .ok()
                        .and_then(|v| v.parse().ok())
                })
                .unwrap_or(port);
            mcp::run(mcp_port)
        }
        "status" => cli_status(port),
        "profile" => cli_profile(&args, port),
        "power" => cli_power(&args, port),
        "set" => cli_set(&args, port),
        "stop" => post(port, "/api/shutdown", json!({})).map(|_| println!("Domador detenido")),
        "version" | "--version" | "-V" => {
            println!("Domador {}", model::VERSION);
            Ok(())
        }
        "help" | "--help" | "-h" => {
            usage();
            Ok(())
        }
        other => {
            eprintln!("comando desconocido: {other}");
            usage();
            std::process::exit(2);
        }
    };

    if let Err(err) = result {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run_server(port: u16, open: bool) -> Result<(), String> {
    let config_path = config::default_path();
    // Un solo motor por config: dos motores con el mismo config.json se pisan
    // los cambios al guardar (QA A2). Si ya hay uno, se reusa.
    let _instance = match win::single_instance(&config_path.display().to_string()) {
        Some(guard) => guard,
        None => {
            println!("Domador ya está corriendo con este config; abro la UI contra ese motor.");
            if open {
                open_ui(port, &format!("http://127.0.0.1:{port}/"));
            }
            return Ok(());
        }
    };
    let engine = Arc::new(Mutex::new(engine::Engine::new(config_path.clone())));
    {
        let mut guard = lock(&engine);
        guard.tick();
    }

    // GPU (nvidia-smi) y planes (powercfg) se consultan fuera del lock: antes
    // corrían dentro de /api/state y frenaban a todos los endpoints (QA A1).
    lock(&engine).use_external_probes();
    let probe_engine = engine.clone();
    thread::spawn(move || {
        let mut tick: u64 = 0;
        loop {
            let gpu = domador::gpu::query();
            lock(&probe_engine).set_gpu(gpu);
            if tick % 12 == 0 {
                let listed = domador::power::list_plans();
                let active = domador::power::active_plan();
                lock(&probe_engine).apply_plans(listed, active);
            }
            tick += 1;
            thread::sleep(Duration::from_secs(5));
        }
    });

    let tick_engine = engine.clone();
    thread::spawn(move || loop {
        let wait = {
            let mut guard = lock(&tick_engine);
            guard.tick();
            guard.config.interval.max(0.3)
        };
        thread::sleep(Duration::from_secs_f64(wait));
    });

    let actual_port = api::serve(engine.clone(), port)?;
    let url = format!("http://127.0.0.1:{actual_port}/");
    println!("Domador {} escuchando en {}", model::VERSION, url);
    println!("config: {}", config_path.display());
    if !win::is_admin() {
        println!("sin admin: algunos procesos van a rechazar cambios");
    }

    let shutdown_engine = engine.clone();
    let _ = ctrlc::set_handler(move || {
        lock(&shutdown_engine).shutdown();
        std::process::exit(0);
    });

    if open {
        open_ui(actual_port, &url);
    }

    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}

fn open_ui(port: u16, url: &str) {
    // Preferir la UI nativa; el navegador queda como fallback de desarrollo.
    // La UI recibe el puerto real: si 8787 estaba ocupado, el motor cae al
    // siguiente y antes la UI no lo encontraba (QA A7).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let ui = dir.join("domador-ui.exe");
            if ui.exists()
                && Command::new(ui)
                    .env("DOMADOR_PORT", port.to_string())
                    .spawn()
                    .is_ok()
            {
                return;
            }
        }
    }
    let candidates = [
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
    ];
    for browser in candidates {
        if PathBuf::from(browser).exists() {
            let _ = Command::new(browser)
                .arg(format!("--app={url}"))
                .arg("--window-size=1400,900")
                .spawn();
            return;
        }
    }
    let _ = Command::new("cmd").args(["/C", "start", "", url]).spawn();
}

fn cli_status(port: u16) -> Result<(), String> {
    let state = get(port, "/api/state")?;
    let profile = state
        .pointer("/profile/active")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let plan = state
        .pointer("/profile/plan")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let counts = state.get("counts").cloned().unwrap_or(json!({}));
    println!("perfil: {profile}   plan: {plan}");
    println!("{counts}");
    if let Some(rows) = state.get("processes").and_then(|v| v.as_array()) {
        println!(
            "\n{:<8} {:<30} {:>7} {:>9}  {}",
            "PID", "NOMBRE", "CPU", "RAM", "PRIORIDAD"
        );
        for row in rows.iter().take(15) {
            let rss = row.get("rss").and_then(|v| v.as_u64()).unwrap_or(0) / 1048576;
            println!(
                "{:<8} {:<30} {:>6.1}% {:>7}MB  {}",
                row.get("pid").and_then(|v| v.as_u64()).unwrap_or(0),
                row.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                row.get("cpu").and_then(|v| v.as_f64()).unwrap_or(0.0),
                rss,
                row.get("prio_label").and_then(|v| v.as_str()).unwrap_or("")
            );
        }
    }
    Ok(())
}

fn cli_profile(args: &[String], port: u16) -> Result<(), String> {
    let name = args
        .get(2)
        .ok_or("uso: domador profile <full_power|gaming|ai|balanced> [juego.exe]")?;
    let target = args.get(3).cloned();
    let response = post(
        port,
        "/api/profile",
        json!({"name": name, "target": target}),
    )?;
    println!(
        "{}",
        serde_json::to_string_pretty(&response).unwrap_or_default()
    );
    Ok(())
}

fn cli_power(args: &[String], port: u16) -> Result<(), String> {
    let action = args.get(2).map(|s| s.as_str()).unwrap_or("list");
    match action {
        "list" => {
            let state = get(port, "/api/state")?;
            let active = state
                .pointer("/power/active")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if let Some(plans) = state.pointer("/power/plans").and_then(|v| v.as_array()) {
                for plan in plans {
                    let guid = plan.get("guid").and_then(|v| v.as_str()).unwrap_or("");
                    let name = plan.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    println!("{} {}", if guid == active { "*" } else { " " }, name);
                }
            }
            Ok(())
        }
        "full" => {
            post(port, "/api/profile", json!({"name": "full_power"}))?;
            println!("Full Power activado");
            Ok(())
        }
        "restore" => {
            post(port, "/api/profile", json!({"name": "balanced"}))?;
            println!("plan anterior restaurado");
            Ok(())
        }
        "set" => {
            let needle = args.get(3).ok_or("uso: domador power set <nombre|guid>")?;
            let state = get(port, "/api/state")?;
            let plans = state
                .pointer("/power/plans")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let found = plans.iter().find(|p| {
                p.get("guid").and_then(|v| v.as_str()) == Some(needle.as_str())
                    || p.get("name")
                        .and_then(|v| v.as_str())
                        .map(|n| n.eq_ignore_ascii_case(needle))
                        .unwrap_or(false)
            });
            let Some(plan) = found else {
                return Err(format!("no encontré el plan «{needle}»"));
            };
            let guid = plan.get("guid").and_then(|v| v.as_str()).unwrap_or("");
            post(port, "/api/plan", json!({"guid": guid}))?;
            println!(
                "plan activado: {}",
                plan.get("name").and_then(|v| v.as_str()).unwrap_or("")
            );
            Ok(())
        }
        other => Err(format!("acción desconocida: {other}")),
    }
}

fn cli_set(args: &[String], port: u16) -> Result<(), String> {
    let pid = flag(args, "--pid")
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or("falta --pid")?;
    let mut did = false;
    if let Some(priority) = flag(args, "--priority") {
        post(port, "/api/priority", json!({"pid": pid, "cls": priority}))?;
        println!("prioridad: {priority}");
        did = true;
    }
    if let Some(affinity) = flag(args, "--affinity") {
        let cpus = parse_cpus(&affinity)?;
        post(port, "/api/affinity", json!({"pid": pid, "cpus": cpus}))?;
        println!("afinidad: {affinity}");
        did = true;
    }
    if let Some(eco) = flag(args, "--eco") {
        let enabled = eco == "on" || eco == "1" || eco == "true";
        post(port, "/api/eco", json!({"pid": pid, "enabled": enabled}))?;
        println!("eco: {enabled}");
        did = true;
    }
    if !did {
        return Err("nada para hacer: usá --priority, --affinity o --eco".into());
    }
    Ok(())
}

fn parse_cpus(text: &str) -> Result<Vec<u32>, String> {
    let mut cpus = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((a, b)) = part.split_once('-') {
            let start: u32 = a.trim().parse().map_err(|_| "rango inválido")?;
            let end: u32 = b.trim().parse().map_err(|_| "rango inválido")?;
            for value in start..=end {
                cpus.push(value);
            }
        } else {
            cpus.push(part.parse().map_err(|_| "núcleo inválido")?);
        }
    }
    cpus.sort_unstable();
    cpus.dedup();
    if cpus.is_empty() {
        return Err("afinidad vacía".into());
    }
    Ok(cpus)
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .map(|s| s.as_str())
}

fn port_arg(args: &[String], default: u16) -> u16 {
    flag(args, "--port")
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn lock(engine: &Shared) -> std::sync::MutexGuard<'_, engine::Engine> {
    engine.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn base(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

fn get(port: u16, path: &str) -> Result<Value, String> {
    let response = ureq::get(&format!("{}{path}", base(port)))
        .call()
        .map_err(|err| format!("no pude conectar con Domador en {port}: {err}"))?;
    response
        .into_json()
        .map_err(|err| format!("respuesta inválida: {err}"))
}

fn post(port: u16, path: &str, body: Value) -> Result<Value, String> {
    match ureq::post(&format!("{}{path}", base(port)))
        .set("X-Domador", "1")
        .set("Content-Type", "application/json")
        .send_json(body)
    {
        Ok(response) => response
            .into_json()
            .map_err(|err| format!("respuesta inválida: {err}")),
        Err(ureq::Error::Status(code, response)) => Err(format!(
            "HTTP {code}: {}",
            response.into_string().unwrap_or_default()
        )),
        Err(err) => Err(format!("no pude conectar con Domador: {err}")),
    }
}

fn usage() {
    println!(
        "Domador {}\n\
         uso:\n  domador                 inicia el motor y abre la UI\n  \
         domador serve [--port N]  sólo motor + API\n  \
         domador mcp             servidor MCP por stdio (para agentes)\n  \
         domador status          estado actual\n  \
         domador profile <full_power|gaming|ai|balanced> [juego.exe]\n  \
         domador power list|set <nombre|guid>|full|restore\n  \
         domador set --pid N --priority high --affinity 0-3,8 --eco on\n  \
         domador stop            apaga el motor y restaura el plan\n",
        model::VERSION
    );
}
