# Trawl（元搜浏览器）

> **把浏览器做薄，把搜索做快。Less browser. More search.**

个人浏览器，**Tauri 2 + WebView2** 实现。默认主页与搜索指向元宝搜索（so.qq.com）。
Trawl 取意「拖网」——一张网撒过所有搜索引擎，捞回全部结果再筛。

## 功能亮点

- **搜索优先**：新标签页即搜索（元宝搜索 + 引擎切换 + 书签），地址栏搜索直达
- **两行 chrome**：标签条 + 工具栏，无边框窗口自绘窗口按钮；标签拖拽排序、固定、分组、竖排布局
- **设置中心**（`trawl://settings`，Edge 式信息架构 + 顶部搜索）：18 个分类，
  含隐私与安全（Edge 式删除浏览数据弹窗）、外观、标签、搜索引擎、关闭行为、更新检查等
- **「…」主菜单**（Edge 交互）：新建标签/窗口、缩放、收藏夹/历史/下载/扩展、删除浏览数据、
  截图、打印、翻译、分屏、命令面板、设置、退出
- **关闭行为可选**：默认点 × 缩到任务栏托盘（点托盘图标恢复），可改为完全退出
- **启动更新检测**：后台检查 GitHub Releases 新版本，提示「立即更新 / 以后再说」；
  设置 → 关于 Trawl 可手动检查
- **扩展系统**：内置插件（页内沉浸式翻译 / 链接净化 / 关闭确认 / 阅读模式）+ 本地未打包扩展
  （manifest 校验 + content_scripts 自动注入）
- **本机密码管理**（`trawl://passwords`）：DPAPI 加密、Windows Hello 验证、CSV 导入、密码检查
- **分屏 / Glance 预览**：左右/上下分屏、Alt+点击 Glance 预览
- **稳定性**：单实例锁、原子写配置、损坏自愈、UTF-8(BOM)/UTF-16 兼容、会话恢复

## 技术架构

```
主窗口（纯原生窗口，无系统 UI）
 ├─ "ui" webview        —— dist/index.html：标签条 + 工具栏 + 书签栏
 ├─ "tab-*" webviews    —— 每个标签页一个网页 webview，Rust 管理位置与可见性（懒创建）
 ├─ "warm-*" / "overlay-*" —— 地址栏建议、菜单等浮层（启动预热 / 按需创建）
 └─ 托盘图标            —— 关闭窗口默认驻留，左键恢复、右键菜单（打开 / 退出）
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

## 本地构建

```powershell
cd src-tauri
cargo build                # 调试版 → target\debug\trawl.exe
npx tauri build            # 发行版 + MSI / NSIS（target\release\bundle\）
```

日常启动：双击 `启动元搜.cmd`。用户数据在 `%USERPROFILE%\AppData\LocalLow\Trawl`。

## GitHub Actions 云端构建

推送 `v*` 标签或 main 分支自动构建（`.github/workflows/release.yml`）：

| 平台 | 产物 |
|------|------|
| Windows | `.msi` + `.exe`（NSIS）安装包 + 便携版 zip |
| macOS | Apple Silicon `aarch64.dmg` + Intel `x64.dmg` |
| Android / iOS | ⏸️ 暂停：Tauri 移动端不支持桌面多 webview API，需移动端单 webview UI 重构后启用（job 已保留 `if: false`） |
| 鸿蒙 OS | Tauri 暂不支持 HarmonyOS，需独立 ArkTS 工程 |

CI（`.github/workflows/ci.yml`）：每次 push / PR 在 Windows / macOS / Linux 三平台
`cargo check` + `cargo test`（15 个单元测试）。

发版步骤：

```bash
git push origin main            # 自动触发构建
git tag v0.7.14 && git push origin v0.7.14
# 构建完成后 Release 为草稿态，检查产物后点发布
```

签名（可选，不配置也能出包）：macOS 公证配 `APPLE_CERTIFICATE` 等 Secrets；Windows 配
Azure Trusted Signing 四个 `AZURE_*` Secrets；均为空时 macOS ad-hoc 签名、Windows 无签名。
