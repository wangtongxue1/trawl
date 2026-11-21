# Trawl（元搜浏览器）

> **把浏览器做薄，把搜索做快。Less browser. More search.**

仿 Zen 的个人浏览器，使用 **Tauri 2 + WebView2** 实现。默认主页和搜索指向元宝搜索（so.qq.com）。

> 命名：英文 **Trawl**，中文 **元搜浏览器**。产品定位：把浏览器做薄，把搜索做快。

## 功能（v0.7.7）

界面全面对齐夸克 / Tabbit PC 版：**无系统标题栏**，两行 chrome —— 标签条（左侧 Brand 徽标 + 标签 + 新建 + 标签列表 + 自绘窗口按钮），
工具栏（导航组 | 胶囊地址栏（内嵌收藏 / 翻译）| 扩展 / 历史 / 下载 / 更多），地址栏下沿 2px 进度条，书签栏随开关显隐。

- **无边框窗口**：自绘最小化 / 最大化 / 关闭固定在右上角（水平、竖直标签栏都对齐），标签栏空白处可拖拽、双击最大化
- 标签条：拖拽排序、Ctrl+点击多选、中键关闭；新建 `+` 按钮紧跟最后一个标签
- **Brand 面板**（点左上角 Logo，再点一次收回）：分组重命名 + 配色圆点（含「无配色」虚线圆点）、智能标签整理（按域名归档）、
  切换垂直/水平标签栏、标签排序、在组内新建标签、将标签移到新窗口、关闭分组（保留分组）/ 取消分组（保留标签）/ 删除分组、分组列表与新建分组；
  危险操作改用**面板内自绘确认框**（原生 confirm 会阻塞 WebView2，导致浮层假死）
- 固定标签（自动排到固定区）、标签文件夹（chip 收纳）、标签列表为夸克式网格卡片（偏中上、尺寸大、四周留白给阴影）
- 胶囊地址栏：锁图标 + 域名高亮 + `回车打开` 提示；编辑态显示**完整地址**并全选，退出编辑（点走 / Esc / 回车 / 点到页面）
  自动恢复精简展示并清掉选区，不再停留在编辑高亮
- **地址栏内侧右侧**：收藏星标 + 翻译（文A）。翻译为**页内沉浸式双语翻译**（原文 + 译文对照），面板可选目标语言、随时还原；
  走微软免费翻译通道（国内直连可用，海外自动降级 Google gtx），不再跳转百度翻译页面
- 右侧工具组：扩展程序（进扩展页）、历史、下载（有任务时出现）、更多，两组之间用细线分隔
- 「更多」下拉菜单（21 项，按「标签页 / 当前页面 / 工具与设置」分组）：新建标签页、新建窗口、无痕窗口、复制标签页、
  关闭其他标签页、固定标签页、历史、书签、下载、重新加载、页内查找、缩放、复制当前网址、翻译此页、分屏、收藏、垂直标签栏、命令面板、扩展程序、设置
- **扩展程序页**（`trawl://extensions`，Ctrl+Shift+E / 地址栏拼图按钮 / 设置里都能进）：Tabbit 式布局 —— 顶栏搜索 + 开发者模式、
  加载未打包 / 更新、左侧「我的扩展程序 / 键盘快捷键 / 获取更多扩展程序（Chrome / Edge 商店入口）」；
  全部插件统一放在「我的扩展程序」一个网格里（内置带标签）；「加载未打包」读 manifest.json 校验并登记，
  带 content_scripts 的本地扩展会在匹配页面上**自动注入执行**（WebView2 下扩展的真实执行层）
- **书签导入**（书签面板 → 导入书签）：从 Chrome / Edge 自动探测导入，或导入导出的 Netscape HTML 文件
- **本机密码管理**（设置 → 自动填充和密码）：增删改查 + 浏览器导出 CSV 导入；密码用 **Windows DPAPI**（当前用户作用域）加密落盘
- **工具栏按钮可增删**：扩展 / 历史 / 下载按钮都可在「设置-外观」开关，或右键工具栏按钮直接隐藏
- 多标签切换：**Ctrl+Tab / Ctrl+Shift+Tab** 循环切换；标签栏滚轮 —— 放不下时滚动列表，放得下时直接切换标签
- **设置中心**（`trawl://settings`，18 个分类，支持 `#appearance` / `#search` / `#tabs` 等深链）：自动填充和密码、隐私与安全、
  AI 相关功能、个性化、性能、外观、标签和标签组、快捷键与手势、搜索引擎、默认浏览器、起始页面、语言、下载内容、无障碍、系统、
  重置设置、扩展程序、关于；界面字号 / 减少动效 / 高对比度 / 开机自启 / 主页地址 / 关闭标签后切换行为都真实生效
- **书签栏右键**：显示/隐藏书签栏、收藏当前页、管理全部书签；「全部书签」固定在最右侧
- **通用右键菜单浮层**：标签右键内容完整不再被页面 webview 截断
- 新标签页：圆形引擎图标、紧凑搜索框、4 种背景风格
- favicon + 加载转圈（加载中 reload 变 stop）、页面缩放、下载徽标与下载面板
- Glance 预览（Alt+点击）、左右/上下分屏、分屏比例微调
- 工作区 Ctrl+1..9、命令面板 Ctrl+K、历史 Ctrl+H、页内查找 Ctrl+F、收藏 Ctrl+D、关闭标签 Ctrl+W、刷新 Ctrl+R、
  下载 Ctrl+J、书签 Ctrl+Shift+B、扩展程序 Ctrl+Shift+E、缩放 Ctrl+= / - / 0
- 标签页懒创建 + 会话恢复、状态节流落盘（异常退出不丢会话）

> 0.7.13 主要改动：**功能迭代四件套**——①扩展页商店图标修正（Chrome 三色扇形 / Edge 蓝青渐变品牌 SVG）；
> ②设置「隐私与安全」升级：新增 Edge 式**删除浏览数据弹窗**（时间范围 1小时/24小时/7天/全部 + 浏览记录/Cookie/缓存/下载复选框，
> Rust `clear_browsing_data` 命令按时间过滤历史 + 尽力删除缓存与 Cookie（占用文件提示重启后重删）），散落的登录/Cookie 说明收敛到本分区，
> 新增「数据安全与隐私说明」卡；③主菜单新增**屏幕截图**（PrintWindow+GDI+ 存 PNG 到 图片\Trawl截图）、**打印 Ctrl+P**、
> **反馈**（mailto）、**关于 Trawl**（版本 toast）、**退出**（exit_app）；④应用图标统一定稿「层速玻璃」Logo（安装脚本此前误用旧占位 ico 的 bug 一并修复）。
>
> 0.7.12 主要改动：**全面稳定性加固**——①单实例锁（tauri-plugin-single-instance，重复启动聚焦已有窗口，杜绝多实例覆盖会话）；
> ②全部用户数据（书签/历史/设置/扩展/密码/会话）改**原子写入**（临时文件+rename，进程被杀/断电不再留半截 JSON）；
> ③数据文件损坏自动备份为 `*.json.corrupt-<时间戳>` 再走默认值，保留现场可找回；
> ④settings.json 损坏/引擎列表被清空时启动自愈并**立即落盘**；
> ⑤配置读取兼容记事本的 UTF-8(BOM)/UTF-16 编码；⑥地址栏 `resolve_input` 去掉 expect panic（引擎列表空/模板空时兜底元宝搜索），
> `save_settings` 过滤空名/空模板引擎；⑦新增 14 项 Rust 单元测试（边界输入/原子写/损坏备份/编码兼容）。
> 附：PrintWindow 截图在 DPI 虚拟化下右侧会被裁切（目检"内容溢出"为截图伪影，实测 innerW=1240/secW=860/dpr=1.25 布局正常）。
>
> 0.7.11 主要改动：**修复元宝搜索无结果**——新标签页/地址栏搜索用旧模板 `so.qq.com/s?w={query}` 会被 302 回首页（元宝搜索前端实际从 `?query=` 读关键词），模板改为 `https://so.qq.com/?query={query}`（newtab 默认列表、设置页默认列表、Rust 默认引擎三处）；`load_settings` 对已持久化的旧模板自动迁移，老配置无感修复。
>
> 0.7.10 主要改动：设置→自动填充和密码**移除「记住新密码」手动添加表单**（站点/用户名/密码/保存——与密码管理工具页的添加功能重复），密码添加统一收口到 `trawl://passwords`；空列表文案同步更新。
>
> 0.7.9 主要改动：新标签页品牌区与底部恢复为「**Trawl**(绿色加粗) 元搜浏览器」组合展示；设置页各分区内容改为**水平居中**（`.section` 加 `margin: 0 auto`）。
>
> 0.7.8 主要改动：新标签页**品牌名与底部标语改回中文**——中间品牌区恢复为「元搜浏览器 / 把浏览器做薄，把搜索做快」、底部恢复为「元搜浏览器 · Less browser. More search.」（0.7.6 的品牌名缩短仅应作用于桌面快捷方式与窗口标题，不应改动页面内品牌展示）。
>
> 0.7.7 主要改动：**新增独立的密码管理工具页**（`trawl://passwords`，设置→自动填充一键进入）——Edge 式布局：搜索 / 按站点分组 / 帐号卡片（明文显示 / 复制 / 修改 / 删除 / 备注）+ 真实的「密码检查」（重复使用与弱口令统计）+ 设置（验证开关 / CSV 导入 / 数据目录）；进入站点详情前必须通过 **Windows Hello 本机身份验证**（登录密码 / PIN / 指纹，WinRT UserConsentVerifier，未配置 Hello 时降级确认弹窗）；PwRecord 增加备注字段（老数据兼容）。
> 修复扩展页**商店入口点击无反应**：侧栏视图切换 forEach 把商店入口的 onclick 一并覆盖（且无 data-view 时两个视图全部隐藏 → 内容空白），选择器改为 `.item[data-view]`；商店文字改为链接样式。
>
> 0.7.6 主要改动：关掉最后一个标签兜底为**新标签页**（跟随「新建标签页打开内容」设置，不再落到元宝搜索）；
> 新标签页移除书签区；窗口名/品牌名统一缩短为 **Trawl**；右上角窗口控制按钮统一 28×28 圆角（与新建/标签列表一致）；
> **修复窗口无法拖动**：根因是 ACL 缺 `core:window:allow-start-dragging` 权限（每次拖动都被拒绝），已在 capabilities 补齐并顺带支持双击标签栏最大化；
> 设置页删除「AI 相关功能」分区；「翻译默认目标语言」改为直写翻译插件配置（此前写 localStorage 不生效）；
> 新标签页背景四档加大视觉差异并支持设置改动后**实时生效**。
>
> 0.7.4 主要改动：**页内沉浸式翻译**（双语对照 + 悬浮面板，微软免费通道国内直连，海外自动降级 Google，超时自动切换，不再跳百度）；
> 本地扩展真实生效（加载未打包读 manifest.json 校验，content_scripts 按匹配模式自动注入页面执行，移除假的「打包扩展程序」）；
> 书签导入（Chrome / Edge 自动探测 + HTML 文件）；本机密码管理（DPAPI 加密 + 浏览器 CSV 导入）；
> 工具栏按钮支持增删（设置-外观开关 / 右键隐藏）；「更多」菜单高度按窗口钳制（修复内容超出窗口且无法滚动）；
> 扩展页合并为单一「我的扩展程序」网格、商店入口移到左侧导航并更换正确图标；设置移除不生效项；
> 新增 Ctrl+Tab / Ctrl+Shift+Tab 循环切换与标签栏滚轮切换；**修复主线程 emit_to 静默丢事件**（关闭确认弹框不出现导致窗口「关不掉」）。

> 0.6.3 主要改动：去掉 AI 星标、扩展拼图与头像账户三个冗余按钮，收藏移进地址栏内侧并新增翻译；
> 新增扩展程序页（内置插件 + 本地扩展 + 开发者模式 + 快捷键）与地址栏拼图入口；设置页扩到 18 个分类并补上无障碍 / 系统 / 重置 / 扩展 / 默认浏览器；
> 地址栏退出编辑态彻底还原（不再保留高亮与「新标签页」标题）；标签列表面板再上移（3%）；旧版 confirm 通道全部换自绘对话框。
> **内置插件全部接真实行为**：网页翻译按插件开关/配置（目标语言、标签内/新标签打开）翻译当前页；关闭确认在关窗且标签数达到阈值时弹 chrome 内确认框（取消/全部关闭）；
> 阅读模式从「更多菜单」进入，抽正文去广告，再点一次退出；链接净化在复制网址时自动去 utm/spm/vd_/share_ 等跟踪参数（可配置保留 from/ref）；
> 插件配置存入 extensions.json（Rust 侧可读，不再只存扩展页 localStorage）；扩展页新增 Chrome 应用商店与 Edge 扩展商店入口；
> 本地页面 URL 统一带 ?v=版本号，修复 WebView2 持久缓存导致「升级后前端改动不生效」的问题。
>
> 0.6.2 主要改动：标签列表面板放大并改为偏中上、留白足够让阴影不被裁切；分组删除去掉原生 confirm 死锁，改面板内对话框；
> 右侧工具按钮重排 + 分组线 + 真实图标；更多菜单从 13 项扩到 20 项且全部接真实命令；设置中心新增「标签页」「快捷键」分类，
> 并让界面密度、新标签位置、标签列表按钮、启动行为、新标签页背景全部真实生效。
>
> 0.6.1 主要修复：标签列表面板居中并改成协调比例；「全部关闭」只留一个新标签页；地址栏右侧补齐 AI / 扩展 / 侧栏 / 历史 / 头像；
> Brand 面板可再次点击收回、去掉表情图标、改为标签分组面板（关闭分组与删除分组语义分开）；设置页整体重做为卡片式设置中心（含下载记录）；更多菜单分组 + 图标化。
>
> 0.6 主要修复：原生标题栏去掉并自绘窗口按钮；左侧按钮换成 Logo + Tabbit 风格功能菜单；
> 标签右键菜单内容被原生图层截断；书签栏右键编辑 + 全部书签靠右；搜索引擎图标化；标签列表放大 + 圆角。
>
> 0.4 主要修复：启动即假死（`save_state` 自锁）、全局热键抢占系统 Ctrl+T/F、地址栏建议被页面遮挡、
> 下载提示不可见、设置按钮无响应、书签栏开关失效、target=_blank 链接打不开、事件广播到所有远程页面。

## 技术架构

```
主窗口（纯原生窗口，无系统 UI）
 ├─ "ui" webview        —— dist/index.html：标签条 + 工具栏 + 书签栏
 ├─ "tab-*" webviews    —— 每个标签页一个网页 webview，Rust 管理位置与可见性（懒创建）
 │    "glance-*"        —— Glance 预览
 ├─ "warm-quickbar"     —— 地址栏建议，启动即预热、常驻、显示时只改 bounds
 ├─ "warm-menu"         —— 「更多」下拉，同上
 ├─ "overlay-ctx"       —— 通用右键菜单（标签 / 书签栏 / Brand 菜单共用）
 └─ "overlay-*"         —— 查找条 / 历史 / 书签 / 命令面板 / 下载 / 标签列表（按需创建）
```

### 硬约束（踩过的坑，改代码前必读）

1. **`Window::add_child` / `Webview::close()` 只能在「创建线程」调用。**
   `add_child` 内部是 `run_on_main_thread(closure)` + `rx.recv()` 阻塞等待；在主线程（命令回调、
   `RunEvent`）调用就是自己等自己，整个窗口 `Responding=False`。`close()` 同理：销毁 webview 时
   WebView2 会做同步收尾，和排队中的 `add_child` 撞上会把主线程卡死。
   现在全部串行到单条创建线程（`CREATOR` + `spawn_create`），命令里只做入队；
   关 webview 一律走 `close_webview_later()`。

2. **不要在同一个语句里重复 `lock()` 同一个 `Mutex`。**
   Rust 临时量存活到语句结束，`SavedState { tabs: state.tabs.lock()…, active_tab_index:
   state.tabs.lock()… }` 这种写法会自锁（0.3 启动假死就是这么来的）。
   每个字段先 `{ let g = …; }` 取出来再构造。

3. **`emit_to` 会对目标 webview 执行 `eval`。**
   不要在事件回调里同步推状态，统一 `push_state()` 投递到单线程队列（合并积压 + 节流落盘）；
   而且只发给 `"ui"`，不要 `app.emit`——那会把书签/设置 eval 进每个远程页面的 JS 环境。

4. **chrome 与页面是不同原生图层。**
   任何在 chrome 里向下溢出到内容区的 DOM（下拉建议、toast）都会被页面 webview 压住，
   这类 UI 必须走浮层；浮层 webview 的尺寸要等于面板尺寸，否则整块区域鼠标点不进去。

5. **新建 / 显示 webview 会抢焦点。**
   wry 默认给新建 controller 调 `MoveFocus`，所以 `WebviewBuilder` 要显式 `.focused(false)`；
   但 `show()` 仍然会让它拿走键盘焦点，chrome 收到 blur → 地址输入框掉焦 → IME 组合串被 cancel
   （Chromium 发 `deleteContentBackward`，整段字消失）、回车也落到浮层上。
   所以：地址栏建议 / 更多菜单改成**启动预热、显示时只改 bounds**，并在显示后用
   `set_focus(ui)` 重试 + `emit_to("ui", "refocus")` 把 DOM 焦点还给地址框。

6. **浮层的 z 序由创建顺序决定，`set_bounds` 动不了。**
   `set_bounds` 内部用 `SWP_NOZORDER`；预热浮层在标签页 webview 之前创建，就在它们下面，
   会被页面整块盖住（只露出超出页面顶边的一条）。显示时 `reparent` 到同一个父窗口
   （等价于再 `SetParent` 一次）能把它提到兄弟窗口最上层，之后再 `set_bounds` 一次定位。

7. **`resize_overlay` 只认「调用者自己」。**
   预热浮层常驻，后台渲染时也会回调一次；早期按 `state.overlay` 找目标，会把当前正在显示的
   另一个浮层改错尺寸。改成用命令参数 `tauri::Webview` 的 label 匹配。


## 本地构建

```powershell
cd src-tauri
cargo build                # 调试版 → target\debug\trawl.exe
npx tauri build            # 发行版 + MSI（输出 target\release\bundle\msi\）
```

日常启动：双击 `启动元搜.cmd`。

## GitHub Actions 自动发版（tauri-action）

`.github/workflows/release.yml` 已配置好（v0.7.13 版），推送 `v*` 标签或手动触发后：

| 平台 | Runner | 产物 |
|------|--------|------|
| Windows | `windows-latest` | `.msi` + `.exe`（NSIS）安装包 + 便携版 zip |
| macOS (Apple Silicon) | `macos-latest` | `aarch64.dmg` |
| macOS (Intel) | `macos-latest` | `x64.dmg` |
| Android | `ubuntu-latest` | `arm64 .apk`（默认 debug 签名，可侧载；配置 keystore Secrets 后自动换正式签名） |
| iOS（可选） | `macos-latest` | `.ipa`（手动触发并勾选 build_ios，需签名 Secrets） |

另有 `.github/workflows/ci.yml`：每次 push / PR 在 Windows / macOS / Linux
三平台跑 `cargo check` + `cargo test`（13 个单元测试），确保改动不破坏跨平台编译。

发版步骤：

```bash
git push -u origin main
git tag v0.7.13 && git push origin v0.7.13
# 到 GitHub → Actions → Release Trawl 查看进度
# 构建完成后 Release 是草稿态，检查产物后点发布
```

签名（可选，不配置也能出包）：

- **macOS 公证**：在仓库 Secrets 配置 `APPLE_CERTIFICATE`、`APPLE_CERTIFICATE_PASSWORD`、
  `APPLE_SIGNING_IDENTITY`、`APPLE_ID`、`APPLE_PASSWORD`、`APPLE_TEAM_ID`
  （不配置时用户首次打开需右键 → 打开，绕过 Gatekeeper）
- **Windows 签名**：配置 Azure Trusted Signing 的四个 AZURE_* Secrets（消除 SmartScreen 警告）
- **Android 正式签名**：`ANDROID_KEYSTORE`（keystore 文件 base64）、
  `ANDROID_KEYSTORE_PASSWORD`、`ANDROID_KEY_ALIAS`、`ANDROID_KEY_PASSWORD`
- **iOS**：复用 macOS 桌面的证书 Secrets（`APPLE_CERTIFICATE` 等）

平台说明：

- Android 工程不在仓库提交，CI 首次构建时自动 `tauri android init`（ubuntu runner 自带 SDK/NDK）
- **鸿蒙 OS**：Tauri 2 暂不支持 HarmonyOS，无法云端出包；如需鸿蒙版要独立 ArkTS 工程
  （WebView 加载同一套 dist/ 页面），属于独立开发项
- iOS 不配签名时无法出 ipa（苹果要求归档必须签名），有 Apple Developer 账号后配置 Secrets 即可


## 本机适配备忘

- 本机 `AppData\Local` 对新目录写入受限，数据统一放 `AppData\LocalLow\Trawl`
- WiX 工具集缓存：`%LOCALAPPDATA%\tauri\WixTools314`（GitHub 直连被拒时用镜像下载后解压到该目录）

## 后续可做

- 分屏比例拖拽调节、多分屏
- Glance 内一键转正为标签页
- 聚合搜索主页（Trawl 的核心构思：多引擎并发检索、聚合结果再筛）
- 已知限制：地址栏建议面板**首次**从隐藏态显示时仍会让 chrome 短暂失焦，
  如果用户此刻正在用中文输入法打拼音，那一段未上屏的组合串会丢（Chromium 收到 blur 就丢弃）。
  彻底解法是把建议面板渲染进 chrome 自己的 webview（需要让 chrome 向下延伸并保持背景透明），
  或者给浮层窗口加 `WS_EX_NOACTIVATE`。当前表现：已输入的内容保留，继续打字正常。
