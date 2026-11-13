//! Incrusta el icono y los metadatos de versión en los .exe.
//!
//! No usamos `winres`: pasa el .rc como nombre relativo y ejecuta windres con
//! un cwd que no siempre coincide, así que fallaba con "preprocessing failed"
//! y el .exe quedaba con el icono genérico. Acá escribimos el .rc nosotros
//! (con la ruta del .ico absoluta y en formato "/") y llamamos a windres con
//! `-i`/`-o`, capturando su stderr para poder diagnosticar.

use std::path::Path;
use std::process::Command;

fn version_strings() -> (String, String) {
    let ver = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    let parts: Vec<&str> = ver.split(|c| c == '.' || c == '-').collect();
    let num = |i: usize| -> u32 {
        parts
            .get(i)
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0)
    };
    let quad = format!("{}, {}, {}, 0", num(0), num(1), num(2));
    (quad, ver)
}

fn main() {
    println!("cargo:rerun-if-changed=assets/domador.ico");
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    // windres no traduce "\": la ruta va con "/" para no depender de escapes.
    let icon = format!("{}/assets/domador.ico", manifest.replace('\\', "/"));
    if !Path::new(&icon).exists() {
        println!("cargo:warning=no existe {icon}; el .exe quedará sin icono");
        return;
    }

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR siempre está en un build script");
    let out = Path::new(&out_dir);
    let rc_path = out.join("domador.rc");
    let obj = out.join("domador_res.o");
    // Un objeto viejo haría que el enlace reuse el icono anterior si esta
    // compilación falla: se borra antes de intentar.
    let _ = std::fs::remove_file(&obj);

    let (quad, ver) = version_strings();
    let rc = format!(
        "#pragma code_page(65001)\n\
         1 VERSIONINFO\n\
         FILEOS 0x40004\n\
         FILETYPE 0x1\n\
         FILEFLAGSMASK 0x3f\n\
         PRODUCTVERSION {quad}\n\
         FILEVERSION {quad}\n\
         FILESUBTYPE 0x0\n\
         FILEFLAGS 0x0\n\
         {{\n\
         BLOCK \"StringFileInfo\"\n\
         {{\n\
         BLOCK \"000004b0\"\n\
         {{\n\
         VALUE \"FileVersion\", \"{ver}\"\n\
         VALUE \"ProductVersion\", \"{ver}\"\n\
         VALUE \"FileDescription\", \"Domador - control de procesos\"\n\
         VALUE \"CompanyName\", \"José Milano\"\n\
         VALUE \"ProductName\", \"Domador\"\n\
         VALUE \"LegalCopyright\", \"MIT\"\n\
         }}\n\
         }}\n\
         BLOCK \"VarFileInfo\" {{\n\
         VALUE \"Translation\", 0x0, 0x04b0\n\
         }}\n\
         }}\n\
         1 ICON \"{icon}\"\n"
    );
    if let Err(err) = std::fs::write(&rc_path, rc) {
        println!(
            "cargo:warning=no se pudo escribir {}: {err}",
            rc_path.display()
        );
        return;
    }

    let mut errs: Vec<String> = Vec::new();
    let mut built = false;
    for exe in ["windres", "x86_64-w64-mingw32-windres"] {
        match Command::new(exe)
            .arg("-i")
            .arg(&rc_path)
            .arg("-o")
            .arg(&obj)
            .current_dir(out)
            .output()
        {
            Ok(o) if o.status.success() => {
                built = true;
                break;
            }
            Ok(o) => {
                errs.push(format!(
                    "{exe}: {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                ));
            }
            Err(e) => {
                errs.push(format!("{exe}: {e}"));
            }
        }
    }

    if !built || !obj.exists() {
        // No romper el build si falta windres: el binario funciona igual, sólo
        // queda con el icono por defecto de Windows.
        println!(
            "cargo:warning=no se pudo incrustar el icono ({})",
            errs.join(" / ")
        );
        return;
    }

    // En GNU ld, un objeto dentro de un .a sin símbolos no se arrastra y el
    // .rsrc quedaría fuera del .exe; se enlaza directo.
    println!("cargo:rustc-link-arg={}", obj.display());
}
