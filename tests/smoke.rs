use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use domador::{api, engine::Engine, model::VERSION, win};
use serde_json::{json, Value};

fn temp_config(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("domador-test-{}-{}.json", name, std::process::id()))
}

fn spawn_hog(seconds: &str) -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_hog"))
        .args([seconds, "1"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("no pude lanzar el hog")
}

#[test]
fn process_controls_roundtrip() {
    let mut engine = Engine::new(temp_config("controls"));
    engine.tick();
    let mut child = spawn_hog("20");
    std::thread::sleep(Duration::from_millis(800));
    let pid = child.id();
    let above = win::priority_class_from_key("above_normal").unwrap();
    win::set_priority_class(pid, above).expect("no pude normalizar el hog");
    let original = win::get_priority_class(pid).expect("sin prioridad");

    engine.set_priority(pid, "high").expect("set priority");
    assert_eq!(
        win::get_priority_class(pid),
        win::priority_class_from_key("high"),
        "la prioridad no cambió"
    );

    engine.set_affinity(pid, &[0, 1]).expect("set affinity");
    assert_eq!(win::get_affinity(pid), Some(vec![0, 1]));

    match engine.set_eco(pid, true) {
        Ok(()) => assert_eq!(win::get_eco(pid), Some(true)),
        Err(err) if err.contains("87") || err.to_lowercase().contains("incorrect") => {
            eprintln!("EcoQoS no soportado en este equipo; salto esa parte");
        }
        Err(err) => panic!("set eco: {err}"),
    }

    let _ = engine.trim(pid);
    engine.restore(pid).expect("restore");
    assert_eq!(win::get_priority_class(pid), Some(original));
    if win::get_eco(pid).is_some() {
        assert_eq!(win::get_eco(pid), Some(false));
    }

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_file(engine.config_path.clone());
}

#[test]
fn rules_persist_and_validate() {
    let path = temp_config("rules");
    let mut engine = Engine::new(path.clone());
    let rule = engine
        .add_rule(&json!({"pattern": "domador-test.exe", "priority": "high"}))
        .expect("crear regla");
    assert_eq!(rule.pattern, "domador-test.exe");
    let raw = fs::read_to_string(&path).expect("config escrito");
    assert!(raw.contains("domador-test.exe"));

    let reloaded = Engine::new(path.clone());
    assert_eq!(reloaded.config.rules.len(), 1);

    assert!(
        engine
            .add_rule(&json!({"pattern": "domador-test.exe", "priority": "low"}))
            .is_err(),
        "debe rechazar duplicados"
    );
    assert!(engine.add_rule(&json!({"pattern": ""})).is_err());
    let _ = fs::remove_file(path);
}

#[test]
fn api_roundtrip_and_auth() {
    let config = temp_config("api");
    let engine = Arc::new(Mutex::new(Engine::new(config.clone())));
    engine.lock().unwrap().tick();
    let port = api::serve(engine.clone(), 0).expect("servidor");
    let base = format!("http://127.0.0.1:{port}");

    let health: Value = ureq::get(&format!("{base}/api/health"))
        .call()
        .expect("health")
        .into_json()
        .expect("json");
    assert_eq!(health["app"], "domador");
    assert_eq!(health["version"], VERSION);

    let state: Value = ureq::get(&format!("{base}/api/state"))
        .call()
        .expect("state")
        .into_json()
        .expect("json");
    assert!(state["processes"].as_array().unwrap().len() > 20);

    let unauthorized =
        ureq::post(&format!("{base}/api/priority")).send_json(json!({"pid": 1, "cls": "normal"}));
    match unauthorized {
        Err(ureq::Error::Status(code, _)) => assert_eq!(code, 403),
        other => panic!("se esperaba 403, llegó {other:?}"),
    }

    let mut child = spawn_hog("15");
    std::thread::sleep(Duration::from_millis(600));
    let pid = child.id();
    let above = win::priority_class_from_key("above_normal").unwrap();
    let _ = win::set_priority_class(pid, above);

    let response: Value = ureq::post(&format!("{base}/api/priority"))
        .set("X-Domador", "1")
        .send_json(json!({"pid": pid, "cls": "high"}))
        .expect("post priority")
        .into_json()
        .expect("json");
    assert_eq!(response["ok"], true);
    assert_eq!(
        win::get_priority_class(pid),
        win::priority_class_from_key("high")
    );

    let profile: Value = ureq::post(&format!("{base}/api/profile"))
        .set("X-Domador", "1")
        .send_json(json!({"name": "ai"}))
        .expect("profile ai")
        .into_json()
        .expect("json");
    assert_eq!(profile["profile"]["active"], "ai");

    let _ = ureq::post(&format!("{base}/api/profile"))
        .set("X-Domador", "1")
        .send_json(json!({"name": "balanced"}))
        .expect("profile balanced");

    let _ = ureq::post(&format!("{base}/api/restore"))
        .set("X-Domador", "1")
        .send_json(json!({"pid": pid}))
        .expect("restore");

    // QA A5: `hist` ya no viaja en el payload.
    assert!(state["processes"][0].get("hist").is_none());

    // QA A9: tipos inválidos en cpus no se confunden con lista vacía.
    let bad_affinity = ureq::post(&format!("{base}/api/affinity"))
        .set("X-Domador", "1")
        .send_json(json!({"pid": pid, "cpus": ["0", "1"]}));
    match bad_affinity {
        Err(ureq::Error::Status(400, resp)) => {
            let body: Value = resp.into_json().expect("json");
            assert!(body["error"].as_str().unwrap().contains("enteros"));
        }
        other => panic!("se esperaba 400, llegó {other:?}"),
    }

    // QA A4: Juego sin target no adivina ni cambia el plan de energía.
    let gaming = ureq::post(&format!("{base}/api/profile"))
        .set("X-Domador", "1")
        .send_json(json!({"name": "gaming"}));
    match gaming {
        Err(ureq::Error::Status(code, _)) => assert_eq!(code, 400),
        other => panic!("gaming sin target debería fallar, llegó {other:?}"),
    }

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_file(config);
}

#[test]
fn mcp_lists_tools_and_reports_status() {
    let output = Command::new(env!("CARGO_BIN_EXE_domador"))
        .arg("mcp")
        .env("DOMADOR_PORT", "9")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("mcp");
    let mut child = output;
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}})
        )
        .unwrap();
        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}})
        )
        .unwrap();
    }
    drop(child.stdin.take());
    let result = child.wait_with_output().expect("salida mcp");
    let text = String::from_utf8_lossy(&result.stdout);
    assert!(text.contains("\"protocolVersion\""));
    assert!(text.contains("domador_status"));
    assert!(text.contains("domador_apply_profile") || text.contains("domador_profile"));
}
