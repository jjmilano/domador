//! QA v2 — contrato de la API local.
//!
//! Estos tests levantan el servidor en un puerto libre (0) con un config
//! temporal: no dependen de un motor ya instalado ni de 8787. No tocan
//! procesos del usuario salvo el `hog` que ellos mismos lanzan.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use domador::{api, engine::Engine};
use serde_json::{json, Value};

fn temp_config(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("domador-qa-{}-{}.json", name, std::process::id()))
}

/// Motor + API en un puerto efímero. Devuelve la base y el path del config
/// para poder limpiarlo.
fn start(name: &str) -> (String, Arc<Mutex<Engine>>, PathBuf) {
    let path = temp_config(name);
    let engine = Arc::new(Mutex::new(Engine::new(path.clone())));
    engine.lock().unwrap().tick();
    let port = api::serve(engine.clone(), 0).expect("servidor en puerto efímero");
    (format!("http://127.0.0.1:{port}"), engine, path)
}

fn status_of<T>(result: Result<T, ureq::Error>) -> u16 {
    match result {
        Ok(_) => 200,
        Err(ureq::Error::Status(code, _)) => code,
        Err(err) => panic!("error de transporte: {err}"),
    }
}

fn post(base: &str, path: &str, body: Value) -> Result<Value, ureq::Error> {
    ureq::post(&format!("{base}{path}"))
        .set("X-Domador", "1")
        .set("Content-Type", "application/json")
        .send_json(body)
        .map(|r| r.into_json().unwrap_or(json!({})))
}

/// Petición HTTP/1.1 cruda: sirve para lo que ureq no expone (chunked,
/// Host inventado, cuerpos gigantes sin Content-Length).
fn raw(base: &str, request: &[u8]) -> String {
    let addr = base.trim_start_matches("http://");
    let mut stream = TcpStream::connect(addr).expect("conexión");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    stream.write_all(request).expect("escritura");
    stream.flush().ok();
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                // Un cuerpo JSON completo cierra el intercambio.
                if out.last() == Some(&b'}') {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn first_line(response: &str) -> String {
    response.lines().next().unwrap_or("").trim().to_string()
}

fn spawn_hog(seconds: &str) -> Child {
    Command::new(env!("CARGO_BIN_EXE_hog"))
        .args([seconds, "1"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("no pude lanzar el hog")
}

fn stop(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn auth_host_y_semantica_http() {
    let (base, _engine, config) = start("http");

    // Sin el encabezado X-Domador no se puede escribir nada.
    let sin_header = ureq::post(&format!("{base}/api/priority")).send_json(json!({"pid": 4}));
    assert_eq!(status_of(sin_header), 403);

    // Con el encabezado pero Host no-loopback: 403 (anti DNS rebinding).
    let bad_host = raw(
        &base,
        format!("GET /api/health HTTP/1.1\r\nHost: evil.example\r\n\r\n").as_bytes(),
    );
    assert!(
        first_line(&bad_host).contains("403"),
        "Host no-loopback debe dar 403, llegó {bad_host:?}"
    );

    // Un POST a un endpoint de sólo lectura es 405, no 400.
    assert_eq!(
        status_of(ureq::post(&format!("{base}/api/state")).send_json(json!({}))),
        405
    );
    assert_eq!(
        status_of(ureq::post(&format!("{base}/api/health")).send_json(json!({}))),
        405
    );

    // Endpoint inexistente: 404 tanto en GET como en POST con auth.
    assert_eq!(
        status_of(ureq::get(&format!("{base}/api/no-existe")).call()),
        404
    );
    assert_eq!(status_of(post(&base, "/api/no-existe", json!({}))), 404);

    // Método no soportado.
    assert_eq!(
        status_of(
            ureq::request("DELETE", &format!("{base}/api/priority"))
                .set("X-Domador", "1")
                .call()
        ),
        405
    );

    let _ = fs::remove_file(config);
}

#[test]
fn body_cap_con_content_length_es_413() {
    let (base, _engine, config) = start("cap");
    let payload = format!("{{\"pad\":\"{}\"}}", "a".repeat(70 * 1024));
    let response = raw(
        &base,
        format!(
            "POST /api/memguard HTTP/1.1\r\nHost: localhost\r\nX-Domador: 1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            payload.len(),
            payload
        )
        .as_bytes(),
    );
    assert!(
        first_line(&response).contains("413"),
        "70 KiB debe dar 413, llegó {response:?}"
    );
    let _ = fs::remove_file(config);
}

/// BUG conocido (QA v2): el tope de 64 KiB sólo se aplica si el cliente manda
/// Content-Length. Con `Transfer-Encoding: chunked` el cuerpo se trunca a
/// 64 KiB y el JSON parcial devuelve 400 "JSON inválido" en vez de 413.
/// Cuando se arregle, sacar `#[ignore]`.
#[test]
#[ignore = "bug conocido: cuerpo chunked > 64 KiB devuelve 400 en vez de 413"]
fn body_cap_chunked_deberia_ser_413() {
    let (base, _engine, config) = start("chunked");
    let payload = format!("{{\"pad\":\"{}\"}}", "a".repeat(200 * 1024));
    let addr = base.trim_start_matches("http://");
    let mut stream = TcpStream::connect(addr).expect("conexión");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .write_all(
            b"POST /api/memguard HTTP/1.1\r\nHost: localhost\r\nX-Domador: 1\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n",
        )
        .unwrap();
    for chunk in payload.as_bytes().chunks(8192) {
        write!(stream, "{:x}\r\n", chunk.len()).unwrap();
        stream.write_all(chunk).unwrap();
        stream.write_all(b"\r\n").unwrap();
    }
    stream.write_all(b"0\r\n\r\n").unwrap();
    let mut out = Vec::new();
    stream.read_to_end(&mut out).ok();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("413"),
        "cuerpo chunked de 200 KiB debe dar 413, llegó {text:?}"
    );
    let _ = fs::remove_file(config);
}

#[test]
fn snapshot_contract_y_counts_consistentes() {
    let (base, _engine, config) = start("state");
    let state: Value = ureq::get(&format!("{base}/api/state"))
        .call()
        .expect("state")
        .into_json()
        .expect("json");

    for key in [
        "app",
        "system",
        "processes",
        "rules",
        "probalance",
        "power",
        "history",
        "log",
        "counts",
        "profile",
        "gpu",
        "families",
        "memguard",
        "mem_pressure",
    ] {
        assert!(
            state.get(key).is_some(),
            "falta la clave {key} en /api/state"
        );
    }

    let processes = state["processes"].as_array().expect("processes");
    assert!(
        processes.len() > 20,
        "muy pocos procesos: {}",
        processes.len()
    );
    assert_eq!(
        state["counts"]["processes"].as_u64().unwrap_or(0) as usize,
        processes.len(),
        "counts.processes no coincide con la lista"
    );
    assert!(
        state["history"].as_array().unwrap().len() <= 150,
        "history debe estar acotado a 150 puntos"
    );
    assert!(
        state["log"].as_array().unwrap().len() <= 400,
        "log debe estar acotado a 400 líneas"
    );

    for row in processes {
        for key in ["pid", "name", "cpu", "rss", "prio_label", "affinity_label"] {
            assert!(row.get(key).is_some(), "fila sin {key}: {row}");
        }
        assert!(
            !row["name"].as_str().unwrap_or("").is_empty(),
            "fila sin nombre"
        );
    }

    let _ = fs::remove_file(config);
}

#[test]
fn familias_suman_lo_que_dicen_los_procesos() {
    let (base, _engine, config) = start("families");
    let state: Value = ureq::get(&format!("{base}/api/state"))
        .call()
        .expect("state")
        .into_json()
        .expect("json");
    let families = state["families"].as_array().expect("families");
    let processes = state["processes"].as_array().expect("processes");

    let con_familia = processes
        .iter()
        .filter(|row| row.get("family").is_some())
        .count();
    let suma: usize = families
        .iter()
        .map(|f| f["count"].as_u64().unwrap_or(0) as usize)
        .sum();
    assert_eq!(
        con_familia, suma,
        "las familias no cubren exactamente los procesos con family"
    );

    for family in families {
        let pids = family["pids"].as_array().expect("pids");
        assert!(
            pids.len() <= 256,
            "pids de una familia debe estar acotado a 256"
        );
        let count = family["count"].as_u64().unwrap_or(0) as usize;
        assert!(
            pids.len() <= count,
            "no puede haber más pids listados que procesos contados"
        );
    }

    let _ = fs::remove_file(config);
}

#[test]
fn memguard_acota_los_umbrales() {
    let (base, _engine, config) = start("memguard");
    let cfg = post(
        &base,
        "/api/memguard",
        json!({
            "enabled": false,
            "warn_commit_pct": 500.0,
            "warn_available_pct": 0.0,
            "relief_interval_s": 1,
            "warn_cooldown_s": 999_999
        }),
    )
    .expect("memguard")
    .get("memguard")
    .cloned()
    .expect("memguard en la respuesta");

    assert_eq!(cfg["warn_commit_pct"], 99.0);
    assert_eq!(cfg["warn_available_pct"], 0.5);
    assert_eq!(cfg["relief_interval_s"], 60);
    assert_eq!(cfg["warn_cooldown_s"], 86_400);
    assert_eq!(cfg["enabled"], false);

    let _ = fs::remove_file(config);
}

#[test]
fn errores_de_validacion_son_400_con_mensaje() {
    let (base, _engine, config) = start("valid");

    let sin_pid = post(&base, "/api/priority", json!({"cls": "high"}));
    assert_eq!(status_of(sin_pid), 400);

    let cls_mala = post(&base, "/api/priority", json!({"pid": 4, "cls": "turbo"}));
    assert_eq!(status_of(cls_mala), 400);

    let afinidad_vacia = post(&base, "/api/affinity", json!({"pid": 4, "cpus": []}));
    assert_eq!(status_of(afinidad_vacia), 400);

    let familia_mala = post(
        &base,
        "/api/family",
        json!({"family": "no-existe", "action": "trim"}),
    );
    assert_eq!(status_of(familia_mala), 400);

    let perfil_malo = post(&base, "/api/profile", json!({"name": "turbo"}));
    assert_eq!(status_of(perfil_malo), 400);

    // restore de un pid que nunca se tocó: error explícito, no 200 mudo.
    let restore_sin_cambios = post(&base, "/api/restore", json!({"pid": 4}));
    assert_eq!(status_of(restore_sin_cambios), 400);

    // JSON roto.
    let roto = ureq::post(&format!("{base}/api/memguard"))
        .set("X-Domador", "1")
        .set("Content-Type", "application/json")
        .send_string("{oops");
    assert_eq!(status_of(roto), 400);

    let _ = fs::remove_file(config);
}

#[test]
fn trim_de_un_proceso_propio_reporta_bytes() {
    let (base, _engine, config) = start("trim");
    let hog = spawn_hog("30");
    std::thread::sleep(Duration::from_millis(900));

    let first = post(&base, "/api/trim", json!({"pid": hog.id()})).expect("trim");
    assert_eq!(first["ok"], true);
    assert!(first["freed"].is_u64(), "freed debe ser número: {first}");

    // Segundo trim inmediato: no debe romper ni devolver basura.
    let second = post(&base, "/api/trim", json!({"pid": hog.id()})).expect("trim 2");
    assert_eq!(second["ok"], true);

    stop(hog);
    let _ = fs::remove_file(config);
}

#[test]
fn api_sigue_viva_despues_de_pedidos_invalidos() {
    let (base, _engine, config) = start("alive");

    for _ in 0..5 {
        let bad = raw(
            &base,
            b"POST /api/memguard HTTP/1.1\r\nHost: localhost\r\nX-Domador: 1\r\nContent-Length: 5\r\n\r\n{oops",
        );
        assert!(!bad.is_empty(), "el servidor tiene que contestar algo");
    }

    let health: Value = ureq::get(&format!("{base}/api/health"))
        .call()
        .expect("health después de pedidos inválidos")
        .into_json()
        .expect("json");
    assert_eq!(health["ok"], true);

    let _ = fs::remove_file(config);
}
