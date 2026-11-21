// 无条件声明为窗口程序：日常使用不弹出控制台黑窗
// （调试时如需看 stderr，可临时注释掉这一行重新编译）
#![windows_subsystem = "windows"]

//! Trawl 桌面端入口。全部应用逻辑在 lib（trawl_lib），
//! Android/iOS 通过 tauri::mobile_entry_point 复用同一套 run()。

fn main() {
    trawl_lib::run();
}
