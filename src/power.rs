use std::os::windows::process::CommandExt;
use std::process::Command;

use crate::model::PowerPlan;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const ULTIMATE_GUID: &str = "e9a42b02-d5df-448d-aa00-03f14749eb61";
const HIGH_PERF_GUID: &str = "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c";
const FULL_POWER_NAME: &str = "Domador Full Power";

pub fn list_plans() -> Result<Vec<PowerPlan>, String> {
    let out = run(&["/list"])?;
    let mut plans = Vec::new();
    for line in out.lines() {
        let Some(guid) = find_guid(line) else {
            continue;
        };
        let mut name = guid.clone();
        if let Some(start) = line.find('(') {
            if let Some(end) = line[start + 1..].find(')') {
                name = line[start + 1..start + 1 + end].trim().to_string();
            }
        }
        let active = line.trim_end().ends_with('*');
        plans.push(PowerPlan { guid, name, active });
    }
    if plans.is_empty() {
        return Err("powercfg no devolvió planes".into());
    }
    Ok(plans)
}

pub fn active_plan() -> Option<String> {
    run(&["/getactivescheme"])
        .ok()
        .and_then(|out| find_guid(&out))
}

pub fn set_active(guid: &str) -> Result<(), String> {
    let normalized = guid.trim().to_lowercase();
    if find_guid(&normalized).as_deref() != Some(normalized.as_str()) {
        return Err(format!("GUID de energía inválido: {guid}"));
    }
    run(&["/setactive", &normalized]).map(|_| ())
}

pub fn find_plan(plans: &[PowerPlan], needle: &str) -> Option<PowerPlan> {
    let needle = needle.to_lowercase();
    plans
        .iter()
        .find(|p| p.name.to_lowercase() == needle)
        .or_else(|| {
            plans
                .iter()
                .find(|p| p.name.to_lowercase().contains(&needle))
        })
        .cloned()
}

pub fn ensure_full_power() -> Result<(String, String, Vec<String>), String> {
    let mut warnings = Vec::new();
    let plans = list_plans()?;
    let mut plan = plans
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(FULL_POWER_NAME))
        .cloned();

    if plan.is_none() {
        for (source, label) in [
            (ULTIMATE_GUID, "Ultimate Performance"),
            (HIGH_PERF_GUID, "High Performance"),
        ] {
            match run(&["/duplicatescheme", source]) {
                Ok(out) => {
                    if let Some(guid) = find_guid(&out) {
                        let _ = run(&[
                            "/changename",
                            &guid,
                            FULL_POWER_NAME,
                            "Máximo rendimiento, sin límites de energía",
                        ]);
                        plan = Some(PowerPlan {
                            guid,
                            name: FULL_POWER_NAME.into(),
                            active: false,
                        });
                        break;
                    }
                    warnings.push(format!("no pude leer el GUID del plan duplicado ({label})"));
                }
                Err(err) => warnings.push(format!("no pude duplicar {label}: {err}")),
            }
        }
    }

    if plan.is_none() {
        plan = find_plan(&plans, "ultimate performance")
            .or_else(|| find_plan(&plans, "bitsum highest performance"))
            .or_else(|| find_plan(&plans, "alto rendimiento"))
            .or_else(|| find_plan(&plans, "high performance"));
        if let Some(found) = &plan {
            warnings.push(format!("usando plan existente «{}»", found.name));
        }
    }

    let plan =
        plan.ok_or_else(|| "no encontré ni pude crear un plan de máximo rendimiento".to_string())?;

    for (sub, setting, value) in [
        ("SUB_PROCESSOR", "PROCTHROTTLEMIN", "100"),
        ("SUB_PROCESSOR", "PROCTHROTTLEMAX", "100"),
        ("SUB_PROCESSOR", "CPMINCORES", "100"),
        ("SUB_PROCESSOR", "PERFBOOSTMODE", "2"),
        ("SUB_PROCESSOR", "SYSCOOLPOL", "1"),
    ] {
        if let Err(err) = run(&["/setacvalueindex", &plan.guid, sub, setting, value]) {
            warnings.push(format!("{setting}: {err}"));
        }
    }
    let _ = run(&["/setactive", &plan.guid]);
    if let Err(err) = run(&["/setactive", &plan.guid]) {
        return Err(format!("no pude activar el plan: {err}"));
    }

    Ok((plan.guid, plan.name, warnings))
}

fn run(args: &[&str]) -> Result<String, String> {
    let output = Command::new("powercfg")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("powercfg {args:?}: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        let msg = if stderr.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        return Err(if msg.is_empty() {
            format!("powercfg {:?} falló", args)
        } else {
            msg
        });
    }
    Ok(stdout)
}

pub fn find_guid(text: &str) -> Option<String> {
    for (i, _) in text.char_indices() {
        let Some(candidate) = text.get(i..i + 36) else {
            continue;
        };
        let parts: Vec<&str> = candidate.split('-').collect();
        let ok = parts.len() == 5
            && parts[0].len() == 8
            && parts[1].len() == 4
            && parts[2].len() == 4
            && parts[3].len() == 4
            && parts[4].len() == 12
            && candidate.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
        if ok {
            return Some(candidate.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::find_guid;

    #[test]
    fn encuentra_guid_y_sobrevive_texto_no_ascii() {
        assert_eq!(find_guid("Esquemas de energía existentes:"), None);
        assert_eq!(
            find_guid("Power Scheme GUID: 381b4222-f694-41f0-9685-ff5bb260df2e  (Balanced)"),
            Some("381b4222-f694-41f0-9685-ff5bb260df2e".to_string())
        );
    }
}
