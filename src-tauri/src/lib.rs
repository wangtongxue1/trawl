//! Trawl 元搜浏览器 —— 应用逻辑库（桌面 bin 与 Android/iOS lib 共用）
//! 架构：主窗口（纯窗口，无 UI）+ "ui" webview（标签条/工具栏）+ 每个标签页一个远程 webview
//! 默认主页与搜索：元宝搜索 https://so.qq.com


//! Trawl 元搜浏览器 —— 仿 Zen / Tabbit 的个人浏览器
//! 架构：主窗口（纯窗口，无 UI）+ "ui" webview（标签条/工具栏）+ 每个标签页一个远程 webview
//! 默认主页与搜索：元宝搜索 https://so.qq.com
//! v0.4：
//!   - 热键仅在窗口聚焦时注册（不再抢占系统 Ctrl+T / Ctrl+F）
//!   - 事件只发给 "ui"，不再往每个远程页面 eval
//!   - 拦截 target=_blank / window.open，转为新标签页
//!   - 标签页懒创建（会话恢复不再一次性开满 webview）
//!   - 浮层 webview 按面板实际尺寸创建，不再整窗遮挡页面
//!   - 地址栏建议改为独立浮层（原先被页面 webview 压住不可见）
//!   - 下载记录、页内缩放、加载状态

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{
    webview::{DownloadEvent, NewWindowResponse, PageLoadEvent, WebviewBuilder},
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, RunEvent, State, WebviewUrl,
    WindowBuilder, WebviewWindowBuilder,
};

const HOME_URL: &str = "https://so.qq.com";
const STATE_FILE: &str = "state.json";
const SETTINGS_FILE: &str = "settings.json";
const BOOKMARKS_FILE: &str = "bookmarks.json";
const HISTORY_FILE: &str = "history.json";
const EXTENSIONS_FILE: &str = "extensions.json";
const PASSWORDS_FILE: &str = "passwords.json";
const HISTORY_CAP: usize = 1000;
const DOWNLOAD_CAP: usize = 60;
const DEBUG_LOG_MAX: u64 = 512 * 1024;
const CHROME_LABEL: &str = "ui";

/// 关闭确认插件放行的窗口关闭（用户在确认框里点了「全部关闭」）
static CLOSE_BYPASS: AtomicBool = AtomicBool::new(false);

// ==================== 数据模型 ====================

#[derive(Clone)]
struct Tab {
    id: String,
    workspace_id: String,
    url: String,
    title: String,
    pinned: bool,
    folder_id: Option<String>,
    /// 页面是否正在加载（Started=true / Finished=false）
    loading: bool,
    /// 页面缩放比例
    zoom: f64,
}

/// 下载记录（用于工具栏徽标与下载面板）
#[derive(Clone, Serialize, Deserialize)]
struct DownloadInfo {
    url: String,
    /// started | done | failed
    state: String,
    path: String,
    /// 已下载字节数（后端接管下载后实时更新）
    #[serde(default)]
    received: u64,
    /// 总字节数（0 = 服务器未给出）
    #[serde(default)]
    total: u64,
    ts: u64,
    /// 下载任务唯一 id（取消/重试按 id 定位，解决同 URL 并发互相覆盖）。
    /// 老状态文件无此字段，反序列化时补空串。
    #[serde(default)]
    id: String,
    /// 失败原因分类（成功/进行中为空串）：超时 / 连接失败 / 需要登录 /
    /// 客户端错误 / 服务器错误 / 写盘失败 / 内容不完整 / 未知错误 / 已取消。
    /// 老状态文件无此字段，反序列化时补空串。
    #[serde(default)]
    error: String,
}

/// 通用右键菜单项（标签页 / 书签栏 / 工作区共用同一个浮层）
#[derive(Clone, Serialize, Deserialize)]
struct CtxMenuItem {
    label: String,
    /// 显示在右侧的快捷键提示，可为空
    #[serde(default)]
    key: String,
    /// 分隔线（此时 label/key 无意义）
    #[serde(default)]
    sep: bool,
    /// 前端回传的动作标识；点击后原样 emit 给 chrome，由 renderer.js 分发
    action: String,
    /// 是否禁用（灰显不可点）
    #[serde(default)]
    disabled: bool,
}

/// 当前浮层（find / history / bookmarks / palette / downloads / quickbar）
struct OverlayState {
    label: String,
    kind: String,
    x: f64,
    y: f64,
    w: f64,
    height: f64,
}

struct Workspace {
    id: String,
    name: String,
    icon: String,
}

struct Folder {
    id: String,
    name: String,
    workspace_id: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct Bookmark {
    url: String,
    title: String,
    /// 所属文件夹路径（"/" 分隔，根书签为空串）。老数据无此字段时默认空，向后兼容
    #[serde(default)]
    folder: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct HistoryEntry {
    url: String,
    title: String,
    ts: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct SearchEngine {
    name: String,
    template: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Settings {
    engines: Vec<SearchEngine>,
    default_engine: String,
    theme: String,      // dark | light
    accent: String,     // purple | blue | green | orange
    newtab: String,     // internal（内页）| yuanbao（直接打开元宝搜索）
    auto_hide_sidebar: bool,
    // 竖直标签栏：false=顶部横向（夸克默认） true=左侧纵向（Tabbit 风格）
    #[serde(default)]
    vertical_tabs: bool,
    // 注意：历史版本前端写成 showBookmarkBar（少了 s），这里用 alias 兼容，避免老配置文件失效
    #[serde(default = "default_true", alias = "showBookmarkBar")]
    show_bookmarks_bar: bool,
    /// 新标签页内页是否显示时钟
    #[serde(default = "default_true")]
    show_clock: bool,
    /// 新标签页内页是否显示常用网站
    #[serde(default = "default_true")]
    show_shortcuts: bool,
    /// 界面密度：standard（标准）| compact（紧凑）
    #[serde(default = "default_density")]
    density: String,
    /// 新建标签页插入位置：end（末尾）| after（当前标签之后）
    #[serde(default = "default_new_tab_position")]
    new_tab_position: String,
    /// 标签栏右侧的「标签列表」按钮是否显示
    #[serde(default = "default_true")]
    show_tablist_btn: bool,
    /// 启动行为：restore（恢复上次会话）| newtab（打开新标签页）
    #[serde(default = "default_startup")]
    startup: String,
    /// 新标签页背景：plain | gray | gradient | dark
    #[serde(default = "default_newtab_bg")]
    newtab_bg: String,
    /// 地址栏自动补全：关闭后只有输入时才给建议
    #[serde(default = "default_true")]
    autocomplete: bool,
    /// 关闭标签页后激活哪个：right（右侧）| left（左侧）| newtab（新标签页）
    #[serde(default = "default_close_behavior")]
    close_tab_behavior: String,
    /// 主页按钮打开的内页 / 网址
    #[serde(default = "default_homepage")]
    homepage: String,
    /// 界面字号百分比：90 / 100 / 110 / 120
    #[serde(default = "default_font_size")]
    font_size: i32,
    /// 减少界面动效（关闭过渡与动画）
    #[serde(default)]
    reduce_motion: bool,
    /// 高对比度：加深边框与次要文字，方便弱视用户
    #[serde(default)]
    high_contrast: bool,
    /// 开机自动启动（写 HKCU Run 项）
    #[serde(default)]
    autostart: bool,
    /// 工具栏「扩展程序」按钮是否显示
    #[serde(default = "default_true")]
    show_ext_btn: bool,
    /// 工具栏「历史记录」按钮是否显示
    #[serde(default = "default_true")]
    show_history_btn: bool,
    /// 工具栏「下载」按钮是否显示（true 时常驻，下载中有徽标）
    #[serde(default = "default_true")]
    show_dl_btn: bool,
    /// 点击关闭按钮的行为：tray（最小化到任务栏托盘，默认）| exit（完全关闭）
    #[serde(default = "default_close_action")]
    close_action: String,
    /// 已选择「以后再说」的更新版本号，避免每次启动都提醒
    #[serde(default)]
    update_skipped: String,
}

fn default_close_action() -> String { "tray".into() }

fn default_newtab_url(settings: &Settings) -> String {
    // 「新建标签页打开内容」设置统一决定兜底页：internal=内置新标签页，
    // 其余（直接打开元宝搜索）才用 HOME_URL。关掉最后一个标签也走这里，
    // 绝不能在用户没改设置时落到第三方搜索引擎页面。
    if settings.newtab == "internal" {
        "trawl://newtab".to_string()
    } else {
        HOME_URL.to_string()
    }
}

fn default_true() -> bool { true }
fn default_density() -> String { "standard".into() }
fn default_new_tab_position() -> String { "end".into() }
fn default_startup() -> String { "restore".into() }
fn default_newtab_bg() -> String { "plain".into() }
fn default_close_behavior() -> String { "right".into() }
fn default_homepage() -> String { "trawl://newtab".into() }
fn default_font_size() -> i32 { 100 }

impl Default for Settings {
    fn default() -> Self {
        Settings {
            engines: vec![
                SearchEngine { name: "Bing".into(), template: "https://www.bing.com/search?q={query}".into() },
                SearchEngine { name: "搜狗搜索".into(), template: "https://www.sogou.com/web?query={query}".into() },
                SearchEngine { name: "百度".into(), template: "https://www.baidu.com/s?wd={query}".into() },
                SearchEngine { name: "Google".into(), template: "https://www.google.com/search?q={query}".into() },
            ],
            default_engine: "Bing".into(),
            theme: "light".into(),
            accent: "green".into(),
            newtab: "internal".into(),
            auto_hide_sidebar: false,
            vertical_tabs: false,
            show_bookmarks_bar: true,
            show_clock: true,
            show_shortcuts: true,
            density: default_density(),
            new_tab_position: default_new_tab_position(),
            show_tablist_btn: true,
            startup: default_startup(),
            newtab_bg: default_newtab_bg(),
            autocomplete: true,
            close_tab_behavior: default_close_behavior(),
            homepage: default_homepage(),
            font_size: default_font_size(),
            reduce_motion: false,
            high_contrast: false,
            autostart: false,
            show_ext_btn: true,
            show_history_btn: true,
            show_dl_btn: true,
            close_action: default_close_action(),
            update_skipped: String::new(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ExtensionItem {
    /// 扩展标识（内置扩展用固定 id，本地扩展用目录名）
    id: String,
    name: String,
    version: String,
    desc: String,
    enabled: bool,
    /// 内置插件：随浏览器提供，不可移除
    builtin: bool,
    /// 本地未打包扩展登记的目录（内置扩展为空）
    path: String,
    /// 插件配置（扩展页「配置」面板保存的键值，如关闭确认的阈值）
    #[serde(default)]
    cfg: serde_json::Map<String, serde_json::Value>,
}

impl Default for ExtensionItem {
    fn default() -> Self {
        ExtensionItem {
            id: String::new(),
            name: String::new(),
            version: "1.0.0".into(),
            desc: String::new(),
            enabled: true,
            builtin: false,
            path: String::new(),
            cfg: Default::default(),
        }
    }
}

/// 读取某个内置插件的开关与配置
fn builtin_ext(id: &str) -> Option<ExtensionItem> {
    load_json_list::<ExtensionItem>(EXTENSIONS_FILE)
        .unwrap_or_else(default_extensions)
        .into_iter()
        .find(|x| x.id == id)
}

/// 内置插件清单：首次打开扩展页时写盘，之后以 extensions.json 为准
fn default_extensions() -> Vec<ExtensionItem> {
    vec![
        ExtensionItem {
            id: "trawl-translate".into(),
            name: "网页翻译".into(),
            version: "1.2.0".into(),
            desc: "地址栏「文A」按钮或菜单「翻译此页」一键整页翻译，支持中英日韩；目标语言与打开方式在「配置」里调整。".into(),
            enabled: true,
            builtin: true,
            path: String::new(),
            cfg: Default::default(),
        },
        ExtensionItem {
            id: "trawl-close-guard".into(),
            name: "关闭确认".into(),
            version: "1.0.1".into(),
            desc: "关闭窗口且标签数达到阈值（可在「配置」里调整）时先弹确认框，避免误关整组标签。".into(),
            enabled: true,
            builtin: true,
            path: String::new(),
            cfg: Default::default(),
        },
        ExtensionItem {
            id: "trawl-reading".into(),
            name: "阅读模式".into(),
            version: "1.0.0".into(),
            desc: "菜单「阅读模式」抽取正文、去掉广告与侧栏，适合长文阅读；再点一次退出。".into(),
            enabled: false,
            builtin: true,
            path: String::new(),
            cfg: Default::default(),
        },
        ExtensionItem {
            id: "trawl-shortlink".into(),
            name: "链接净化".into(),
            version: "1.0.3".into(),
            desc: "复制网址时自动去掉 utm、spm 等跟踪参数（可在「配置」里保留 from/ref 来源参数）。".into(),
            enabled: true,
            builtin: true,
            path: String::new(),
            cfg: Default::default(),
        },
    ]
}

#[derive(Clone, Serialize)]
#[allow(non_snake_case)]
struct TabInfo {
    id: String,
    workspaceId: String,
    url: String,
    title: String,
    pinned: bool,
    folderId: Option<String>,
    loading: bool,
    zoom: f64,
}

#[derive(Clone, Serialize)]
#[allow(non_snake_case)]
struct FolderInfo {
    id: String,
    name: String,
    workspaceId: String,
}

#[derive(Clone, Serialize)]
struct WsInfo {
    id: String,
    name: String,
    icon: String,
}

#[derive(Clone, Serialize)]
#[allow(non_snake_case)]
struct StatePayload {
    tabs: Vec<TabInfo>,
    activeTab: Option<String>,
    workspaces: Vec<WsInfo>,
    activeWorkspace: String,
    glanceOpen: bool,
    split: Option<Vec<String>>,
    splitVertical: bool,
    folders: Vec<FolderInfo>,
    bookmarks: Vec<Bookmark>,
    settings: Settings,
    downloading: usize,
}

#[derive(Serialize, Deserialize)]
struct SavedTab {
    url: String,
    workspace_id: String,
    title: String,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    folder_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct SavedFolder {
    id: String,
    name: String,
    workspace_id: String,
}

#[derive(Serialize, Deserialize)]
struct SavedState {
    workspaces: Vec<SavedWorkspace>,
    active_workspace: String,
    tabs: Vec<SavedTab>,
    #[serde(default)]
    folders: Vec<SavedFolder>,
    active_tab_index: Option<usize>,
}

#[derive(Serialize, Deserialize)]
struct SavedWorkspace {
    id: String,
    name: String,
    icon: String,
}

struct AppState {
    tabs: Mutex<Vec<Tab>>,
    workspaces: Mutex<Vec<Workspace>>,
    active_tab: Mutex<Option<String>>,
    active_workspace: Mutex<String>,
    layout: Mutex<(f64, f64, f64, f64)>,
    split: Mutex<Option<(String, String)>>,
    split_vertical: Mutex<bool>, // false=左右分屏 true=上下分屏
    split_ratio: Mutex<f64>,     // 分屏比例（0.2-0.8）
    glance: Mutex<Option<String>>,
    folders: Mutex<Vec<Folder>>,
    bookmarks: Mutex<Vec<Bookmark>>,
    history: Mutex<Vec<HistoryEntry>>,
    downloads: Mutex<Vec<DownloadInfo>>,
    overlay: Mutex<Option<OverlayState>>,
    /// 通用右键菜单（ctxmenu.html）当前要显示的项
    ctx_items: Mutex<Vec<CtxMenuItem>>,
    /// 上次落盘时间，用于把高频状态变化节流成「最多每 2 秒写一次」
    last_save: Mutex<Option<Instant>>,
    settings: Mutex<Settings>,
    /// 处于阅读模式的标签（label -> true），再点一次退出并刷新页面
    reader_tabs: Mutex<HashMap<String, bool>>,
    /// 本地扩展 content_scripts 缓存（save_extensions / 启动时重建），
    /// 页面加载完成时按 match 模式注入，让本地扩展真实生效
    ext_scripts: Mutex<Vec<LocalExtScript>>,
    /// 已完成首次加载（至少触发过一次页面加载完成事件）的 webview。
    /// 新建的标签 webview 要等这个标记才显示——否则会先闪一帧
    /// WebView2 默认底色（黑屏闪烁的来源），再被页面内容覆盖。
    ready_webviews: Mutex<std::collections::HashSet<String>>,
    /// 移动端：打开的标签元信息（url/title），页面状态不保留（单 webview 架构）
    mob_tabs: Mutex<Vec<MobTab>>,
    /// 移动端：当前激活标签 id
    mob_active: Mutex<Option<String>>,
    /// 最近一次本机身份验证通过的时间戳（秒）。pw_reveal 据此判断是否在有效期内，
    /// 避免 pw_list 把明文密码一次性下发（安全边界放到后端）。
    pw_verified_at: Mutex<u64>,
    /// 下载取消令牌：下载 id -> 取消标志。download_cancel 置位，下载线程在读取循环里轮询退出
    cancel_tokens: Mutex<HashMap<String, std::sync::Arc<AtomicBool>>>,
    /// 最近关闭的普通标签 (url, title)，最多 20 条，供「重新打开已关闭标签」弹栈使用
    closed_tabs: Mutex<Vec<(String, String)>>,
}

/// 一个本地扩展解析出的可注入脚本
#[derive(Clone)]
struct LocalExtScript {
    id: String,
    matches: Vec<String>,
    js: String,
}

impl Default for AppState {
    fn default() -> Self {
        AppState {
            tabs: Mutex::new(Vec::new()),
            workspaces: Mutex::new(Vec::new()),
            active_tab: Mutex::new(None),
            active_workspace: Mutex::new(String::new()),
            layout: Mutex::new((0.0, 0.0, 800.0, 600.0)),
            split: Mutex::new(None),
            split_vertical: Mutex::new(false),
            split_ratio: Mutex::new(0.5),
            glance: Mutex::new(None),
            folders: Mutex::new(Vec::new()),
            bookmarks: Mutex::new(Vec::new()),
            history: Mutex::new(Vec::new()),
            downloads: Mutex::new(Vec::new()),
            overlay: Mutex::new(None),
            ctx_items: Mutex::new(Vec::new()),
            last_save: Mutex::new(None),
            settings: Mutex::new(Settings::default()),
            reader_tabs: Mutex::new(HashMap::new()),
            ext_scripts: Mutex::new(Vec::new()),
            ready_webviews: Mutex::new(std::collections::HashSet::new()),
            mob_tabs: Mutex::new(Vec::new()),
            mob_active: Mutex::new(None),
            pw_verified_at: Mutex::new(0),
            cancel_tokens: Mutex::new(HashMap::new()),
            closed_tabs: Mutex::new(Vec::new()),
        }
    }
}

// ==================== 工具函数 ====================

fn gen_id(prefix: &str) -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{}-{:x}", prefix, ts, std::process::id())
}

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn data_root() -> PathBuf {
    // 移动端：setup 时把应用私有目录写进 MOB_DATA_ROOT（USERPROFILE 在 Android 不存在）
    #[cfg(mobile)]
    if let Some(p) = MOB_DATA_ROOT.get() {
        let _ = std::fs::create_dir_all(p);
        return p.clone();
    }
    // Windows：%USERPROFILE%\AppData\LocalLow\Trawl（沿用历史位置）
    // macOS：~/Library/Application Support/Trawl
    // Linux：~/.local/share/Trawl（遵循 XDG）
    #[cfg(desktop)]
    let base = {
        #[cfg(windows)]
        {
            std::env::var("USERPROFILE")
                .map(|h| PathBuf::from(h).join("AppData").join("LocalLow").join("Trawl"))
                .unwrap_or_else(|_| std::env::temp_dir().join("com.trawl.browser"))
        }
        #[cfg(not(windows))]
        {
            let home = std::env::var("HOME").unwrap_or_default();
            if home.is_empty() {
                std::env::temp_dir().join("com.trawl.browser")
            } else if cfg!(target_os = "macos") {
                PathBuf::from(&home).join("Library").join("Application Support").join("Trawl")
            } else {
                // XDG_DATA_HOME 未设置时按默认 ~/.local/share
                match std::env::var("XDG_DATA_HOME") {
                    Ok(d) if !d.is_empty() => PathBuf::from(d).join("Trawl"),
                    _ => PathBuf::from(&home).join(".local").join("share").join("Trawl"),
                }
            }
        }
    };
    #[cfg(mobile)]
    let base = std::env::temp_dir().join("com.trawl.browser");
    let _ = std::fs::create_dir_all(&base);
    base
}

fn webview_data_dir() -> PathBuf {
    data_root().join("webview-data")
}

/// 本地资源协议根地址（所有平台统一入口）。
/// Tauri 内部规则（manager::get_app_url）：Windows/Android 是
/// `http://tauri.localhost/`，macOS/iOS/Linux 是 `tauri://localhost/`。
/// 之前桌面端硬编码了 http://tauri.localhost，导致 macOS 版 chrome/内容区
/// 全部无法加载（打开即白屏），必须按平台选择。
fn tauri_origin() -> &'static str {
    if cfg!(any(windows, target_os = "android")) {
        "http://tauri.localhost/"
    } else {
        "tauri://localhost/"
    }
}

/// 本地页面 URL 带版本号：WebView2 会把 tauri.localhost 资源持久缓存到磁盘，
/// 升级 exe 后旧 HTML 会被缓存复活（表现为「改了没生效」）。带 ?v= 后每次发版 URL 变化，绕开缓存。
fn versioned_url(path: &str) -> String {
    let sep = if path.contains('?') { '&' } else { '?' };
    format!("{}{}{}v={}", tauri_origin(), path, sep, env!("CARGO_PKG_VERSION"))
}

fn encode_query(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn looks_like_host(s: &str) -> bool {
    if s.is_empty() || s.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    if s.starts_with("localhost") {
        return true;
    }
    let host = s.split(['/', ':', '?', '#']).next().unwrap_or("");
    if host.split('.').count() == 4 && host.split('.').all(|p| p.parse::<u8>().is_ok()) {
        return true;
    }
    host.contains('.')
        && !host.ends_with('.')
        && host.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '.')
}

fn resolve_input(input: &str, settings: &Settings) -> String {
    let s = input.trim();
    if s.is_empty() {
        return HOME_URL.to_string();
    }
    if s.starts_with("http://") || s.starts_with("https://") {
        return s.to_string();
    }
    if looks_like_host(s) {
        return format!("https://{}", s);
    }
    let engine = settings
        .engines
        .iter()
        .find(|e| e.name == settings.default_engine)
        .or_else(|| settings.engines.first());
    let q = encode_query(s);
    // 兜底：引擎列表被外部改空 / 模板被清空时不能崩，落到元宝搜索
    let Some(engine) = engine else {
        return format!("https://www.bing.com/search?q={}", q);
    };
    if engine.template.is_empty() {
        return format!("https://www.bing.com/search?q={}", q);
    }
    if engine.template.contains("{query}") {
        engine.template.replace("{query}", &q)
    } else if engine.template.contains("%s") {
        engine.template.replace("%s", &q)
    } else {
        format!("{}{}", engine.template, q)
    }
}

#[cfg(desktop)]
fn content_bounds(state: &AppState) -> tauri::Rect {
    let (x, y, w, h) = *state.layout.lock().unwrap();
    tauri::Rect {
        position: LogicalPosition::new(x, y).into(),
        size: LogicalSize::new(w, h).into(),
    }
}

fn payload_of(state: &AppState) -> StatePayload {
    let tabs = state
        .tabs
        .lock()
        .unwrap()
        .iter()
        .map(|t| TabInfo {
            id: t.id.clone(),
            workspaceId: t.workspace_id.clone(),
            url: t.url.clone(),
            title: t.title.clone(),
            pinned: t.pinned,
            folderId: t.folder_id.clone(),
            loading: t.loading,
            zoom: t.zoom,
        })
        .collect();
    let workspaces = state
        .workspaces
        .lock()
        .unwrap()
        .iter()
        .map(|w| WsInfo {
            id: w.id.clone(),
            name: w.name.clone(),
            icon: w.icon.clone(),
        })
        .collect();
    let folders = state
        .folders
        .lock()
        .unwrap()
        .iter()
        .map(|f| FolderInfo {
            id: f.id.clone(),
            name: f.name.clone(),
            workspaceId: f.workspace_id.clone(),
        })
        .collect();
    let downloading = state
        .downloads
        .lock()
        .unwrap()
        .iter()
        .filter(|d| d.state == "started")
        .count();
    StatePayload {
        tabs,
        activeTab: state.active_tab.lock().unwrap().clone(),
        workspaces,
        activeWorkspace: state.active_workspace.lock().unwrap().clone(),
        glanceOpen: state.glance.lock().unwrap().is_some(),
        split: state
            .split
            .lock()
            .unwrap()
            .as_ref()
            .map(|(a, b)| vec![a.clone(), b.clone()]),
        splitVertical: *state.split_vertical.lock().unwrap(),
        folders,
        bookmarks: state.bookmarks.lock().unwrap().clone(),
        settings: state.settings.lock().unwrap().clone(),
        downloading,
    }
}

/// 只推给 chrome（"ui"）webview。
/// 不能再用 app.emit：它会把 payload eval 进每一个标签页 webview，
/// 等于把书签/设置塞进任意远程页面的 JS 环境，同时每次推送 N 次 eval。
/// 状态推送队列。
///
/// 为什么不能直接 emit：
/// `emit_to` 会对目标 webview 执行 `eval`，而 eval 要等主线程跑脚本。
/// 一旦在「主线程回调里」推状态（on_page_load / on_document_title_changed /
/// Ready 里创建 webview 期间），就会自己等自己 → 整个窗口卡死（Responding=False）。
/// 这里改成：调用方只投递信号，由独立线程顺序推送，并合并积压请求。
static STATE_TX: std::sync::OnceLock<std::sync::mpsc::Sender<()>> = std::sync::OnceLock::new();

fn start_state_worker(app: AppHandle) {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    if STATE_TX.set(tx).is_err() {
        return;
    }
    std::thread::spawn(move || {
        while rx.recv().is_ok() {
            // 合并积压：一次导航可能触发 3~4 次 push，只推最后一版快照
            while rx.try_recv().is_ok() {}
            let state = app.state::<AppState>();
            // 广播给所有 webview：chrome 渲染标签栏，新标签页等内页也要实时跟随设置/数据变化
            let _ = app.emit("state", payload_of(&state));
            drop(state);

            // 高频状态变化（跳转、标题变化）节流落盘，异常退出也不丢会话
            let need_save = {
                let st = app.state::<AppState>();
                let mut last = st.last_save.lock().unwrap();
                match *last {
                    Some(t) if t.elapsed() < Duration::from_secs(2) => false,
                    _ => {
                        *last = Some(Instant::now());
                        true
                    }
                }
            };
            if need_save {
                save_state(&app);
            }
        }
    });
}

fn push_state(_app: &AppHandle) {
    if let Some(tx) = STATE_TX.get() {
        let _ = tx.send(());
    }
}

// ==================== 后端接管下载 ====================

/// 目标路径若已存在则生成 "名字 (1).ext" 风格的新路径
#[cfg(desktop)]
fn unique_download_path(dest: &std::path::Path) -> std::path::PathBuf {
    use std::path::Path;
    if !dest.exists() {
        return dest.to_path_buf();
    }
    let dir = dest.parent().unwrap_or(Path::new(".")).to_path_buf();
    let stem = dest
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".into());
    let ext = dest
        .extension()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    for n in 1..1000u32 {
        let name = if ext.is_empty() {
            format!("{stem} ({n})")
        } else {
            format!("{stem} ({n}).{ext}")
        };
        let p = dir.join(name);
        if !p.exists() {
            return p;
        }
    }
    dir.join(format!("{stem}-{}.bin", now_ts()))
}

/// 后端线程下载：实时更新 DownloadInfo 并推送进度（裸 GET，不附加 Cookie/Referer）
#[cfg(desktop)]
fn spawn_download(app: AppHandle, url: String, dest: std::path::PathBuf) {
    std::thread::spawn(move || {
        let dest = unique_download_path(&dest);
        // 每个任务独立 id 与取消令牌：进度/失败都按 id 定位，同 URL 并发互不覆盖
        let id = gen_id("dl");
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        {
            let st = app.state::<AppState>();
            st.cancel_tokens.lock().unwrap().insert(id.clone(), cancel.clone());
            let mut list = st.downloads.lock().unwrap();
            list.push(DownloadInfo {
                url: url.clone(),
                state: "started".into(),
                path: dest.display().to_string(),
                received: 0,
                total: 0,
                ts: now_ts(),
                id: id.clone(),
                error: String::new(),
            });
            if list.len() > DOWNLOAD_CAP {
                let excess = list.len() - DOWNLOAD_CAP;
                list.drain(0..excess);
            }
        }
        push_state(&app);

        // 按 id 定位记录（不再按 url 反查），避免同 URL 并发下载互相覆盖
        let mark = |app: &AppHandle, state: &str, received: u64, total: u64, path: &std::path::Path| {
            let st = app.state::<AppState>();
            let mut list = st.downloads.lock().unwrap();
            if let Some(d) = list.iter_mut().find(|d| d.id == id) {
                d.state = state.into();
                d.received = received;
                d.total = total;
                d.path = path.display().to_string();
                d.ts = now_ts();
            }
        };
        // 失败收口：清理临时文件、写入错误分类、回收取消令牌、落 debug 日志
        let fail = |app: &AppHandle, reason: &str, path: &std::path::Path| {
            let _ = std::fs::remove_file(path);
            {
                let st = app.state::<AppState>();
                if let Some(d) = st.downloads.lock().unwrap().iter_mut().find(|d| d.id == id) {
                    d.state = "failed".into();
                    d.error = reason.to_string();
                    d.received = 0;
                    d.total = 0;
                    d.path = path.display().to_string();
                    d.ts = now_ts();
                }
                st.cancel_tokens.lock().unwrap().remove(&id);
            }
            debug_log(&format!("下载失败 url={} id={} 原因={}", url, id, reason));
            push_state(app);
        };

        let client = match reqwest::blocking::Client::builder()
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0")
            .connect_timeout(std::time::Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                debug_log(&format!("下载失败 url={} id={} 阶段=client.build code=- 原因=未知错误 详情={}", url, id, e));
                return fail(&app, "未知错误", &dest);
            }
        };
        // 裸 GET：不附加 Cookie / Referer（带身份的请求是暂缓项）
        let mut resp = match client.get(&url).send() {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                let status = r.status().as_u16();
                let reason = match status {
                    401 | 403 => "需要登录",
                    400..=499 => "客户端错误",
                    500..=599 => "服务器错误",
                    _ => "未知错误",
                };
                debug_log(&format!("下载失败 url={} id={} 阶段=http status={} 原因={}", url, id, status, reason));
                return fail(&app, reason, &dest);
            }
            Err(e) => {
                let reason = if e.is_timeout() { "超时" }
                    else if e.is_connect() { "连接失败" }
                    else { "未知错误" };
                debug_log(&format!("下载失败 url={} id={} 阶段=send code={:?} 原因={}", url, id, e.status(), reason));
                return fail(&app, reason, &dest);
            }
        };
        let total = resp.content_length().unwrap_or(0);
        mark(&app, "started", 0, total, &dest);
        push_state(&app);

        let mut file = match std::fs::File::create(&dest) {
            Ok(f) => f,
            Err(e) => {
                debug_log(&format!("下载失败 url={} id={} 阶段=File::create code={} 原因=写盘失败", url, id, e.raw_os_error().unwrap_or(-1)));
                return fail(&app, "写盘失败", &dest);
            }
        };
        use std::io::{Read, Write};
        let mut received: u64 = 0;
        let mut last_emit = std::time::Instant::now();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            // 取消令牌：download_cancel 置位后尽快退出并清理半成品
            if cancel.load(Ordering::Relaxed) {
                debug_log(&format!("下载已取消 url={} id={}", url, id));
                return fail(&app, "已取消", &dest);
            }
            match resp.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if let Err(e) = file.write_all(&buf[..n]) {
                        debug_log(&format!("下载失败 url={} id={} 阶段=write_all code={} 原因=写盘失败", url, id, e.raw_os_error().unwrap_or(-1)));
                        return fail(&app, "写盘失败", &dest);
                    }
                    received += n as u64;
                    if last_emit.elapsed().as_millis() >= 400 {
                        mark(&app, "started", received, total, &dest);
                        push_state(&app);
                        last_emit = std::time::Instant::now();
                    }
                }
                Err(e) => {
                    let reason = if e.kind() == std::io::ErrorKind::TimedOut { "超时" } else { "未知错误" };
                    debug_log(&format!("下载失败 url={} id={} 阶段=read code={} 原因={}", url, id, e.raw_os_error().unwrap_or(-1), reason));
                    return fail(&app, reason, &dest);
                }
            }
        }
        let _ = file.flush();
        // total == 0 表示服务器未给出长度（如 chunked），正常读到 EOF 即成功
        if total == 0 || received >= total {
            mark(&app, "done", received, total, &dest);
            app.state::<AppState>().cancel_tokens.lock().unwrap().remove(&id);
        } else {
            debug_log(&format!("下载失败 url={} id={} 阶段=length total={} received={} 原因=内容不完整", url, id, total, received));
            return fail(&app, "内容不完整", &dest);
        }
        push_state(&app);
    });
}

// ==================== 历史 / 书签 ====================

fn record_history(state: &AppState, url: &str, title: &str) {
    if url.starts_with("http") {
        let mut h = state.history.lock().unwrap();
        if h.last().map(|e| e.url == url).unwrap_or(false) {
            if let Some(e) = h.last_mut() {
                e.title = title.to_string();
                e.ts = now_ts();
            }
        } else {
            h.push(HistoryEntry {
                url: url.to_string(),
                title: title.to_string(),
                ts: now_ts(),
            });
            if h.len() > HISTORY_CAP {
                let excess = h.len() - HISTORY_CAP;
                h.drain(0..excess);
            }
        }
    }
}

fn save_bookmarks(state: &AppState) {
    let bookmarks = state.bookmarks.lock().unwrap().clone();
    if let Ok(json) = serde_json::to_string(&bookmarks) {
        if let Err(e) = write_atomic(&data_root().join(BOOKMARKS_FILE), &json) { debug_log(&format!("写盘失败: {}", e)); }
    }
}

fn save_history(state: &AppState) {
    let history = state.history.lock().unwrap().clone();
    if let Ok(json) = serde_json::to_string(&history) {
        if let Err(e) = write_atomic(&data_root().join(HISTORY_FILE), &json) { debug_log(&format!("写盘失败: {}", e)); }
    }
}

/// 原子写序号：给临时文件名加唯一后缀，避免多线程并发写同一文件时互相踩踏
static WRITE_SEQ: AtomicU32 = AtomicU32::new(0);

/// 原子写：先写同目录唯一临时文件再 rename 覆盖。
/// 直接 fs::write 覆盖时，若进程被杀 / 磁盘满，会留下半截 JSON——下次启动解析失败，
/// 书签、历史、密码、会话会整体丢回默认值。rename 是原子操作，要么旧内容要么新内容。
/// 返回 Result，调用方应处理失败（数据写盘失败必须让用户知道）。
fn write_atomic(path: &std::path::Path, data: &str) -> Result<(), String> {
    let seq = WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp.{}.{}", std::process::id(), seq));
    std::fs::write(&tmp, data).map_err(|e| format!("写临时文件失败 {}: {}", tmp.display(), e))?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        // 不回退到非原子直写——那会留下半截 JSON；删除临时文件并报错
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("原子替换失败 {}: {}", path.display(), e));
    }
    Ok(())
}

/// 数据文件损坏时保留现场：改名为 xxx.json.corrupt-<时间戳>，方便用户/排障找回内容，
/// 而不是被下一次写入直接覆盖掉。
fn backup_corrupt(path: &std::path::Path) {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("data.json")
        .to_string();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dst = path.with_file_name(format!("{}.corrupt-{}", name, ts));
    if std::fs::rename(path, &dst).is_ok() {
        debug_log(&format!("数据文件损坏，已备份：{:?}", dst));
    }
}

/// 读 JSON 文本，兼容 BOM 与 UTF-16 编码。
/// 用户用记事本改配置文件是常见操作：记事本「UTF-8」带 BOM、「Unicode」是 UTF-16 LE，
/// 直接按 UTF-8 读会解析失败 → 被当成损坏文件重置，用户数据全丢。
fn read_json_text(path: &std::path::Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let s = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        // UTF-8 BOM
        String::from_utf8_lossy(&bytes[3..]).into_owned()
    } else if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        // UTF-16 LE（记事本「Unicode」）
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        // UTF-16 BE
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let trimmed = s.trim_start_matches('\u{FEFF}');
    if trimmed.trim().is_empty() { None } else { Some(trimmed.to_string()) }
}

fn load_json_list<T: for<'de> Deserialize<'de>>(file: &str) -> Option<Vec<T>> {
    let path = data_root().join(file);
    let content = read_json_text(&path)?;
    match serde_json::from_str(&content) {
        Ok(v) => Some(v),
        Err(e) => {
            debug_log(&format!("解析 {} 失败：{}", file, e));
            backup_corrupt(&path);
            None
        }
    }
}

// ==================== 核心逻辑 ====================

/// 计算某个标签页当前的显示区域：分屏时按比例取半幅，否则全幅
#[cfg(desktop)]
fn bounds_for_tab(state: &AppState, tab_id: &str, active: &Option<String>) -> tauri::Rect {
    let (x, y, w, h) = *state.layout.lock().unwrap();
    let split = state.split.lock().unwrap().clone();
    let vertical = *state.split_vertical.lock().unwrap();
    let ratio = state.split_ratio.lock().unwrap().clone();
    let is_left = split.as_ref().map(|(a, _)| a == tab_id).unwrap_or(false);
    let is_right = split.as_ref().map(|(_, b)| b == tab_id).unwrap_or(false);
    let active_in_split = active
        .as_ref()
        .map(|a| split.as_ref().map(|(l, r)| l == a || r == a).unwrap_or(false))
        .unwrap_or(false);
    if active_in_split && (is_left || is_right) {
        if vertical {
            let half = h * ratio;
            if is_left {
                tauri::Rect {
                    position: LogicalPosition::new(x, y).into(),
                    size: LogicalSize::new(w, half).into(),
                }
            } else {
                tauri::Rect {
                    position: LogicalPosition::new(x, y + half).into(),
                    size: LogicalSize::new(w, h - half).into(),
                }
            }
        } else {
            let half = w * ratio;
            if is_left {
                tauri::Rect {
                    position: LogicalPosition::new(x, y).into(),
                    size: LogicalSize::new(half, h).into(),
                }
            } else {
                tauri::Rect {
                    position: LogicalPosition::new(x + half, y).into(),
                    size: LogicalSize::new(w - half, h).into(),
                }
            }
        }
    } else {
        content_bounds(state)
    }
}

#[tauri::command]
fn debug_msg(msg: String) {
    debug_log(&format!("UI: {}", msg));
}

#[tauri::command]
#[cfg(desktop)]
fn emit_shortcut(app: AppHandle, what: String) {
    // 同步命令跑在主线程，直接 emit_to（内部走 eval）会静默丢事件，
    // 必须丢到工作线程发送（与 push_state 的队列线程同理）
    let app2 = app.clone();
    std::thread::spawn(move || {
        let _ = app2.emit_to(CHROME_LABEL, "shortcut", what);
    });
}

fn debug_log(msg: &str) {
    use std::io::Write;
    let file = data_root().join("debug.log");
    // 单文件超过 512KB 就清空重写，避免长期使用后日志无限膨胀
    if let Ok(meta) = std::fs::metadata(&file) {
        if meta.len() > DEBUG_LOG_MAX {
            let _ = std::fs::write(&file, b"");
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(file) {
        let _ = writeln!(f, "[{}] {}", now_ts(), msg);
    }
}

/// 把 "trawl://xxx" 映射到内置资源；其余按外链处理
fn webview_url(url: &str) -> Result<WebviewUrl, String> {
    // 支持 trawl://settings#privacy-clear 之类的深链：hash 转成 ?h= 查询参数
    // 传给 settings.html（内部页通过 versioned_url 做缓存键控，不能直接挂 hash）
    if let Some(rest) = url.strip_prefix("trawl://settings") {
        // 按字符边界跳过开头的 '/'（深链形如 trawl://settings#privacy-clear）。
        // 直接用 &rest[1..] 会在首字符为多字节 UTF-8 时落在字符中间而 panic。
        let target = if rest.chars().count() > 1 {
            let skip = rest.chars().next().map(|c| c.len_utf8()).unwrap_or(0);
            versioned_url(&format!("settings.html?h={}", &rest[skip..]))
        } else {
            versioned_url("settings.html")
        };
        return target.parse().map(WebviewUrl::External).map_err(|e| format!("{:?}", e));
    }
    match url {
        "trawl://newtab" => versioned_url("newtab.html").parse().map(WebviewUrl::External).map_err(|e| format!("{:?}", e)),
        "trawl://extensions" => versioned_url("extensions.html").parse().map(WebviewUrl::External).map_err(|e| format!("{:?}", e)),
        "trawl://passwords" => versioned_url("passwords.html").parse().map(WebviewUrl::External).map_err(|e| format!("{:?}", e)),
        _ => url
            .parse::<tauri::Url>()
            .map(WebviewUrl::External)
            .map_err(|e| format!("无效网址 {}: {}", url, e)),
    }
}

/// 串行化 webview 创建。
/// 主线程和命令线程可能同时想给同一个标签建 webview（如启动瞬间 chrome 上报 layout），
/// 并发 add_child 会互相等锁，直接把整个窗口拖死。
static WEBVIEW_LOCK: Mutex<()> = Mutex::new(());

fn lock_webviews() -> std::sync::MutexGuard<'static, ()> {
    match WEBVIEW_LOCK.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// 专用「创建线程」队列。
///
/// `Window::add_child` 内部是 `run_on_main_thread(...)` + `rx.recv()` 阻塞等待，
/// 而命令处理跑在主线程上 —— 主线程里调它必然自己等自己，窗口立刻假死。
/// 但只丢到「随便一个工作线程」也不安全：两个线程同时 add_child 会互相等
/// （主线程一次只能处理一条构建消息），偶发整窗卡死。
/// 所以所有 webview 创建都排队给**同一个**线程串行执行。
type CreatorJob = Box<dyn FnOnce(&AppHandle) + Send>;

static CREATOR: std::sync::OnceLock<std::sync::mpsc::Sender<CreatorJob>> =
    std::sync::OnceLock::new();

/// 启动创建线程（在 setup 里调用一次）
#[cfg(desktop)]
fn start_creator(app: AppHandle) {
    let (tx, rx) = std::sync::mpsc::channel::<CreatorJob>();
    let _ = CREATOR.set(tx);
    std::thread::spawn(move || {
        while let Ok(f) = rx.recv() {
            // 单个任务 panic 不能带走整条线程，否则后面所有创建请求都石沉大海
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&app)));
        }
    });
}

/// 把创建动作丢进队列（非阻塞，可从主线程安全调用）
fn spawn_create<F>(app: &AppHandle, f: F)
where
    F: FnOnce(&AppHandle) + Send + 'static,
{
    let mut job: Option<CreatorJob> = Some(Box::new(f));
    if let Some(tx) = CREATOR.get() {
        if let Some(j) = job.take() {
            if tx.send(j).is_ok() {
                return;
            }
        }
    }
    // 创建线程还没起来（极早期）时兜底：直接开线程
    let app2 = app.clone();
    if let Some(j) = job {
        std::thread::spawn(move || j(&app2));
    }
}

/// 真正创建某个标签页的 webview。
/// 标签页现在是「懒创建」：会话恢复时只建数据，等第一次显示才建 webview，
/// 否则 30 个标签页 = 30 个 webview 同时抢 CPU 加载，启动会卡死。
#[cfg(desktop)]
fn spawn_tab_webview(app: &AppHandle, id: &str, url: &str) -> Result<(), String> {
    if app.get_webview(id).is_some() {
        return Ok(());
    }
    let _guard = lock_webviews();
    // 拿到锁之后要再确认一次，可能别的线程已经建好了
    if app.get_webview(id).is_some() {
        return Ok(());
    }
    let target = webview_url(url)?;
    let label = id.to_string();

    // ---- 标题变化 ----
    // 底色跟主题走：加载间隙露出的是这层底色而不是 WebView2 默认黑/白
    let bg = {
        let st = app.state::<AppState>();
        let dark = st.settings.lock().unwrap().theme == "dark";
        if dark {
            tauri::window::Color(0x15, 0x18, 0x1c, 0xff)
        } else {
            tauri::window::Color(0xff, 0xff, 0xff, 0xff)
        }
    };
    let builder = WebviewBuilder::new(&label, target)
        .data_directory(webview_data_dir())
        .background_color(bg)
        // 页面被点一下要通知 chrome（见 PAGE_BRIDGE 注释）
        .initialization_script(PAGE_BRIDGE);
    let builder = builder.on_document_title_changed(move |wv, title| {
        let app2 = wv.app_handle().clone();
        let lid = wv.label().to_string();
        {
            let st = app2.state::<AppState>();
            let mut tabs = st.tabs.lock().unwrap();
            if let Some(t) = tabs.iter_mut().find(|t| t.id == lid) {
                if !t.url.starts_with("trawl://") {
                    t.title = title;
                }
            }
        }
        push_state(&app2);
    });

    // ---- 加载状态 / 提交后的真实 URL ----
    let app_pl = app.clone();
    let builder = builder.on_page_load(move |wv, payload| {
        let lid = wv.label().to_string();
        let started = matches!(payload.event(), PageLoadEvent::Started);
        let real_url = payload.url().to_string();
        let is_http = real_url.starts_with("http://") || real_url.starts_with("https://");
        // 内置页（新标签页/设置）不覆盖 trawl:// 标识，也不写浏览历史
        let is_internal_page =
            real_url.contains("/newtab.html") || real_url.contains("/settings.html");
        let mut record: Option<(String, String)> = None;
        {
            let st = app_pl.state::<AppState>();
            let mut tabs = st.tabs.lock().unwrap();
            if let Some(t) = tabs.iter_mut().find(|t| t.id == lid) {
                t.loading = started;
                if is_http && !is_internal_page {
                    t.url = real_url.clone();
                    if !started {
                        record = Some((real_url.clone(), t.title.clone()));
                    }
                }
            }
        }
        if let Some((u, ti)) = record {
            let st = app_pl.state::<AppState>();
            record_history(&st, &u, &ti);
        }
        // 本地扩展 content_scripts：页面加载完成后按 match 模式注入
        if !started {
            inject_local_extensions(&app_pl, &lid, &real_url);
            // 首次加载完成才允许显示（黑屏闪烁修复）：把 webview 标记为就绪
            // 并立即按当前布局定位显示。is_internal_page 等内置页同样走这里。
            {
                let st = app_pl.state::<AppState>();
                let newly = st.ready_webviews.lock().unwrap().insert(lid.clone());
                if newly {
                    let active = st.active_tab.lock().unwrap().as_deref() == Some(lid.as_str());
                    if active {
                        show_active_tab(&app_pl, &st);
                    }
                }
            }
        }
        push_state(&app_pl);
    });

    // ---- target=_blank / window.open：转成本应用的新标签页 ----
    let app_nw = app.clone();
    let builder = builder.on_new_window(move |url, _features| {
        let app2 = app_nw.clone();
        let target = url.to_string();
        // 在 NewWindowRequested 回调里直接建 webview 会重入主线程，放到独立线程稍后执行
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            let st = app2.state::<AppState>();
            let ws = st.active_workspace.lock().unwrap().clone();
            let _ = create_tab(&app2, &st, target, ws, true);
        });
        NewWindowResponse::Deny
    });

    // ---- 下载 ----
    // 由后端全量接管：WebView2 的 DownloadOperation 在部分场景下会静默失败
    // （Requested 触发后文件始终不落盘，StateChanged 也不触发），因此这里返回
    // false 取消 WebView2 下载，改由 spawn_download 用 HTTP 下载并推送进度。
    let app_dl = app.clone();
    let builder = builder.on_download(move |_wv, event| {
        match event {
            DownloadEvent::Requested { url, destination } => {
                // 记录创建与 id 生成统一放到 spawn_download：进度按 id 定位，
                // 同 URL 并发下载互不覆盖（不再按 url 反查/去重）。
                spawn_download(app_dl.clone(), url.to_string(), destination.clone());
                push_state(&app_dl);
                false
            }
            _ => true,
        }
    });

    let window = app.get_window("main").ok_or("主窗口不存在")?;
    let (x, y, w, h) = { *app.state::<AppState>().layout.lock().unwrap() };
    window
        .add_child(builder, LogicalPosition::new(x, y), LogicalSize::new(w, h))
        .map_err(|e| format!("创建标签页失败: {}", e))?;
    // 先隐藏，交给 show_active_tab 在正确位置显示，避免所有标签同时闪一下
    if let Some(wv) = app.get_webview(&label) {
        let _ = wv.hide();
        debug_log(&format!(
            "tab webview created: asked label=`{}` real label=`{}`",
            label,
            wv.label()
        ));
    }
    Ok(())
}

/// 当前需要显示的标签（活动标签 + 分屏伙伴）
fn want_visible(state: &AppState) -> Vec<String> {
    let active = state.active_tab.lock().unwrap().clone();
    let split = state.split.lock().unwrap().clone();
    let mut want: Vec<String> = Vec::new();
    if let Some(a) = active {
        want.push(a);
    }
    if let Some((l, r)) = split {
        for id in [l, r] {
            if !want.contains(&id) {
                want.push(id);
            }
        }
    }
    want
}

#[cfg(desktop)]
fn missing_webview(app: &AppHandle, state: &AppState) -> bool {
    want_visible(state).iter().any(|id| app.get_webview(id).is_none())
}

/// 补建缺失的 webview。**只能在非主线程调用**。
#[cfg(desktop)]
fn spawn_missing_webviews(app: &AppHandle, state: &AppState) -> bool {
    let mut created = false;
    for id in want_visible(state) {
        if app.get_webview(&id).is_some() {
            continue;
        }
        let url = state
            .tabs
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.url.clone());
        if let Some(u) = url {
            match spawn_tab_webview(app, &id, &u) {
                Ok(()) => created = true,
                Err(e) => debug_log(&format!("spawn tab failed: {}", e)),
            }
        }
    }
    created
}

/// 刷新视图（必要时补建 webview）。
///
/// 铁律：`Window::add_child` 内部是 run_on_main_thread + rx.recv() 阻塞等待，
/// 所以在**主线程**里调用它必然自己等自己 → 整个窗口假死（表现为 Responding=False）。
/// 因此补建 webview 必须放到工作线程，主线程只负责投递。
#[cfg(desktop)]
fn refresh_view(app: &AppHandle) {
    spawn_create(app, |app| {
        let st = app.state::<AppState>();
        spawn_missing_webviews(app, &st);
        show_active_tab(app, &st);
    });
}

/// 纯定位：不动 webview 的创建，主线程可以放心调用（如窗口 resize、layout 上报）
#[cfg(desktop)]
fn show_active_tab(app: &AppHandle, state: &AppState) {
    let active = state.active_tab.lock().unwrap().clone();
    let active_ws = state.active_workspace.lock().unwrap().clone();
    let split = state.split.lock().unwrap().clone();
    let split_ids: Vec<String> = split
        .as_ref()
        .map(|(a, b)| vec![a.clone(), b.clone()])
        .unwrap_or_default();
    let active_in_split = active
        .as_ref()
        .map(|a| split_ids.iter().any(|x| x == a))
        .unwrap_or(false);

    let tabs: Vec<(String, String)> = state
        .tabs
        .lock()
        .unwrap()
        .iter()
        .map(|t| (t.id.clone(), t.workspace_id.clone()))
        .collect();
    for (id, ws) in tabs {
        let Some(wv) = app.get_webview(&id) else { continue };
        let is_active = active.as_deref() == Some(id.as_str()) && ws == active_ws;
        let is_partner = active_in_split && split_ids.iter().any(|x| x == &id) && ws == active_ws;
        if is_active || is_partner {
            // 只显示已完成首次加载的 webview：新创建的 webview 在页面画出第一帧
            // 之前先保持隐藏（ready_webviews），避免黑屏/白屏闪烁。
            let ready = state.ready_webviews.lock().unwrap().contains(&id);
            if ready {
                // 先定位再显示，避免从旧位置闪到新位置
                let _ = wv.set_bounds(bounds_for_tab(state, &id, &active));
                let _ = wv.show();
            }
        } else {
            let _ = wv.hide();
        }
    }

    let glance = state.glance.lock().unwrap().clone();
    if let Some(glabel) = glance {
        if let Some(gwv) = app.get_webview(&glabel) {
            let b = match &active {
                Some(a) => bounds_for_tab(state, a, &active),
                None => content_bounds(state),
            };
            let _ = gwv.set_bounds(b);
            let _ = gwv.show();
        }
    }
}

#[cfg(desktop)]
fn create_tab(
    app: &AppHandle,
    state: &AppState,
    url: String,
    ws_id: String,
    activate: bool,
) -> Result<String, String> {
    let id = gen_id("tab");
    let title = if url.starts_with("trawl://settings") {
        "设置".to_string()
    } else if url == "trawl://extensions" {
        "扩展程序".to_string()
    } else if url == "trawl://passwords" {
        "密码管理工具".to_string()
    } else if url.starts_with("trawl://") {
        "新标签页".to_string()
    } else {
        "加载中…".to_string()
    };
    state.tabs.lock().unwrap().push(Tab {
        id: id.clone(),
        workspace_id: ws_id,
        url,
        title,
        pinned: false,
        folder_id: None,
        loading: true,
        zoom: 1.0,
    });

    if activate {
        *state.active_tab.lock().unwrap() = Some(id.clone());
        // webview 懒创建，且必须离开当前线程（可能在主线程/命令线程上）
        refresh_view(app);
    }
    push_state(app);
    Ok(id)
}

#[cfg(desktop)]
fn close_tab(app: &AppHandle, id: &str) {
    let state = app.state::<AppState>();
    // 关标签后的继任者由设置决定：右侧 / 左侧 / 新标签页
    let behavior = state.settings.lock().unwrap().close_tab_behavior.clone();
    let active_ws = state.active_workspace.lock().unwrap().clone();
    let mut tabs = state.tabs.lock().unwrap();
    let mut removed: Option<String> = None;
    // 最近关闭的普通标签 (url, title)，用于「重新打开已关闭标签」
    let mut closed: Option<(String, String)> = None;
    let mut neighbor: Option<String> = None;
    if let Some(pos) = tabs.iter().position(|t| t.id == id) {
        // 先按「本工作区 + 未固定」排出可见顺序，再算左右邻居
        let pool: Vec<String> = tabs
            .iter()
            .filter(|t| t.workspace_id == active_ws && !t.pinned)
            .map(|t| t.id.clone())
            .collect();
        let ppos = pool.iter().position(|x| x == id).unwrap_or(0);
        let tab = tabs.remove(pos);
        removed = Some(tab.id.clone());
        // 内部页（trawl://）不入栈，避免「重新打开」塞回一堆设置/新标签页
        if !tab.url.starts_with("trawl://") {
            closed = Some((tab.url.clone(), tab.title.clone()));
        }
        neighbor = match behavior.as_str() {
            "left" => pool
                .get(ppos.wrapping_sub(1))
                .cloned()
                .or_else(|| pool.get(ppos + 1).cloned()),
            "newtab" => None,
            // 默认：优先右侧，没有右侧就退回左侧（和 Chrome 一致）
            _ => pool
                .get(ppos + 1)
                .cloned()
                .or_else(|| ppos.checked_sub(1).and_then(|i| pool.get(i).cloned())),
        };
    }
    drop(tabs);
    // 压入「最近关闭」栈，最多保留 20 条
    if let Some((url, title)) = closed {
        let mut stack = state.closed_tabs.lock().unwrap();
        stack.push((url, title));
        if stack.len() > 20 {
            let excess = stack.len() - 20;
            stack.drain(0..excess);
        }
    }
    if let Some(label) = removed {
        close_webview_later(app, label);
    }

    let mut split = state.split.lock().unwrap();
    if let Some((a, b)) = split.as_ref() {
        if a == id || b == id {
            *split = None;
        }
    }
    drop(split);

    let need_switch = state.active_tab.lock().unwrap().as_deref() == Some(id);
    if need_switch {
        *state.active_tab.lock().unwrap() = None;
        if let Some(next_id) = neighbor {
            *state.active_tab.lock().unwrap() = Some(next_id);
        } else if behavior == "newtab" {
            let _ = create_tab(app, &state, "trawl://newtab".to_string(), active_ws.clone(), true);
        } else {
            let url = {
                let s = state.settings.lock().unwrap();
                default_newtab_url(&s)
            };
            let _ = create_tab(app, &state, url, active_ws.clone(), true);
        }
        refresh_view(app);
    }
    push_state(app);
}

#[cfg(desktop)]
fn switch_workspace(app: &AppHandle, id: &str) {
    let state = app.state::<AppState>();
    if !state.workspaces.lock().unwrap().iter().any(|w| w.id == id) {
        return;
    }
    *state.active_workspace.lock().unwrap() = id.to_string();
    let last = state
        .tabs
        .lock()
        .unwrap()
        .iter()
        .filter(|t| t.workspace_id == id)
        .last()
        .map(|t| t.id.clone());
    match last {
        Some(tid) => {
            *state.active_tab.lock().unwrap() = Some(tid);
            refresh_view(app);
            push_state(app);
        }
        None => {
            let url = {
                let s = state.settings.lock().unwrap();
                default_newtab_url(&s)
            };
            let _ = create_tab(app, &state, url, id.to_string(), true);
        }
    }
}

// ==================== Tauri 命令 ====================

#[tauri::command]
fn initial_state(state: State<'_, AppState>) -> StatePayload {
    payload_of(&state)
}

#[tauri::command]
#[cfg(desktop)]
fn new_tab(app: AppHandle) {
    let state = app.state::<AppState>();
    let ws = state.active_workspace.lock().unwrap().clone();
    let (use_internal, after) = {
        let s = state.settings.lock().unwrap();
        (s.newtab == "internal", s.new_tab_position == "after")
    };
    let url = if use_internal { "trawl://newtab".to_string() } else { HOME_URL.to_string() };
    // 记住插入基准：create_tab 会把新标签挂到末尾，之后按设置挪到当前标签后面
    let prev = if after { state.active_tab.lock().unwrap().clone() } else { None };
    let Ok(id) = create_tab(&app, &state, url, ws, true) else { return };
    if let Some(prev_id) = prev {
        let mut tabs = state.tabs.lock().unwrap();
        if let Some(pos) = tabs.iter().position(|t| t.id == prev_id) {
            if let Some(cur) = tabs.iter().position(|t| t.id == id) {
                let t = tabs.remove(cur);
                let target = (pos + 1).min(tabs.len());
                tabs.insert(target, t);
            }
        }
        drop(tabs);
        push_state(&app);
    }
}

/// 打开内置页面（settings 等）为新标签页
/// 注入到每个页面 webview 的小桥：把「页面被按下」上报给 chrome。
/// chrome 是独立 webview，用户点在页面上时它既收不到 mousedown、也未必拿到失焦，
/// 结果是地址栏一直停在编辑态（全选高亮 + 完整 URL），建议面板也不收起。
const PAGE_BRIDGE: &str = r#"
(function () {
  if (window.__trawlBridge) return;
  window.__trawlBridge = true;
  var last = 0;
  document.addEventListener('mousedown', function () {
    var now = Date.now();
    if (now - last < 250) return;
    last = now;
    try { window.__TAURI_INTERNALS__.invoke('page_pressed'); } catch (e) {}
  }, true);
})();
"#;

#[tauri::command]
#[cfg(desktop)]
fn page_pressed(app: AppHandle) {
    let _ = app.emit_to(CHROME_LABEL, "page-pressed", ());
}

/// 打开内置页面（settings / extensions 等）为新标签页
#[tauri::command]
#[cfg(desktop)]
fn open_internal(app: AppHandle, page: String) {
    let state = app.state::<AppState>();
    let ws = state.active_workspace.lock().unwrap().clone();
    let url = format!("trawl://{}", page);
    let _ = create_tab(&app, &state, url, ws, true);
}

// ---- 批量操作 ----

#[tauri::command]
#[cfg(desktop)]
fn close_tabs(app: AppHandle, ids: Vec<String>) {
    for id in ids {
        close_tab(&app, &id);
    }
}

/// 「全部关闭」专用：一次性清空当前工作区的所有标签，并只开一个内置新标签页。
/// 老实现循环调用 close_tab，每关掉一个活动标签就自动补一个 HOME_URL（元宝搜索），
/// 面板自己又 open_internal 开一个，于是出现「元宝搜索 + 新标签页」两个标签。
#[tauri::command]
#[cfg(desktop)]
fn close_all_tabs_and_newtab(app: AppHandle) {
    let state = app.state::<AppState>();
    let ws = state.active_workspace.lock().unwrap().clone();
    let ids: Vec<String> = state
        .tabs
        .lock()
        .unwrap()
        .iter()
        .filter(|t| t.workspace_id == ws)
        .map(|t| t.id.clone())
        .collect();
    for id in &ids {
        close_webview_later(&app, id.clone());
    }
    state.tabs.lock().unwrap().retain(|t| t.workspace_id != ws);
    *state.split.lock().unwrap() = None;
    *state.active_tab.lock().unwrap() = None;
    let _ = create_tab(&app, &state, "trawl://newtab".to_string(), ws, true);
    push_state(&app);
}

#[tauri::command]
#[cfg(desktop)]
fn set_pins(app: AppHandle, ids: Vec<String>, pinned: bool) {
    let state = app.state::<AppState>();
    for t in state.tabs.lock().unwrap().iter_mut() {
        if ids.contains(&t.id) {
            t.pinned = pinned;
        }
    }
    push_state(&app);
}

/// 关闭其他标签页：保留指定标签，清掉同工作区的其余标签（不自动补新页）
#[tauri::command]
#[cfg(desktop)]
fn close_other_tabs(app: AppHandle, id: String) {
    let state = app.state::<AppState>();
    let ws = state.active_workspace.lock().unwrap().clone();
    let keep_exists = state.tabs.lock().unwrap().iter().any(|t| t.id == id);
    if !keep_exists {
        return;
    }
    *state.active_tab.lock().unwrap() = Some(id.clone());
    let ids: Vec<String> = state
        .tabs
        .lock()
        .unwrap()
        .iter()
        .filter(|t| t.workspace_id == ws && t.id != id)
        .map(|t| t.id.clone())
        .collect();
    for tid in &ids {
        close_webview_later(&app, tid.clone());
    }
    state.tabs.lock().unwrap().retain(|t| !ids.contains(&t.id));
    *state.split.lock().unwrap() = None;
    refresh_view(&app);
    push_state(&app);
}

/// 复制标签页：在原标签右侧开一个同地址的新标签
#[tauri::command]
#[cfg(desktop)]
fn duplicate_tab(app: AppHandle, id: String) {
    let state = app.state::<AppState>();
    let found = state
        .tabs
        .lock()
        .unwrap()
        .iter()
        .find(|t| t.id == id)
        .map(|t| (t.url.clone(), t.workspace_id.clone()));
    let Some((url, ws)) = found else { return };
    let Ok(new_id) = create_tab(&app, &state, url, ws, true) else { return };
    // 插到原标签后面，而不是永远排在末尾
    {
        let mut tabs = state.tabs.lock().unwrap();
        if let Some(pos) = tabs.iter().position(|t| t.id == id) {
            if let Some(cur) = tabs.iter().position(|t| t.id == new_id) {
                let t = tabs.remove(cur);
                let target = if cur < pos { pos } else { pos + 1 };
                let target = target.min(tabs.len());
                tabs.insert(target, t);
            }
        }
    }
    push_state(&app);
}

/// 分屏比例调节（delta 正负 0.05），返回是否生效
#[tauri::command]
#[cfg(desktop)]
fn adjust_split_ratio(app: AppHandle, delta: f64) {
    let state = app.state::<AppState>();
    {
        let mut ratio = state.split_ratio.lock().unwrap();
        *ratio = (*ratio + delta).clamp(0.2, 0.8);
    }
    refresh_view(&app);
}

#[tauri::command]
#[cfg(desktop)]
fn activate_tab(app: AppHandle, id: String) {
    let state = app.state::<AppState>();
    *state.active_tab.lock().unwrap() = Some(id);
    refresh_view(&app);
    spawn_create(&app, |app| close_overlay_inner(app));
    push_state(&app);
}

#[tauri::command]
#[cfg(desktop)]
fn close_tab_cmd(app: AppHandle, id: String) {
    close_tab(&app, &id);
}

#[tauri::command]
#[allow(non_snake_case)]
fn move_tab(app: AppHandle, id: String, beforeId: Option<String>, pinned: bool) {
    let state = app.state::<AppState>();
    let mut tabs = state.tabs.lock().unwrap();
    let pos = match tabs.iter().position(|t| t.id == id) {
        Some(p) => p,
        None => return,
    };
    let mut tab = tabs.remove(pos);
    tab.pinned = pinned;
    let insert_at = match &beforeId {
        Some(b) => tabs.iter().position(|t| t.id == *b).unwrap_or(tabs.len()),
        None => tabs.len(),
    };
    tabs.insert(insert_at, tab);
    drop(tabs);
    push_state(&app);
}

#[tauri::command]
#[cfg(desktop)]
fn toggle_pin(app: AppHandle, id: String) {
    let state = app.state::<AppState>();
    let mut tabs = state.tabs.lock().unwrap();
    let Some(pos) = tabs.iter().position(|t| t.id == id) else { return };
    let mut tab = tabs.remove(pos);
    tab.pinned = !tab.pinned;
    if tab.pinned {
        // 固定后要挪到固定区末尾，否则每次渲染顺序都不对
        let insert_at = tabs.iter().rposition(|t| t.pinned && t.workspace_id == tab.workspace_id);
        match insert_at {
            Some(i) => tabs.insert(i + 1, tab),
            None => {
                let first = tabs
                    .iter()
                    .position(|t| t.workspace_id == tab.workspace_id)
                    .unwrap_or(0);
                tabs.insert(first, tab);
            }
        }
    } else {
        tabs.push(tab);
    }
    drop(tabs);
    push_state(&app);
}

// ---- 文件夹 ----

#[tauri::command]
#[allow(non_snake_case)]
fn create_folder(app: AppHandle, name: String, workspaceId: Option<String>) -> String {
    let state = app.state::<AppState>();
    let ws = workspaceId.unwrap_or_else(|| state.active_workspace.lock().unwrap().clone());
    let id = gen_id("folder");
    state.folders.lock().unwrap().push(Folder {
        id: id.clone(),
        name: if name.trim().is_empty() { "新文件夹".into() } else { name.trim().to_string() },
        workspace_id: ws,
    });
    push_state(&app);
    // 返回真实 id：前端以前靠 setTimeout(150) 猜「最后一个文件夹」，并发时会挂错标签
    id
}

#[tauri::command]
#[cfg(desktop)]
fn delete_folder(app: AppHandle, id: String) {
    let state = app.state::<AppState>();
    state.folders.lock().unwrap().retain(|f| f.id != id);
    for t in state.tabs.lock().unwrap().iter_mut() {
        if t.folder_id.as_deref() == Some(id.as_str()) {
            t.folder_id = None;
        }
    }
    push_state(&app);
}

#[tauri::command]
#[cfg(desktop)]
fn rename_folder(app: AppHandle, id: String, name: String) {
    let state = app.state::<AppState>();
    if let Some(f) = state.folders.lock().unwrap().iter_mut().find(|f| f.id == id) {
        if !name.trim().is_empty() {
            f.name = name.trim().to_string();
        }
    }
    push_state(&app);
}

#[tauri::command]
#[allow(non_snake_case)]
fn move_tab_to_folder(app: AppHandle, id: String, folderId: Option<String>) {
    let state = app.state::<AppState>();
    if let Some(t) = state.tabs.lock().unwrap().iter_mut().find(|t| t.id == id) {
        t.folder_id = folderId;
        t.pinned = false;
    }
    push_state(&app);
}

// ---- Glance ----

#[tauri::command]
#[cfg(desktop)]
fn open_glance(app: AppHandle, id: Option<String>, url: Option<String>) {
    // add_child 必须离开主线程，见 spawn_create 的注释
    spawn_create(&app, move |app| open_glance_inner(app, id, url));
}

#[cfg(desktop)]
fn open_glance_inner(app: &AppHandle, id: Option<String>, url: Option<String>) {
    let state = app.state::<AppState>();
    let target = match (&id, &url) {
        (Some(tid), _) => state
            .tabs
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.id == *tid)
            .map(|t| t.url.clone()),
        (_, Some(u)) => Some(u.clone()),
        _ => None,
    };
    let Some(target) = target else { return };
    let parsed: tauri::Url = match target.parse() {
        Ok(u) => u,
        Err(_) => return,
    };

    {
        let mut glance = state.glance.lock().unwrap();
        if let Some(old) = glance.take() {
            if let Some(wv) = app.get_webview(&old) {
                let _ = wv.close();
            }
        }
    }

    let label = gen_id("glance");
    let builder = WebviewBuilder::new(&label, WebviewUrl::External(parsed))
        .data_directory(webview_data_dir());
    let window = match app.get_window("main") {
        Some(w) => w,
        None => return,
    };
    let active = state.active_tab.lock().unwrap().clone();
    let bounds = match &active {
        Some(a) => bounds_for_tab(&state, a, &active),
        None => content_bounds(&state),
    };
    if window
        .add_child(builder, LogicalPosition::new(0.0, 0.0), LogicalSize::new(100.0, 100.0))
        .is_err()
    {
        return;
    }
    if let Some(wv) = app.get_webview(&label) {
        let _ = wv.set_bounds(bounds);
        let _ = wv.show();
    }
    *state.glance.lock().unwrap() = Some(label);
    push_state(app);
}

#[tauri::command]
#[cfg(desktop)]
fn close_glance(app: AppHandle) {
    let state = app.state::<AppState>();
    let label = state.glance.lock().unwrap().take();
    if let Some(l) = label {
        close_webview_later(&app, l);
        show_active_tab(&app, &state);
    }
    push_state(&app);
}

/// 把当前 Glance 转为正式标签页
#[tauri::command]
#[cfg(desktop)]
fn promote_glance(app: AppHandle) {
    let state = app.state::<AppState>();
    let label = state.glance.lock().unwrap().take();
    let Some(l) = label else { return };
    let url = app.get_webview(&l).and_then(|wv| wv.url().ok().map(|u| u.to_string()));
    close_webview_later(&app, l);
    if let Some(u) = url {
        let ws = state.active_workspace.lock().unwrap().clone();
        let _ = create_tab(&app, &state, u, ws, true);
    } else {
        push_state(&app);
    }
}

// ---- 分屏 ----

#[tauri::command]
#[cfg(desktop)]
fn toggle_split(app: AppHandle, id: String) {
    let state = app.state::<AppState>();
    let active = state.active_tab.lock().unwrap().clone();
    let Some(a) = active else { return };
    if a == id {
        return;
    }
    let mut split = state.split.lock().unwrap();
    let already = split.as_ref().map(|(l, r)| l == &a && r == &id).unwrap_or(false);
    if already {
        *split = None;
    } else {
        *split = Some((a, id));
    }
    drop(split);
    refresh_view(&app);
    push_state(&app);
}

#[tauri::command]
#[cfg(desktop)]
fn exit_split(app: AppHandle) {
    let state = app.state::<AppState>();
    *state.split.lock().unwrap() = None;
    refresh_view(&app);
    push_state(&app);
}

/// 分屏方向切换：左右 ↔ 上下
#[tauri::command]
#[cfg(desktop)]
fn toggle_split_orientation(app: AppHandle) {
    let state = app.state::<AppState>();
    // 只取一次 guard：同一语句内两次 lock 同一个 Mutex 会自锁（历史 bug）
    {
        let mut vertical = state.split_vertical.lock().unwrap();
        *vertical = !*vertical;
    }
    refresh_view(&app);
    push_state(&app);
}

// ---- 书签 ----

#[tauri::command]
fn toggle_bookmark(app: AppHandle, url: String, title: String) {
    let state = app.state::<AppState>();
    let mut bookmarks = state.bookmarks.lock().unwrap();
    if let Some(pos) = bookmarks.iter().position(|b| b.url == url) {
        bookmarks.remove(pos);
    } else {
        bookmarks.push(Bookmark { url, title, folder: String::new() });
        bookmarks.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    }
    drop(bookmarks);
    save_bookmarks(&state);
    push_state(&app);
}

#[tauri::command]
fn remove_bookmark(app: AppHandle, url: String) {
    let state = app.state::<AppState>();
    state.bookmarks.lock().unwrap().retain(|b| b.url != url);
    save_bookmarks(&state);
    push_state(&app);
}

/// 批量删除书签：一次写盘，避免逐条删除时重复 IO（全选几百条会明显卡顿）
#[tauri::command]
fn remove_bookmarks_batch(app: AppHandle, urls: Vec<String>) -> usize {
    let state = app.state::<AppState>();
    let url_set: std::collections::HashSet<String> = urls.into_iter().collect();
    // 在同一次持锁内完成「记旧长度 → 删除 → 算差值」，避免中途其它线程增改导致计数下溢
    let removed = {
        let mut bookmarks = state.bookmarks.lock().unwrap();
        let before = bookmarks.len();
        bookmarks.retain(|b| !url_set.contains(&b.url));
        before.saturating_sub(bookmarks.len())
    };
    save_bookmarks(&state);
    push_state(&app);
    removed
}

// ---- 历史 ----

#[tauri::command]
fn get_history(state: State<'_, AppState>) -> Vec<HistoryEntry> {
    let mut h = state.history.lock().unwrap().clone();
    h.reverse();
    h
}

#[tauri::command]
fn clear_history(app: AppHandle) {
    let state = app.state::<AppState>();
    state.history.lock().unwrap().clear();
    save_history(&state);
    push_state(&app);
}

/// 删除单条历史记录（供地址栏建议逐条清除使用）
#[tauri::command]
fn remove_history_entry(app: AppHandle, url: String) {
    let state = app.state::<AppState>();
    let before = { state.history.lock().unwrap().len() };
    {
        let mut h = state.history.lock().unwrap();
        h.retain(|e| e.url != url);
    }
    if { state.history.lock().unwrap().len() } != before {
        save_history(&state);
        push_state(&app);
    }
}

// ---- 页内查找 ----

#[tauri::command]
#[cfg(desktop)]
fn find_in_page(app: AppHandle, text: String) {
    let state = app.state::<AppState>();
    let active = state.active_tab.lock().unwrap().clone();
    if let Some(id) = active {
        if let Some(wv) = app.get_webview(&id) {
            let escaped = text.replace('\\', "\\\\").replace('\'', "\\'");
            let _ = wv.eval(&format!("window.find('{}', false, true, true)", escaped));
        }
    }
}

// ---- 工作区 ----

#[tauri::command]
#[cfg(desktop)]
fn new_workspace(app: AppHandle) {
    let state = app.state::<AppState>();
    let id = gen_id("ws");
    state.workspaces.lock().unwrap().push(Workspace {
        id: id.clone(),
        name: "新工作区".to_string(),
        icon: "🌐".to_string(),
    });
    drop(state);
    switch_workspace(&app, &id);
}

#[tauri::command]
#[cfg(desktop)]
fn switch_workspace_cmd(app: AppHandle, id: String) {
    switch_workspace(&app, &id);
}

#[tauri::command]
#[cfg(desktop)]
fn close_workspace(app: AppHandle, id: String) {
    let state = app.state::<AppState>();
    if state.workspaces.lock().unwrap().len() <= 1 {
        return;
    }
    let idx = state
        .workspaces
        .lock()
        .unwrap()
        .iter()
        .position(|w| w.id == id);
    if let Some(idx) = idx {
        let tab_ids: Vec<String> = state
            .tabs
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.workspace_id == id)
            .map(|t| t.id.clone())
            .collect();
        let folder_ids: Vec<String> = state
            .folders
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f.workspace_id == id)
            .map(|f| f.id.clone())
            .collect();
        drop(state);
        for tid in tab_ids {
            close_tab(&app, &tid);
        }
        let state = app.state::<AppState>();
        state.folders.lock().unwrap().retain(|f| !folder_ids.contains(&f.id));
        state.workspaces.lock().unwrap().remove(idx);
        let was_active = state.active_workspace.lock().unwrap().clone() == id;
        if was_active {
            let fallback = state
                .workspaces
                .lock()
                .unwrap()
                .get(idx.saturating_sub(1))
                .map(|w| w.id.clone());
            drop(state);
            if let Some(fid) = fallback {
                switch_workspace(&app, &fid);
            }
        } else {
            push_state(&app);
        }
    }
}

/// 关闭分组：关掉该组内所有标签页，但保留分组本身（组里留一个空白新标签页）
#[tauri::command]
#[cfg(desktop)]
fn close_group_tabs(app: AppHandle, id: String) {
    let state = app.state::<AppState>();
    let tab_ids: Vec<String> = state
        .tabs
        .lock()
        .unwrap()
        .iter()
        .filter(|t| t.workspace_id == id)
        .map(|t| t.id.clone())
        .collect();
    for tid in tab_ids {
        close_tab(&app, &tid);
    }
    let state = app.state::<AppState>();
    // 组空了以后补一个空白新标签页，避免切过去是一片空白
    let empty = !state.tabs.lock().unwrap().iter().any(|t| t.workspace_id == id);
    if empty {
        let _ = create_tab(&app, &state, "trawl://newtab".to_string(), id, true);
    }
    push_state(&app);
}

/// 重命名工作区（标签分组面板顶部的「为分组命名」）
#[tauri::command]
#[cfg(desktop)]
fn rename_workspace(app: AppHandle, id: String, name: String) {
    let state = app.state::<AppState>();
    let name = name.trim().to_string();
    if name.is_empty() { return; }
    {
        let mut workspaces = state.workspaces.lock().unwrap();
        if let Some(ws) = workspaces.iter_mut().find(|w| w.id == id) {
            ws.name = name;
        } else {
            return;
        }
    }
    save_state(&app);
    push_state(&app);
}

/// 取消分组：保留组内标签，把它们并入另一个工作区后删掉当前分组。
/// 与「删除分组」(close_workspace) 的区别是后者会把组内标签一起关掉。
#[tauri::command]
#[cfg(desktop)]
fn ungroup_workspace(app: AppHandle, id: String) {
    let state = app.state::<AppState>();
    let target = {
        let workspaces = state.workspaces.lock().unwrap();
        if workspaces.len() <= 1 { return; }
        workspaces.iter().find(|w| w.id != id).map(|w| w.id.clone())
    };
    let Some(target) = target else { return };
    {
        let mut tabs = state.tabs.lock().unwrap();
        for t in tabs.iter_mut() {
            if t.workspace_id == id { t.workspace_id = target.clone(); }
        }
        let mut folders = state.folders.lock().unwrap();
        for f in folders.iter_mut() {
            if f.workspace_id == id { f.workspace_id = target.clone(); }
        }
    }
    state.workspaces.lock().unwrap().retain(|w| w.id != id);
    if *state.active_workspace.lock().unwrap() == id {
        *state.active_workspace.lock().unwrap() = target;
    }
    save_state(&app);
    push_state(&app);
    refresh_view(&app);
}

#[tauri::command]
#[cfg(desktop)]
fn navigate(app: AppHandle, input: String) {
    let state = app.state::<AppState>();
    let settings = state.settings.lock().unwrap().clone();
    let target = resolve_input(&input, &settings);
    let active = state.active_tab.lock().unwrap().clone();
    match active {
        Some(id) => {
            {
                let mut tabs = state.tabs.lock().unwrap();
                if let Some(t) = tabs.iter_mut().find(|t| t.id == id) {
                    t.url = target.clone();
                    t.loading = true;
                }
            }
            let url: tauri::Url = match target.parse() {
                Ok(u) => u,
                Err(e) => {
                    debug_log(&format!("navigate 解析失败 {}: {}", target, e));
                    return;
                }
            };
            match app.get_webview(&id) {
                Some(wv) => {
                    let _ = wv.navigate(url);
                }
                None => {
                    // 懒创建：该标签还没有 webview，交给工作线程补建
                    refresh_view(&app);
                }
            }
            push_state(&app);
        }
        None => {
            let ws = state.active_workspace.lock().unwrap().clone();
            let _ = create_tab(&app, &state, target, ws, true);
        }
    }
}

#[tauri::command]
#[cfg(desktop)]
fn nav_back(app: AppHandle) {
    let state = app.state::<AppState>();
    let active = state.active_tab.lock().unwrap().clone();
    if let Some(id) = active {
        if let Some(wv) = app.get_webview(&id) {
            let _ = wv.eval("history.back()");
        }
    }
}

#[tauri::command]
#[cfg(desktop)]
fn nav_forward(app: AppHandle) {
    let state = app.state::<AppState>();
    let active = state.active_tab.lock().unwrap().clone();
    if let Some(id) = active {
        if let Some(wv) = app.get_webview(&id) {
            let _ = wv.eval("history.forward()");
        }
    }
}

#[tauri::command]
#[cfg(desktop)]
fn stop_loading(app: AppHandle) {
    let state = app.state::<AppState>();
    let active = state.active_tab.lock().unwrap().clone();
    if let Some(id) = active {
        if let Some(wv) = app.get_webview(&id) {
            let _ = wv.eval("try{window.stop()}catch(e){}");
        }
    }
}

#[tauri::command]
#[cfg(desktop)]
fn nav_reload(app: AppHandle) {
    let state = app.state::<AppState>();
    let active = state.active_tab.lock().unwrap().clone();
    if let Some(id) = active {
        if let Some(wv) = app.get_webview(&id) {
            let _ = wv.reload();
        }
    }
}

#[tauri::command]
#[cfg(desktop)]
fn layout(app: AppHandle, x: f64, y: f64, width: f64, height: f64) {
    let state = app.state::<AppState>();
    *state.layout.lock().unwrap() = (x, y, width, height);
    if missing_webview(&app, &state) {
        refresh_view(&app);
    } else {
        show_active_tab(&app, &state);
    }
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn save_settings(app: AppHandle, settings: Settings) {
    let state = app.state::<AppState>();
    // 校验：丢弃空名称/空模板的引擎（用户手滑或外部改文件），确保至少有一个可用引擎
    let mut settings = settings;
    settings
        .engines
        .retain(|e| !e.name.trim().is_empty() && !e.template.trim().is_empty());
    if settings.engines.is_empty() {
        settings.engines = Settings::default().engines;
    }
    // 校验：默认引擎必须在列表中
    if !settings.engines.iter().any(|e| e.name == settings.default_engine) {
        settings.default_engine = settings.engines.first().map(|e| e.name.clone()).unwrap_or_default();
    }
    *state.settings.lock().unwrap() = settings.clone();
    let file = data_root().join(SETTINGS_FILE);
    if let Ok(json) = serde_json::to_string(&settings) {
        if let Err(e) = write_atomic(&file, &json) { debug_log(&format!("写盘失败: {}", e)); }
    }
    push_state(&app);
}

/// 恢复全部设置到出厂状态（「重置设置」页面用）
#[tauri::command]
fn reset_settings(app: AppHandle) -> Settings {
    let state = app.state::<AppState>();
    let fresh = Settings::default();
    *state.settings.lock().unwrap() = fresh.clone();
    if let Ok(json) = serde_json::to_string(&fresh) {
        if let Err(e) = write_atomic(&data_root().join(SETTINGS_FILE), &json) { debug_log(&format!("写盘失败: {}", e)); }
    }
    push_state(&app);
    fresh
}

/// 开机自启：直接写 HKCU 的 Run 项，不额外引第三方插件
#[tauri::command]
#[cfg(desktop)]
fn set_autostart(app: AppHandle, enable: bool) {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    #[cfg(windows)]
    {
        // 直接写注册表，避免 reg.exe 子进程在某些环境被拦截（返回 code=1、值写成空串）
        let result = (|| -> Result<(), String> {
            use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};
            use winreg::RegKey;
            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            let key = r"Software\Microsoft\Windows\CurrentVersion\Run";
            let (k, _) = hkcu
                .create_subkey_with_flags(key, KEY_READ | KEY_SET_VALUE)
                .map_err(|e| format!("open key: {}", e))?;
            if enable {
                k.set_value("Trawl", &exe)
                    .map_err(|e| format!("set value: {}", e))?;
            } else {
                let _ = k.delete_value("Trawl");
            }
            Ok(())
        })();
        match result {
            Ok(()) => debug_log(&format!("autostart enable={} exe=`{}` ok", enable, exe)),
            Err(e) => debug_log(&format!("autostart enable={} failed: {}", enable, e)),
        }
    }
    let state = app.state::<AppState>();
    let mut s = state.settings.lock().unwrap().clone();
    s.autostart = enable;
    *state.settings.lock().unwrap() = s.clone();
    if let Ok(json) = serde_json::to_string(&s) {
        if let Err(e) = write_atomic(&data_root().join(SETTINGS_FILE), &json) { debug_log(&format!("写盘失败: {}", e)); }
    }
    push_state(&app);
}

/// 打开 Windows「默认应用」设置页，方便把 Trawl 设为默认浏览器
#[tauri::command]
#[cfg(desktop)]
fn open_default_apps() {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", "ms-settings:defaultapps"])
            .creation_flags(0x08000000)
            .spawn();
    }
}

// ==================== 扩展程序 ====================

#[tauri::command]
fn get_extensions() -> Vec<ExtensionItem> {
    load_json_list::<ExtensionItem>(EXTENSIONS_FILE).unwrap_or_else(default_extensions)
}

#[tauri::command]
fn save_extensions(app: AppHandle, list: Vec<ExtensionItem>) {
    if let Ok(json) = serde_json::to_string(&list) {
        if let Err(e) = write_atomic(&data_root().join(EXTENSIONS_FILE), &json) { debug_log(&format!("写盘失败: {}", e)); }
    }
    // 启停 / 登记变化后重建本地扩展脚本缓存
    reload_ext_scripts(&app);
}

/// 阅读模式注入脚本：抽出正文（article/main/[role=main]/body）重排为干净的阅读版。
/// __W__ 会被替换成插件配置的正文宽度（680/820/960）。
const READER_JS: &str = r#"(() => {
  const root = document.querySelector('article') || document.querySelector('main')
    || document.querySelector('[role="main"]') || document.body;
  const title = document.title || '';
  const html = root.innerHTML;
  document.open();
  document.write('<!DOCTYPE html><html><head><meta charset="utf-8"><title>' + title
    + ' · 阅读模式</title><style>'
    + 'body{margin:0;background:#faf9f5;color:#232323}'
    + 'main{max-width:__W__px;margin:0 auto;padding:52px 24px 80px;'
    + 'font:16px/1.9 -apple-system,"Segoe UI","Microsoft YaHei",sans-serif}'
    + 'h1{font-size:26px;line-height:1.4;margin:0 0 8px}'
    + '.trawl-reader-meta{color:#8a8a80;font-size:13px;border-bottom:1px solid #e5e2d8;padding-bottom:14px;margin-bottom:26px}'
    + 'img,video{max-width:100%;height:auto;border-radius:6px}'
    + 'pre{overflow:auto;background:#f1efe6;padding:12px;border-radius:8px;font-size:13px}'
    + 'a{color:#2f7d5d}blockquote{border-left:3px solid #d8d4c4;margin:0;padding:2px 16px;color:#555}'
    + '</style></head><body><main><h1>' + title + '</h1>'
    + '<div class="trawl-reader-meta">阅读模式 · 关闭请从「更多菜单」再点一次阅读模式</div>'
    + html + '</main></body></html>');
  document.close();
})()"#;

/// 关闭确认：用户在 chrome 确认框里点了「全部关闭」，放行真正的窗口关闭
#[tauri::command]
#[cfg(desktop)]
fn force_close_app(app: AppHandle) {
    CLOSE_BYPASS.store(true, Ordering::SeqCst);
    // 主窗口是纯 Window（非 WebviewWindow），必须用 get_window
    if let Some(w) = app.get_window("main") {
        let _ = w.close();
    }
}

/// 新标签页打开 URL（网页翻译插件「结果在新标签页打开」等场景用）
#[tauri::command]
#[cfg(desktop)]
fn navigate_new_tab(app: AppHandle, input: String) {
    let state = app.state::<AppState>();
    let settings = state.settings.lock().unwrap().clone();
    let url = resolve_input(&input, &settings);
    let ws = state.active_workspace.lock().unwrap().clone();
    let _ = create_tab(&app, &state, url, ws, true);
}

/// 阅读模式开关：开启注入阅读版式，再次执行时刷新页面还原
#[tauri::command]
#[cfg(desktop)]
fn toggle_reader(app: AppHandle, id: String) {
    let on = builtin_ext("trawl-reading")
        .map(|x| x.enabled)
        .unwrap_or(false);
    if !on {
        let app2 = app.clone();
        std::thread::spawn(move || {
            let _ = app2.emit_to(CHROME_LABEL, "ext-toast", "阅读模式插件未开启，请在扩展程序页打开");
        });
        return;
    }
    // 前端传来的 tab id 同时就是 webview label（create_tab 用 id.to_string() 建 webview）
    let label = id;
    let state = app.state::<AppState>();
    let was_reader = state.reader_tabs.lock().unwrap().remove(&label).is_some();
    // 前端传来的 tab id 就是 webview label（spawn_tab_webview 用 id 建 webview），
    // 用 Manager::get_webview 查（child webview 不在 window.webview_windows() 集合里）
    let found = app.get_webview(&label);
    let Some(wv) = found else {
        let tabs: Vec<String> = state
            .tabs
            .lock()
            .unwrap()
            .iter()
            .map(|t| t.id.clone())
            .collect();
        debug_log(&format!(
            "reader: no webview for label={} tabs={:?}",
            label, tabs
        ));
        return;
    };
    if was_reader {
        let _ = wv.reload();
        debug_log("reader: reload to exit");
    } else {
        let width = match builtin_ext("trawl-reading") {
            Some(x) => match x.cfg.get("width").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => "820".to_string(),
            },
            None => "820".to_string(),
        };
        let js = READER_JS.replace("__W__", &width);
        match wv.eval(&js) {
            Ok(()) => debug_log(&format!("reader: eval ok label={} width={}", label, width)),
            Err(e) => debug_log(&format!("reader: eval err {:?}", e)),
        }
        state.reader_tabs.lock().unwrap().insert(label, true);
    }
}

// ==================== 页内沉浸式翻译 ====================
//
// 走微软免费翻译通道（Edge 浏览器同款）：
//   1) GET  edge.microsoft.com/translation/service/v2.0/auth 拿匿名令牌
//   2) POST api.cognitive.microsofttranslator.com/translate 批量翻译
// 国内网络直连可用，不需要任何 API Key。翻译在页面内完成、双语对照展示，
// 不再跳转百度翻译页面。脚本自带开关：再次注入即为还原。
const IMMERSIVE_JS: &str = r#"(() => {
  if (window.__trawlTrActive) {
    document.querySelectorAll('.trawl-tr-block').forEach(n => n.remove());
    document.querySelectorAll('.trawl-tr-panel').forEach(n => n.remove());
    window.__trawlTrActive = false;
    return 'off';
  }
  window.__trawlTrActive = true;
  const TO0 = __TO__;
  const LANGS = { zh: '中文（简体）', en: 'English', ja: '日本語', ko: '한국어' };

  const panel = document.createElement('div');
  panel.className = 'trawl-tr-panel';
  panel.style.cssText = 'position:fixed;top:14px;right:14px;z-index:2147483647;background:#fff;'
    + 'border:1px solid #e3e7ec;border-radius:12px;box-shadow:0 10px 30px rgba(16,24,40,.20);'
    + 'padding:12px 14px;font:13px/1.6 "Segoe UI","Microsoft YaHei",sans-serif;color:#1f2329;width:236px;';
  panel.innerHTML = '<div style="display:flex;align-items:center;gap:8px">'
    + '<b style="flex:1;font-size:13px">Trawl 网页翻译</b>'
    + '<span class="tt-close" style="cursor:pointer;color:#8a94a0;font-size:16px;line-height:1;padding:0 2px">×</span></div>'
    + '<div style="display:flex;gap:6px;align-items:center;margin-top:10px">'
    + '<span style="color:#6b7480;font-size:12px;flex-shrink:0">翻译成</span>'
    + '<select class="tt-lang" style="flex:1;height:28px;border:1px solid #d6dde3;border-radius:7px;'
    + 'font:12px sans-serif;background:#fff"></select></div>'
    + '<button class="tt-go" style="width:100%;margin-top:10px;height:30px;border:none;border-radius:8px;'
    + 'background:#10b981;color:#fff;font:12.5px sans-serif;cursor:pointer">翻译此页</button>'
    + '<div class="tt-stat" style="margin-top:8px;color:#8a94a0;font-size:11.5px">正在获取翻译服务…</div>';
  document.documentElement.appendChild(panel);
  const sel = panel.querySelector('.tt-lang');
  for (const [k, label] of Object.entries(LANGS)) {
    const o = document.createElement('option'); o.value = k; o.textContent = label; sel.appendChild(o);
  }
  sel.value = LANGS[TO0] ? TO0 : 'zh';
  const goBtn = panel.querySelector('.tt-go');
  const stat = panel.querySelector('.tt-stat');
  const closeAll = () => {
    document.querySelectorAll('.trawl-tr-block').forEach(n => n.remove());
    panel.remove();
    window.__trawlTrActive = false;
  };
  panel.querySelector('.tt-close').onclick = closeAll;
  goBtn.onclick = () => { translateAll(sel.value); };
  sel.onchange = () => { if (window.__trawlTrDone) translateAll(sel.value); };

  function collect() {
    const tags = 'p,h1,h2,h3,h4,h5,h6,li,dd,dt,blockquote,figcaption,td,th,caption';
    const out = [];
    for (const el of document.querySelectorAll(tags)) {
      if (el.closest('.trawl-tr-panel') || el.closest('.trawl-tr-block')) continue;
      if (el.querySelector('.trawl-tr-block')) continue;
      if (el.offsetParent === null && getComputedStyle(el).position !== 'fixed') continue;
      const txt = (el.innerText || '').trim();
      if (txt.length < 2 || txt.length > 4000) continue;
      if (/^(https?:\/\/|[\w.+-]+@[\w-]+\.)|^[\d\s.:,%\-/]+$/.test(txt)) continue;
      // 只取元素自身的直接文本太少的跳过（纯容器）
      out.push(el);
    }
    return out;
  }

  // 翻译统一走后端代理：后端用 reqwest 直连翻译服务，不受网页 CORS 限制，
  // 主通道 B站 Index-Translate（免费、国内直连），失败自动降级 Google。
  // 旧版在页面内 fetch 微软/Google 的架构已废弃：微软 Edge 免费令牌端点已于
  // 2026-07 下线，Google 国内不可达，页面内跨域 fetch 还被 CORS 拦截。
  const invokeTr = (() => {
    try {
      const it = window.__TAURI_INTERNALS__;
      return it && typeof it.invoke === 'function' ? it.invoke : null;
    } catch (e) { return null; }
  })();
  let channel = '';
  async function translateBatch(texts, to) {
    if (!invokeTr) throw new Error('翻译桥不可用');
    const outs = await invokeTr('translate_text', { texts, to });
    if (!Array.isArray(outs) || outs.length !== texts.length) {
      throw new Error('译文格式异常');
    }
    channel = '智能翻译';
    return outs;
  }

  let running = false;
  async function translateAll(to) {
    if (running) return;
    running = true;
    window.__trawlTrDone = false;
    goBtn.textContent = '翻译中…';
    document.querySelectorAll('.trawl-tr-block').forEach(n => n.remove());
    try {
      const els = collect();
      // 分批：每批最多 40 条 / 6000 字符，串行发送避免限流
      const batches = [];
      let cur = [], len = 0;
      for (const el of els) {
        const t = (el.innerText || '').trim();
        if (cur.length >= 40 || len + t.length > 6000) { batches.push(cur); cur = []; len = 0; }
        cur.push(el); len += t.length;
      }
      if (cur.length) batches.push(cur);
      let done = 0;
      stat.textContent = '待翻译 ' + els.length + ' 个段落';
      for (const batch of batches) {
        const texts = batch.map(el => (el.innerText || '').trim());
        const outs = await translateBatch(texts, to);
        for (let i = 0; i < batch.length; i++) {
          const el = batch[i];
          const t = (outs[i] || '').trim();
          if (!t || t === texts[i]) continue;
          const block = document.createElement('div');
          block.className = 'trawl-tr-block';
          block.style.cssText = 'margin:4px 0 0;color:#2563eb;font-size:.97em;line-height:1.7;'
            + 'user-select:text;cursor:auto;';
          block.textContent = t;
          const tag = el.tagName;
          if (tag === 'TD' || tag === 'TH' || tag === 'LI' || tag === 'DD' || tag === 'DT'
            || tag === 'BLOCKQUOTE' || tag === 'FIGCAPTION' || tag === 'CAPTION') {
            el.appendChild(block);
          } else {
            el.after(block);
          }
        }
        done += batch.length;
        stat.textContent = '已翻译 ' + done + ' / ' + els.length;
      }
      window.__trawlTrDone = true;
      stat.textContent = (channel || '翻译') + ' · 已译 ' + els.length + ' 段（双语对照）';
      goBtn.textContent = '还原页面';
    } catch (e) {
      const reason = (e && e.message) ? String(e.message) : String(e || '');
      stat.textContent = reason
        ? ('翻译失败：' + reason.slice(0, 60) + '，请检查网络后重试')
        : '翻译服务暂不可用，请检查网络后重试';
      goBtn.textContent = '重试';
    }
    running = false;
  }
  translateAll(sel.value);
})()"#;

/// 翻译默认目标语言码 → 各翻译服务接受的语言码
fn tr_lang_code(to: &str) -> &'static str {
    match to {
        "zh" | "zh-Hans" => "zh",
        "zh-TW" | "zh-Hant" => "zh-TW",
        "en" => "en",
        "ja" => "ja",
        "ko" => "ko",
        "fr" => "fr",
        "de" => "de",
        "es" => "es",
        "ru" => "ru",
        _ => "zh",
    }
}

/// 从编号文本里解析译文，按原顺序返回。
///  LLM 返回形如 "1|译文\n2|译文"，逐行解析编号；缺失的条目回退为空串，
///  由调用方决定是否保留原文。
fn parse_numbered_translation(raw: &str, count: usize) -> Vec<String> {
    let mut out = vec![String::new(); count];
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        // 支持 "1|xxx"、"1. xxx"、"1、xxx" 等常见编号格式
        let sep = line.find(|c| c == '|' || c == '.');
        let Some(pos) = sep.or_else(|| line.find('、')) else { continue };
        let num_part = line[..pos].trim().trim_start_matches('#').trim();
        if let Ok(n) = num_part.parse::<usize>() {
            if n >= 1 && n <= count {
                let val = line[pos + 1..].trim().trim_matches('"').to_string();
                out[n - 1] = val;
            }
        }
    }
    out
}

/// 主翻译通道：B站 Index-Translate 免费文本翻译 API（无需 Key / Cookie，国内直连）。
///  OpenAI 兼容格式，批量把多段文本一次翻完，用编号对齐。
#[cfg(desktop)]
fn translate_via_bilibili(texts: &[String], to: &str) -> Result<Vec<String>, String> {
    // B站网关会拒绝非浏览器 UA（返回 412），必须带浏览器 UA
    let client = reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
        .connect_timeout(Duration::from_secs(12))
        .timeout(Duration::from_secs(40))
        .build()
        .map_err(|e| format!("client: {}", e))?;

    let lang = tr_lang_code(to);
    let lang_name = match lang {
        "zh" => "简体中文", "zh-TW" => "繁体中文", "en" => "English",
        "ja" => "日本語", "ko" => "한국어", other => other,
    };
    let body_src = texts.iter().enumerate()
        .map(|(i, t)| format!("{}|{}", i + 1, t))
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = format!(
        "Translate each numbered line below into {}. Keep the same number prefix, one line per input, output ONLY the translations with no explanation.\n{}",
        lang_name, body_src
    );
    let payload = serde_json::json!({
        "model": "Index-Translate-35B-A3B",
        "messages": [{ "role": "user", "content": prompt }],
        "stream": false,
        "thinking": { "type": "disabled" }
    });
    let resp = client
        .post("https://index-translate.bilibili.com/v1/chat/completions")
        .json(&payload)
        .send()
        .map_err(|e| format!("请求失败: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let data: serde_json::Value = resp.json().map_err(|e| format!("响应解析: {}", e))?;
    let content = data["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| "响应无译文".to_string())?;
    let out = parse_numbered_translation(content, texts.len());
    if out.iter().all(|s| s.is_empty()) {
        return Err("译文为空".to_string());
    }
    Ok(out)
}

/// 备用翻译通道：Google gtx（海外网络可用时降级）。国内通常不通，仅作兜底。
#[cfg(desktop)]
fn translate_via_google(texts: &[String], to: &str) -> Result<Vec<String>, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| format!("client: {}", e))?;
    let lang = tr_lang_code(to);
    let mut out = Vec::with_capacity(texts.len());
    for text in texts {
        // 用表单参数提交，reqwest 自动做 URL 编码，避免手动编码遗漏特殊字符
        let resp = client
            .post("https://translate.googleapis.com/translate_a/single")
            .query(&[("client", "gtx"), ("dt", "t"), ("sl", "auto"), ("tl", lang)])
            .form(&[("q", text.as_str())])
            .send()
            .map_err(|e| format!("请求失败: {}", e))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status().as_u16()));
        }
        let data: serde_json::Value = resp.json().map_err(|e| format!("响应解析: {}", e))?;
        let piece = data[0].as_array()
            .map(|arr| arr.iter().filter_map(|x| x[0].as_str()).collect::<Vec<_>>().join(""))
            .unwrap_or_default();
        out.push(piece);
    }
    if out.iter().all(|s| s.is_empty()) {
        return Err("译文为空".to_string());
    }
    Ok(out)
}

/// 供注入脚本调用的整页翻译：把收集到的段落交给后端翻译，返回同序译文数组。
///  后端用 reqwest 直连，不受网页 CORS 限制；主通道 B站，失败自动降级 Google。
#[tauri::command]
#[cfg(desktop)]
fn translate_text(texts: Vec<String>, to: String) -> Result<Vec<String>, String> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    debug_log(&format!("translate_text: 段落={} 目标={}", texts.len(), to));
    match translate_via_bilibili(&texts, &to) {
        Ok(out) => {
            debug_log(&format!("translate_text: B站通道成功 段落={}", texts.len()));
            return Ok(out);
        }
        Err(e) => debug_log(&format!("translate_text: B站通道失败 {} → 降级 Google", e)),
    }
    translate_via_google(&texts, &to).map_err(|e| {
        debug_log(&format!("translate_text: 全部通道失败 {}", e));
        format!("翻译服务暂不可用：{}", e)
    })
}

/// 页内翻译开关：注入沉浸式翻译脚本（再次注入 = 还原）
#[tauri::command]
#[cfg(desktop)]
fn toggle_translate(app: AppHandle, id: String, to: Option<String>) {
    let ext = builtin_ext("trawl-translate");
    if ext.as_ref().map(|x| x.enabled).unwrap_or(true) == false {
        let app2 = app.clone();
        std::thread::spawn(move || {
            let _ = app2.emit_to(CHROME_LABEL, "ext-toast", "网页翻译插件未开启，请在扩展程序页打开");
        });
        return;
    }
    let Some(wv) = app.get_webview(&id) else { return };
    let target = to
        .or_else(|| {
            ext.as_ref()
                .and_then(|x| x.cfg.get("target"))
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "zh".to_string());
    // 用 JSON 序列化生成 JS 字符串字面量（自动转义引号/反斜杠/换行），
    // 防止 to 参数注入任意 JS；旧做法 IMMERSIVE_JS.replace("__TO__", &target) 可被 `x';alert(1);//` 突破。
    let target_js = serde_json::to_string(&target).unwrap_or_else(|_| "\"zh\"".into());
    let js = IMMERSIVE_JS.replace("__TO__", &target_js);
    let _ = wv.eval(&js);
    debug_log(&format!("translate: eval label={} to={}", id, target));
}

// ==================== 本地扩展真实生效 ====================
//
// 「加载未打包的扩展程序」读取目录里的 manifest.json 校验并登记；
// 启用中的本地扩展若带 content_scripts，页面加载完成时按 match 模式注入其 JS。
// 这是 WebView2 架构下能做到的扩展真实执行层（Chrome 完整 API 不可用）。

/// Chrome 扩展 match pattern 简化匹配：<all_urls>、*://*.host/* 等
fn pattern_matches(pattern: &str, url: &str) -> bool {
    let pattern = pattern.trim();
    if pattern == "<all_urls>" {
        return url.starts_with("http://") || url.starts_with("https://");
    }
    let Some((scheme, rest)) = pattern.split_once("://") else { return false };
    if scheme != "*" && !url.starts_with(&format!("{}://", scheme)) {
        return false;
    }
    let after = url.splitn(2, "://").nth(1).unwrap_or("");
    let url_host = after.split(['/', ':', '?']).next().unwrap_or("");
    let mut pat_host = rest.split('/').next().unwrap_or("");
    if pat_host.is_empty() {
        return false;
    }
    if let Some(suffix) = pat_host.strip_prefix("*.") {
        let h = url_host.trim_start_matches("www.");
        return h == suffix || h.ends_with(&format!(".{}", suffix));
    }
    pat_host = pat_host.trim_start_matches("www.");
    url_host.trim_start_matches("www.") == pat_host || url_host == pat_host
}

/// 从登记的本地扩展重建 content_scripts 缓存
fn reload_ext_scripts(app: &AppHandle) {
    let list = load_json_list::<ExtensionItem>(EXTENSIONS_FILE).unwrap_or_else(default_extensions);
    let mut scripts = Vec::new();
    for it in list {
        if it.builtin || !it.enabled || it.path.is_empty() {
            continue;
        }
        let dir = std::path::Path::new(&it.path);
        let manifest = dir.join("manifest.json");
        let Ok(content) = std::fs::read_to_string(&manifest) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else { continue };
        let mut js_all = String::new();
        let mut matches: Vec<String> = Vec::new();
        if let Some(css) = v.get("content_scripts").and_then(|x| x.as_array()) {
            for cs in css {
                if let Some(ms) = cs.get("matches").and_then(|x| x.as_array()) {
                    for m in ms {
                        if let Some(s) = m.as_str() {
                            matches.push(s.to_string());
                        }
                    }
                }
                if let Some(js) = cs.get("js").and_then(|x| x.as_array()) {
                    for f in js {
                        if let Some(name) = f.as_str() {
                            if let Ok(code) = std::fs::read_to_string(dir.join(name)) {
                                js_all.push_str(&code);
                                js_all.push('\n');
                            }
                        }
                    }
                }
            }
        }
        // 单个扩展脚本上限 512KB，防止误登记巨型目录拖慢页面加载
        if !js_all.is_empty() && js_all.len() < 512 * 1024 && !matches.is_empty() {
            scripts.push(LocalExtScript { id: it.id.clone(), matches, js: js_all });
        }
    }
    *app.state::<AppState>().ext_scripts.lock().unwrap() = scripts;
}

/// 页面加载完成后注入启用的本地扩展脚本
#[cfg(desktop)]
fn inject_local_extensions(app: &AppHandle, label: &str, url: &str) {
    if !url.starts_with("http") {
        return;
    }
    let scripts = app.state::<AppState>().ext_scripts.lock().unwrap().clone();
    for s in &scripts {
        let hit = s.matches.iter().any(|p| pattern_matches(p, url));
        if hit {
            if let Some(wv) = app.get_webview(label) {
                let _ = wv.eval(&s.js);
                debug_log(&format!("ext inject: {} -> {}", s.id, url));
            }
        }
    }
}

/// 加载未打包的扩展程序：读 manifest.json 校验并生成登记项
#[tauri::command]
#[cfg(desktop)]
fn load_unpacked_ext(path: String) -> Result<ExtensionItem, String> {
    let dir = std::path::PathBuf::from(&path);
    let manifest = dir.join("manifest.json");
    if !manifest.exists() {
        return Err("目录里没有找到 manifest.json".to_string());
    }
    let content = std::fs::read_to_string(&manifest)
        .map_err(|e| format!("读取 manifest.json 失败: {}", e))?;
    let v: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("manifest.json 解析失败: {}", e))?;
    let name = v
        .get("name")
        .and_then(|x| x.as_str())
        .unwrap_or("本地扩展")
        .to_string();
    let version = v
        .get("version")
        .and_then(|x| x.as_str())
        .unwrap_or("1.0.0")
        .to_string();
    let desc = v
        .get("description")
        .and_then(|x| x.as_str())
        .unwrap_or("本地未打包扩展")
        .to_string();
    let has_scripts = v
        .get("content_scripts")
        .and_then(|x| x.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    let desc = if has_scripts {
        format!("{}（content_scripts 会在匹配页面上自动执行）", desc)
    } else {
        format!("{}（无 content_scripts，仅登记）", desc)
    };
    Ok(ExtensionItem {
        id: format!("local-{}", now_ts()),
        name,
        version,
        desc,
        enabled: true,
        builtin: false,
        path: path.replace('/', "\\"),
        cfg: Default::default(),
    })
}

// ==================== 书签导入 ====================

/// 从 Chrome / Edge 的 Bookmarks JSON（或 Netscape HTML 文件）导入书签。
/// source: "chrome" | "edge" | "html"；html 需要传 path。
#[tauri::command]
fn import_bookmarks(app: AppHandle, source: String, path: Option<String>) -> Result<usize, String> {
    let found: Vec<Bookmark> = match source.as_str() {
        "html" => {
            let p = path.ok_or("缺少文件路径")?;
            parse_bookmarks_html(&p)?
        }
        "chrome" | "edge" => {
            let local = std::env::var("LOCALAPPDATA").map_err(|_| "找不到 LOCALAPPDATA")?;
            let base = match source.as_str() {
                "chrome" => PathBuf::from(&local).join("Google").join("Chrome").join("User Data"),
                _ => PathBuf::from(&local).join("Microsoft").join("Edge").join("User Data"),
            };
            let mut found = Vec::new();
            // 遍历 Default 和 Profile N
            let entries = std::fs::read_dir(&base).map_err(|_| format!("没有找到 {} 的用户数据", source))?;
            let mut profiles: Vec<PathBuf> = Vec::new();
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name == "Default" || name.starts_with("Profile ") {
                    profiles.push(e.path());
                }
            }
            if profiles.is_empty() {
                return Err(format!("{} 中没有找到浏览器配置目录", source));
            }
            for p in profiles {
                let bf = p.join("Bookmarks");
                if let Ok(content) = std::fs::read_to_string(&bf) {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                        // roots 的三个桶（bookmark_bar/other/synced）是浏览器 UI 概念，
                        // 其 name（"收藏夹栏"等）不计入文件夹路径；只保留桶内真实子文件夹
                        if let Some(roots) = v.get("roots").and_then(|r| r.as_object()) {
                            for (_, bucket) in roots {
                                if let Some(children) = bucket.get("children") {
                                    collect_bookmark_nodes(Some(children), &mut found, "");
                                }
                            }
                        }
                    }
                }
            }
            if found.is_empty() {
                return Err(format!("{} 的书签是空的", source));
            }
            found
        }
        _ => return Err(format!("不支持的来源: {}", source)),
    };
    let state = app.state::<AppState>();
    let mut bookmarks = state.bookmarks.lock().unwrap();
    let mut added = 0;
    let mut updated = 0;
    for b in found {
        if !b.url.starts_with("http") {
            continue;
        }
        if let Some(existing) = bookmarks.iter_mut().find(|x| x.url == b.url) {
            // 已存在：若是之前导入的平铺数据（folder 为空）而这次带上了文件夹，补上 folder
            if existing.folder.is_empty() && !b.folder.is_empty() {
                existing.folder = b.folder.clone();
                existing.title = b.title.clone();
                updated += 1;
            }
        } else {
            bookmarks.push(b);
            added += 1;
        }
    }
    drop(bookmarks);
    save_bookmarks(&state);
    push_state(&app);
    // 返回新增数（含补全文件夹的更新数），让前端有反馈
    Ok(added + updated)
}

/// 递归收集 Chrome Bookmarks JSON 里 type=url 的节点，保留文件夹路径。
/// roots 是 object（bookmark_bar/other/synced）；folder 节点的 name 作为路径一段，
/// 用 "/" 分隔。roots 本身不计入路径（其直属书签显示在书签栏根层）。
fn collect_bookmark_nodes(node: Option<&serde_json::Value>, out: &mut Vec<Bookmark>, folder: &str) {
    let Some(v) = node else { return };
    // url 节点先识别并收集（它本身也是 object，必须在 object 遍历前处理）
    if v.get("type").and_then(|x| x.as_str()) == Some("url") {
        let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let title = v
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        if url.starts_with("http") {
            out.push(Bookmark { url, title, folder: folder.to_string() });
        }
        return;
    }
    // folder 节点：把 name 追加到路径，再递归 children
    if v.get("type").and_then(|x| x.as_str()) == Some("folder") {
        let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("").trim();
        let next = if name.is_empty() {
            folder.to_string()
        } else if folder.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", folder, name)
        };
        if let Some(children) = v.get("children") {
            collect_bookmark_nodes(Some(children), out, &next);
        }
        return;
    }
    // array：逐个递归（沿用当前路径）
    if let Some(arr) = v.as_array() {
        for x in arr {
            collect_bookmark_nodes(Some(x), out, folder);
        }
        return;
    }
    // object：roots 的 bookmark_bar/other/synced 等，沿用当前路径递归
    if let Some(map) = v.as_object() {
        for (_, child) in map {
            collect_bookmark_nodes(Some(child), out, folder);
        }
    }
}

/// 在原文字节中查找 ASCII 标签（大小写不敏感），返回字节偏移。
/// 只在 ASCII 层面比较 needle，非 ASCII 字节不会误匹配；索引始终对应原文，
/// 避免 `content.to_lowercase()` 因 Unicode 变长映射（如 İ/ẞ）导致偏移错位。
fn find_tag_ci(bytes: &[u8], needle_lower: &[u8], from: usize) -> Option<usize> {
    if needle_lower.is_empty() || from > bytes.len() { return None; }
    bytes[from..].windows(needle_lower.len()).position(|w| {
        w.iter().zip(needle_lower.iter()).all(|(&b, &n)| b.to_ascii_lowercase() == n)
    }).map(|p| from + p)
}

/// 解析 Netscape 书签 HTML（各大浏览器「导出书签」的标准格式）。
/// 结构：<DL><p> ... <DT><H3>文件夹名</H3> <DL><p> ... <DT><A HREF="url">标题</A> ... </DL> ... </DL>
/// 用扫描标签的方式维护文件夹栈，保留 <H3> 定义的层级路径。
fn parse_bookmarks_html(path: &str) -> Result<Vec<Bookmark>, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("读取文件失败: {}", e))?;
    let bytes = content.as_bytes();
    let mut out = Vec::new();
    let mut stack: Vec<String> = Vec::new(); // 当前文件夹路径栈
    let mut i = 0usize;
    while i < bytes.len() {
        // 找下一个感兴趣的开始标签：<h3 / <a  / </dl（全部在原文上按 ASCII 不敏感匹配）
        let h3 = find_tag_ci(bytes, b"<h3", i);
        let a = find_tag_ci(bytes, b"<a ", i);
        let dl_close = find_tag_ci(bytes, b"</dl", i);
        let next = [h3, a, dl_close].into_iter().flatten().min();
        let Some(pos) = next else { break };
        if dl_close == Some(pos) {
            stack.pop(); // 关闭一层文件夹
            i = pos + 4;
            continue;
        }
        if h3 == Some(pos) {
            // 文件夹名：<H3 ...>名称</H3>
            let Some(gt_rel) = find_tag_ci(bytes, b">", pos) else { break };
            let ts = gt_rel + 1;
            let Some(end) = find_tag_ci(bytes, b"</h3", ts) else { break };
            let name = content[ts..end].trim().to_string();
            stack.push(name);
            i = end + 5;
            continue;
        }
        // <a href="..." ...>标题</a>
        let start = pos + 3;
        let Some(href_rel) = find_tag_ci(bytes, b"href", start) else { i = start; continue };
        let hstart = href_rel;
        let Some(q1) = find_tag_ci(bytes, b"\"", hstart) else { i = hstart; continue };
        let vstart = q1 + 1;
        if vstart > bytes.len() { break; }
        let Some(q2) = find_tag_ci(bytes, b"\"", vstart) else { i = vstart; continue };
        let url = content[vstart..q2].to_string();
        let Some(gt) = find_tag_ci(bytes, b">", q2) else { break };
        let tstart = gt + 1;
        let Some(text_end) = find_tag_ci(bytes, b"</a", tstart) else { break };
        let title = content[tstart..text_end].trim().to_string();
        if url.starts_with("http") {
            out.push(Bookmark { url, title, folder: stack.join("/") });
        }
        i = text_end + 3;
    }
    if out.is_empty() {
        return Err("文件里没有解析到书签（请用浏览器「导出书签」生成的 HTML）".to_string());
    }
    Ok(out)
}

// ==================== 本机密码管理 ====================
//
// 密码用 Windows DPAPI（CryptProtectData，当前用户作用域）加密后落盘，
// 只能被同一 Windows 账户解密，换电脑/换账户读不出明文。
// 导入走 CSV（各浏览器密码管理器都支持「导出密码」为 CSV）。

#[derive(Clone, Serialize, Deserialize)]
struct PwRecord {
    id: String,
    site: String,
    username: String,
    /// DPAPI 加密后的十六进制密文
    blob: String,
    /// 备注（可选，老数据没有时反序列化为空）
    #[serde(default)]
    note: String,
}

#[derive(Serialize)]
struct PwEntry {
    id: String,
    site: String,
    username: String,
    password: String,
    note: String,
}

#[cfg(windows)]
const DPAPI_ENTROPY: &[u8] = b"Trawl.Pw.v1";

#[cfg(windows)]
fn dpapi_protect(plain: &str) -> Result<String, String> {
    use windows::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};
    let data = CRYPT_INTEGER_BLOB {
        cbData: plain.len() as u32,
        pbData: plain.as_ptr() as *mut u8,
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: DPAPI_ENTROPY.len() as u32,
        pbData: DPAPI_ENTROPY.as_ptr() as *mut u8,
    };
    let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    unsafe {
        CryptProtectData(&data, None, Some(&entropy), None, None, 0, &mut out)
            .map_err(|e| format!("DPAPI 加密失败: {}", e))?;
    }
    let slice = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) };
    let mut hex = String::with_capacity(out.cbData as usize * 2);
    for b in slice {
        hex.push_str(&format!("{:02x}", b));
    }
    unsafe {
        let _ = windows::Win32::Foundation::LocalFree(
            Some(windows::Win32::Foundation::HLOCAL(out.pbData as *mut _)),
        );
    }
    Ok(hex)
}

#[cfg(windows)]
fn dpapi_unprotect(hex: &str) -> Result<String, String> {
    use windows::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let hb = hex.as_bytes();
    let mut i = 0;
    while i + 1 < hb.len() {
        let hi = (hb[i] as char).to_digit(16).ok_or("密文格式错误")? as u8;
        let lo = (hb[i + 1] as char).to_digit(16).ok_or("密文格式错误")? as u8;
        bytes.push(hi << 4 | lo);
        i += 2;
    }
    let data = CRYPT_INTEGER_BLOB { cbData: bytes.len() as u32, pbData: bytes.as_mut_ptr() };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: DPAPI_ENTROPY.len() as u32,
        pbData: DPAPI_ENTROPY.as_ptr() as *mut u8,
    };
    let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    unsafe {
        CryptUnprotectData(&data, None, Some(&entropy), None, None, 0, &mut out)
            .map_err(|e| format!("DPAPI 解密失败: {}", e))?;
    }
    let slice = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) };
    let plain = String::from_utf8_lossy(slice).to_string();
    unsafe {
        let _ = windows::Win32::Foundation::LocalFree(
            Some(windows::Win32::Foundation::HLOCAL(out.pbData as *mut _)),
        );
    }
    Ok(plain)
}

/// 密码存储的平台抽象：Windows=DPAPI 密文；macOS=系统钥匙串（blob 仅存占位标记）；
/// 其它平台（Android/Linux）=不支持，安全降级为空。
#[cfg(target_os = "macos")]
const PW_KEYCHAIN_SERVICE: &str = "com.trawl.browser.passwords";

/// 加密密码并返回可落盘的字符串。id 在 macOS 下作为钥匙串 account。
#[cfg(windows)]
fn pw_protect(_id: &str, plain: &str) -> Result<String, String> {
    dpapi_protect(plain)
}
#[cfg(target_os = "macos")]
fn pw_protect(id: &str, plain: &str) -> Result<String, String> {
    security_framework::passwords::set_generic_password(PW_KEYCHAIN_SERVICE, id, plain.as_bytes())
        .map_err(|e| format!("钥匙串保存失败: {}", e))?;
    Ok("keychain".into())
}
#[cfg(not(any(windows, target_os = "macos")))]
fn pw_protect(_id: &str, _plain: &str) -> Result<String, String> {
    Err("此平台暂不支持密码管理".into())
}

/// 解密密码。macOS 从钥匙串按 id 读取（blob 是占位标记）。
#[cfg(windows)]
fn pw_unprotect(_id: &str, blob: &str) -> Result<String, String> {
    dpapi_unprotect(blob)
}
#[cfg(target_os = "macos")]
fn pw_unprotect(id: &str, _blob: &str) -> Result<String, String> {
    let bytes = security_framework::passwords::get_generic_password(PW_KEYCHAIN_SERVICE, id)
        .map_err(|e| format!("钥匙串读取失败: {}", e))?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}
#[cfg(not(any(windows, target_os = "macos")))]
fn pw_unprotect(_id: &str, _blob: &str) -> Result<String, String> {
    Ok(String::new())
}

/// 删除密码（macOS 需要同步删除钥匙串条目）。
#[cfg(windows)]
fn pw_delete(_id: &str) {}
#[cfg(target_os = "macos")]
fn pw_delete(id: &str) {
    let _ = security_framework::passwords::delete_generic_password(PW_KEYCHAIN_SERVICE, id);
}
#[cfg(not(any(windows, target_os = "macos")))]
fn pw_delete(_id: &str) {}

fn load_pw_records() -> Vec<PwRecord> {
    load_json_list::<PwRecord>(PASSWORDS_FILE).unwrap_or_default()
}

fn save_pw_records(list: &[PwRecord]) {
    if let Ok(json) = serde_json::to_string(list) {
        if let Err(e) = write_atomic(&data_root().join(PASSWORDS_FILE), &json) { debug_log(&format!("写盘失败: {}", e)); }
    }
}

#[tauri::command]
fn pw_list() -> Vec<PwEntry> {
    // 安全边界：列表不再下发明文密码。需要明文的场景（显示/编辑/安全检查）
    // 走 pw_reveal / pw_check，二者在后端校验近期是否通过本机身份验证。
    load_pw_records()
        .into_iter()
        .map(|r| PwEntry {
            id: r.id,
            site: r.site,
            username: r.username,
            password: String::new(),
            note: r.note,
        })
        .collect()
}

/// 密码显示有效期（秒）：验证通过后 5 分钟内可取明文
const PW_REVEAL_WINDOW_SECS: u64 = 300;

/// 单条取明文密码：必须在近期（5 分钟内）通过本机身份验证。
/// 这是真正的安全边界——明文不进列表、不长期缓存在前端。
#[tauri::command]
fn pw_reveal(app: AppHandle, id: String) -> Result<String, String> {
    let state = app.state::<AppState>();
    let verified_at = *state.pw_verified_at.lock().unwrap();
    let now = now_ts();
    if verified_at == 0 || now.saturating_sub(verified_at) > PW_REVEAL_WINDOW_SECS {
        return Err(String::from("需要先通过本机身份验证才能查看密码"));
    }
    let record = load_pw_records().into_iter().find(|r| r.id == id)
        .ok_or_else(|| "密码条目不存在".to_string())?;
    pw_unprotect(&record.id, &record.blob)
}

/// 本机不具备验证能力（not_available）或用户主动关闭验证时，
/// 前端取得显式确认后调用此命令解锁 pw_reveal / pw_check。
/// 单独成命令而非在 verify 返回 not_available 时自动解锁，避免仅调用一次 verify 就放行。
#[tauri::command]
fn pw_confirm_insecure(app: AppHandle) {
    let state = app.state::<AppState>();
    *state.pw_verified_at.lock().unwrap() = now_ts();
}

/// 后端安全检查：只返回重复/弱口令的条目 id，不返回明文密码本身
#[tauri::command]
fn pw_check(app: AppHandle) -> serde_json::Value {
    let state = app.state::<AppState>();
    let verified_at = *state.pw_verified_at.lock().unwrap();
    if verified_at == 0 || now_ts().saturating_sub(verified_at) > PW_REVEAL_WINDOW_SECS {
        return serde_json::json!({ "need_verify": true, "reused": [], "weak": [] });
    }
    let mut plaintexts: Vec<(String, String)> = Vec::new();
    for r in load_pw_records() {
        if let Ok(p) = pw_unprotect(&r.id, &r.blob) {
            plaintexts.push((r.id, p));
        }
    }
    let mut freq: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    let mut weak = Vec::new();
    for (id, p) in &plaintexts {
        if !p.is_empty() { freq.entry(p.clone()).or_default().push(id.clone()); }
        if p.len() < 8 || matches!(p.as_str(), "123456" | "password" | "admin" | "111111" | "000000" | "qwerty") {
            weak.push(id.clone());
        }
    }
    let reused: Vec<Vec<String>> = freq.into_values().filter(|ids| ids.len() > 1).collect();
    serde_json::json!({ "need_verify": false, "reused": reused, "weak": weak })
}

#[tauri::command]
fn pw_add(site: String, username: String, password: String, note: Option<String>) -> Result<(), String> {
    let site = site.trim().to_string();
    if site.is_empty() {
        return Err("站点不能为空".to_string());
    }
    let id = gen_id("pw");
    let blob = pw_protect(&id, &password)?;
    let mut list = load_pw_records();
    if list.len() >= 200 {
        return Err("密码条目已达上限（200）".to_string());
    }
    list.push(PwRecord { id, site, username, blob, note: note.unwrap_or_default() });
    save_pw_records(&list);
    Ok(())
}

/// 修改一条已保存的密码（用户名 / 密码 / 备注任一可改，密码改动后重新加密落盘）
#[tauri::command]
fn pw_update(id: String, username: String, password: String, note: Option<String>) -> Result<(), String> {
    let blob = pw_protect(&id, &password)?;
    let mut list = load_pw_records();
    let rec = list
        .iter_mut()
        .find(|r| r.id == id)
        .ok_or_else(|| "条目不存在".to_string())?;
    rec.username = username;
    rec.blob = blob;
    if let Some(n) = note {
        rec.note = n;
    }
    save_pw_records(&list);
    Ok(())
}

/// 本机身份验证：Windows Hello / macOS Touch ID 或登录密码。
/// 密码管理工具进入站点详情前必须通过。返回 verified / not_available / denied。
#[tauri::command]
async fn verify_user_consent(app: AppHandle) -> String {
    let res = tauri::async_runtime::spawn_blocking(|| {
        #[cfg(windows)]
        {
            use windows::Security::Credentials::UI::{
                UserConsentVerificationResult, UserConsentVerifier,
            };
            let avail = UserConsentVerifier::CheckAvailabilityAsync()
                .and_then(|op| op.join())
                .map(|a| a.0 == 0 /* Available */)
                .unwrap_or(false);
            if !avail {
                return "not_available".to_string();
            }
            let msg = windows::core::HSTRING::from("验证本机身份后才能查看密码");
            let verified = UserConsentVerifier::RequestVerificationAsync(&msg)
                .and_then(|op| op.join())
                .map(|r| r == UserConsentVerificationResult::Verified)
                .unwrap_or(false);
            if verified { "verified".to_string() } else { "denied".to_string() }
        }
        #[cfg(target_os = "macos")]
        {
            use block2::RcBlock;
            use objc2::runtime::Bool;
            use objc2_foundation::{NSError, NSString};
            use objc2_local_authentication::{LAContext, LAPolicy};
            let ctx = unsafe { LAContext::new() };
            // DeviceOwnerAuthentication：Touch ID 可用时走指纹，否则弹登录密码。
            // 先做可用性预检，未 enroll / 未设置密码直接返回 not_available 让前端降级确认。
            if unsafe { ctx.canEvaluatePolicy_error(LAPolicy::DeviceOwnerAuthentication) }.is_err() {
                return "not_available".to_string();
            }
            let reason = NSString::from_str("验证本机身份后才能查看密码");
            let (tx, rx) = std::sync::mpsc::channel::<bool>();
            let block: RcBlock<dyn Fn(Bool, *mut NSError)> =
                RcBlock::new(move |ok: Bool, _err: *mut NSError| { let _ = tx.send(ok.as_bool()); });
            unsafe {
                ctx.evaluatePolicy_localizedReason_reply(
                    LAPolicy::DeviceOwnerAuthentication,
                    &reason,
                    &block,
                );
            }
            // 等待用户操作（Touch ID / 密码），设 5 分钟上限防止永久挂起
            match rx.recv_timeout(std::time::Duration::from_secs(300)) {
                Ok(true) => "verified".to_string(),
                Ok(false) => "denied".to_string(),
                Err(_) => "denied".to_string(),
            }
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            "not_available".to_string()
        }
    });
    let result = res.await.unwrap_or_else(|_| String::from("denied"));
    // 验证通过时写入时间戳：pw_reveal / pw_check 据此判断能否返回明文（安全边界后端化）
    if result == "verified" {
        let state = app.state::<AppState>();
        *state.pw_verified_at.lock().unwrap() = now_ts();
    }
    result
}

#[tauri::command]
fn pw_del(id: String) {
    pw_delete(&id);
    let mut list = load_pw_records();
    list.retain(|r| r.id != id);
    save_pw_records(&list);
}

/// 从浏览器导出的密码 CSV 导入（第一列 url/site，第二列 username，第三列 password；
/// 自动识别带表头的 Chrome / Edge 导出格式）
#[tauri::command]
fn pw_import_csv(path: String) -> Result<usize, String> {
    let content = std::fs::read_to_string(&path).map_err(|e| format!("读取 CSV 失败: {}", e))?;
    let mut lines = content.lines().filter(|l| !l.trim().is_empty());
    let first = lines.next().ok_or("CSV 是空的")?;
    let lower = first.to_lowercase();
    // 带表头（Chrome / Edge 导出格式：name,url,username,password）时跳过表头行
    let header = lower.contains("username") || lower.contains("password");
    let mut records = Vec::new();
    let parse_line = |line: &str| -> Option<(String, String, String)> {
        let fields = parse_csv_line(line);
        let (site, username, password) = if fields.len() >= 4
            && (fields[1].trim().starts_with("http")
                || fields[0].trim().eq_ignore_ascii_case("name"))
        {
            // 四列格式：name, url, username, password
            (
                fields[1].trim().to_string(),
                fields[2].trim().to_string(),
                fields[3].trim().to_string(),
            )
        } else if fields.len() >= 3 {
            // 三列格式：site, username, password
            (
                fields[0].trim().to_string(),
                fields[1].trim().to_string(),
                fields[2].trim().to_string(),
            )
        } else {
            return None;
        };
        let site = site.trim_start_matches("http://").trim_start_matches("https://").to_string();
        if site.is_empty() || !site.contains('.') {
            return None;
        }
        Some((site, username, password))
    };
    if !header {
        if let Some((s, u, p)) = parse_line(first) {
            records.push((s, u, p));
        }
    }
    for line in lines {
        if let Some((s, u, p)) = parse_line(line) {
            records.push((s, u, p));
        }
    }
    {
        let existing = load_pw_records();
        let mut list = existing.clone();
        let mut added = 0;
        for (site, username, password) in records {
            if password.is_empty() {
                continue;
            }
            if existing.iter().any(|r| r.site == site && r.username == username) {
                continue;
            }
            if list.len() >= 200 {
                break;
            }
            let id = gen_id("pw");
            let blob = pw_protect(&id, &password)?;
            list.push(PwRecord { id, site, username, blob, note: String::new() });
            added += 1;
        }
        save_pw_records(&list);
        Ok(added)
    }
}

/// 简易 CSV 解析：支持引号包裹与转义引号
fn parse_csv_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                cur.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == ',' {
            out.push(cur.clone());
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

// ==================== 持久化 ====================

fn save_state(app: &AppHandle) {
    let state = app.state::<AppState>();
    save_history(&state);
    save_bookmarks(&state);

    // 注意：不能把这些 lock() 直接写进结构体字面量。
    // Rust 的临时量存活到整条语句结束，字面量里两次 lock 同一个 Mutex 会**自锁**，
    // 而且第一个 guard 会一直持有到语句末尾，造成跨字段长期占用。
    // 老版本就是这样：启动后第一次 save_state 直接把主线程/工作线程锁死，窗口假死。
    let workspaces: Vec<SavedWorkspace> = {
        let guard = state.workspaces.lock().unwrap();
        guard
            .iter()
            .map(|w| SavedWorkspace {
                id: w.id.clone(),
                name: w.name.clone(),
                icon: w.icon.clone(),
            })
            .collect()
    };
    let active_workspace = { state.active_workspace.lock().unwrap().clone() };
    let tabs: Vec<SavedTab> = {
        let guard = state.tabs.lock().unwrap();
        guard
            .iter()
            .map(|t| SavedTab {
                url: t.url.clone(),
                workspace_id: t.workspace_id.clone(),
                title: t.title.clone(),
                pinned: t.pinned,
                folder_id: t.folder_id.clone(),
            })
            .collect()
    };
    let folders: Vec<SavedFolder> = {
        let guard = state.folders.lock().unwrap();
        guard
            .iter()
            .map(|f| SavedFolder {
                id: f.id.clone(),
                name: f.name.clone(),
                workspace_id: f.workspace_id.clone(),
            })
            .collect()
    };
    let active_tab_index = {
        let active = { state.active_tab.lock().unwrap().clone() };
        let guard = state.tabs.lock().unwrap();
        guard.iter().position(|t| Some(&t.id) == active.as_ref())
    };

    let saved = SavedState {
        workspaces,
        active_workspace,
        tabs,
        folders,
        active_tab_index,
    };
    let file = data_root().join(STATE_FILE);
    if let Ok(json) = serde_json::to_string(&saved) {
        if let Err(e) = write_atomic(&file, &json) { debug_log(&format!("写盘失败: {}", e)); }
    }
}

fn load_settings() -> Settings {
    let path = data_root().join(SETTINGS_FILE);
    let mut recovered = false;
    let mut settings: Settings = match read_json_text(&path) {
        Some(c) => match serde_json::from_str(&c) {
            Ok(s) => s,
            Err(e) => {
                debug_log(&format!("settings.json 解析失败：{}", e));
                backup_corrupt(&path);
                recovered = true;
                Settings::default()
            }
        },
        None => {
            recovered = true;
            Settings::default()
        }
    };
    // 兼容旧版引擎命名，统一为首页使用的名称
    for e in &mut settings.engines {
        if e.name == "必应" { e.name = "Bing".into(); }
        if e.name == "谷歌" { e.name = "Google".into(); }
        // 元宝搜索（so.qq.com）是纯 SPA：?query=、/s?w= 等任何 URL 参数都被忽略，
        // 无法直达结果页。统一切换为搜狗搜索（同属腾讯，URL 直出搜索结果）。
        if e.template.contains("so.qq.com") {
            e.name = "搜狗搜索".into();
            e.template = "https://www.sogou.com/web?query={query}".into();
            recovered = true;
        }
    }
    // 引擎列表被外部改空时补回默认，否则地址栏搜索无处可去
    let before = settings.engines.len();
    settings
        .engines
        .retain(|e| !e.name.trim().is_empty() && !e.template.trim().is_empty());
    if settings.engines.len() != before { recovered = true; }
    if settings.engines.is_empty() {
        settings.engines = Settings::default().engines;
        recovered = true;
    }
    if settings.default_engine == "必应" { settings.default_engine = "Bing".into(); }
    if settings.default_engine == "元宝搜索" { settings.default_engine = "Bing".into(); recovered = true; }
    if settings.default_engine == "谷歌" { settings.default_engine = "Google".into(); }
    if !settings.engines.iter().any(|e| e.name == settings.default_engine) {
        settings.default_engine = settings.engines[0].name.clone();
        recovered = true;
    }
    // 修复过的配置立刻落盘：否则设置页给内存默认值、磁盘却没有文件，
    // 下次启动又走一遍「文件缺失」分支，用户会以为设置没保存。
    if recovered {
        if let Ok(json) = serde_json::to_string(&settings) {
            if let Err(e) = write_atomic(&path, &json) { debug_log(&format!("写盘失败: {}", e)); }
        }
    }
    settings
}

#[cfg(desktop)]
fn restore_state(app: &AppHandle) {
    let state = app.state::<AppState>();
    *state.settings.lock().unwrap() = load_settings();
    if let Some(b) = load_json_list::<Bookmark>(BOOKMARKS_FILE) {
        *state.bookmarks.lock().unwrap() = b;
    }
    if let Some(h) = load_json_list::<HistoryEntry>(HISTORY_FILE) {
        *state.history.lock().unwrap() = h;
    }

    let content = match read_json_text(&data_root().join(STATE_FILE)) {
        Some(c) => c,
        None => return,
    };
    let saved: SavedState = match serde_json::from_str(&content) {
        Ok(s) => s,
        Err(e) => {
            debug_log(&format!("state.json 解析失败：{}", e));
            backup_corrupt(&data_root().join(STATE_FILE));
            return;
        }
    };
    if saved.workspaces.is_empty() {
        return;
    }
    {
        let mut ws = state.workspaces.lock().unwrap();
        ws.clear();
        for w in &saved.workspaces {
            ws.push(Workspace {
                id: w.id.clone(),
                name: w.name.clone(),
                icon: w.icon.clone(),
            });
        }
        state.folders.lock().unwrap().clear();
        for f in &saved.folders {
            state.folders.lock().unwrap().push(Folder {
                id: f.id.clone(),
                name: f.name.clone(),
                workspace_id: f.workspace_id.clone(),
            });
        }
    }
    *state.active_workspace.lock().unwrap() = if saved
        .workspaces
        .iter()
        .any(|w| w.id == saved.active_workspace)
    {
        saved.active_workspace.clone()
    } else {
        saved.workspaces[0].id.clone()
    };

    // 启动行为：设置里选了「打开新标签页」时，不恢复上次会话，
    // 只在当前分组开一张空白新标签页（分组本身仍然保留）。
    if state.settings.lock().unwrap().startup == "newtab" {
        let ws = state.active_workspace.lock().unwrap().clone();
        let _ = create_tab(app, &state, "trawl://newtab".to_string(), ws, true);
        return;
    }

    let mut created: Vec<(String, String)> = Vec::new();
    for t in &saved.tabs {
        if !saved.workspaces.iter().any(|w| w.id == t.workspace_id) {
            continue;
        }
        if let Ok(id) = create_tab(app, &state, t.url.clone(), t.workspace_id.clone(), false) {
            if let Some(tab) = state.tabs.lock().unwrap().iter_mut().find(|x| x.id == id) {
                tab.title = t.title.clone();
                tab.pinned = t.pinned;
                tab.folder_id = t.folder_id.clone().filter(|fid| {
                    state.folders.lock().unwrap().iter().any(|f| f.id == *fid)
                });
            }
            created.push((id, t.workspace_id.clone()));
        }
    }
    if let Some(idx) = saved.active_tab_index {
        if let Some((id, ws)) = created.get(idx) {
            *state.active_workspace.lock().unwrap() = ws.clone();
            *state.active_tab.lock().unwrap() = Some(id.clone());
        }
    }
}

// ==================== 入口 ====================

// ==================== 浮层（查找 / 历史 / 书签 / 命令面板 / 下载 / 地址栏） ====================
//
// 关键点：浮层是原生 webview，盖在页面之上，**物理上会拦截鼠标**。
// 所以浮层的尺寸必须等于面板本身，绝不能铺满整个窗口（老版本就是全窗口，
// 打开查找条后整个页面点不动）。同时因为页面 webview 在 chrome 之上，
// 任何在 chrome 里向下溢出到内容区的 DOM（地址栏建议、下载 toast）都会被压住看不见，
// 因此这些 UI 一律走浮层。

/// 根据浮层类型算出窗口内的逻辑坐标矩形
#[cfg(desktop)]
fn overlay_rect(state: &AppState, kind: &str) -> (f64, f64, f64, f64) {
    let (x, y, w, h) = *state.layout.lock().unwrap();
    match kind {
        "find" => {
            let ow = 386.0_f64.min(w - 24.0).max(240.0);
            (x + w - ow - 12.0, y + 10.0, ow, 54.0)
        }
        "palette" => {
            let ow = 560.0_f64.min(w - 48.0);
            let oh = (h * 0.62).min(480.0);
            (x + (w - ow) / 2.0, y + 56.0, ow, oh)
        }
        _ => {
            let ow = 620.0_f64.min(w - 48.0);
            let oh = (h * 0.74).min(600.0);
            (x + (w - ow) / 2.0, y + 40.0, ow, oh)
        }
    }
}

#[cfg(desktop)]
fn close_overlay_inner(app: &AppHandle) {
    let state = app.state::<AppState>();
    let info = state.overlay.lock().unwrap().take();
    if let Some(info) = info {
        if let Some(wv) = app.get_webview(&info.label) {
            // 预热浮层（地址栏建议 / 更多菜单）不销毁，只隐藏：
            // 销毁后下次打开又要新建 webview，期间会抢焦点并把正在输入的
            // IME 组合串整段丢掉，而且有明显延迟。
            if is_warm_kind(&info.kind) {
                let _ = wv.hide();
            } else {
                let _ = wv.close();
            }
        }
    }
}

// ==================== 预热浮层 ====================
//
// 地址栏建议和「更多」菜单都是在用户输入/点击的瞬间弹出，如果每次都 add_child
// 新建 webview，会有两个后果：
//   1) 新建 controller 会抢一次焦点，正在输入的 IME 组合串会被 cancel
//      （Chromium 收到 blur 后发 deleteContentBackward，整段文字消失）；
//   2) 首帧要等 200~400ms，弹层明显「慢半拍」。
// 所以这两类浮层改成启动时创建好、平时隐藏，打开时只改 bounds + show。

const WARM_KINDS: [&str; 2] = ["quickbar", "menu"];

fn is_warm_kind(kind: &str) -> bool {
    WARM_KINDS.contains(&kind)
}

fn warm_basis(kind: &str) -> &'static str {
    match kind {
        "menu" => "menu.html",
        _ => "quickbar.html",
    }
}

fn warm_label(kind: &str) -> String {
    format!("warm-{}", kind)
}

/// 启动时把预热浮层建出来（隐藏、不取焦点）
#[cfg(desktop)]
fn warm_popups(app: &AppHandle) {
    let Some(window) = app.get_window("main") else { return };
    for kind in WARM_KINDS {
        let label = warm_label(kind);
        if app.get_webview(&label).is_some() {
            continue;
        }
        // 预热窗口不能给 1x1：页面会按 1x1 视口排一次版，之后复用时
        // 自己量出来的高度就是被裁掉的值（菜单会只剩一条）。
        let (ww, wh) = match kind {
            "menu" => (240.0, 430.0),
            _ => (420.0, 240.0),
        };
        let builder = WebviewBuilder::new(&label, WebviewUrl::App(warm_basis(kind).into()))
            .data_directory(webview_data_dir())
            .focused(false)
            .transparent(true);
        if window
            .add_child(builder, LogicalPosition::new(0.0, 0.0), LogicalSize::new(ww, wh))
            .is_err()
        {
            debug_log(&format!("warm popup failed: {}", kind));
            continue;
        }
        // 建完立刻藏起来，避免启动时闪一个方块
        if let Some(wv) = app.get_webview(&label) {
            let _ = wv.hide();
        }
    }
    // 创建过程会把焦点从 chrome 抢走，启动最后还回去
    if let Some(ui) = app.get_webview(CHROME_LABEL) {
        let _ = ui.set_focus();
    }
}

/// 显示预热浮层；返回 false 表示预热失败，调用方退回新建路径
#[cfg(desktop)]
fn show_warm(app: &AppHandle, kind: &str, x: f64, y: f64, w: f64, h: f64) -> bool {
    let label = warm_label(kind);
    let Some(wv) = app.get_webview(&label) else { return false };
    let state = app.state::<AppState>();
    // 先把当前浮层收掉（可能是别类型的，比如标签列表）
    let prev = state.overlay.lock().unwrap().take();
    if let Some(prev) = prev {
        if prev.label != label {
            if let Some(pwv) = app.get_webview(&prev.label) {
                if is_warm_kind(&prev.kind) {
                    let _ = pwv.hide();
                } else {
                    let _ = pwv.close();
                }
            }
        }
    }
    let _ = wv.set_bounds(tauri::Rect {
        position: LogicalPosition::new(x, y).into(),
        size: LogicalSize::new(w, h).into(),
    });
    let _ = wv.show();
    // 预热浮层是在标签页 webview 之前建的，z 序在下面，会被页面盖住；
    // set_bounds 用的是 SWP_NOZORDER，改不动层级。reparent 到同一个父窗口
    // 相当于再 SetParent 一次，能把它提到兄弟窗口最上层。
    if let Some(win) = app.get_window("main") {
        let _ = wv.reparent(&win);
        // reparent 之后位置可能被重置，再定位一次
        let _ = wv.set_bounds(tauri::Rect {
            position: LogicalPosition::new(x, y).into(),
            size: LogicalSize::new(w, h).into(),
        });
    }
    *state.overlay.lock().unwrap() = Some(OverlayState {
        label: label.clone(),
        kind: kind.to_string(),
        x,
        y,
        w,
        height: h,
    });
    // 让页面重新量一次内容高度 / 刷新一遍数据（预热时拿到的是旧快照）
    let _ = app.emit_to(&label, "warm-shown", ());
    // show() 会让新窗口拿走键盘焦点（chrome 收到 blur，地址输入框掉焦，
    // 用户接着打字/回车就全落空），所以显示之后要把焦点还回去。
    let app2 = app.clone();
    std::thread::spawn(move || {
        for delay in [50_u64, 160] {
            std::thread::sleep(std::time::Duration::from_millis(delay));
            if let Some(ui) = app2.get_webview(CHROME_LABEL) {
                let _ = ui.set_focus();
            }
        }
        // 再让 chrome 把 DOM 焦点放回地址输入框
        let _ = app2.emit_to(CHROME_LABEL, "refocus", ());
    });
    true
}

/// 关闭某个 webview。
///
/// 只能从创建线程调用：`close()` 销毁 webview 时 WebView2 会做同步收尾，
/// 与排队中的 `add_child` 撞在一起会把主线程卡死（第二个 add_child 永远等不到消息循环）。
#[cfg(desktop)]
fn close_webview_now(app: &AppHandle, label: &str) {
    if let Some(wv) = app.get_webview(label) {
        let _ = wv.close();
    }
}

/// 把关闭动作排进创建队列（主线程/命令回调里一律用这个）
#[cfg(desktop)]
fn close_webview_later(app: &AppHandle, label: String) {
    spawn_create(app, move |app| close_webview_now(app, &label));
}

/// 创建浮层 webview（尺寸 == 面板尺寸，透明背景由页面自己铺卡片）。
///
/// 前提：必须在工作线程里调用（`add_child` 阻塞等主线程，否则窗口假死）。
#[cfg(desktop)]
fn create_popup(
    app: &AppHandle,
    page: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    kind: &str,
    focus: bool,
) {
    close_overlay_inner(app);
    let state = app.state::<AppState>();
    let Some(window) = app.get_window("main") else { return };
    // 标签唯一：旧浮层是异步销毁的，复用同名 label 会和它撞车导致创建失败
    let label = gen_id(&format!("overlay-{}", kind));
    let builder = WebviewBuilder::new(&label, WebviewUrl::External(versioned_url(&page).parse().expect("popup url")))
        .data_directory(webview_data_dir())
        // 关键：不传 focused(false) 时 wry 会对新建的 controller 调 MoveFocus，
        // 直接把焦点从地址栏抢走——正在输入的 IME 组合串会被整段丢弃，
        // 回车也会落到浮层上（表现为「输完网址按回车没反应」）。
        .focused(focus)
        .transparent(true);

    if window
        .add_child(builder, LogicalPosition::new(x, y), LogicalSize::new(w, h))
        .is_err()
    {
        debug_log(&format!("popup create failed: {} at ({},{}) {}x{}", kind, x, y, w, h));
        return;
    }
    if let Some(wv) = app.get_webview(&label) {
        let _ = wv.show();
        // 地址栏建议 / 下拉菜单不能抢焦点：焦点必须留在 chrome 的地址输入框里，
        // 否则用户接着打字会打到浮层上，回车也会被浮层吃掉。
        // WebView2 在 controller 挂载完成后还会再抢一次焦点，所以「创建后立刻
        // set_focus 回去」是无效的：必须延迟重试，最后再让 chrome 把 DOM 焦点
        // 还给地址框（浮层是被点开/打字弹出的，DOM 焦点这时已经掉到 body 上了）。
        if focus {
            let _ = wv.set_focus();
        } else {
            let app2 = app.clone();
            std::thread::spawn(move || {
                for delay in [70_u64, 180, 340] {
                    std::thread::sleep(std::time::Duration::from_millis(delay));
                    if let Some(ui) = app2.get_webview(CHROME_LABEL) {
                        let _ = ui.set_focus();
                    }
                }
                let _ = app2.emit_to(CHROME_LABEL, "refocus", ());
            });
        }
    }
    *state.overlay.lock().unwrap() = Some(OverlayState {
        label,
        kind: kind.to_string(),
        x,
        y,
        w,
        height: h,
    });
}

/// 居中面板（历史 / 书签 / 下载 / 命令面板）
#[cfg(desktop)]
fn show_overlay(app: &AppHandle, kind: &str) {
    let state = app.state::<AppState>();
    let (ox, oy, ow, oh) = overlay_rect(&state, kind);
    create_popup(
        app,
        format!("overlay.html?action={}", kind),
        ox,
        oy,
        ow,
        oh,
        kind,
        true,
    );
}

/// 主窗口逻辑尺寸（宽, 高），用于把浮层高度钳制在窗口范围内
#[cfg(desktop)]
fn window_logical_size(app: &AppHandle) -> (f64, f64) {
    if let Some(w) = app.get_window("main") {
        if let Ok(s) = w.inner_size() {
            let scale = w.scale_factor().unwrap_or(1.0);
            return (s.width as f64 / scale, s.height as f64 / scale);
        }
    }
    (0.0, 0.0)
}

/// 浮层可用高度上限：窗口高度 - 顶部偏移 - 余量。
/// 超出部分会被窗口边缘裁掉且无法滚动到（菜单溢出 bug 的根因）。
#[cfg(desktop)]
fn popup_max_h(app: &AppHandle, y: f64) -> f64 {
    let (_, wh) = window_logical_size(app);
    (wh - y - 6.0).max(120.0)
}

/// 「更多」下拉菜单：贴着触发按钮向下展开，尺寸由 chrome 侧给出
#[tauri::command]
#[cfg(desktop)]
fn open_menu(app: AppHandle, x: f64, y: f64, width: f64, height: f64) {
    spawn_create(&app, move |app| {
        let w = width.max(200.0);
        // 菜单项比之前多了一倍，高度上限放宽，屏幕不够时页面内部滚动；
        // 但绝不能超过窗口底边 —— 超出部分既显示不出来也滚不到
        let h = height.clamp(160.0, 780.0).min(popup_max_h(app, y));
        if show_warm(app, "menu", x, y, w, h) {
            return;
        }
        create_popup(app, "menu.html".to_string(), x, y, w, h, "menu", false);
    });
}

/// 左上角 Logo 的「标签分组 / 工作区」面板（Tabbit 风格）。
/// 与右键菜单不同，它带输入框和配色圆点，所以用独立页面；再次点击 Logo 收回。
#[tauri::command]
#[cfg(desktop)]
fn open_brandpanel(app: AppHandle, x: f64, y: f64, width: f64, height: f64) {
    let state = app.state::<AppState>();
    let is_open = state
        .overlay
        .lock()
        .unwrap()
        .as_ref()
        .map(|i| i.kind == "brandpanel")
        .unwrap_or(false);
    if is_open {
        spawn_create(&app, |app| close_overlay_inner(app));
        return;
    }
    spawn_create(&app, move |app| {
        let w = width.max(280.0);
        let h = height.clamp(360.0, 760.0);
        create_popup(app, "brandpanel.html".to_string(), x, y, w, h, "brandpanel", false);
    });
}

/// 标签列表（夸克式网格视图）
#[tauri::command]
#[cfg(desktop)]
fn open_tablist(app: AppHandle) {
    spawn_create(&app, move |app| {
        let state = app.state::<AppState>();
        // 再次点击标签列表按钮时收回面板（toggle 行为）
        let already_open = state
            .overlay
            .lock()
            .unwrap()
            .as_ref()
            .map(|i| i.kind == "tablist")
            .unwrap_or(false);
        if already_open {
            close_overlay_inner(app);
            return;
        }
        let (x, y, w, h) = { *state.layout.lock().unwrap() };
        // 参考夸克的标签管理页：面板要够大，位置在中上（不要垂直居中），
        // 四周留白给阴影，否则 webview 的矩形边界会把阴影裁成硬边。
        let ow = (w * 0.78).clamp(900.0, 1300.0);
        let oh = (h * 0.78).clamp(480.0, 720.0);
        let ox = x + (w - ow) / 2.0;
        let oy = y + (h * 0.03).max(12.0);
        create_popup(
            app,
            "tablist.html".to_string(),
            ox,
            oy,
            ow,
            oh,
            "tablist",
            true,
        );
    });
}

/// 地址栏浮层：位置与尺寸由 chrome 侧按地址栏实际位置给出，高度会随条目数回调调整
#[tauri::command]
#[cfg(desktop)]
fn open_quickbar(app: AppHandle, x: f64, y: f64, width: f64, height: f64) {
    spawn_create(&app, move |app| open_quickbar_inner(app, x, y, width, height));
}

#[cfg(desktop)]
fn open_quickbar_inner(app: &AppHandle, x: f64, y: f64, width: f64, height: f64) {
    let ow = width.clamp(360.0, 900.0);
    let oh = height.clamp(120.0, 460.0);
    if show_warm(app, "quickbar", x, y, ow, oh) {
        return;
    }
    create_popup(app, "quickbar.html".to_string(), x, y, ow, oh, "quickbar", false);
}

/// 地址栏输入：把关键词转发给 quickbar 浮层做实时建议。
/// chrome 与浮层是两个 webview，没法直接通信，走一趟 Rust。
#[tauri::command]
#[cfg(desktop)]
fn push_suggest(app: AppHandle, q: String) {
    let state = app.state::<AppState>();
    let label = {
        let guard = state.overlay.lock().unwrap();
        match guard.as_ref() {
            Some(i) if i.kind == "quickbar" => Some(i.label.clone()),
            _ => None,
        }
    };
    if let Some(label) = label {
        let _ = app.emit_to(label, "suggest", q);
    }
}

/// 浮层自己报告需要多高（地址栏建议条数变化时）
#[tauri::command]
#[cfg(desktop)]
fn resize_overlay(webview: tauri::Webview, app: AppHandle, height: f64) {
    let h = height.clamp(56.0, 780.0);
    let state = app.state::<AppState>();
    // 只改「调用者自己」的窗口：预热浮层是常驻的，它在后台加载/渲染时也会
    // 回调一次 resize_overlay，如果按 state.overlay 去找目标，会把当前正在
    // 显示的那个浮层改错尺寸。
    let me = webview.label().to_string();
    // 用创建时记下的逻辑坐标，别去读 bounds()：那是物理像素，
    // 拿 to_logical(1.0) 转一次再当 Logical 用，尺寸会越改越偏。
    let target = {
        let mut guard = state.overlay.lock().unwrap();
        match guard.as_mut() {
            Some(i) if i.label == me => {
                i.height = h;
                Some((i.x, i.y, i.w, i.kind.clone()))
            }
            _ => None,
        }
    };
    let Some((x, mut y, w, kind)) = target else { return };
    if kind == "tablist" {
        // 面板高度变化时保持「中上」位置：顶部偏移按内容区高度取 3%（至少 12px），
        // 不能垂直居中，否则窗口一矮面板会往下掉。
        let (cy, ch) = {
            let l = state.layout.lock().unwrap();
            (l.1, l.3)
        };
        y = cy + (ch * 0.03).max(12.0);
        let mut guard = state.overlay.lock().unwrap();
        if let Some(i) = guard.as_mut() {
            if i.label == me { i.y = y; }
        }
    }
    // 钳制到窗口内：超出窗口底边的部分既看不到也滚不到
    let h_fit = h.min(popup_max_h(&app, y));
    let mut guard = state.overlay.lock().unwrap();
    if let Some(i) = guard.as_mut() {
        if i.label == me { i.height = h_fit; }
    }
    drop(guard);
    if let Some(wv) = app.get_webview(&me) {
        let _ = wv.set_bounds(tauri::Rect {
            position: LogicalPosition::new(x, y).into(),
            size: LogicalSize::new(w, h_fit).into(),
        });
    }
}

#[tauri::command]
#[cfg(desktop)]
fn close_overlay(app: AppHandle) {
    spawn_create(&app, |app| close_overlay_inner(app));
}

#[tauri::command]
#[cfg(desktop)]
fn ui_action(app: AppHandle, what: String) {
    spawn_create(&app, move |app| show_overlay(app, &what));
}

// ==================== 通用右键菜单 ====================

#[cfg(desktop)]
fn open_ctxmenu_inner(app: &AppHandle, x: f64, y: f64, width: f64, items: Vec<CtxMenuItem>) {
    let h = (items.iter().filter(|i| !i.sep).count() as f64) * 36.0
        + (items.iter().filter(|i| i.sep).count() as f64) * 9.0
        + 12.0;
    create_popup(
        app,
        "ctxmenu.html".to_string(),
        x,
        y,
        width.max(168.0),
        h.max(60.0),
        "ctx",
        false,
    );
}

#[tauri::command]
#[cfg(desktop)]
fn open_ctxmenu(app: AppHandle, x: f64, y: f64, width: f64, items: Vec<CtxMenuItem>) {
    let state = app.state::<AppState>();
    *state.ctx_items.lock().unwrap() = items.clone();
    spawn_create(&app, move |app| open_ctxmenu_inner(app, x, y, width, items));
}

#[tauri::command]
#[cfg(desktop)]
fn get_ctxmenu(state: State<'_, AppState>) -> Vec<CtxMenuItem> {
    state.ctx_items.lock().unwrap().clone()
}

/// 右键菜单项被点击：先把动作转给 chrome（它持有标签/书签状态），再收起浮层。
/// 注意顺序：必须先 emit 再 close，否则 webview 销毁后 IPC 丢失。
#[tauri::command]
#[cfg(desktop)]
fn ctxmenu_pick(app: AppHandle, action: String) {
    // 记录当前浮层：如果前端在 ctx-action 回调里开了子菜单，overlay.label 会变，
    // 这时排队在后面的 close 就不能把刚弹出的子菜单关掉。
    let state = app.state::<AppState>();
    let cur = state
        .overlay
        .lock()
        .unwrap()
        .as_ref()
        .map(|i| i.label.clone());
    let _ = app.emit_to(CHROME_LABEL, "ctx-action", action);
    spawn_create(&app, move |app| {
        let st = app.state::<AppState>();
        let still = st
            .overlay
            .lock()
            .unwrap()
            .as_ref()
            .map(|i| i.label.clone());
        if still == cur {
            close_overlay_inner(app);
        }
    });
}

// ==================== 无边框窗口控制 ====================

#[tauri::command]
#[cfg(desktop)]
fn win_minimize(app: AppHandle) {
    if let Some(w) = app.get_window("main") {
        let _ = w.minimize();
    }
}

#[tauri::command]
#[cfg(desktop)]
fn win_toggle_maximize(app: AppHandle) {
    if let Some(w) = app.get_window("main") {
        let _ = if w.is_maximized().unwrap_or(false) {
            w.unmaximize()
        } else {
            w.maximize()
        };
    }
}

#[tauri::command]
#[cfg(desktop)]
fn win_close(app: AppHandle) {
    // close 走 send_event，非阻塞；但保持一致仍投到队列里
    spawn_create(&app, |app| {
        if let Some(w) = app.get_window("main") {
            let _ = w.close();
        }
    });
}

/// 显示并激活主窗口（托盘单击 / 托盘菜单「打开」共用）
#[cfg(desktop)]
fn show_main_window(app: &AppHandle) {
    if let Some(w) = app.get_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

// ==================== 更新检测 ====================

/// 更新源：GitHub Releases 最新版。仓库转公开后此地址即生效；
/// 拿不到（私库 / 断网 / 被墙）时静默跳过，不影响使用。
const UPDATE_API_URL: &str =
    "https://api.github.com/repos/wangtongxue1/trawl/releases/latest";

#[derive(serde::Serialize)]
struct UpdateInfo {
    has_update: bool,
    latest: String,
    url: String,
    notes: String,
}

fn version_is_newer(latest: &str, current: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.trim_start_matches('v')
            .split('.')
            .map(|p| p.split(['-', '+']).next().unwrap_or("0").parse::<u64>().unwrap_or(0))
            .collect()
    };
    let (a, b) = (parse(latest), parse(current));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

/// 检查更新：拉取 GitHub 最新 Release 并与当前版本比较。
/// 手动（设置页按钮）与启动时共用；失败返回 has_update=false 的兜底信息。
#[tauri::command]
#[cfg(desktop)]
fn check_update() -> UpdateInfo {
    check_update_impl()
}

#[cfg(desktop)]
fn check_update_impl() -> UpdateInfo {
    let fallback = UpdateInfo {
        has_update: false,
        latest: String::new(),
        url: String::new(),
        notes: "暂时无法获取更新信息（可能是网络原因，或更新源尚未公开）".into(),
    };
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .user_agent("trawl-browser")
        .build()
    {
        Ok(c) => c,
        Err(_) => return fallback,
    };
    let resp = match client
        .get(UPDATE_API_URL)
        .header("Accept", "application/vnd.github+json")
        .send()
    {
        Ok(r) => r,
        Err(_) => return fallback,
    };
    if !resp.status().is_success() {
        return fallback;
    }
    #[derive(serde::Deserialize)]
    struct Rel {
        tag_name: String,
        html_url: String,
    }
    let rel: Rel = match resp.json() {
        Ok(r) => r,
        Err(_) => return fallback,
    };
    let current = env!("CARGO_PKG_VERSION");
    let has = version_is_newer(&rel.tag_name, current);
    UpdateInfo {
        has_update: has,
        latest: rel.tag_name.trim_start_matches('v').to_string(),
        url: rel.html_url,
        notes: if has {
            format!("发现新版本 v{}（当前 v{}）", rel.tag_name.trim_start_matches('v'), current)
        } else {
            format!("当前已是最新版本 v{}", current)
        },
    }
}

/// 「以后再说」：记住被跳过的版本，本次不再弹启动提醒
#[tauri::command]
fn skip_update(app: AppHandle, version: String) {
    let state = app.state::<AppState>();
    {
        let mut s = state.settings.lock().unwrap();
        s.update_skipped = version;
    }
    let snap = state.settings.lock().unwrap().clone();
    if let Ok(json) = serde_json::to_string(&snap) {
        if let Err(e) = write_atomic(&data_root().join(SETTINGS_FILE), &json) { debug_log(&format!("写盘失败: {}", e)); }
    }
    push_state(&app);
}

/// 用系统默认浏览器打开更新页（立即更新 → 跳转下载页）
#[tauri::command]
#[cfg(desktop)]
fn open_external(app: AppHandle, url: String) {
    // 严格校验：必须是合法 http/https URL 且有 host，拒绝 shell 元字符
    // 旧做法 cmd /c start 会把 & | ^ 当命令分隔符，造成命令注入
    if !url.starts_with("http://") && !url.starts_with("https://") { return; }
    if url.chars().any(|c| matches!(c, '&' | '|' | '^' | '<' | '>' | '`' | '$' | '\r' | '\n')) {
        return;
    }
    // host 必须非空（形如 http:// 的裸协议头不允许）
    let after_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or("");
    if after_scheme.is_empty() || after_scheme.starts_with('/') { return; }
    let _ = &app;
    #[cfg(windows)]
    {
        use windows::core::HSTRING;
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SHOW_WINDOW_CMD;
        let url_h = HSTRING::from(&url);
        let open_h = HSTRING::from("open");
        unsafe {
            // ShellExecuteW 直接交给系统 shell，不经 cmd.exe，参数不存在命令解析
            ShellExecuteW(None, &open_h, &url_h, None, None, SHOW_WINDOW_CMD(1));
        }
    }
    #[cfg(not(windows))]
    {
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        let _ = std::process::Command::new(opener).arg(&url).spawn();
    }
}

/// 启动 3 秒后后台检查更新；有新版本且用户没跳过该版本时通知 chrome 弹提示
#[cfg(desktop)]
fn startup_update_check(app: AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(3));
        let info = check_update_impl();
        if !info.has_update {
            return;
        }
        let skipped = {
            let st = app.state::<AppState>();
            let s = st.settings.lock().unwrap();
            s.update_skipped.clone()
        };
        if skipped == info.latest {
            return;
        }
        let _ = app.emit_to(CHROME_LABEL, "update-available", &info);
    });
}

// ==================== 标签整理 / 排序 / 竖直标签栏 ====================

/// 智能标签整理：按域名把当前工作区未固定、未归档的标签分进文件夹
#[tauri::command]
#[cfg(desktop)]
fn smart_organize(app: AppHandle) {
    let state = app.state::<AppState>();
    let ws = state.active_workspace.lock().unwrap().clone();
    // 先只读收集分组，避免边遍历边改
    let mut groups: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    {
        let tabs = state.tabs.lock().unwrap();
        for t in tabs.iter() {
            if t.workspace_id != ws || t.pinned || t.folder_id.is_some() {
                continue;
            }
            let host = t
                .url
                .split("://")
                .nth(1)
                .unwrap_or(&t.url)
                .split('/')
                .next()
                .unwrap_or("")
                .trim_start_matches("www.")
                .to_string();
            let key = if host.is_empty() {
                "其他".to_string()
            } else {
                host
            };
            groups.entry(key).or_default().push(t.id.clone());
        }
    }
    // 只有 2 个及以上同域名标签才值得建文件夹
    for (host, ids) in groups {
        if ids.len() < 2 {
            continue;
        }
        let existing = {
            let folders = state.folders.lock().unwrap();
            folders
                .iter()
                .find(|f| f.workspace_id == ws && f.name == host)
                .map(|f| f.id.clone())
        };
        let fid = existing.unwrap_or_else(|| {
            let id = gen_id("folder");
            state.folders.lock().unwrap().push(Folder {
                id: id.clone(),
                name: host.clone(),
                workspace_id: ws.clone(),
            });
            id
        });
        let mut tabs = state.tabs.lock().unwrap();
        for t in tabs.iter_mut() {
            if ids.contains(&t.id) {
                t.folder_id = Some(fid.clone());
            }
        }
    }
    push_state(&app);
}

/// 标签排序：title=按标题 A-Z / domain=按域名 / recent=按最近访问（当前顺序即最近）
#[tauri::command]
#[cfg(desktop)]
fn sort_tabs(app: AppHandle, mode: String) {
    let state = app.state::<AppState>();
    let ws = state.active_workspace.lock().unwrap().clone();
    let mut tabs = state.tabs.lock().unwrap();
    // 固定标签、文件夹内的标签保持原位，只排序当前工作区“散装”标签
    let mut loose: Vec<usize> = tabs
        .iter()
        .enumerate()
        .filter(|(_, t)| t.workspace_id == ws && !t.pinned && t.folder_id.is_none())
        .map(|(i, _)| i)
        .collect();
    if loose.is_empty() {
        return;
    }
    loose.sort_by(|&a, &b| match mode.as_str() {
        "domain" => {
            let ha = host_key(&tabs[a].url);
            let hb = host_key(&tabs[b].url);
            ha.cmp(&hb).then_with(|| tabs[a].title.cmp(&tabs[b].title))
        }
        "recent" => b.cmp(&a),
        _ => tabs[a]
            .title
            .to_lowercase()
            .cmp(&tabs[b].title.to_lowercase()),
    });
    // 先按目标顺序把 Tab 克隆出来（Tab 已 derive Clone），再写回槽位
    let picked: Vec<Tab> = loose.iter().map(|&i| tabs[i].clone()).collect();
    for (slot, t) in loose.iter().zip(picked) {
        tabs[*slot] = t;
    }
    drop(tabs);
    // 标签 ID 没变，webview 位置不变，只需要推状态让 chrome 重绘
    push_state(&app);
}

fn host_key(url: &str) -> String {
    url.split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("")
        .trim_start_matches("www.")
        .to_lowercase()
}

#[tauri::command]
#[cfg(desktop)]
fn toggle_vertical_tabs(app: AppHandle) {
    let state = app.state::<AppState>();
    {
        let mut s = state.settings.lock().unwrap();
        s.vertical_tabs = !s.vertical_tabs;
    }
    // 立刻落盘，避免改完关窗丢失
    let settings = state.settings.lock().unwrap().clone();
    if let Ok(json) = serde_json::to_string(&settings) {
        if let Err(e) = write_atomic(&data_root().join(SETTINGS_FILE), &json) { debug_log(&format!("写盘失败: {}", e)); }
    }
    push_state(&app);
}

/// 新建独立窗口 / 无痕窗口：一个只含单页的内置浏览窗口（不写历史）
#[tauri::command]
#[cfg(desktop)]
fn open_standalone_window(app: AppHandle, incognito: bool) {
    let label = gen_id("standalone");
    let title = if incognito { "无痕窗口 · Trawl" } else { "Trawl" };
    let url = if incognito {
        versioned_url("newtab.html?incognito=1")
    } else {
        versioned_url("newtab.html")
    };
    let _ = WebviewWindowBuilder::new(&app, &label, WebviewUrl::External(url.parse().expect("standalone url")))
        .title(title)
        .inner_size(1000.0, 720.0)
        .min_inner_size(480.0, 360.0)
        .build();
}

// ==================== 页面缩放 ====================

#[tauri::command]
#[cfg(desktop)]
fn zoom_page(app: AppHandle, delta: f64) {
    let state = app.state::<AppState>();
    let Some(id) = state.active_tab.lock().unwrap().clone() else { return };
    let zoom = {
        let mut tabs = state.tabs.lock().unwrap();
        match tabs.iter_mut().find(|t| t.id == id) {
            Some(t) => {
                t.zoom = if delta == 0.0 { 1.0 } else { (t.zoom + delta).clamp(0.5, 2.0) };
                t.zoom
            }
            None => return,
        }
    };
    if let Some(wv) = app.get_webview(&id) {
        let _ = wv.set_zoom(zoom);
    }
    push_state(&app);
}

// ==================== 下载 ====================

#[tauri::command]
fn get_downloads(state: State<'_, AppState>) -> Vec<DownloadInfo> {
    let mut list = state.downloads.lock().unwrap().clone();
    list.reverse();
    list
}

#[tauri::command]
fn clear_downloads(app: AppHandle) {
    let state = app.state::<AppState>();
    state.downloads.lock().unwrap().retain(|d| d.state == "started");
    push_state(&app);
}

/// 取消指定下载：置取消令牌，下载线程在读取循环里轮询退出、清理临时文件并标记 failed
#[tauri::command]
#[cfg(desktop)]
fn download_cancel(app: AppHandle, id: String) -> bool {
    let st = app.state::<AppState>();
    // 绑到局部变量，确保 MutexGuard 临时量在 st 之前释放（否则尾部表达式借用冲突）
    let found = {
        let tokens = st.cancel_tokens.lock().unwrap();
        if let Some(flag) = tokens.get(&id) {
            flag.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    };
    found
}

/// 重试下载：复用历史记录的目标路径重新发起裸 GET；无记录时落到系统下载目录
#[tauri::command]
#[cfg(desktop)]
fn download_retry(app: AppHandle, url: String) -> bool {
    let st = app.state::<AppState>();
    let dest = {
        let list = st.downloads.lock().unwrap();
        list.iter()
            .rev()
            .find(|d| d.url == url && !d.path.is_empty())
            .map(|d| std::path::PathBuf::from(&d.path))
    };
    let dest = dest.unwrap_or_else(|| {
        let name = url.rsplit('/').next().unwrap_or("download");
        let name = name.split('?').next().unwrap_or("download");
        app.path()
            .download_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(if name.is_empty() { "download" } else { name })
    });
    spawn_download(app, url, dest);
    true
}

/// 重新打开最近关闭的普通标签：弹栈返回 url，前端负责用 new_tab 打开
#[tauri::command]
#[cfg(desktop)]
fn reopen_tab(app: AppHandle) -> Option<String> {
    app.state::<AppState>()
        .closed_tabs
        .lock()
        .unwrap()
        .pop()
        .map(|(url, _title)| url)
}

/// 备份数据结构：带版本号，便于后续格式升级兼容
#[derive(Serialize, Deserialize)]
struct BackupPayload {
    version: u32,
    exported_at: u64,
    history: Vec<HistoryEntry>,
    bookmarks: Vec<Bookmark>,
    passwords: Vec<PwRecord>,
    settings: Settings,
    downloads: Vec<DownloadInfo>,
}

/// 导出备份：历史/书签/密码/设置/下载 -> 下载目录 trawl-backup-<时间戳>.json，返回绝对路径；失败返回空串
#[tauri::command]
#[cfg(desktop)]
fn export_backup(app: AppHandle) -> String {
    let st = app.state::<AppState>();
    let payload = BackupPayload {
        version: 1,
        exported_at: now_ts(),
        history: st.history.lock().unwrap().clone(),
        bookmarks: st.bookmarks.lock().unwrap().clone(),
        passwords: load_pw_records(),
        settings: st.settings.lock().unwrap().clone(),
        downloads: st.downloads.lock().unwrap().clone(),
    };
    let json = match serde_json::to_string_pretty(&payload) {
        Ok(j) => j,
        Err(e) => {
            debug_log(&format!("导出备份失败：序列化失败 {}", e));
            return String::new();
        }
    };
    let dir = match app.path().download_dir() {
        Ok(d) => d,
        Err(e) => {
            debug_log(&format!("导出备份失败：获取下载目录失败 {}", e));
            return String::new();
        }
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        debug_log(&format!("导出备份失败：创建目录失败 {}", e));
        return String::new();
    }
    let path = dir.join(format!("trawl-backup-{}.json", now_ts()));
    if let Err(e) = write_atomic(&path, &json) {
        debug_log(&format!("导出备份失败：写盘失败 {}", e));
        return String::new();
    }
    let out = path.to_string_lossy().into_owned();
    debug_log(&format!(
        "导出备份成功 path={} 历史={} 书签={} 密码={} 下载={}",
        out, payload.history.len(), payload.bookmarks.len(), payload.passwords.len(), payload.downloads.len()
    ));
    out
}

/// 导入备份：校验版本/结构，按 url/site 去重合并回各状态并持久化；失败返回 false
#[tauri::command]
#[cfg(desktop)]
fn import_backup(app: AppHandle, path: String) -> bool {
    let content = match read_json_text(std::path::Path::new(&path)) {
        Some(c) => c,
        None => {
            debug_log(&format!("导入备份失败：读取文件失败 path={}", path));
            return false;
        }
    };
    let payload: BackupPayload = match serde_json::from_str(&content) {
        Ok(p) => p,
        Err(e) => {
            debug_log(&format!("导入备份失败：结构解析失败 path={} err={}", path, e));
            return false;
        }
    };
    if payload.version != 1 {
        debug_log(&format!("导入备份失败：版本不支持 version={}", payload.version));
        return false;
    }
    let st = app.state::<AppState>();
    // 历史：按 url 去重，同 url 保留较新的 ts
    {
        let mut list = st.history.lock().unwrap();
        for h in payload.history {
            if let Some(old) = list.iter_mut().find(|x| x.url == h.url) {
                if h.ts > old.ts { *old = h; }
            } else {
                list.push(h);
            }
        }
        if list.len() > HISTORY_CAP {
            let excess = list.len() - HISTORY_CAP;
            list.drain(0..excess);
        }
    }
    // 书签：按 url 去重
    {
        let mut list = st.bookmarks.lock().unwrap();
        for b in payload.bookmarks {
            if !list.iter().any(|x| x.url == b.url) { list.push(b); }
        }
    }
    // 密码：按 site + username 去重（blob 为 DPAPI/钥匙串密文，仅同机同账户可解）
    {
        let mut list = load_pw_records();
        for p in payload.passwords {
            if !list.iter().any(|x| x.site == p.site && x.username == p.username) {
                list.push(p);
            }
        }
        save_pw_records(&list);
    }
    // 设置：用户主动导入，以备份为准整体覆盖
    *st.settings.lock().unwrap() = payload.settings;
    // 下载：按 url + ts 去重
    {
        let mut list = st.downloads.lock().unwrap();
        for d in payload.downloads {
            if !list.iter().any(|x| x.url == d.url && x.ts == d.ts) {
                list.push(d);
            }
        }
        if list.len() > DOWNLOAD_CAP {
            let excess = list.len() - DOWNLOAD_CAP;
            list.drain(0..excess);
        }
    }
    // 持久化（下载记录本应用不单独落盘，保留在内存并推送状态）
    save_history(&st);
    save_bookmarks(&st);
    let settings = st.settings.lock().unwrap().clone();
    if let Ok(json) = serde_json::to_string(&settings) {
        if let Err(e) = write_atomic(&data_root().join(SETTINGS_FILE), &json) {
            debug_log(&format!("写盘失败: {}", e));
        }
    }
    push_state(&app);
    debug_log(&format!("导入备份成功 path={}", path));
    true
}

/// 弹窗打开时显示的历史总条数（预估用）
#[tauri::command]
#[cfg(desktop)]
fn get_initial_history_count(state: State<'_, AppState>) -> usize {
    state.history.lock().unwrap().len()
}

/// 当前页调起系统打印对话框（等同 Ctrl+P）
#[tauri::command]
#[cfg(desktop)]
fn print_page(app: AppHandle) {
    let state = app.state::<AppState>();
    let id = state.active_tab.lock().unwrap().clone();
    if let Some(id) = id {
        if let Some(wv) = app.get_webview(&id) {
            let _ = wv.eval("window.print()");
        }
    }
}

/// 应用版本号（菜单「关于」显示用）
#[tauri::command]
fn get_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

/// 打开系统邮件客户端发送反馈
#[tauri::command]
#[cfg(desktop)]
fn open_feedback() {
    let url = "mailto:feedback@trawlbrowser.com?subject=Trawl%20%E5%8F%8D%E9%A6%88";
    #[cfg(windows)]
    let _ = std::process::Command::new("cmd").args(["/c", "start", "", url]).spawn();
    #[cfg(not(windows))]
    {
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        let _ = std::process::Command::new(opener).arg(url).spawn();
    }
}

/// 退出整个应用（菜单「退出」）
#[tauri::command]
fn exit_app(app: AppHandle) {
    app.exit(0);
}

/// 全窗口截图（含浏览器界面），保存 PNG 到「图片\Trawl截图」，返回文件路径
#[cfg(windows)]
#[tauri::command]
fn capture_screen(app: AppHandle) -> Result<String, String> {
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateCompatibleBitmap, SelectObject, DeleteObject, DeleteDC,
        GetWindowDC, ReleaseDC, HBITMAP,
    };
    use windows::Win32::Graphics::GdiPlus::{
        GdipCreateBitmapFromHBITMAP, GdipDisposeImage, GdipSaveImageToFile, GdiplusShutdown,
        GdiplusStartup, GdiplusStartupInput, GpBitmap, GpImage, Ok as GdiOk,
    };
    use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, PW_RENDERFULLCONTENT};
    use windows::core::PCWSTR;

    // 主窗口是纯 Window（无关联 webview），必须用 get_window；
    // get_webview_window("main") 永远返回 None，导致截图 100% 失败。
    let Some(win) = app.get_window("main") else {
        return Err("主窗口不存在".into());
    };
    let hwnd = win.hwnd().map_err(|e| e.to_string())?;
    let mut rect = windows::Win32::Foundation::RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect).map_err(|e| e.to_string())? };
    let w = (rect.right - rect.left).max(1);
    let h = (rect.bottom - rect.top).max(1);

    let hdc_win = unsafe { GetWindowDC(Some(hwnd)) };
    let hdc_mem = unsafe { CreateCompatibleDC(None) };
    let hbmp: HBITMAP = unsafe { CreateCompatibleBitmap(hdc_win, w, h) };
    let old = unsafe { SelectObject(hdc_mem, hbmp.into()) };
    let _ = unsafe { PrintWindow(hwnd, hdc_mem, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)) };
    unsafe { SelectObject(hdc_mem, old) };
    unsafe { ReleaseDC(Some(hwnd), hdc_win) };

    // 保存目标：图片\Trawl截图\trawl-时间.png（取不到图片目录则落到数据目录）
    let dir = unsafe {
        use windows::Win32::UI::Shell::{SHGetKnownFolderPath, FOLDERID_Pictures};
        match SHGetKnownFolderPath(&FOLDERID_Pictures, Default::default(), None) {
            Ok(p) => {
                let os = p.to_string().unwrap_or_default();
                if os.is_empty() { data_root().join("screenshots") } else { std::path::PathBuf::from(os).join("Trawl截图") }
            }
            Err(_) => data_root().join("screenshots"),
        }
    };
    let _ = std::fs::create_dir_all(&dir);
    let ts = now_ts();
    let file = dir.join(format!("trawl-{}-{:06}.png", ts, ts % 1000000));

    // GDI+ 编码 PNG
    let mut token = 0usize;
    let input = GdiplusStartupInput::default();
    unsafe { GdiplusStartup(&mut token, &input, std::ptr::null_mut()) };
    let mut bmp: *mut GpBitmap = std::ptr::null_mut();
    let mut ok = false;
    unsafe {
        if GdipCreateBitmapFromHBITMAP(hbmp, windows::Win32::Graphics::Gdi::HPALETTE::default(), &mut bmp) == GdiOk && !bmp.is_null() {
            let wpath: Vec<u16> = file.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
            // PNG 编码器 CLSID {557CF406-1A04-11D3-9A73-0000F81EF32E}
            let clsid = windows::core::GUID::from_u128(0x557cf406_1a04_11d3_9a73_0000f81ef32e);
            let img: *mut GpImage = bmp.cast();
            ok = GdipSaveImageToFile(img, PCWSTR::from_raw(wpath.as_ptr()), &clsid, std::ptr::null()) == GdiOk;
            GdipDisposeImage(img);
        }
        GdiplusShutdown(token);
        let _ = DeleteObject(hbmp.into());
        let _ = DeleteDC(hdc_mem);
    }
    if ok { Ok(file.to_string_lossy().into_owned()) } else { Err("PNG 编码失败".into()) }
}

/// 删除浏览数据（设置页「隐私与安全」弹窗）。
/// hours: 1=过去1小时 / 24=过去24小时 / 168=过去7天 / 0=全部时间。
/// cookies/cache 为尽力删除：浏览器运行中部分文件被占用时返回 pending，重启浏览器后再次执行可清干净。
#[tauri::command]
#[cfg(desktop)]
fn clear_browsing_data(
    app: AppHandle,
    hours: u64,
    history: bool,
    cookies: bool,
    cache: bool,
    downloads: bool,
) -> serde_json::Value {
    let state = app.state::<AppState>();
    // hours 前端可控：乘法与减法都要防溢出，否则 debug panic / release 回绕导致误删历史
    let cutoff: u64 = if hours == 0 {
        0
    } else {
        hours.checked_mul(3600)
            .and_then(|secs| now_ts().checked_sub(secs))
            .unwrap_or(0)
    };
    let mut result = serde_json::json!({
        "history": 0, "downloads": 0, "cache_items": 0, "cache_pending": false
    });

    if history {
        let mut list = state.history.lock().unwrap();
        let before = list.len();
        list.retain(|h| h.ts < cutoff);
        result["history"] = serde_json::json!(before - list.len());
        drop(list);
        save_history(&state);
        push_state(&app);
    }

    if downloads {
        let mut list = state.downloads.lock().unwrap();
        let before = list.len();
        // 与 history 分支同语义：删除 cutoff 之后（所选时间范围内）的已完成/失败记录，
        // 保留范围外的旧记录 + 进行中的任务。hours=0 时 cutoff=0，仅留 started。
        // 注意：契约里写的 d.ts >= cutoff 方向相反（那样 hours=0 会一条不删），已按实际语义纠正。
        list.retain(|d| d.ts < cutoff || d.state == "started");
        result["downloads"] = serde_json::json!(before - list.len());
        drop(list);
        push_state(&app);
    }

    if cookies || cache {
        let base = webview_data_dir().join("EBWebView").join("Default");
        let mut targets: Vec<PathBuf> = Vec::new();
        if cache {
            for d in ["Cache", "Code Cache", "GPUCache", "DawnCache", "Service Worker/CacheStorage", "Service Worker/ScriptCache"] {
                targets.push(base.join(d));
            }
        }
        if cookies {
            targets.push(base.join("Cookies"));
            targets.push(base.join("Sessions"));
        }
        let mut removed = 0u64;
        let mut pending = false;
        for t in targets {
            if t.is_dir() {
                match std::fs::remove_dir_all(&t) { Ok(_) => removed += 1, Err(_) => pending = true }
            } else if t.is_file() {
                match std::fs::remove_file(&t) { Ok(_) => removed += 1, Err(_) => pending = true }
            }
        }
        result["cache_items"] = serde_json::json!(removed);
        result["cache_pending"] = serde_json::json!(pending);
    }

    debug_log(&format!("clear_browsing_data hours={} -> {}", hours, result));
    result
}

/// 截图存根：非 Windows 平台暂不支持（GDI+ 截图逻辑依赖 Win32 API）
#[cfg(not(windows))]
#[tauri::command]
fn capture_screen(_app: AppHandle) -> Result<String, String> {
    Err("截图功能目前仅支持 Windows 版".to_string())
}

/// 在资源管理器里定位下载文件
#[tauri::command]
#[cfg(desktop)]
fn reveal_download(path: String) {
    if path.is_empty() || !std::path::Path::new(&path).exists() {
        return;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path))
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .spawn();
    }
    // macOS：Finder 选中文件；Linux：打开所在目录
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .args(["-R", &path])
            .spawn();
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        if let Some(dir) = std::path::Path::new(&path).parent() {
            let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
        }
    }
}

/// 在系统文件管理器里打开指定目录
fn open_dir(dir: PathBuf) {
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer")
            .arg(dir.as_os_str())
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&dir).spawn();
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
    }
}

/// 打开系统「下载」文件夹
#[tauri::command]
#[cfg(desktop)]
fn open_download_dir(app: AppHandle) {
    let dir = app
        .path()
        .download_dir()
        .unwrap_or_else(|_| PathBuf::from("."));
    open_dir(dir);
}

/// 打开应用数据目录（状态、书签、历史等都存在这里）
#[tauri::command]
#[cfg(desktop)]
fn open_data_dir() {
    open_dir(data_root());
}

// ==================== 键盘快捷键 ====================

/// 热键线程 id（0 表示未就绪）
static HOTKEY_TID: AtomicU32 = AtomicU32::new(0);
/// 自定义线程消息：注册 / 注销热键
const WM_HOTKEYS_ON: u32 = 0x8001; // WM_APP + 1
const WM_HOTKEYS_OFF: u32 = 0x8002; // WM_APP + 2

// MOD_ALT = 0x0001, MOD_CONTROL = 0x0002, MOD_SHIFT = 0x0004, MOD_NOREPEAT = 0x4000
const MOD_ALT_NR: u32 = 0x0001 | 0x4000;
const MOD_CTRL_NR: u32 = 0x0002 | 0x4000;
const MOD_CTRL_SHIFT_NR: u32 = 0x0002 | 0x0004 | 0x4000;

const HOTKEYS: [(i32, u32, u32); 19] = [
    (1, MOD_CTRL_NR, b'T' as u32),
    (2, MOD_CTRL_NR, b'H' as u32),
    (3, MOD_CTRL_NR, b'F' as u32),
    (4, MOD_CTRL_NR, b'K' as u32),
    (5, MOD_CTRL_NR, b'D' as u32),
    (6, MOD_CTRL_NR, b'L' as u32),
    (7, MOD_ALT_NR, 0x25), // VK_LEFT
    (8, MOD_ALT_NR, 0x27), // VK_RIGHT
    (9, MOD_CTRL_NR, b'W' as u32),
    (10, MOD_CTRL_NR, b'R' as u32),
    (11, MOD_CTRL_NR, b'J' as u32),
    (12, MOD_CTRL_SHIFT_NR, b'B' as u32),
    (13, MOD_CTRL_NR, 0xBB), // VK_OEM_PLUS  Ctrl+=
    (14, MOD_CTRL_NR, 0xBD), // VK_OEM_MINUS Ctrl+-
    (15, MOD_CTRL_NR, b'0' as u32),
    (16, MOD_CTRL_NR, 0x09),        // VK_TAB       Ctrl+Tab   下一个标签
    (17, MOD_CTRL_SHIFT_NR, 0x09),  // VK_TAB       Ctrl+Shift+Tab 上一个标签
    // 只在 Trawl 窗口聚焦期间注册（set_hotkeys_enabled 按 Focused 切换），
    // 焦点在标签页 webview 里时 chrome 的 keydown 收不到，靠这两条全局热键补齐。
    // Ctrl+Shift+T 恢复最近关闭标签；Ctrl+Shift+L 标签列表。
    (18, MOD_CTRL_SHIFT_NR, b'T' as u32),
    (19, MOD_CTRL_SHIFT_NR, b'L' as u32),
];

/// 非 Windows 平台：热键依赖 Win32 RegisterHotKey，暂不提供
#[cfg(not(windows))]
fn set_hotkeys_enabled(_enabled: bool) {}

/// 只在主窗口获得焦点时注册热键。
/// RegisterHotKey 是**系统级**抢占：老版本不问焦点就注册 Ctrl+T/F/L…，
/// 结果用户在 Word、VS Code 里按 Ctrl+F 会弹 Trawl 的查找条，这是最招人恨的缺陷。
#[cfg(windows)]
fn set_hotkeys_enabled(enabled: bool) {
    let tid = HOTKEY_TID.load(Ordering::Relaxed);
    if tid == 0 {
        return;
    }
    use windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW;
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    unsafe {
        let _ = PostThreadMessageW(
            tid,
            if enabled { WM_HOTKEYS_ON } else { WM_HOTKEYS_OFF },
            WPARAM(0),
            LPARAM(0),
        );
    }
}

/// 非 Windows 平台：全局热键暂不提供
#[cfg(not(windows))]
fn start_global_hotkeys(_app: AppHandle) {}

/// 全局热键线程：独立消息循环，不依赖任何 webview 是否有焦点
/// （webview 内部按键不会冒泡到宿主窗口，这是 Tauri 多 webview 架构的硬限制）
#[cfg(windows)]
fn start_global_hotkeys(app: AppHandle) {
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, TranslateMessage, MSG, WM_HOTKEY,
    };

    std::thread::spawn(move || {
        HOTKEY_TID.store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);
        debug_log("hotkey thread ready（等待窗口聚焦后注册）");

        let mut msg = MSG::default();
        loop {
            let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
            if r.0 <= 0 {
                break;
            }
            match msg.message {
                WM_HOTKEY => {
                    let id = msg.wParam.0 as u32;
                    match id {
                        1 => { let _ = app.emit_to(CHROME_LABEL, "shortcut", serde_json::json!({ "what": "new_tab" })); }
                        2 => show_overlay(&app, "history"),
                        3 => show_overlay(&app, "find"),
                        4 => show_overlay(&app, "palette"),
                        5 => { let _ = app.emit_to(CHROME_LABEL, "shortcut", serde_json::json!({ "what": "bookmark" })); }
                        6 => { let _ = app.emit_to(CHROME_LABEL, "shortcut", serde_json::json!({ "what": "address" })); }
                        7 => { let _ = app.emit_to(CHROME_LABEL, "shortcut", serde_json::json!({ "what": "back" })); }
                        8 => { let _ = app.emit_to(CHROME_LABEL, "shortcut", serde_json::json!({ "what": "forward" })); }
                        9 => { let _ = app.emit_to(CHROME_LABEL, "shortcut", serde_json::json!({ "what": "close_tab" })); }
                        10 => { let _ = app.emit_to(CHROME_LABEL, "shortcut", serde_json::json!({ "what": "reload" })); }
                        11 => { let _ = app.emit_to(CHROME_LABEL, "shortcut", serde_json::json!({ "what": "downloads" })); }
                        12 => { let _ = app.emit_to(CHROME_LABEL, "shortcut", serde_json::json!({ "what": "bookmarks" })); }
                        13 => { let _ = app.emit_to(CHROME_LABEL, "shortcut", serde_json::json!({ "what": "zoom_in" })); }
                        14 => { let _ = app.emit_to(CHROME_LABEL, "shortcut", serde_json::json!({ "what": "zoom_out" })); }
                        15 => { let _ = app.emit_to(CHROME_LABEL, "shortcut", serde_json::json!({ "what": "zoom_reset" })); }
                        16 => { let _ = app.emit_to(CHROME_LABEL, "shortcut", serde_json::json!({ "what": "tab_next" })); }
                        17 => { let _ = app.emit_to(CHROME_LABEL, "shortcut", serde_json::json!({ "what": "tab_prev" })); }
                        18 => { let _ = app.emit_to(CHROME_LABEL, "shortcut", serde_json::json!({ "what": "reopen_tab" })); }
                        19 => { let _ = app.emit_to(CHROME_LABEL, "shortcut", serde_json::json!({ "what": "tablist" })); }
                        _ => {}
                    }
                }
                WM_HOTKEYS_ON => {
                    for (id, mods, vk) in HOTKEYS {
                        let ok = unsafe {
                            RegisterHotKey(None, id, HOT_KEY_MODIFIERS(mods), vk).is_ok()
                        };
                        if !ok {
                            debug_log(&format!("hotkey register failed id={}", id));
                        }
                    }
                }
                WM_HOTKEYS_OFF => {
                    for (id, _, _) in HOTKEYS {
                        let _ = unsafe { UnregisterHotKey(None, id) };
                    }
                }
                _ => {}
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                let _ = DispatchMessageW(&msg);
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 窗口程序没有控制台，panic 默认静默；统一落盘，方便排查
    std::panic::set_hook(Box::new(|info| {
        debug_log(&format!("PANIC: {}", info));
    }));
    #[cfg(mobile)]
    return mobile_run();
    #[cfg(desktop)]
    desktop_run();
}

/// 桌面入口（Win/macOS/Linux）：多 webview 架构（chrome UI + 每标签一个 webview）
#[cfg(desktop)]
fn desktop_run() {
    let app = tauri::Builder::default()
        // 单实例：两个实例会共享同一份数据目录与 WebView2 缓存，会话互相覆盖、窗口互相抢焦点。
        // 第二次启动时把已有窗口提到前台，然后本进程退出。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            use tauri::Manager;
            if let Some(w) = app.get_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        // 文件选择器：导入书签/密码/备份时让用户挑文件（desktop_run 已 cfg(desktop) 门控）
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            settings: Mutex::new(load_settings()),
            ..Default::default()
        })
        .invoke_handler(tauri::generate_handler![
            initial_state,
            new_tab,
            open_internal,
            page_pressed,
            close_tabs,
            close_all_tabs_and_newtab,
            set_pins,
            close_other_tabs,
            duplicate_tab,
            adjust_split_ratio,
            activate_tab,
            close_tab_cmd,
            reopen_tab,
            move_tab,
            toggle_pin,
            create_folder,
            delete_folder,
            rename_folder,
            move_tab_to_folder,
            open_glance,
            close_glance,
            promote_glance,
            toggle_split,
            exit_split,
            toggle_split_orientation,
            toggle_bookmark,
            remove_bookmark,
            remove_bookmarks_batch,
            get_history,
            clear_history,
            remove_history_entry,
            find_in_page,
            new_workspace,
            rename_workspace,
            ungroup_workspace,
            switch_workspace_cmd,
            close_workspace,
            close_group_tabs,
            navigate,
            nav_back,
            nav_forward,
            nav_reload,
            stop_loading,
            layout,
            get_settings,
            save_settings,
            reset_settings,
            set_autostart,
            open_default_apps,
            get_extensions,
            save_extensions,
            load_unpacked_ext,
            toggle_translate,
            translate_text,
            import_bookmarks,
            pw_list,
            pw_reveal,
            pw_check,
            pw_confirm_insecure,
            pw_add,
            pw_update,
            pw_del,
            pw_import_csv,
            verify_user_consent,
            force_close_app,
            navigate_new_tab,
            toggle_reader,
            debug_msg,
            emit_shortcut,
            ui_action,
            close_overlay,
            open_quickbar,
            open_menu,
            open_brandpanel,
            open_tablist,
            push_suggest,
            resize_overlay,
            zoom_page,
            get_downloads,
            clear_downloads,
            download_cancel,
            download_retry,
            export_backup,
            import_backup,
            clear_browsing_data,
            get_initial_history_count,
            print_page,
            get_version,
            open_feedback,
            exit_app,
            capture_screen,
            reveal_download,
            open_download_dir,
            open_data_dir,
            open_ctxmenu,
            get_ctxmenu,
            ctxmenu_pick,
            win_minimize,
            win_toggle_maximize,
            win_close,
            check_update,
            skip_update,
            open_external,
            smart_organize,
            sort_tabs,
            toggle_vertical_tabs,
            open_standalone_window
        ])
        .setup(|app| {
            // 白屏排查锚点：setup 里每一步都落盘。进程活着但没有 WebView2 子进程、
            // 且看不到 "app ready" 时，靠这几条日志可定位卡在哪一步（常见于
            // WebView2 用户数据目录被上一次未退出的实例占用）。
            debug_log("=== setup start ===");
            start_creator(app.handle().clone());
            // 任务栏托盘：关闭窗口默认缩到这里（设置里可改成完全关闭）
            {
                use tauri::menu::{Menu, MenuItem};
                use tauri::tray::{TrayIconBuilder, TrayIconEvent};
                let open = MenuItem::with_id(app, "tray-open", "打开 Trawl", true, None::<&str>)
                    .map_err(|e| format!("创建托盘菜单项失败: {e}"))?;
                let quit = MenuItem::with_id(app, "tray-quit", "退出 Trawl", true, None::<&str>)
                    .map_err(|e| format!("创建托盘菜单项失败: {e}"))?;
                let menu = Menu::with_items(app, &[&open, &quit])
                    .map_err(|e| format!("创建托盘菜单失败: {e}"))?;
                TrayIconBuilder::with_id("trawl-tray")
                    .icon(
                        app.default_window_icon()
                            .cloned()
                            .expect("应用图标缺失（打包配置未包含 icon）"),
                    )
                    .tooltip("Trawl 元搜浏览器")
                    .menu(&menu)
                    .show_menu_on_left_click(false)
                    .on_menu_event(|app, event| match event.id().as_ref() {
                        "tray-open" => show_main_window(app),
                        "tray-quit" => app.exit(0),
                        _ => {}
                    })
                    .on_tray_icon_event(|tray, event| {
                        // 左键单击托盘图标：恢复主窗口
                        if let TrayIconEvent::Click {
                            button: tauri::tray::MouseButton::Left,
                            button_state: tauri::tray::MouseButtonState::Up,
                            ..
                        } = event
                        {
                            show_main_window(tray.app_handle());
                        }
                    })
                    .build(app)
                    .map_err(|e| format!("创建托盘失败: {e}"))?;
            }
            let window = WindowBuilder::new(app, "main")
                .title("Trawl")
                .inner_size(1240.0, 820.0)
                .min_inner_size(720.0, 480.0)
                // 无边框：标题栏由 chrome webview 自己画（夸克 / Tabbit 风格），
                // Windows 下 undecorated_resizing 会自动挂上边缘缩放钩子。
                .decorations(false)
                .shadow(true)
                .build()
                .map_err(|e| format!("创建主窗口失败: {e}"))?;
            // 启动居中 + 分辨率适配：默认尺寸不超过主屏的 88%（逻辑像素），
            // 小屏幕按比例缩小，避免窗口超出屏幕或偏在角落。
            {
                let _ = window.center();
                if let Ok(Some(mon)) = window.primary_monitor() {
                    let scale = mon.scale_factor();
                    let mw = mon.size().width as f64 / scale;
                    let mh = mon.size().height as f64 / scale;
                    let w = (mw * 0.88).min(1240.0).max(760.0);
                    let h = (mh * 0.88).min(820.0).max(540.0);
                    let _ = window.set_size(tauri::LogicalSize::new(w, h));
                    let _ = window.center();
                }
            }
            let size = window.inner_size().unwrap_or_default();
            debug_log("=== setup: main window created, adding chrome webview ===");
            // 初始化内容区布局（逻辑像素），避免第一帧标签页用 800x600 的默认值
            {
                let st = app.state::<AppState>();
                let scale = window.scale_factor().unwrap_or(1.0);
                let lw = size.width as f64 / scale;
                let lh = size.height as f64 / scale;
                *st.layout.lock().unwrap() = (0.0, 0.0, lw, lh);
            }
            window
                .add_child(
                    WebviewBuilder::new(
                        CHROME_LABEL,
                        WebviewUrl::External(versioned_url("index.html").parse().expect("chrome url")),
                    )
                        .data_directory(webview_data_dir())
                        // 启动瞬间 chrome 底色：避免窗口先闪一帧黑/白
                        .background_color(tauri::window::Color(0xf5, 0xf7, 0xf9, 0xff)),
                    tauri::PhysicalPosition::new(0, 0),
                    size,
                )
                .map_err(|e| format!("创建UI webview失败: {e}"))?;
            debug_log("=== setup: chrome webview added ===");
            // 预热浮层放在创建线程里做：add_child 不能在主线程调用
            spawn_create(app.handle(), |app| warm_popups(app));
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            match event {
                tauri::WindowEvent::Resized(_) => {
                    let size = window.inner_size().unwrap_or_default();
                    if let Some(ui) = window.get_webview(CHROME_LABEL) {
                        let _ = ui.set_bounds(tauri::Rect {
                            position: tauri::PhysicalPosition::new(0, 0).into(),
                            size: size.into(),
                        });
                    }
                    let app = window.app_handle().clone();
                    let state = app.state::<AppState>();
                    {
                        let scale = window.scale_factor().unwrap_or(1.0);
                        let mut l = state.layout.lock().unwrap();
                        // 保留 x：竖直标签栏时内容区向右让出 188px，resize 不能把它清零
                        let old_x = l.0;
                        let chrome_h = l.1;
                        let lw = size.width as f64 / scale;
                        let lh = size.height as f64 / scale;
                        *l = (old_x, chrome_h, (lw - old_x).max(0.0), (lh - chrome_h).max(0.0));
                    }
                    // 内容区立即重排：以前只等 chrome 的 resize 事件上报，
                    // 拖动窗口时页面区会滞后一拍，露出白边。
                    // 注意：如果需要新建 webview，不能在主线程做（会自锁），丢给工作线程。
                    if missing_webview(&app, &state) {
                        refresh_view(&app);
                    } else {
                        show_active_tab(&app, &state);
                    }

                    // 浮层跟随窗口：地址栏浮层位置依赖 chrome 布局，直接关掉
                    let info = state
                        .overlay
                        .lock()
                        .unwrap()
                        .as_ref()
                        .map(|i| (i.label.clone(), i.kind.clone()));
                    if let Some((label, kind)) = info {
                        if kind == "quickbar" {
                            close_webview_later(&app, label);
                            *state.overlay.lock().unwrap() = None;
                        } else {
                            let (ox, oy, ow, oh) = overlay_rect(&state, &kind);
                            if let Some(wv) = app.get_webview(&label) {
                                let _ = wv.set_bounds(tauri::Rect {
                                    position: LogicalPosition::new(ox, oy).into(),
                                    size: LogicalSize::new(ow, oh).into(),
                                });
                            }
                        }
                    }
                }
                tauri::WindowEvent::Focused(focused) => {
                    set_hotkeys_enabled(*focused);
                }
                // 「关闭确认」插件：标签数达到阈值时先弹 chrome 内确认框，而不是直接关窗
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    let guard = builtin_ext("trawl-close-guard");
                    let enabled = guard.as_ref().map(|x| x.enabled).unwrap_or(false);
                    let threshold = guard
                        .as_ref()
                        .and_then(|x| x.cfg.get("threshold"))
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<usize>().ok())
                        .unwrap_or(5);
                    let count = window
                        .app_handle()
                        .state::<AppState>()
                        .tabs
                        .lock()
                        .unwrap()
                        .len();
                    let bypass = CLOSE_BYPASS.swap(false, Ordering::SeqCst);
                    let app2 = window.app_handle().clone();
                    if enabled && !bypass && count >= threshold {
                        api.prevent_close();
                        // 主线程 emit_to 会静默丢事件（确认框永远不出现 → 窗口“关不掉”），
                        // 必须丢到工作线程发送
                        std::thread::spawn(move || {
                            let _ = app2.emit_to(CHROME_LABEL, "close-guard", count);
                        });
                        return;
                    }
                    // 关闭行为设置：tray = 缩到任务栏托盘（默认）；exit = 完全关闭。
                    // bypass（force_close_app / 托盘「退出」）直通，不受影响。
                    let action = app2
                        .state::<AppState>()
                        .settings
                        .lock()
                        .unwrap()
                        .close_action
                        .clone();
                    if !bypass && action != "exit" {
                        api.prevent_close();
                        let _ = window.hide();
                        // 托盘还没建（理论上 setup 就建了）就兜底提示，避免窗口"消失"
                        if app2.tray_by_id("trawl-tray").is_none() {
                            let _ = window.show();
                        }
                    }
                }
                _ => {}
            }
        })
        .build(tauri::generate_context!())
        .expect("构建 Trawl 应用失败");

    start_state_worker(app.handle().clone());

    app.run(|app_handle, event| match event {
        RunEvent::Ready => {
            debug_log(&format!(
                "=== app ready v{} exe={:?} ===",
                env!("CARGO_PKG_VERSION"),
                std::env::current_exe()
            ));
            start_global_hotkeys(app_handle.clone());
            restore_state(app_handle);
            // 启动更新检测（3 秒后后台执行，不打扰启动流程）
            startup_update_check(app_handle.clone());
            // 重建本地扩展 content_scripts 缓存（登记目录里的文件可能已变化）
            reload_ext_scripts(app_handle);
            let state = app_handle.state::<AppState>();
            let has_ws = !state.workspaces.lock().unwrap().is_empty();
            if !has_ws {
                state.workspaces.lock().unwrap().push(Workspace {
                    id: "ws-default".into(),
                    name: "日常".into(),
                    icon: "🏠".into(),
                });
                state.workspaces.lock().unwrap().push(Workspace {
                    id: "ws-work".into(),
                    name: "工作".into(),
                    icon: "💼".into(),
                });
                state.workspaces.lock().unwrap().push(Workspace {
                    id: "ws-fun".into(),
                    name: "娱乐".into(),
                    icon: "🎮".into(),
                });
                *state.active_workspace.lock().unwrap() = "ws-default".into();
            }
            drop(state);
            let active_ws = app_handle
                .state::<AppState>()
                .active_workspace
                .lock()
                .unwrap()
                .clone();
            let ws_has_tab = app_handle
                .state::<AppState>()
                .tabs
                .lock()
                .unwrap()
                .iter()
                .any(|t| t.workspace_id == active_ws);
            let use_internal = app_handle
                .state::<AppState>()
                .settings
                .lock()
                .unwrap()
                .newtab
                == "internal";
            let start_url = if use_internal { "trawl://newtab".to_string() } else { HOME_URL.to_string() };
            // 第一个标签页的 webview 不能在 Ready 回调里建：主线程此时还在事件分发中，
            // add_child 会等主线程消息循环 → 整个窗口假死。统一丢到工作线程。
            let st = app_handle.state::<AppState>();
            if !ws_has_tab {
                let _ = create_tab(app_handle, &st, start_url, active_ws, true);
            } else {
                refresh_view(app_handle);
                push_state(app_handle);
            }
        }
        RunEvent::ExitRequested { .. } => {
            save_state(app_handle);
        }
        _ => {}
    });
}

// ==================== 移动端（Android/iOS） ====================
// Tauri 移动端只有单 webview：整个应用就是一个 webview。
// - mobile.html 是「主页」（搜索入口 / 标签列表 / 书签 / 设置入口）
// - 点搜索结果后整个 webview 导航到目标网址，初始化脚本在远程页面注入
//   「‹ 后退 / ⌂ 主页」悬浮条：‹ 用纯 JS history.back()，⌂ 直接 location 跳回主页
// - 页面标题通过 capability 授权的 mob_report 命令上报（remote 上下文仅放行这一个命令）
// - 标签只保留元信息（url/title），不保留页面滚动/表单状态（单 webview 架构的 v1 权衡）

// MobTab 跨平台定义（桌面端不使用，仅供 AppState 字段类型一致）
#[derive(Clone, Serialize)]
struct MobTab {
    id: String,
    url: String,
    title: String,
    ts: u64,
}

#[cfg(mobile)]
#[derive(Clone, Serialize)]
struct MobStatePayload {
    tabs: Vec<MobTab>,
    active: Option<String>,
}

#[cfg(mobile)]
static MOB_DATA_ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// 移动端本地资源协议地址。
/// Tauri 内部规则（manager::get_app_url）：Android/Windows 是
/// `http://tauri.localhost/`，其它平台是 `tauri://localhost/`。
/// 主页首次加载用 WebviewUrl::App 让 Tauri 自己解析，这里的地址只用于
/// 「回主页」跳转，运行时还会被 MOB_HOME（真实解析结果）覆盖。
#[cfg(mobile)]
fn mob_base() -> &'static str {
    tauri_origin()
}

#[cfg(mobile)]
static MOB_HOME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

#[cfg(mobile)]
fn mob_home_url() -> String {
    if let Some(h) = MOB_HOME.get() {
        return h.clone();
    }
    format!("{}mobile.html", mob_base())
}

/// 注入到所有页面的初始化脚本：本地页（tauri://localhost）不注入浮层，
/// 远程页注入「‹ 后退 / ⌂ 主页」悬浮条并上报页面标题。
/// ⌂ 用 location 跳转（不需要 IPC），标题上报用 capability 放行的 mob_report。
#[cfg(mobile)]
fn mob_init_js() -> String {
    let home = mob_home_url();
    r#"(() => {
  if (window.__TRAWL_MOBILE_OVERLAY__) return;
  window.__TRAWL_MOBILE_OVERLAY__ = true;
  const local = location.protocol === 'tauri:' || location.hostname === 'tauri.localhost';
  if (local) return;
  const HOME = '__HOME__';
  const invoke = (() => { try { return window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke; } catch (e) { return null; } })();
  let bar = null;
  function ensureBar() {
    if (bar || !document.body) return;
    bar = document.createElement('div');
    bar.style.cssText = 'position:fixed;top:0;left:0;right:0;height:42px;display:flex;align-items:center;gap:6px;padding:0 8px;z-index:2147483647;background:rgba(247,249,251,0.96);border-bottom:1px solid rgba(0,0,0,0.08);font-family:system-ui,-apple-system,"Segoe UI",sans-serif;';
    const back = document.createElement('div');
    back.textContent = '‹';
    back.style.cssText = 'min-width:34px;height:34px;line-height:30px;text-align:center;font-size:24px;color:#333;border-radius:17px;cursor:pointer;user-select:none;';
    back.addEventListener('click', () => { try { history.back(); } catch (e) {} });
    const homeBtn = document.createElement('div');
    homeBtn.textContent = '⌂ Trawl';
    homeBtn.style.cssText = 'height:30px;line-height:30px;font-size:13px;font-weight:600;color:#1a73e8;background:#fff;border:1px solid #dde3ea;border-radius:15px;cursor:pointer;user-select:none;padding:0 12px;';
    homeBtn.addEventListener('click', () => {
      if (invoke) {
        try {
          Promise.resolve(invoke('mob_home')).catch(() => { location.href = HOME; });
          return;
        } catch (e) {}
      }
      location.href = HOME;
    });
    bar.appendChild(back);
    bar.appendChild(homeBtn);
    document.body.appendChild(bar);
    setTimeout(report, 300);
  }
  function report() {
    if (!invoke) return;
    try {
      invoke('mob_report', { url: location.href.split('#')[0], title: (document.title || location.hostname || '').slice(0, 120) });
    } catch (e) {}
  }
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', () => { ensureBar(); report(); });
  } else {
    ensureBar();
    report();
  }
})();
"#
    .replace("__HOME__", &home)
}

#[cfg(mobile)]
fn mob_emit(app: &AppHandle) {
    let payload = {
        let state = app.state::<AppState>();
        let tabs = state.mob_tabs.lock().unwrap().clone();
        let active = state.mob_active.lock().unwrap().clone();
        MobStatePayload { tabs, active }
    };
    let _ = app.emit("mob-state", payload);
}

#[cfg(mobile)]
fn mob_navigate(app: &AppHandle, url_str: &str) -> Result<(), String> {
    let win = app
        .get_webview_window("main")
        .ok_or_else(|| "主窗口不存在".to_string())?;
    let url: tauri::Url = url_str
        .parse()
        .map_err(|e| format!("URL 解析失败 {}: {}", url_str, e))?;
    win.navigate(url).map_err(|e| e.to_string())
}

// ---- 移动端命令 ----

#[cfg(mobile)]
#[tauri::command]
fn mob_state(app: AppHandle) -> MobStatePayload {
    let state = app.state::<AppState>();
    let tabs = state.mob_tabs.lock().unwrap().clone();
    let active = state.mob_active.lock().unwrap().clone();
    MobStatePayload { tabs, active }
}

/// 解析输入（搜索词或网址）并导航整个 webview。
/// new_tab=true 或当前无活动空标签时新建标签记录。
#[cfg(mobile)]
#[tauri::command]
fn mob_open(app: AppHandle, input: String, new_tab: Option<bool>) -> Result<MobTab, String> {
    let input = input.trim().to_string();
    if input.is_empty() {
        return Err("输入为空".to_string());
    }
    let state = app.state::<AppState>();
    let settings = state.settings.lock().unwrap().clone();
    let target = resolve_input(&input, &settings);
    let id = {
        let mut tabs = state.mob_tabs.lock().unwrap();
        let mut act = state.mob_active.lock().unwrap();
        let reuse = !new_tab.unwrap_or(false)
            && act
                .as_ref()
                .and_then(|a| tabs.iter().find(|t| &t.id == a))
                .map(|t| t.url.is_empty())
                .unwrap_or(false);
        let id = if reuse {
            let a = act.clone().unwrap();
            let t = tabs.iter_mut().find(|t| t.id == a).unwrap();
            t.url = target.clone();
            a
        } else {
            let t = MobTab {
                id: gen_id("tab"),
                url: target.clone(),
                title: String::new(),
                ts: now_ts(),
            };
            let id = t.id.clone();
            tabs.push(t);
            id
        };
        *act = Some(id.clone());
        id
    };
    record_history(&state, &target, "");
    mob_navigate(&app, &target)?;
    mob_emit(&app);
    let tabs = state.mob_tabs.lock().unwrap();
    Ok(tabs
        .iter()
        .find(|t| t.id == id)
        .cloned()
        .unwrap_or(MobTab { id, url: target, title: String::new(), ts: now_ts() }))
}

/// 新建空白标签并回到主页
#[cfg(mobile)]
#[tauri::command]
fn mob_new_tab(app: AppHandle) {
    let state = app.state::<AppState>();
    {
        let mut tabs = state.mob_tabs.lock().unwrap();
        tabs.push(MobTab {
            id: gen_id("tab"),
            url: String::new(),
            title: String::new(),
            ts: now_ts(),
        });
        *state.mob_active.lock().unwrap() = tabs.last().map(|t| t.id.clone());
    }
    drop(state);
    let _ = mob_navigate(&app, &mob_home_url());
    mob_emit(&app);
}

/// 切换标签：导航到该标签的 url（空白标签回主页）
#[cfg(mobile)]
#[tauri::command]
fn mob_switch_tab(app: AppHandle, id: String) {
    let state = app.state::<AppState>();
    let url = {
        // 锁顺序统一为 mob_tabs → mob_active（与其它 mob_* 一致，避免 ABBA 死锁）
        let url = state
            .mob_tabs
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.url.clone());
        *state.mob_active.lock().unwrap() = Some(id);
        url
    };
    drop(state);
    let target = match url {
        Some(u) if !u.is_empty() => u,
        _ => mob_home_url(),
    };
    let _ = mob_navigate(&app, &target);
    mob_emit(&app);
}

/// 关闭标签；若关的是活动标签，自动切到最近一个有内容的标签（都没有就回主页）
#[cfg(mobile)]
#[tauri::command]
fn mob_close_tab(app: AppHandle, id: String) {
    let state = app.state::<AppState>();
    let was_active;
    let next_url;
    {
        let mut tabs = state.mob_tabs.lock().unwrap();
        was_active = state.mob_active.lock().unwrap().as_deref() == Some(id.as_str());
        tabs.retain(|t| t.id != id);
        let next = tabs.iter().rev().find(|t| !t.url.is_empty()).cloned();
        *state.mob_active.lock().unwrap() = next.as_ref().map(|t| t.id.clone());
        next_url = next.map(|t| t.url);
    }
    drop(state);
    if was_active {
        let target = next_url.unwrap_or_else(mob_home_url);
        let _ = mob_navigate(&app, &target);
    }
    mob_emit(&app);
}

/// 跳回主页（⌂ 按钮的 IPC 路径；浮层主路径是 location 跳转，此命令作兜底）
#[cfg(mobile)]
#[tauri::command]
fn mob_home(app: AppHandle) {
    let _ = mob_navigate(&app, &mob_home_url());
    mob_emit(&app);
}

/// 远程页面上报页面信息（capability 仅对 remote 上下文放行此命令）
#[cfg(mobile)]
#[tauri::command]
fn mob_report(app: AppHandle, url: String, title: String) {
    let state = app.state::<AppState>();
    {
        let mut tabs = state.mob_tabs.lock().unwrap();
        let mut act = state.mob_active.lock().unwrap();
        // 优先精确匹配 url（去掉 fragment 后上报），匹配不到就更新活动标签的标题
        let exact = tabs.iter_mut().find(|t| t.url == url);
        if let Some(t) = exact {
            t.title = title.clone();
        } else if let Some(a) = act.clone() {
            if let Some(t) = tabs.iter_mut().find(|t| t.id == a) {
                if !t.url.is_empty() && url.starts_with(t.url.as_str()) {
                    t.title = title.clone();
                }
            }
        }
    }
    record_history(&state, &url, &title);
    drop(state);
    mob_emit(&app);
}

/// 移动端入口：单 webview，主页 = mobile.html
#[cfg(mobile)]
fn mobile_run() {
    tauri::Builder::default()
        .manage(AppState {
            settings: Mutex::new(load_settings()),
            ..Default::default()
        })
        .invoke_handler(tauri::generate_handler![
            // 数据 / 设置（与桌面共用，均为纯数据命令）
            get_settings,
            save_settings,
            reset_settings,
            get_history,
            clear_history,
            remove_history_entry,
            toggle_bookmark,
            remove_bookmark,
            remove_bookmarks_batch,
            import_bookmarks,
            get_extensions,
            save_extensions,
            get_downloads,
            clear_downloads,
            skip_update,
            pw_list,
            pw_reveal,
            pw_check,
            pw_confirm_insecure,
            pw_add,
            pw_update,
            pw_del,
            pw_import_csv,
            verify_user_consent,
            get_version,
            debug_msg,
            exit_app,
            initial_state,
            // 移动端专属
            mob_state,
            mob_open,
            mob_new_tab,
            mob_switch_tab,
            mob_close_tab,
            mob_home,
            mob_report
        ])
        .setup(|app| {
            // 数据目录：Android/iOS 没有 USERPROFILE，用应用私有目录
            if let Ok(dir) = app.path().app_data_dir() {
                let _ = MOB_DATA_ROOT.set(dir.join("data"));
            }
            start_state_worker(app.handle().clone());
            // 用 WebviewUrl::App 交给 Tauri 解析成平台正确的本地资源地址
            // （Android: http://tauri.localhost/mobile.html）
            let win = tauri::WebviewWindowBuilder::new(
                app,
                "main",
                WebviewUrl::App(std::path::PathBuf::from("mobile.html")),
            )
            .title("Trawl")
            .initialization_script(&mob_init_js())
            .build()
            .map_err(|e| format!("创建主窗口失败: {e}"))?;
            // 记录真实的主页地址，供注入浮层的「回主页」使用
            if let Ok(u) = win.url() {
                let _ = MOB_HOME.set(u.to_string());
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Trawl 移动端启动失败");
}


#[cfg(test)]
mod tests {
    use super::*;

    fn engine(name: &str, tpl: &str) -> SearchEngine {
        SearchEngine { name: name.into(), template: tpl.into() }
    }
    fn settings_with(engines: Vec<SearchEngine>, def: &str) -> Settings {
        Settings { engines, default_engine: def.into(), ..Settings::default() }
    }

    #[test]
    fn chrome_bookmarks_roots_object_parsed() {
        // Chrome/Edge 的 Bookmarks 文件：roots 是 object（bookmark_bar/other/synced），
        // 每个 folder 才有 children —— 必须递归遍历 object 的 value
        let json = serde_json::json!({
            "checksum": "x",
            "roots": {
                "bookmark_bar": {
                    "type": "folder", "name": "Bookmarks bar",
                    "children": [
                        {"type": "url", "name": "Bing", "url": "https://www.bing.com/"},
                        {"type": "folder", "name": "子文件夹", "children": [
                            {"type": "url", "name": "GitHub", "url": "https://github.com/"}
                        ]}
                    ]
                },
                "other": {"type": "folder", "name": "Other bookmarks", "children": [
                    {"type": "url", "name": "百度", "url": "https://www.baidu.com/"}
                ]},
                "synced": {"type": "folder", "name": "Mobile bookmarks", "children": []}
            },
            "version": 1
        });
        let mut out = Vec::new();
        // 与 import_bookmarks 一致：遍历 roots 的各桶 children，桶名不计入路径
        if let Some(roots) = json.get("roots").and_then(|r| r.as_object()) {
            for (_, bucket) in roots {
                if let Some(children) = bucket.get("children") {
                    collect_bookmark_nodes(Some(children), &mut out, "");
                }
            }
        }
        assert_eq!(out.len(), 3, "应收集全部 3 条书签（含嵌套文件夹）");
        assert!(out.iter().any(|b| b.url == "https://www.bing.com/"));
        assert!(out.iter().any(|b| b.url == "https://github.com/"));
        assert!(out.iter().any(|b| b.url == "https://www.baidu.com/"));
        // 文件夹路径：根桶名（Bookmarks bar）不计入，只保留真实子文件夹
        let gh = out.iter().find(|b| b.url == "https://github.com/").unwrap();
        assert_eq!(gh.folder, "子文件夹", "嵌套文件夹应记录路径，不含根桶名");
        let bing = out.iter().find(|b| b.url == "https://www.bing.com/").unwrap();
        assert_eq!(bing.folder, "", "根桶直属书签 folder 应为空");
    }

    #[test]
    fn netscape_html_folder_path_parsed() {
        let html = r#"<!DOCTYPE NETSCAPE-Bookmark-file-1>
<TITLE>Bookmarks</TITLE>
<H1>Bookmarks</H1>
<DL><p>
    <DT><A HREF="https://root.example/">根书签</A>
    <DT><H3>工作</H3>
    <DL><p>
        <DT><A HREF="https://work.example/">工作书签</A>
        <DT><H3>子目录</H3>
        <DL><p>
            <DT><A HREF="https://sub.example/">子书签</A>
        </DL><p>
        <DT><H3>İstanbul ẞ</H3>
        <DL><p>
            <DT><A HREF="https://unicode.example/İ">Unicode 书签</A>
        </DL><p>
    </DL><p>
</DL><p>"#;
        let dir = std::env::temp_dir().join(format!("trawl-html-{}", now_ts()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("bookmarks.html");
        std::fs::write(&f, html).unwrap();
        let out = parse_bookmarks_html(f.to_str().unwrap()).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(out.len(), 4, "含 Unicode 字符的书签不应丢失（M1 回归）");
        assert_eq!(out[0].folder, "", "根书签 folder 为空");
        assert_eq!(out[1].folder, "工作");
        assert_eq!(out[2].folder, "工作/子目录", "嵌套文件夹路径用 / 分隔");
        assert_eq!(out[3].folder, "工作/İstanbul ẞ", "变长小写映射字符的文件夹名应完整保留");
        assert_eq!(out[3].url, "https://unicode.example/İ", "Unicode URL 应完整保留");
    }

    #[test]
    fn update_version_compare() {
        assert!(version_is_newer("v0.7.14", "0.7.13"));
        assert!(version_is_newer("0.8", "0.7.99"));
        assert!(version_is_newer("1.0.0", "0.9.9"));
        assert!(!version_is_newer("v0.7.13", "0.7.13"));
        assert!(!version_is_newer("0.7.12", "0.7.13"));
        assert!(!version_is_newer("0.7.13-beta", "0.7.13"));
    }

    #[cfg(desktop)]
    #[test]
    fn download_path_dedup() {
        let dir = std::env::temp_dir().join(format!("trawl-dl-test-{}", now_ts()));
        std::fs::create_dir_all(&dir).unwrap();
        let base = dir.join("setup.exe");
        // 不存在 → 原样返回
        assert_eq!(unique_download_path(&base), base);
        // 存在 → (1)，再存在 → (2)
        std::fs::write(&base, b"x").unwrap();
        let one = unique_download_path(&base);
        assert_eq!(one, dir.join("setup (1).exe"));
        std::fs::write(&one, b"x").unwrap();
        assert_eq!(unique_download_path(&base), dir.join("setup (2).exe"));
        // 无扩展名文件
        let noext = dir.join("file");
        std::fs::write(&noext, b"x").unwrap();
        assert_eq!(unique_download_path(&noext), dir.join("file (1)"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn empty_input_goes_home() {
        let s = Settings::default();
        assert_eq!(resolve_input("", &s), HOME_URL);
        assert_eq!(resolve_input("   ", &s), HOME_URL);
    }

    #[test]
    fn explicit_url_preserved() {
        let s = Settings::default();
        assert_eq!(resolve_input("https://a.com/x?y=1", &s), "https://a.com/x?y=1");
        assert_eq!(resolve_input("http://b.cn", &s), "http://b.cn");
    }

    #[test]
    fn host_like_gets_scheme() {
        let s = Settings::default();
        assert_eq!(resolve_input("example.com", &s), "https://example.com");
        assert_eq!(resolve_input("localhost:5173", &s), "https://localhost:5173");
        assert_eq!(resolve_input("192.168.1.1", &s), "https://192.168.1.1");
    }

    #[test]
    fn keyword_uses_default_engine() {
        let s = settings_with(vec![engine("Bing", "https://www.bing.com/search?q={query}")], "Bing");
        let out = resolve_input("你好 世界", &s);
        assert!(out.starts_with("https://www.bing.com/search?q="), "{}", out);
        assert!(out.contains("%E4%BD%A0%E5%A5%BD"), "中文应被编码: {}", out);
    }

    #[test]
    fn percent_placeholder_supported() {
        let s = settings_with(vec![engine("X", "https://x.com/s?q=%s")], "X");
        assert_eq!(resolve_input("abc", &s), "https://x.com/s?q=abc");
    }

    #[test]
    fn template_without_placeholder_appends() {
        let s = settings_with(vec![engine("X", "https://x.com/s?q=")], "X");
        assert_eq!(resolve_input("abc", &s), "https://x.com/s?q=abc");
    }

    /// 回归：引擎列表为空时曾经 expect panic（外部改坏 settings.json 就能触发进程崩溃）
    #[test]
    fn empty_engine_list_falls_back() {
        let s = settings_with(vec![], "");
        let out = resolve_input("hello", &s);
        assert!(out.starts_with("https://www.bing.com/search?q="), "{}", out);
    }

    #[test]
    fn empty_template_falls_back() {
        let s = settings_with(vec![engine("坏引擎", "")], "坏引擎");
        let out = resolve_input("hello", &s);
        assert!(out.starts_with("https://www.bing.com/search?q="), "{}", out);
    }

    /// 超长输入不应 panic / 不应截断出错
    #[test]
    fn very_long_input_ok() {
        let s = Settings::default();
        let q = "长".repeat(5000);
        let out = resolve_input(&q, &s);
        assert!(out.starts_with("https://www.bing.com/search?q="));
        assert!(out.len() > 5000);
    }

    /// javascript: 之类不应被当成关键字搜索（安全：不执行脚本）
    #[test]
    fn javascript_scheme_is_searched_not_executed() {
        let s = Settings::default();
        let out = resolve_input("javascript:alert(1)", &s);
        assert!(out.starts_with("https://www.bing.com/search?q="), "{}", out);
    }

    #[test]
    fn write_atomic_roundtrip() {
        let dir = std::env::temp_dir().join(format!("trawl-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("t.json");
        if let Err(e) = write_atomic(&f, "{\"a\":1}") { debug_log(&format!("写盘失败: {}", e)); }
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "{\"a\":1}");
        if let Err(e) = write_atomic(&f, "{\"a\":2}") { debug_log(&format!("写盘失败: {}", e)); }
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "{\"a\":2}");
        assert!(!dir.join("t.tmp").exists(), "临时文件应被 rename 掉");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backup_corrupt_renames_file() {
        let dir = std::env::temp_dir().join(format!("trawl-bk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("bad.json");
        std::fs::write(&f, "not json").unwrap();
        backup_corrupt(&f);
        assert!(!f.exists(), "坏文件应被移走");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
            .collect();
        assert_eq!(leftovers.len(), 1, "应生成一个 .corrupt 备份");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod encoding_tests {
    use super::*;

    #[test]
    fn read_json_text_strips_utf8_bom() {
        let dir = std::env::temp_dir().join(format!("trawl-bom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("bom.json");
        let mut bytes = vec![0xEFu8, 0xBB, 0xBF];
        bytes.extend_from_slice(b"{\"a\":1}");
        std::fs::write(&f, &bytes).unwrap();
        let s = read_json_text(&f).unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&s).is_ok(), "BOM 文件应可解析: {:?}", s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_json_text_supports_utf16le() {
        let dir = std::env::temp_dir().join(format!("trawl-u16-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("u16.json");
        let text = "{\"书签\":1}";
        let mut bytes = vec![0xFFu8, 0xFE];
        for u in text.encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        std::fs::write(&f, &bytes).unwrap();
        let s = read_json_text(&f).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).expect("UTF-16LE 文件应可解析");
        assert_eq!(v["书签"], 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}