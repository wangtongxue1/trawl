fn main() {
    // 为 app 自定义命令生成 allow-<cmd> 权限：capability 才能在 remote 上下文里
    // 放行移动端注入浮层要用的 mob_home / mob_report。声明了 app manifest 后，
    // 所有 app 命令都走 ACL，desktop capability 里必须显式放行全部命令。
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
        "activate_tab", "adjust_split_ratio", "capture_screen", "check_update",
        "clear_browsing_data", "clear_downloads", "clear_history", "remove_history_entry", "close_all_tabs_and_newtab",
        "close_glance", "close_group_tabs", "close_other_tabs", "close_overlay",
        "close_tab_cmd", "close_tabs", "close_workspace", "ctxmenu_pick",
        "debug_msg", "delete_folder", "download_cancel", "download_retry", "duplicate_tab", "emit_shortcut",
        "exit_app", "exit_split", "export_backup", "find_in_page", "force_close_app",
        "get_ctxmenu", "get_downloads", "get_extensions", "get_history",
        "get_initial_history_count", "get_settings", "get_version", "import_backup", "import_bookmarks",
        "initial_state", "layout", "load_unpacked_ext", "mob_close_tab",
        "mob_home", "mob_new_tab", "mob_open", "mob_report",
        "mob_state", "mob_switch_tab", "nav_back", "nav_forward",
        "move_tab", "move_tab_to_folder", "create_folder", "remove_bookmarks_batch",
        "nav_reload", "navigate", "navigate_new_tab", "new_tab",
        "new_workspace", "open_brandpanel", "open_ctxmenu", "open_data_dir",
        "open_default_apps", "open_download_dir", "open_external", "open_feedback",
        "open_glance", "open_internal", "open_menu", "open_quickbar",
        "open_standalone_window", "open_tablist", "page_pressed", "print_page",
        "promote_glance", "push_suggest", "pw_add", "pw_del",
        "pw_import_csv", "pw_list", "pw_reveal", "pw_check", "pw_confirm_insecure", "pw_update", "remove_bookmark",
        "rename_folder", "rename_workspace", "reset_settings", "resize_overlay", "reopen_tab",
        "reveal_download", "save_extensions", "save_settings", "set_autostart",
        "set_pins", "skip_update", "smart_organize", "sort_tabs",
        "stop_loading", "switch_workspace_cmd", "toggle_bookmark", "toggle_pin",
        "toggle_reader", "toggle_split", "toggle_split_orientation", "toggle_translate",
        "toggle_vertical_tabs", "translate_text", "ui_action", "ungroup_workspace", "verify_user_consent",
        "win_close", "win_minimize", "win_toggle_maximize", "zoom_page"
        ])),
    )
    .expect("failed to run tauri-build");
}
