# Trawl（元搜浏览器）

> **把浏览器做薄，把搜索做快。Less browser. More search.**

个人浏览器，**Tauri 2 + WebView2/WKWebView** 实现，覆盖 Windows、macOS、Android。
Trawl 取意「拖网」——一张网撒过所有搜索引擎，捞回全部结果再筛。

## 功能亮点

- **搜索优先**：新标签页即搜索，默认引擎 Bing（v0.7.18 起——元宝搜索 so.qq.com 是纯 SPA，
  任何 URL 参数都无法直达结果页，故降为常用站点之一；地址栏搜索同样走 Bing 兜底）
- **两行 chrome**：标签条 + 工具栏，无边框窗口自绘窗口按钮；标签拖拽排序、固定、分组、竖排布局
- **设置中心**（`trawl://settings`，Edge 式信息架构 + 顶部搜索）：18 个分类，
  含隐私与安全（Edge 式删除浏览数据弹窗）、外观、标签、搜索引擎、关闭行为、更新检查等
- **「…」主菜单**（Edge 交互）：新建标签/窗口、缩放、收藏夹/历史/下载/扩展、删除浏览数据、
  截图、打印、翻译、分屏、命令面板、设置、退出
- **下载管理**（v0.7.19 重构）：下载由后端线程全量接管（HTTP 直拉），面板显示
  实时进度条 / 百分比 / 已下载大小，重名文件自动 `(1)` 去重，失败自动清理残留并红字提示；
  完成后一键定位文件（Windows 资源管理器 / macOS Finder / Linux xdg-open）
- **关闭行为可选**：默认点 × 缩到任务栏托盘（点托盘图标恢复），可改为完全退出
- **启动更新检测**：后台检查 GitHub Releases 新版本，提示「立即更新 / 以后再说」；
  设置 → 关于 Trawl 可手动检查
- **扩展系统**：内置插件（页内沉浸式翻译 / 链接净化 / 关闭确认 / 阅读模式）+ 本地未打包扩展
  （manifest 校验 + content_scripts 自动注入）
- **本机密码管理**（`trawl://passwords`，Windows 专属）：DPAPI 加密、Windows Hello 验证、
  CSV 导入、密码检查
- **分屏 / Glance 预览**：左右/上下分屏、Alt+点击 Glance 预览
- **移动端（Android）**：单 webview 架构——`mobile.html` 主页（搜索 / 标签列表 / 书签 / 底部设置面板）+
  远程页面注入「‹ 后退 / ⌂ 主页」浮层，标签保留元信息
- **稳定性**：单实例锁、原子写配置、损坏自愈、UTF-8(BOM)/UTF-16 兼容、会话恢复

## 平台支持

| 平台 | 状态 | 说明 |
|------|------|------|
| Windows x64 | ✅ 完整支持 | 主力平台，全部功能可用 |
| macOS 12+（Intel / Apple Silicon） | ✅ 支持 | v0.7.20 修复启动白屏（本地资源协议地址按平台区分）；密码管理与截图暂缺 |
| Android | ✅ 支持侧载 | 单 webview 架构，debug 签名 APK 可直接安装；硬件返回键会退出应用（已知限制） |
| iOS | ⏸️ 暂停 | 移动端架构已就绪，配置签名 Secrets 后可启用 |
| HarmonyOS | ❌ | Tauri 暂不支持，需独立 ArkTS 工程 |

用户数据目录（设置 / 书签 / 历史 / 下载记录 / debug.log）：

- Windows：`%USERPROFILE%\AppData\LocalLow\Trawl`
- macOS：`~/Library/Application Support/Trawl`（v0.7.20 起修正，此前误落临时目录）
- Linux：`~/.local/share/Trawl`（遵循 XDG）

## 技术架构

```
桌面：主窗口（纯原生窗口，无系统 UI）
 ├─ "ui" webview        —— dist/index.html：标签条 + 工具栏 + 书签栏
 ├─ "tab-*" webviews    —— 每个标签页一个网页 webview，Rust 管理位置与可见性（懒创建）
 ├─ "warm-*" / "overlay-*" —— 地址栏建议、菜单等浮层（启动预热 / 按需创建）
 └─ 托盘图标            —— 关闭窗口默认驻留，左键恢复、右键菜单（打开 / 退出）

移动端：单 webview
 └─ "main" webview —— mobile.html 主页 + 导航跳转 + 注入浮层
```

### 硬约束（改代码前必读）

1. **`Window::add_child` / `Webview::close()` 只能在「创建线程」调用**——在主线程调用是
   `run_on_main_thread` 自己等自己，整个窗口假死。统一走 `spawn_create` / `close_webview_later`。
2. **不要在同一语句里重复 `lock()` 同一个 `Mutex`**（临时量存活到语句结束，会自锁）。
3. **`emit_to` 会对目标 webview 执行 `eval`**——状态推送统一走 `push_state()` 队列（节流落盘）。
4. **chrome 与页面是不同原生图层**——溢出内容区的 DOM 会被页面 webview 压住，这类 UI 必须走浮层。
5. **新建 / 显示 webview 会抢焦点**——预热浮层显示后要 `set_focus(ui)` + `emit_to("ui", "refocus")` 还焦点。
6. **标签 webview 首次加载完成前保持隐藏**（`ready_webviews`），底色用 `.background_color()` 跟主题走，
   否则会黑屏闪烁。
7. **本地资源地址必须走 `tauri_origin()`**——Windows/Android 是 `http://tauri.localhost/`，
   macOS/Linux 是 `tauri://localhost/`。硬编码前者会导致 macOS 全部本地页面加载失败
   （v0.7.20 之前的启动白屏就是这个原因），本地页面 URL 统一用 `versioned_url()` 构造。
8. **平台专属 API 必须 `#[cfg]` 门控**——Win32（热键 / DPAPI / GDI 截图）、winreg 等只在
   `#[cfg(windows)]` 内使用；非 Windows 目标提供无害降级实现。

## 本地构建

```bash
cd src-tauri
cargo check               # 快速类型检查
cargo test                # 单元测试（16 个）
cargo build               # 调试版 → target/debug/trawl(.exe)
npx tauri build           # 发行版 + 安装包（target/release/bundle/）
```

Android 交叉检查（不链接、无需 NDK）：`cargo check --target aarch64-linux-android`。

## GitHub Actions 云端构建

推送 `v*` 标签（或手动 workflow_dispatch）触发完整构建（`.github/workflows/release.yml`）；
main 分支 push 只跑快速检查（`.github/workflows/ci.yml`，Windows / macOS / Linux 三平台
`cargo check` + `cargo test`），避免重复构建与产物上传竞态。

| 平台 | 产物 |
|------|------|
| Windows | `.msi` + `.exe`（NSIS，含安装器图标）+ 便携版 zip |
| macOS | Apple Silicon `aarch64.dmg` + Intel `x64.dmg` |
| Android | `aarch64` APK（debug 签名可直接侧载；配置 `ANDROID_KEYSTORE` 等 Secrets 后走正式签名） |

移动端架构说明：Tauri 移动端只有**单 webview**，桌面「主窗口 + chrome webview + 每标签一个 webview」
的 API 不可用。`lib.rs` 按 `#[cfg(mobile)]` 分出独立入口：整个应用就是一个 webview，`mobile.html`
是主页，导航时整个 webview 跳转 URL，注入脚本提供后退/主页浮层（remote 上下文经 capability
仅放行 `mob_report` / `mob_home` 命令）。

发版步骤：

```bash
git push origin main
git tag v0.7.20 && git push origin v0.7.20   # 只有 tag 会触发 Release 构建
# 构建完成后 Release 为草稿态，检查产物后在网页/API 上转正式（make_latest）
```

签名（可选，不配置也能出包）：macOS 公证配 `APPLE_CERTIFICATE` 等 Secrets（未配置时 ad-hoc 签名，
**首次打开需右键 → 打开绕过 Gatekeeper**）；Windows 配 Azure Trusted Signing 四个 `AZURE_*`
Secrets；Android 配 `ANDROID_KEYSTORE` / `ANDROID_KEY_ALIAS` / `ANDROID_KEY_PASSWORD` /
`ANDROID_KEYSTORE_PASSWORD` 后出正式签名包，否则为 debug 签名。
