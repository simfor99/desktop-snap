//! Build-Hash aus Quellcode und Cargo.toml (für `--version` und `doctor`) sowie Manifest-Einbettung.
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

fn collect(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut paths: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
            out.push(path);
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=app.manifest");

    let mut files = Vec::new();
    collect(Path::new("src"), &mut files);
    let mut hasher = Sha256::new();
    hasher.update(fs::read("Cargo.toml").unwrap_or_default());
    for file in &files {
        hasher.update(file.to_string_lossy().as_bytes());
        hasher.update(fs::read(file).unwrap_or_default());
    }
    let digest = hasher.finalize();
    let hash: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
    println!("cargo:rustc-env=DESKTOP_SNAP_BUILD_HASH={hash}");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_manifest_file("app.manifest");
        if let Err(error) = res.compile() {
            panic!("Manifest-Einbettung fehlgeschlagen: {error}");
        }
    }
}
