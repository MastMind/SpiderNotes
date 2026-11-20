//! Keeps the app alive in the background on Android via a foreground
//! service (`FgService`), so the mDNS/TCP sync loops keep running while the
//! screen is off. On desktop the plugin registers nothing and is a no-op.

use tauri::{plugin::PluginHandle, Runtime};

/// Managed state: the handle to the loaded Android plugin (bridge to Java).
pub struct FgState<R: Runtime> {
    pub handle: PluginHandle<R>,
}

pub fn init<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("spidernotes-fgservice")
        .setup(|app, api| {
            #[cfg(target_os = "android")]
            {
                use tauri::Manager;
                // Instantiates app.spidernotes.fgservice.FgPlugin and registers
                // it with the activity's PluginManager. A failure here (e.g. the
                // Java class is missing from the APK) must NOT take the whole
                // app down — background sync just stays unavailable.
                match api.register_android_plugin("app.spidernotes.fgservice", "FgPlugin") {
                    Ok(handle) => {
                        app.manage(FgState { handle });
                    }
                    Err(e) => {
                        eprintln!("spidernotes-fgservice: plugin registration failed: {e}");
                    }
                }
                return Ok(());
            }
            #[cfg(not(target_os = "android"))]
            {
                let _ = (app, api);
                Ok(())
            }
        })
        .build()
}
