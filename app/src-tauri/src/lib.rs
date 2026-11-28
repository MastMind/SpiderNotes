mod commands;
mod media_server;
mod server_sync;
mod state;

use state::AppState;
use std::time::Duration;
use tauri::{Emitter, Manager};

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
                let peer_names = spidernotes_core::PeerNames::load_or_create_in(&dir)
                    .unwrap_or_else(|e| panic!("cannot load peer name overrides: {e}"));
                // Self-heal: an older build listed the device in its own relay
                // roster, and with two identically-named devices a user could
                // have trusted their OWN id. Syncing with oneself can never
                // work — drop it from the allowlist.
                let _ = trust.remove(&identity.id);
                let peers_path = dir.join("peers.json");
                app.manage(AppState::new(identity, trust, peer_names, peers_path));
            }
            #[cfg(not(mobile))]
            {
                let identity = spidernotes_core::DeviceIdentity::load_or_create(&name)
                    .unwrap_or_else(|e| panic!("cannot load device identity: {e}"));
                let trust = spidernotes_core::TrustStore::load_or_create()
                    .unwrap_or_else(|e| panic!("cannot load trust list: {e}"));
                let peer_names = spidernotes_core::PeerNames::load_or_create()
                    .unwrap_or_else(|e| panic!("cannot load peer name overrides: {e}"));
                // Self-heal (same as the mobile branch above).
                let _ = trust.remove(&identity.id);
                let peers_path = spidernotes_core::identity::config_dir()
                    .expect("no config dir")
                    .join("peers.json");
                app.manage(AppState::new(identity, trust, peer_names, peers_path));
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
            // Close the window only after the frontend has flushed pending
            // edits: prevent the close, ask it to save ("sn-before-close"),
            // and force-destroy after a timeout if it never confirms (the
            // normal path is the force_close command right after the flush;
            // destroy() does not re-emit CloseRequested, so no loop).
            if let Some(win) = app.get_webview_window("main") {
                let ask = win.clone();
                let killer = win.clone();
                win.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = ask.emit("sn-before-close", ());
                        let w = killer.clone();
                        tauri::async_runtime::spawn(async move {
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            let _ = w.destroy();
                        });
                    }
                });
            }
            Ok(())
        })
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(spidernotes_fgservice::init())
        .invoke_handler(tauri::generate_handler![
            commands::open_vault,
            commands::default_vault_path,
            commands::vault_status,
            commands::list_notes,
            commands::list_folders,
            commands::create_folder,
            commands::rename_folder,
            commands::delete_folder,
            commands::read_note,
            commands::save_note,
            commands::delete_note,
            commands::rename_note,
            commands::import_media_file,
            commands::asset_report,
            commands::delete_assets,
            commands::create_encrypted_note,
            commands::unlock_note,
            commands::lock_note,
            commands::search_notes,
            commands::backlinks,
            commands::all_tags,
            commands::notes_with_tag,
            commands::rescan,
            commands::device_info,
            commands::set_device_name,
            commands::list_peers,
            commands::list_trusted,
            commands::trust_device,
            commands::untrust_device,
            commands::rename_peer,
            commands::mdns_refresh,
            commands::sync_with,
            commands::sync_all,
            commands::p2p_settings,
            commands::set_bg_sync,
            commands::set_autosync_secs,
            commands::set_p2p_port_setting,
            commands::set_download_media,
            commands::restart_p2p,
            commands::set_server_sync,
            commands::server_sync_status,
            commands::list_conflicts,
            commands::conflict_content,
            commands::resolve_conflict,
            commands::force_close
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
