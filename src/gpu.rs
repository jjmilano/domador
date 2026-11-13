//! Lectura de la GPU NVIDIA y preferencia por aplicación en Windows.

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

use crate::model::GpuInfo;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const GPU_KEY: &str = r"HKCU\Software\Microsoft\DirectX\UserGpuPreferences";

pub fn query() -> GpuInfo {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,driver_version,utilization.gpu,memory.used,memory.total,temperature.gpu,power.draw,pstate",
            "--format=csv,noheader,nounits",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let Ok(output) = output else {
        return GpuInfo::default();
    };
    if !output.status.success() {
        return GpuInfo::default();
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let Some(line) = text.lines().next() else {
        return GpuInfo::default();
    };
    let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
    if parts.len() < 8 {
        return GpuInfo::default();
    }
    let number = |value: &str| value.parse::<f32>().ok().filter(|n| n.is_finite());
    GpuInfo {
        available: true,
        name: parts[0].to_string(),
        driver: parts[1].to_string(),
        utilization: number(parts[2]).unwrap_or(0.0),
        memory_used: number(parts[3]).unwrap_or(0.0) as u64 * 1024 * 1024,
        memory_total: number(parts[4]).unwrap_or(0.0) as u64 * 1024 * 1024,
        temperature: number(parts[5]).unwrap_or(0.0),
        power: number(parts[6]).unwrap_or(0.0),
        pstate: parts[7].to_string(),
    }
}

pub fn set_preference(exe: &str, mode: &str) -> Result<(), String> {
    if exe.trim().is_empty() || !Path::new(exe).is_file() {
        return Err("no pude resolver el ejecutable del proceso".into());
    }
    let output = if mode == "high" {
        Command::new("reg")
            .args([
                "add",
                GPU_KEY,
                "/v",
                exe,
                "/t",
                "REG_SZ",
                "/d",
                "GpuPreference=2;",
                "/f",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
    } else if mode == "default" {
        Command::new("reg")
            .args(["delete", GPU_KEY, "/v", exe, "/f"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
    } else {
        return Err("modo de GPU inválido".into());
    };
    match output {
        Ok(result) if result.status.success() => Ok(()),
        Ok(result) => {
            let err = String::from_utf8_lossy(&result.stderr).trim().to_string();
            Err(if err.is_empty() {
                "reg falló".into()
            } else {
                err
            })
        }
        Err(err) => Err(format!("no pude ejecutar reg: {err}")),
    }
}
