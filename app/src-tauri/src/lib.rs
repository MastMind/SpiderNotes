mod commands;
mod state;

use state::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt().init();
    let name = std::env::var("HOSTNAME").unwrap_or_else(|_| "device".into());
    let args: Vec<String> = std::env::args().collect();
    let arg_after = |flag: &str| -> Option<String> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let open_path = arg_after("--open");
    let port: Option<u16> = arg_after("--port").and_then(|s| s.parse().ok());
    tauri::Builder::default()
        .setup(move |app| {
            // On mobile `dirs::config_dir()` resolves to `/.config` (HOME=/),
            // which is not writable — persist identity/trust in the app data
            // dir Tauri provides (on Android: the app's internal files dir).
            #[cfg(mobile)]
            {
                let dir = app
                    .path()
                    .app_data_dir()
                    .expect("no app data dir on mobile");
                let identity = spidernotes_core::DeviceIdentity::load_or_create_in(&name, &dir)
                    .unwrap_or_else(|e| panic!("cannot load device identity: {e}"));
                let trust = spidernotes_core::TrustStore::load_or_create_in(&dir)
                    .unwrap_or_else(|e| panic!("cannot load trust list: {e}"));
                let peers_path = dir.join("peers.json");
                app.manage(AppState::new(identity, trust, peers_path));
            }
            #[cfg(not(mobile))]
            {
                let identity = spidernotes_core::DeviceIdentity::load_or_create(&name)
                    .unwrap_or_else(|e| panic!("cannot load device identity: {e}"));
                let trust = spidernotes_core::TrustStore::load_or_create()
                    .unwrap_or_else(|e| panic!("cannot load trust list: {e}"));
                let peers_path = spidernotes_core::identity::config_dir()
                    .expect("no config dir")
                    .join("peers.json");
                app.manage(AppState::new(identity, trust, peers_path));
            }
            if let Some(path) = open_path.clone() {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = commands::do_open_vault(handle, path, port).await {
                        tracing::error!("--open failed: {e}");
                        eprintln!("--open failed: {e}");
                    }
                });
            }
            Ok(())
        })
        .plugin(spidernotes_fgservice::init())
        .invoke_handler(tauri::generate_handler![
            commands::open_vault,
            commands::default_vault_path,
            commands::vault_status,
            commands::list_notes,
            commands::read_note,
            commands::save_note,
            commands::delete_note,
            commands::rename_note,
            commands::search_notes,
            commands::backlinks,
            commands::all_tags,
            commands::notes_with_tag,
            commands::rescan,
            commands::device_info,
            commands::list_peers,
            commands::list_trusted,
            commands::trust_device,
            commands::untrust_device,
            commands::mdns_refresh,
            commands::sync_with,
            commands::sync_all,
            commands::p2p_settings,
            commands::set_bg_sync,
            commands::set_autosync_secs,
            commands::set_p2p_port_setting,
            commands::restart_p2p
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
