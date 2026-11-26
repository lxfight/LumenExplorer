//! Lumen build-time resources: app icon and application manifest.

fn main() {
    println!("cargo:rerun-if-changed=assets/lumen.rc");
    println!("cargo:rerun-if-changed=assets/lumen.manifest");
    println!("cargo:rerun-if-changed=assets/lumen.ico");
    let _ = embed_resource::compile("assets/lumen.rc", embed_resource::NONE);
}
