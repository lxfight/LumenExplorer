//! Lumen build-time resources: app icon, manifest, and Cargo-synced VERSIONINFO.

fn main() {
    println!("cargo:rerun-if-changed=assets/lumen.rc");
    println!("cargo:rerun-if-changed=assets/lumen.manifest");
    println!("cargo:rerun-if-changed=assets/lumen.ico");
    println!("cargo:rerun-if-env-changed=CARGO_PKG_VERSION");

    let major = std::env::var("CARGO_PKG_VERSION_MAJOR").unwrap_or_else(|_| "0".into());
    let minor = std::env::var("CARGO_PKG_VERSION_MINOR").unwrap_or_else(|_| "2".into());
    let patch = std::env::var("CARGO_PKG_VERSION_PATCH").unwrap_or_else(|_| "0".into());
    let full = format!("{major}.{minor}.{patch}.0");

    let macros = [
        format!("LUMEN_VER_MAJOR={major}"),
        format!("LUMEN_VER_MINOR={minor}"),
        format!("LUMEN_VER_PATCH={patch}"),
        format!("LUMEN_VER_STR={full}"),
    ];
    let _ = embed_resource::compile("assets/lumen.rc", &macros);
}
