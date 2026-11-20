use std::path::{Path, PathBuf};

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "android" {
        return; // desktop: nothing to do
    }
    let android = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("android");

    // The module depends on project(":tauri-android") (Tauri's Kotlin bridge).
    // For standalone builds of the module, copy the bridge out of the `tauri`
    // crate (its build script publishes the path as DEP_TAURI_ANDROID_LIBRARY_PATH).
    if let Ok(tauri_api) = std::env::var("DEP_TAURI_ANDROID_LIBRARY_PATH") {
        println!("cargo:rerun-if-env-changed=DEP_TAURI_ANDROID_LIBRARY_PATH");
        let dest = android.join(".tauri").join("tauri-api");
        let _ = std::fs::remove_dir_all(&dest);
        let _ = copy_dir(Path::new(&tauri_api), &dest);
    }

    // tauri-build scans the app's build env for DEP_*_ANDROID_LIBRARY_PATH and
    // includes each path as a gradle module in tauri.settings.gradle.
    println!("cargo:android_library_path={}", android.display());
}
