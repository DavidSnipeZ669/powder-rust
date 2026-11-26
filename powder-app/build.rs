// 1. Packs the custom-game engine (../powder-rs: the built exe, Lua library, bundled games, OCR
//    dictionary) into engine-pack.zip, which Rewind.exe carries and unpacks on first run.
//    build-app.bat builds the engine first; without it the pack is empty and Rewind says so.
// 2. On Windows, embeds Rewind's icon and version info into the exe.
use std::io::Write;
use std::path::{Path, PathBuf};

fn add_dir(z: &mut zip::ZipWriter<std::fs::File>, dir: &Path, prefix: &str, opt: zip::write::SimpleFileOptions) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut ents: Vec<_> = rd.flatten().collect();
    ents.sort_by_key(|e| e.file_name());
    for e in ents {
        let name = format!("{prefix}/{}", e.file_name().to_string_lossy());
        if e.path().is_dir() {
            add_dir(z, &e.path(), &name, opt);
        } else if let Ok(b) = std::fs::read(e.path()) {
            z.start_file(name, opt).unwrap();
            z.write_all(&b).unwrap();
        }
    }
}

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("engine-pack.zip");
    let eng = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("..").join("powder-rs");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let exe_name = if windows { "powder-rs.exe" } else { "powder-rs" };
    let exe = std::env::var("REWIND_ENGINE_EXE").map(PathBuf::from).unwrap_or_else(|_| eng.join("target").join("release").join(exe_name));
    println!("cargo:rerun-if-env-changed=REWIND_ENGINE_EXE");
    println!("cargo:rerun-if-changed={}", exe.display());
    for d in ["lua", "games", "models/ocr/dict.txt"] {
        println!("cargo:rerun-if-changed={}", eng.join(d).display());
    }
    let f = std::fs::File::create(&out).unwrap();
    if exe.exists() {
        let mut z = zip::ZipWriter::new(f);
        let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).compression_level(Some(9));
        z.start_file(exe_name, opt).unwrap();
        z.write_all(&std::fs::read(&exe).unwrap()).unwrap();
        add_dir(&mut z, &eng.join("lua"), "lua", opt);
        add_dir(&mut z, &eng.join("games"), "games", opt);
        for (from, to) in [("models/ocr/dict.txt", "models/ocr/dict.txt"), ("models/ocr/README.txt", "models/ocr/README.txt"), ("runtime/ONNXRUNTIME_LICENSE.txt", "ONNXRUNTIME_LICENSE.txt")] {
            if let Ok(b) = std::fs::read(eng.join(from)) {
                z.start_file(to, opt).unwrap();
                z.write_all(&b).unwrap();
            }
        }
        z.finish().unwrap();
    } else {
        println!("cargo:warning=custom-game engine not built ({}): Rewind.exe will be built without it", exe.display());
    }

    println!("cargo:rerun-if-changed=assets/branding/icon.ico");
    #[cfg(windows)]
    {
        if windows {
            let mut r = winresource::WindowsResource::new();
            r.set_icon("assets/branding/icon.ico");
            r.set("ProductName", "Rewind");
            r.set("FileDescription", "Rewind");
            if let Err(e) = r.compile() {
                println!("cargo:warning=could not embed the icon: {e}");
            }
        }
    }
}
