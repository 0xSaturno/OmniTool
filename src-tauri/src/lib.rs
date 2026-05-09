pub mod core;
pub mod tools;
mod commands;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            commands::model_to_ascii,
            commands::list_model_lookgroups,
            commands::ascii_to_model,
            commands::read_model_materials,
            commands::save_model_materials,
            commands::get_app_dir,
            commands::get_hashes_path,
            commands::hashes_exist,
            commands::load_hashes,
            commands::load_toc,
            commands::list_toc_assets,
            commands::extract_asset_to_project,
            commands::extract_asset_to_path,
            commands::extract_asset_as_dds,
            commands::create_project,
            commands::list_projects,
            commands::delete_project,
            commands::list_project_assets,
            commands::export_stage,
            commands::compute_crc64,
            commands::rename_project_asset,
            commands::delete_project_asset,
            commands::get_project_path,
            commands::import_assets_to_project,
            commands::open_project_in_explorer,
            commands::update_project_version,
            commands::read_config,
            commands::write_config,
            commands::export_config_json,
            commands::read_atmosphere,
            commands::write_atmosphere,
            commands::extract_to_temp,
            commands::import_file_to_project,
            commands::download_hashes,
            commands::read_zonelightbin,
            commands::write_zonelightbin_sections,
            commands::diff_zonelightbin,
            commands::tauri_get_texture_info,
            commands::tauri_get_dds_info,
            commands::tauri_get_texture_preview,
            commands::tauri_get_dds_preview,
            commands::tauri_extract_texture,
            commands::tauri_replace_texture,
            commands::tauri_scan_stager_textures,
            commands::tauri_batch_extract_textures,
            commands::tauri_batch_replace_textures,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
