fn main() {
    // El icono se incrusta en los .exe para que el Explorador, la barra de
    // tareas y los accesos directos no muestren el icono genérico de Windows.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/domador.ico");
        res.set("ProductName", "Domador");
        res.set("FileDescription", "Domador - control de procesos");
        res.set("CompanyName", "José Milano");
        res.set("LegalCopyright", "MIT");
        if let Err(err) = res.compile() {
            // No romper un build de desarrollo si falta windres; el binario
            // funciona igual, sólo queda con el icono por defecto.
            println!("cargo:warning=no se pudo incrustar el icono: {err}");
        } else if let Ok(out_dir) = std::env::var("OUT_DIR") {
            // En GNU ld, un objeto dentro de un archivo .a sin símbolos no se
            // arrastra; el .rsrc quedaría fuera del .exe. Enlazarlo directo.
            let obj = std::path::Path::new(&out_dir).join("resource.o");
            if obj.exists() {
                println!("cargo:rustc-link-arg={}", obj.display());
            }
        }
    }
    println!("cargo:rerun-if-changed=assets/domador.ico");
    println!("cargo:rerun-if-changed=build.rs");
}
