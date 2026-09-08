fn main() {
    let manifest_dir = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let staged_manifest =
        manifest_dir.join("../../../target/managed-worker/macos-arm64/manifest-v1.json");
    println!("cargo:rerun-if-changed={}", staged_manifest.display());
    let enabled = std::env::var_os("CARGO_FEATURE_MANAGED_WORKER").is_some();
    let bytes = if enabled && staged_manifest.is_file() {
        std::fs::read(&staged_manifest).expect("read managed worker build manifest")
    } else {
        Vec::new()
    };
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(output.join("managed-worker-manifest.json"), bytes)
        .expect("embed managed worker manifest");
    tauri_build::build();
}
