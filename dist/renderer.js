/* Trawl 元搜浏览器 · chrome 渲染层 v0.5
 * 版式对齐夸克：标签栏 + 胶囊地址栏 + 右侧三个工具入口（收藏/下载/更多）。
 * 约定：
 *   1. chrome 是独立 webview，任何向下溢出到页面区的 DOM 都会被页面 webview 盖住，
 *      所以建议列表、菜单、面板一律走独立浮层 webview（quickbar/menu/overlay/tablist）。
 *   2. 这里只做渲染与转发，状态以 Rust 推来的 state 为准。
 */
const $ = (sel) => document.querySelector(sel);
const core = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const invoke = async (cmd, args) => {
  try {
    return await core.invoke(cmd, args);
  } catch (err) {
    try { core.invoke('debug_msg', { msg: `invoke FAILED ${cmd}: ${String(err)}` }); } catch {}
    throw err;
  }
};
const reportError = (msg) => { try { core.invoke('debug_msg', { msg }); } catch {} };
window.addEventListener('error', (e) => reportError(`ERR ${e.message} @${e.filename}:${e.lineno}`));
window.addEventListener('unhandledrejection', (e) => reportError(`REJ ${String(e.reason)}`));

// P1-6 无障碍：纯图标按钮用 title 补 aria-label（显式可访问名）
document.querySelectorAll('#chrome button[title]').forEach((b) => {
  if (!b.hasAttribute('aria-label')) b.setAttribute('aria-label', b.getAttribute('title') || '');
});

const chromeEl = $('#chrome');
const tabsEl = $('#tabs');
const wsPill = $('#ws-pill');
const bookmarkbarEl = $('#bookmarkbar');
const address = $('#address');
const addrScheme = $('#addr-scheme');
const addrHint = $('#addr-hint');
const progressEl = $('#progress');

let state = {
  tabs: [], activeTab: null, workspaces: [], activeWorkspace: '',
  glanceOpen: false, split: null, folders: [], bookmarks: [], downloading: 0,
  settings: { engines: [], defaultEngine: '', showBookmarksBar: true, verticalTabs: false },
};
let expandedFolders = new Set();
let selectedTabs = new Set();
let lastTabsSignature = '';
let editing = false;          // 地址栏正在输入（此时不被 state 覆盖）
let editTabId = null;         // 开始编辑时所在的标签，切走就退出编辑态
let lastLoading = false;
let progressTimer = null;
let editPoll = null;         // 编辑态焦点轮询（点页面时 blur 不一定送达）

/* ==================== 小工具 ==================== */

// 快捷键修饰键：macOS 用 ⌘(metaKey)，Windows/Linux 用 Ctrl(ctrlKey)。
// 与 index.html / menu.html 的文案替换保持同一套平台判断。
const IS_MAC = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);

/* ---- 前进/后退按钮可用性 ----
 * 后端 state 不含 can_back/can_forward，这里按标签本地记录由 chrome 主动发起的导航，
 * 渲染时据此设置按钮 disabled。页面内点链接的跳转 chrome 收不到，无法计入（见报告）。 */
const navHistory = new Map();
function navEntry(id) {
  if (!navHistory.has(id)) navHistory.set(id, { stack: [], idx: -1 });
  return navHistory.get(id);
}
function navPush(id, url) {
  if (!id || !url) return;
  const h = navEntry(id);
  if (h.stack[h.idx] === url) return;
  h.stack = h.stack.slice(0, h.idx + 1);
  h.stack.push(url);
  h.idx = h.stack.length - 1;
  if (h.stack.length > 50) { h.stack.shift(); h.idx--; }
}
function navStep(id, delta) {
  const h = navEntry(id);
  const next = h.idx + delta;
  if (next < 0 || next >= h.stack.length) return false;
  h.idx = next;
  return true;
}
function goBack() {
  const a = activeTab();
  if (a) navStep(a.id, -1);
  invoke('nav_back');
  render();
}
function goForward() {
  const a = activeTab();
  if (a) navStep(a.id, 1);
  invoke('nav_forward');
  render();
}
function updateNavButtons() {
  const a = activeTab();
  const h = a ? navEntry(a.id) : null;
  // 还没有本地记录时，用当前 url 作为栈底（例如新打开的标签）
  if (h && !h.stack.length && a && a.url) { h.stack = [a.url]; h.idx = 0; }
  $('#btn-back').disabled = !(h && h.idx > 0);
  $('#btn-fwd').disabled = !(h && h.idx < h.stack.length - 1);
}

function hostOf(url) {
  try { return new URL(url).hostname.replace(/^www\./, ''); } catch { return ''; }
}

/* ---- 站点真实图标 ----
 * 知名站点用随包内置的本地真实图标（零网络请求，秒开）；
 * 其它站点才走远程 favicon。这样新标签页/书签栏不再因为一堆远程请求而变慢。 */
const LOCAL_SITE_ICONS = {
  'so.qq.com': 'icons/yuanbao.png',
  'yuanbao.tencent.com': 'icons/yuanbao.png',
  'baidu.com': 'icons/baidu.png',
  'bing.com': 'icons/bing.png',
};
// 内联的品牌 SVG（网络受限站点的兜底真实图标，不用字母缩写）
const BRAND_SVG = {
  'google.com': '<svg viewBox="0 0 48 48" width="100%" height="100%"><path fill="#EA4335" d="M24 9.5c3.54 0 6.71 1.22 9.21 3.6l6.85-6.85C35.9 2.38 30.47 0 24 0 14.62 0 6.51 5.38 2.56 13.22l7.98 6.19C12.43 13.72 17.74 9.5 24 9.5z"/><path fill="#4285F4" d="M46.98 24.55c0-1.57-.15-3.09-.38-4.55H24v9.02h12.94c-.58 2.96-2.26 5.48-4.78 7.18l7.73 6c4.51-4.18 7.09-10.36 7.09-17.65z"/><path fill="#FBBC05" d="M10.53 28.59c-.48-1.45-.76-2.99-.76-4.59s.27-3.14.76-4.59l-7.98-6.19C.92 16.46 0 20.12 0 24c0 3.88.92 7.54 2.56 10.78l7.97-6.19z"/><path fill="#34A853" d="M24 48c6.48 0 11.93-2.13 15.89-5.81l-7.73-6c-2.15 1.45-4.92 2.3-8.16 2.3-6.26 0-11.57-4.22-13.47-9.91l-7.98 6.19C6.51 42.62 14.62 48 24 48z"/></svg>',
  'github.com': '<svg viewBox="0 0 24 24" width="100%" height="100%"><path fill="#fff" d="M12 .3a12 12 0 0 0-3.79 23.4c.6.11.82-.26.82-.58v-2.23c-3.34.73-4.04-1.42-4.04-1.42-.55-1.39-1.34-1.76-1.34-1.76-1.09-.75.08-.73.08-.73 1.2.09 1.84 1.24 1.84 1.24 1.07 1.83 2.81 1.3 3.5.99.11-.78.42-1.3.76-1.6-2.67-.3-5.47-1.34-5.47-5.93 0-1.31.47-2.38 1.24-3.22-.13-.3-.54-1.52.11-3.18 0 0 1.01-.32 3.3 1.23a11.5 11.5 0 0 1 6 0c2.29-1.55 3.3-1.23 3.3-1.23.65 1.66.24 2.88.11 3.18.77.84 1.24 1.91 1.24 3.22 0 4.6-2.8 5.62-5.48 5.92.43.37.81 1.1.81 2.22v3.29c0 .32.21.7.82.58A12 12 0 0 0 12 .3z"/></svg>',
};

/** 按 host 返回 { kind:'local'|'brand'|'remote'|'none', value } */
function iconSource(host) {
  if (!host) return { kind: 'none' };
  if (LOCAL_SITE_ICONS[host]) return { kind: 'local', value: LOCAL_SITE_ICONS[host] };
  for (const key of Object.keys(LOCAL_SITE_ICONS)) {
    if (host.endsWith('.' + key)) return { kind: 'local', value: LOCAL_SITE_ICONS[key] };
  }
  if (BRAND_SVG[host]) return { kind: 'brand', value: BRAND_SVG[host] };
  return { kind: 'remote', value: `https://${host}/favicon.ico` };
}

// favicon 内存缓存：host -> 'ok' | 'fail'。成功/失败都记，避免每次 render 都重新请求
// （标签条重绘很频繁，同一 host 反复打 /favicon.ico 既慢又浪费）。
const FAVICON_CACHE = new Map();

/** 给容器塞入 favicon：优先本地真实图标，失败回退字母。返回字母元素供 onerror 用。 */
function fillFavicon(box, host, letterEl, brandBg) {
  const src = iconSource(host);
  if (src.kind === 'local') {
    const img = document.createElement('img');
    img.alt = ''; img.src = src.value; img.style.display = 'none';
    img.onload = () => { if (img.naturalWidth > 1) { letterEl.style.display = 'none'; img.style.display = 'block'; } };
    img.onerror = () => img.remove();
    box.appendChild(img);
  } else if (src.kind === 'brand') {
    const wrap = document.createElement('div');
    wrap.className = 'brand-icon';
    if (brandBg) wrap.style.background = brandBg;
    wrap.innerHTML = src.value;
    letterEl.style.display = 'none';
    box.appendChild(wrap);
  } else if (src.kind === 'remote') {
    // 上次失败过的 host 不再重复请求，直接留字母占位
    if (FAVICON_CACHE.get(host) === 'fail') return;
    const img = document.createElement('img');
    img.alt = ''; img.src = src.value; img.style.display = 'none';
    img.onload = () => { FAVICON_CACHE.set(host, 'ok'); if (img.naturalWidth > 1) { letterEl.style.display = 'none'; img.style.display = 'block'; } };
    img.onerror = () => { FAVICON_CACHE.set(host, 'fail'); img.remove(); };
    box.appendChild(img);
  }
}
function isInternal(tab) {
  if (!tab) return false;
  return tab.url.startsWith('trawl://') ||
    tab.url.includes('/newtab.html') || tab.url.includes('/settings.html') ||
    tab.url.includes('/extensions.html');
}
function internalName(tab) {
  if (!tab) return '';
  if (tab.url.includes('extensions')) return '扩展程序';
  if (tab.url.includes('settings')) return '设置';
  return '新标签页';
}
function activeTab() { return state.tabs.find(t => t.id === state.activeTab); }
function wsTabs() { return state.tabs.filter(t => t.workspaceId === state.activeWorkspace); }
function isBookmarked(url) { return state.bookmarks.some(b => b.url === url); }
function defaultEngineName() { return state.settings?.defaultEngine || '元宝搜索'; }

/** 地址栏展示文本：去掉协议与末尾斜杠，像夸克那样只留 host+path */
function displayUrl(tab) {
  if (!tab) return '';
  if (isInternal(tab)) return internalName(tab);
  return tab.url.replace(/^https?:\/\//, '').replace(/\/$/, '');
}

/* ==================== 标签页 ==================== */

function buildFavicon(tab) {
  const box = document.createElement('div');
  box.className = 'tab-fav';
  const letter = document.createElement('span');
  letter.className = 'tab-letter';
  const host = hostOf(tab.url);
  // 内部页用带背景的 T；外部站点先用域名首字母，本地/品牌真实图标加载后替换
  letter.textContent = isInternal(tab) ? 'T' : (host ? host.charAt(0).toUpperCase() : '·');
  box.appendChild(letter);
  if (host && !isInternal(tab)) fillFavicon(box, host, letter, '#24292f');
  return box;
}

// 书签栏显示策略（Edge 三档）：always / newtab / never，存 localStorage，向后兼容旧布尔
function bookmarkBarMode() {
  const m = localStorage.getItem('bmBarMode');
  if (m === 'always' || m === 'newtab' || m === 'never') return m;
  return state.settings?.showBookmarksBar === false ? 'never' : 'always';
}
function isNewTabPage(url) {
  return !!url && (url.startsWith('trawl://newtab') || url.includes('/newtab.html'));
}
function applyBookmarkBar() {
  const mode = bookmarkBarMode();
  const show = mode === 'always' || (mode === 'newtab' && isNewTabPage(activeTab()?.url));
  bookmarkbarEl.classList.toggle('hidden', !show);
  if (show) { bookmarkbarEl.dataset.sig = ''; renderBookmarkBar(); }
}

function buildTabEl(tab, pinned) {
  const el = document.createElement('div');
  el.className = 'tab'
    + (tab.id === state.activeTab ? ' active' : '')
    + (selectedTabs.has(tab.id) ? ' selected' : '')
    + (pinned ? ' pinned-only' : '')
    + (tab.loading ? ' loading' : '');
  el.draggable = true;
  el.dataset.id = tab.id;
  el.dataset.pinned = pinned ? '1' : '0';
  el.dataset.tauriDragRegion = 'false';   // 标签本身可点，别被窗口拖拽吞掉
  el.title = tab.title || '新标签页';
  // P1-6 无障碍：标签可键盘聚焦并用 Enter/Space 激活
  el.setAttribute('role', 'tab');
  el.tabIndex = 0;
  el.setAttribute('aria-label', `标签页：${el.title}`);

  el.appendChild(buildFavicon(tab));

  if (!pinned) {
    const title = document.createElement('div');
    title.className = 'tab-title';
    title.textContent = tab.title || '新标签页';
    el.appendChild(title);
    const close = document.createElement('div');
    close.className = 'tab-close';
    close.textContent = '×';
    close.title = '关闭标签页';
    close.onclick = (e) => { e.stopPropagation(); invoke('close_tab_cmd', { id: tab.id }); };
    el.appendChild(close);
  }

  el.addEventListener('click', (e) => {
    if (e.altKey) { invoke('open_glance', { id: tab.id }); return; }
    // 多选修饰键：macOS 用 ⌘+点击，Windows 用 Ctrl+点击
    if (IS_MAC ? e.metaKey : e.ctrlKey) {
      selectedTabs.has(tab.id) ? selectedTabs.delete(tab.id) : selectedTabs.add(tab.id);
      render();
      return;
    }
    // 多选态下普通单击直接切到目标标签并清空选择，不再只清选择却不切标签
    if (selectedTabs.size) { selectedTabs.clear(); render(); }
    invoke('activate_tab', { id: tab.id });
  });
  el.addEventListener('auxclick', (e) => {
    if (e.button === 1) { e.preventDefault(); invoke('close_tab_cmd', { id: tab.id }); }
  });
  el.addEventListener('keydown', (e) => {
    if ((e.key === 'Enter' || e.key === ' ') && e.target === el) { e.preventDefault(); el.click(); }
  });
  el.addEventListener('contextmenu', (e) => { e.preventDefault(); showTabContextMenu(e, tab); });

  el.addEventListener('dragstart', (e) => {
    e.dataTransfer.setData('text/trawl-tab', tab.id);
    e.dataTransfer.effectAllowed = 'move';
    el.classList.add('dragging');
  });
  el.addEventListener('dragend', () => el.classList.remove('dragging'));
  el.addEventListener('dragover', (e) => {
    e.preventDefault();
    e.dataTransfer.dropEffect = 'move';
    const r = el.getBoundingClientRect();
    const before = e.clientX < r.left + r.width / 2;
    el.classList.toggle('drop-before', before);
    el.classList.toggle('drop-after', !before);
  });
  el.addEventListener('dragleave', () => el.classList.remove('drop-before', 'drop-after'));
  el.addEventListener('drop', (e) => {
    e.preventDefault();
    const dragId = e.dataTransfer.getData('text/trawl-tab');
    el.classList.remove('drop-before', 'drop-after');
    if (!dragId || dragId === tab.id) return;
    const r = el.getBoundingClientRect();
    const before = e.clientX < r.left + r.width / 2;
    if (before) {
      invoke('move_tab', { id: dragId, beforeId: tab.id, pinned });
    } else {
      const list = pinned ? state.tabs.filter(t => t.pinned) : state.tabs.filter(t => !t.pinned);
      const idx = list.findIndex(t => t.id === tab.id);
      const next = list[idx + 1];
      invoke('move_tab', { id: dragId, beforeId: next ? next.id : null, pinned });
    }
  });
  return el;
}

function buildFolderChip(folder) {
  const expanded = expandedFolders.has(folder.id);
  const count = state.tabs.filter(t => t.folderId === folder.id).length;
  const chip = document.createElement('div');
  chip.className = 'folder-chip';
  chip.dataset.folderId = folder.id;
  chip.dataset.tauriDragRegion = 'false';
  chip.innerHTML = `<span class="f-toggle">${expanded ? '▾' : '▸'}</span><span class="f-name"></span><span class="f-count">${count}</span>`;
  chip.querySelector('.f-name').textContent = folder.name;
  chip.title = `${folder.name}（${count} 个标签，点击展开/收起，拖标签进来收纳）`;
  chip.onclick = () => {
    expanded ? expandedFolders.delete(folder.id) : expandedFolders.add(folder.id);
    render();
  };
  chip.addEventListener('dragover', (e) => { e.preventDefault(); chip.classList.add('drop-over'); });
  chip.addEventListener('dragleave', () => chip.classList.remove('drop-over'));
  chip.addEventListener('drop', (e) => {
    e.preventDefault();
    chip.classList.remove('drop-over');
    const dragId = e.dataTransfer.getData('text/trawl-tab');
    if (dragId) invoke('move_tab_to_folder', { id: dragId, folderId: folder.id });
  });
  // 文件夹右键：重命名（后端 rename_folder 命令已登记 ACL）
  chip.addEventListener('contextmenu', (e) => {
    e.preventDefault();
    e.stopPropagation();
    openCtxMenu(e, [
      { label: '重命名文件夹', run: async () => {
        const name = await promptBox('重命名文件夹', folder.name);
        if (name && name.trim()) invoke('rename_folder', { id: folder.id, name: name.trim() });
      } },
    ], 180);
  });
  return chip;
}

/* ==================== 书签栏渲染 ==================== */

// 构造单个书签项（带 favicon）
function buildBmItem(b) {
  const el = document.createElement('div');
  el.className = 'bm-item';
  el.dataset.bmUrl = b.url;  // 右键菜单要按真实 url 操作，不能用 title（M8 修复）
  el.title = b.folder ? `${b.folder} / ${b.title}\n${b.url}` : `${b.title}\n${b.url}`;
  // P1-6 无障碍：书签栏项可键盘聚焦激活
  el.setAttribute('role', 'link');
  el.tabIndex = 0;
  el.setAttribute('aria-label', `打开书签：${b.title || hostOf(b.url)}`);
  const fav = document.createElement('span');
  fav.className = 'bm-fav';
  const host = hostOf(b.url);
  const letter = document.createElement('span');
  letter.className = 'tab-letter';
  letter.textContent = host ? host.charAt(0).toUpperCase() : '★';
  fav.appendChild(letter);
  el.appendChild(fav);
  const name = document.createElement('span');
  name.className = 'b-name';
  name.textContent = b.title || host;
  el.appendChild(name);
  if (host) fillFavicon(fav, host, letter, '#24292f');
  el.onclick = () => invoke('navigate', { input: b.url })
    .then(() => navPush(state.activeTab, b.url))
    .catch((err) => {
      reportError(`navigate FAILED: ${String(err)}`);
      chromeToast('打开书签失败，请检查网址');
    });
  el.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); el.click(); }
  });
  return el;
}

// 自动归类：把无 folder 的根书签按域名归到可读目录（Edge 式：栏上只放目录）
const BM_AUTO_RULES = [
  { name: '百度', test: h => /(^|\.)baidu\.(com|cn)$/.test(h) },
  { name: '阿里/购物', test: h => /(^|\.)(taobao|tmall|alibaba|jd|pinduoduo|douyin)\.(com|cn)$/.test(h) },
  { name: '腾讯', test: h => /(^|\.)(qq|tencent|weixin|wechat)\.(com|cn)$/.test(h) },
  { name: 'AI 工具', test: h => /(^|\.)(doubao|chatgpt|openai|claude|kimi|moonshot|zhipu|bigmodel|yuanbao|gemini|deepseek)\.(com|cn|ai)$/.test(h) },
  { name: '开发者', test: h => /(^|\.)(csdn|juejin|github|gitee|stackoverflow|zhihu|segmentfault|oschina|cnblogs)\.(com|cn|net|org)$/.test(h) },
  { name: '本地服务', test: h => /^(\d{1,3}\.){3}\d{1,3}$/.test(h) || h === 'localhost' },
];
function autoCategoryOf(b) {
  const h = hostOf(b.url);
  for (const r of BM_AUTO_RULES) if (r.test(h)) return r.name;
  return '其他';
}

/* ---- 书签目录树（Edge 收藏夹栏：folder 按 / 分层，支持 chevron 逐级展开） ----
 * 节点：{ name, path, children: Map<name,node>, bookmarks: [] }
 * 无 folder 的书签按域名自动归类进第一层目录；有 folder 的按 / 拆成多级。 */
function buildBookmarkTree() {
  const root = { name: '', path: '', children: new Map(), bookmarks: [], count: 0 };
  const ensure = (segments) => {
    let node = root, path = '';
    for (const seg of segments) {
      path = path ? `${path}/${seg}` : seg;
      if (!node.children.has(seg)) {
        node.children.set(seg, { name: seg, path, children: new Map(), bookmarks: [], count: 0 });
      }
      node = node.children.get(seg);
    }
    return node;
  };
  for (const b of state.bookmarks) {
    const segs = b.folder
      ? b.folder.split('/').map(s => s.trim()).filter(Boolean)
      : [autoCategoryOf(b)];
    ensure(segs).bookmarks.push(b);
  }
  const recount = (node) => {
    node.count = node.bookmarks.length;
    for (const c of node.children.values()) node.count += recount(c);
    return node.count;
  };
  recount(root);
  return root;
}
// 顶层目录，按数量降序
function topFolders(tree) {
  return [...tree.children.values()].sort((a, b) => b.count - a.count || a.name.localeCompare(b.name));
}

// 构造顶层目录 chip：点击就地弹出下拉（Edge 风格）
function buildBmFolderChip(node) {
  const el = document.createElement('div');
  el.className = 'bm-folder';
  el.dataset.folder = node.path;
  el.innerHTML = `<span class="bf-ico">📁</span><span class="bf-name"></span><span class="bf-count">${node.count}</span>`;
  el.querySelector('.bf-name').textContent = node.name;
  el.title = `${node.name}（${node.count} 个书签，点击展开）`;
  el.setAttribute('role', 'button');
  el.tabIndex = 0;
  el.setAttribute('aria-label', `${node.name} 文件夹，${node.count} 个书签，点击展开`);
  const openMenu = () => showFolderMenu(el, node);
  el.onclick = (e) => { e.stopPropagation(); openMenu(); };
  el.addEventListener('keydown', (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); openMenu(); } });
  return el;
}

// ---- 就地弹出的目录下拉菜单（fixed 定位；子目录带 chevron 可展开）----
let bmFolderMenu = null;
function closeFolderMenu() {
  if (bmFolderMenu) { bmFolderMenu.remove(); bmFolderMenu = null; }
}
// 打开一个书签（当前标签）
function openBmHere(b) {
  invoke('navigate', { input: b.url })
    .then(() => navPush(state.activeTab, b.url))
    .catch(() => chromeToast('打开书签失败，请检查网址'));
  closeFolderMenu();
}
// 渲染一个目录节点的内容（书签 + 子目录）到容器 panel
function renderBmNode(node, panel) {
  // 子目录在前（Edge 风格：文件夹排在链接上方）
  const subs = [...node.children.values()].sort((a, b) => a.name.localeCompare(b.name));
  for (const sub of subs) {
    const row = document.createElement('div');
    row.className = 'bfm-item bfm-sub';
    row.setAttribute('role', 'menuitem');
    row.tabIndex = 0;
    row.innerHTML = `<span class="bfm-ico">📁</span><span class="bfm-label"></span><span class="bfm-chevron">›</span><span class="bfm-count">${sub.count}</span>`;
    row.querySelector('.bfm-label').textContent = sub.name;
    // 悬停/点击展开子目录（右侧滑出子面板，Edge 交互）
    let childPanel = null;
    const expand = () => {
      if (childPanel) return;
      childPanel = document.createElement('div');
      childPanel.className = 'bm-folder-menu bfm-child';
      renderBmNode(sub, childPanel);
      document.body.appendChild(childPanel);
      const pr = panel.getBoundingClientRect();
      const rr = row.getBoundingClientRect();
      const cw = childPanel.offsetWidth, ch = childPanel.offsetHeight;
      childPanel.style.left = Math.min(pr.right + 2, window.innerWidth - cw - 6) + 'px';
      childPanel.style.top = Math.min(rr.top - 4, window.innerHeight - ch - 6) + 'px';
      childPanel._parent = panel;
    };
    const collapse = () => {
      // 仅关闭自己及更深层子面板
      let p = childPanel;
      while (p) { const next = p._childPanel; p.remove(); p = next; }
      if (childPanel) childPanel = null;
    };
    row.addEventListener('mouseenter', expand);
    row.addEventListener('mouseleave', () => {
      setTimeout(() => { if (childPanel && !childPanel.matches(':hover')) collapse(); }, 180);
    });
    row.onclick = (e) => { e.stopPropagation(); expand(); };
    row.addEventListener('keydown', (e) => { if (e.key === 'ArrowRight' || e.key === 'Enter') { e.preventDefault(); expand(); } });
    panel.appendChild(row);
  }
  // 本目录的书签
  for (const b of node.bookmarks) {
    const row = document.createElement('div');
    row.className = 'bfm-item';
    row.setAttribute('role', 'menuitem');
    row.tabIndex = 0;
    const fav = document.createElement('span');
    fav.className = 'bm-fav';
    const letter = document.createElement('span');
    letter.className = 'tab-letter';
    letter.textContent = (hostOf(b.url).charAt(0) || '★').toUpperCase();
    fav.appendChild(letter);
    const label = document.createElement('span');
    label.className = 'bfm-label';
    label.textContent = b.title || hostOf(b.url);
    row.append(fav, label);
    row.onclick = (e) => { e.stopPropagation(); openBmHere(b); };
    row.addEventListener('keydown', (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); openBmHere(b); } });
    // Edge 风格右键菜单
    row.addEventListener('contextmenu', (e) => {
      e.preventDefault(); e.stopPropagation();
      closeFolderMenu();
      openCtxMenu(e, [
        { label: '打开', run: () => invoke('navigate', { input: b.url }) },
        { label: '在新标签页中打开', run: () => invoke('navigate_new_tab', { input: b.url }) },
        { label: '复制链接', run: () => navigator.clipboard.writeText(b.url).then(() => chromeToast('已复制链接')).catch(() => chromeToast('复制失败')) },
        { sep: true },
        { label: '删除书签', run: async () => {
          if (!(await confirmBox('确定删除这个书签？', '删除书签', '删除'))) return;
          invoke('remove_bookmark', { url: b.url }).catch(() => chromeToast('删除失败'));
        } },
      ], 180);
    });
    panel.appendChild(row);
  }
}
function showFolderMenu(anchor, node) {
  closeFolderMenu();
  const menu = document.createElement('div');
  menu.className = 'bm-folder-menu';
  menu.setAttribute('role', 'menu');
  const head = document.createElement('div');
  head.className = 'bfm-head';
  head.textContent = `${node.name}（${node.count}）`;
  menu.appendChild(head);
  renderBmNode(node, menu);
  // 底部：在新标签页全部打开（递归收集本目录所有书签）
  const collect = (n, acc) => { acc.push(...n.bookmarks); for (const c of n.children.values()) collect(c, acc); return acc; };
  const allBms = collect(node, []);
  if (allBms.length > 1) {
    const all = document.createElement('div');
    all.className = 'bfm-all';
    all.textContent = '在新标签页中全部打开';
    all.onclick = (e) => {
      e.stopPropagation();
      for (const b of allBms) invoke('navigate_new_tab', { input: b.url }).catch(() => {});
      closeFolderMenu();
    };
    menu.appendChild(all);
  }
  document.body.appendChild(menu);
  bmFolderMenu = menu;
  const r = anchor.getBoundingClientRect();
  const mw = menu.offsetWidth, mh = menu.offsetHeight;
  menu.style.left = Math.max(6, Math.min(r.left, window.innerWidth - mw - 6)) + 'px';
  menu.style.top = (r.bottom + 4 + mh > window.innerHeight ? Math.max(6, r.top - mh - 4) : r.bottom + 4) + 'px';
}
document.addEventListener('click', (e) => {
  if (bmFolderMenu && !bmFolderMenu.contains(e.target)) closeFolderMenu();
});
document.addEventListener('keydown', (e) => { if (e.key === 'Escape') closeFolderMenu(); });
window.addEventListener('resize', closeFolderMenu);

// 渲染书签栏：目录 chip + 溢出折叠测量
function renderBookmarkBar() {
  const bmSig = state.bookmarks.map(b => `${b.url}|${b.title}|${b.folder || ''}`).join('|')
    + '@' + bookmarkbarEl.clientWidth;
  if (bookmarkbarEl.dataset.sig === bmSig) return;
  bookmarkbarEl.dataset.sig = bmSig;
  bookmarkbarEl.innerHTML = '';

  const tree = buildBookmarkTree();
  const folders = topFolders(tree);
  const items = folders.map(buildBmFolderChip);
  const more = document.createElement('button');
  more.className = 'bm-more';
  more.id = 'bm-more-btn';

  if (!items.length) {
    more.textContent = '添加书签';
  }
  for (const el of items) bookmarkbarEl.appendChild(el);
  bookmarkbarEl.appendChild(more);

  // 溢出折叠：测量每项宽度，放不下的隐藏，更多按钮显示隐藏数量
  // clientWidth=0 表示尚未布局（首帧），此时不折叠，等下一次 render 重算
  if (items.length && bookmarkbarEl.clientWidth > 0) {
    const avail = bookmarkbarEl.clientWidth - 16; // 减去左右 padding
    let used = more.offsetWidth + 8;
    let hidden = 0;
    for (const el of items) {
      used += el.offsetWidth + 1;
      if (used > avail) { el.style.display = 'none'; hidden++; }
    }
    more.textContent = hidden ? `更多 ${hidden}` : '全部书签';
    more.title = hidden ? `${hidden} 个书签已折叠，点击在书签面板查看` : '书签面板 (Ctrl+Shift+B)';
  }
  more.onclick = () => invoke('ui_action', { what: 'bookmarks' });
  // 首帧若尚未布局（clientWidth=0），下一帧补一次折叠计算
  if (items.length && !bookmarkbarEl.clientWidth) {
    requestAnimationFrame(() => { bookmarkbarEl.dataset.sig = ''; renderBookmarkBar(); });
  }
}

/* ==================== 渲染 ==================== */

function tabsSignature() {
  // 签名不含 title：标题变化很频繁（每页导航都变），放进签名会导致整条标签栏全量重建，
  // 悬停/拖拽态全丢。标题改动由 render() 里的轻量同步单独更新文本。
  return wsTabs().map(t => `${t.id}:${t.pinned}:${t.folderId}:${t.loading}:${t.id === state.activeTab}:${selectedTabs.has(t.id)}`)
    .concat(state.folders.map(f => `f${f.id}:${f.name}:${expandedFolders.has(f.id)}`))
    .join('|');
}

/** 签名不变时的标题轻量同步：只改文本节点，不重建 DOM（避免丢交互态） */
function syncTabTitles() {
  for (const el of tabsEl.children) {
    const id = el.dataset.id;
    if (!id) continue;
    const tab = state.tabs.find(t => t.id === id);
    if (!tab) continue;
    const text = tab.title || '新标签页';
    if (el.title !== text) {
      el.title = text;
      el.setAttribute('aria-label', `标签页：${text}`);
      const t = el.querySelector('.tab-title');
      if (t && t.textContent !== text) t.textContent = text;
    }
  }
}

function render() {
  const theme = state.settings?.theme || 'light';
  const accent = state.settings?.accent || 'green';
  // 「跟随系统」在 chrome 侧解析成具体主题，再写回 localStorage 供新标签页等内页复用
  const resolved = theme === 'auto'
    ? (window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light')
    : theme;
  document.documentElement.dataset.theme = resolved;
  document.documentElement.dataset.accent = accent;
  document.documentElement.dataset.vertical = state.settings?.verticalTabs ? '1' : '0';
  // 界面密度：紧凑模式下标签条/工具栏变矮，一屏能多看一行内容
  document.documentElement.dataset.density = state.settings?.density === 'compact' ? 'compact' : 'standard';
  // 界面字号 / 无障碍（减少动效、高对比度）
  document.documentElement.dataset.font = String(state.settings?.fontSize || 100);
  document.documentElement.dataset.motion = state.settings?.reduceMotion ? 'off' : 'on';
  document.documentElement.dataset.contrast = state.settings?.highContrast ? 'high' : 'normal';
  // 标签列表按钮可以在设置里关掉（有人只用 Ctrl+Shift+T 打开）
  $('#btn-tablist').classList.toggle('hidden', state.settings?.showTablistBtn === false);
  // 无痕窗口全局标识：URL 带 ?incognito=1 或窗口标题含「无痕」时显示（地址栏左侧）
  try {
    const incognito = new URLSearchParams(location.search).has('incognito')
      || (document.title || '').includes('无痕');
    $('#incognito-badge')?.classList.toggle('on', incognito);
  } catch (_) {}
  // 工具栏按钮显隐：可在「设置-外观」开关，也可以右键按钮直接隐藏
  $('#btn-extensions').classList.toggle('hidden', state.settings?.showExtBtn === false);
  $('#btn-history').classList.toggle('hidden', state.settings?.showHistoryBtn === false);
  try {
    localStorage.setItem('trawl-settings', JSON.stringify(state.settings));
    localStorage.setItem('trawl-theme', resolved);
    localStorage.setItem('trawl-accent', accent);
  } catch {}

  const activeWs = state.workspaces.find(w => w.id === state.activeWorkspace);
  wsPill.title = `工作区：${activeWs ? activeWs.name : ''}（点击打开标签分组）`;
  // 分组面板里选的配色：在 Logo 角落显示一个小圆点，让配色真实可见
  let wsColor = '';
  try { wsColor = localStorage.getItem('trawl-ws-color-' + state.activeWorkspace) || ''; } catch {}
  wsPill.style.setProperty('--ws-color', wsColor);

  // 标签条：签名没变就不重建，避免每次导航都闪一下、丢掉悬停与拖拽态
  const sig = tabsSignature();
  if (sig !== lastTabsSignature) {
    lastTabsSignature = sig;
    tabsEl.innerHTML = '';
    const list = wsTabs();
    for (const tab of list.filter(t => t.pinned)) tabsEl.appendChild(buildTabEl(tab, true));
    for (const folder of state.folders.filter(f => f.workspaceId === state.activeWorkspace)) {
      tabsEl.appendChild(buildFolderChip(folder));
      if (expandedFolders.has(folder.id)) {
        for (const tab of list.filter(t => t.folderId === folder.id)) tabsEl.appendChild(buildTabEl(tab, false));
      }
    }
    for (const tab of list.filter(t => !t.pinned && !t.folderId)) tabsEl.appendChild(buildTabEl(tab, false));
  } else {
    // 签名不含 title，标题变化走轻量文本同步，不重建 DOM
    syncTabTitles();
  }

  // 书签栏：目录 chip + 溢出折叠；显示策略三档（始终/仅新标签页/从不，Edge 风格）
  const bmMode = bookmarkBarMode();
  const showBm = bmMode === 'always' || (bmMode === 'newtab' && isNewTabPage(activeTab()?.url));
  bookmarkbarEl.classList.toggle('hidden', !showBm);
  if (showBm) renderBookmarkBar();

  // 地址栏
  const active = activeTab();
  if (editing && state.activeTab !== editTabId) endEdit();
  if (!editing) {
    address.value = displayUrl(active);
    address.placeholder = '搜索，或输入网址';
  }
  const url = active ? active.url : '';
  const https = url.startsWith('https://');
  addrScheme.classList.toggle('secure', https);
  addrScheme.classList.toggle('insecure', url.startsWith('http://'));
  // HTTPS 显示锁，HTTP / 内部页显示不带锁的地球图标；形状差异让色觉障碍也能分辨
  const schemeUse = addrScheme.querySelector('use');
  if (schemeUse) schemeUse.setAttribute('href', https ? '#i-lock' : '#i-globe');

  // 收藏星标
  $('#btn-star').classList.toggle('active', active ? isBookmarked(active.url) : false);

  // 下载按钮：设置里常驻显示；没开启时只在有下载任务时出现
  const n = state.downloading || 0;
  const dlAlways = state.settings?.showDlBtn !== false;
  $('#btn-dl').classList.toggle('hidden', !dlAlways && n === 0);
  $('#dl-count').textContent = n > 0 ? String(n) : '';

  // 加载进度。
  // WebView2 的 NavigationCompleted 有时姗姗来迟（内页尤其明显），
  // 只靠 loading 标志会让进度条长期挂着，所以给一个上限兜底。
  const loading = !!active?.loading;
  if (loading) {
    progressEl.classList.add('on');
    progressEl.classList.remove('done');
    clearTimeout(progressTimer);
    progressTimer = setTimeout(() => progressEl.classList.remove('on'), 1600);
  } else if (lastLoading) {
    clearTimeout(progressTimer);
    progressEl.classList.add('done');
    setTimeout(() => progressEl.classList.remove('on', 'done'), 320);
  }
  lastLoading = loading;

  // 刷新按钮：加载中显示停止图标
  const reload = $('#btn-reload');
  reload.innerHTML = loading
    ? '<svg class="ic"><use href="#i-stop"/></svg>'
    : '<svg class="ic"><use href="#i-reload"/></svg>';
  reload.title = loading ? '停止加载 (Esc)' : '刷新 (Ctrl+R)';
  updateNavButtons();
}

/* ==================== 通用浮层右键菜单 ==================== */
//
// 菜单溢出到内容区时会被页面 webview 盖住（原生图层），所以所有菜单都走
// ctxmenu.html 浮层：这里只生成「动作标识」发给 Rust，点击后再 emit 回来分发。

function hideContextMenu() {
  ctxActions.clear();   // 菜单关闭后本轮动作闭包不再需要，及时释放
  invoke('close_overlay').catch(() => {});
}

/**
 * 打开浮层菜单。
 * items: [{ label, key?, sep?, disabled?, run: Function }]
 * 动作存在本地 Map，Rust 只负责显示/回传一个自增 id。
 */
let ctxSeq = 0;
const ctxActions = new Map();
function openCtxMenuAt(x, y, items, width = 196) {
  // 同时只有一个菜单：打开新的前清掉上一轮未消费的动作闭包，避免 Map 无限增长
  ctxActions.clear();
  const payload = [];
  for (const it of items) {
    const key = `a${++ctxSeq}`;
    if (it.run) ctxActions.set(key, it.run);
    payload.push({
      label: it.label || '',
      key: it.key || '',
      sep: !!it.sep,
      disabled: !!it.disabled,
      action: key,
    });
  }
  let px = Math.min(x, window.innerWidth - width - 10);
  let py = y;
  const approx = items.filter(i => !i.sep).length * 36 + items.filter(i => i.sep).length * 9 + 20;
  if (py + approx > window.innerHeight - 8) py = Math.max(4, py - approx);
  invoke('open_ctxmenu', { x: Math.max(0, Math.round(px)), y: Math.round(py), width, items: payload })
    .catch((err) => reportError(`open_ctxmenu FAILED: ${String(err)}`));
}
function openCtxMenu(e, items, width = 196) {
  openCtxMenuAt(e.clientX, e.clientY, items, width);
}

// 浮层点击后回传动作标识
listen('ctx-action', (ev) => {
  const key = typeof ev.payload === 'string' ? ev.payload : (ev.payload && ev.payload.action);
  const fn = ctxActions.get(key);
  if (fn) {
    ctxActions.delete(key);
    try { fn(); } catch (err) { reportError(`ctx-action FAILED: ${String(err)}`); }
  }
});

/** 左上角 Brand：打开 / 收起标签分组面板（独立浮层，Rust 侧做 toggle） */
function showBrandMenu() {
  const r = wsPill.getBoundingClientRect();
  invoke('open_brandpanel', {
    x: Math.round(r.left),
    y: Math.round(r.bottom + 6),
    width: 300,
    height: 560,
  }).catch((err) => reportError(`open_brandpanel FAILED: ${String(err)}`));
}
wsPill.dataset.tauriDragRegion = 'false';
wsPill.addEventListener('click', (e) => {
  e.stopPropagation();
  showBrandMenu();
});

// 标签栏/工具栏的所有按钮都要禁用窗口拖拽，否则点它们会触发 start_dragging
// （data-tauri-drag-region="deep" 会让子元素空白处也参与拖拽）而吞掉 click。
document.querySelectorAll('button, #btn-newtab, #btn-tablist, .win-controls button, #btn-star, #btn-downloads, #btn-more, .nav-group button, .tool-group button').forEach((b) => {
  b.dataset.tauriDragRegion = 'false';
});

// 窗口拖拽走 Tauri 原生机制：#tabstrip 带 data-tauri-drag-region="deep"，
// 标签栏任意空白处（含标签间空隙）按下即可拖动；按钮已通过上面的 ="false" 排除。
// 此前「拖不动」的根因是 ACL 缺 core:window:allow-start-dragging 权限，已在 capabilities 里补上。

/* ==================== 右键菜单 ==================== */

function showTabContextMenu(e, tab) {
  if (selectedTabs.size > 1) {
    const ids = [...selectedTabs];
    openCtxMenu(e, [
      { label: `关闭所选（${ids.length} 个）`, run: () => invoke('close_tabs', { ids }) },
      { label: '固定所选', run: () => invoke('set_pins', { ids, pinned: true }) },
      { label: '取消固定所选', run: () => invoke('set_pins', { ids, pinned: false }) },
      { sep: true },
      { label: '取消多选', run: () => { selectedTabs.clear(); render(); } },
    ], 190);
    return;
  }
  const items = [
    { label: tab.pinned ? '取消固定' : '固定标签', run: () => invoke('toggle_pin', { id: tab.id }) },
    { label: 'Glance 预览', key: 'Alt+点击', run: () => invoke('open_glance', { id: tab.id }) },
    { label: tab.folderId ? '移出文件夹' : '收入新文件夹', run: async () => {
      if (tab.folderId) {
        invoke('move_tab_to_folder', { id: tab.id, folderId: null });
      } else {
        // 新建时让用户当场命名，避免总是一堆同名的「新文件夹」
        const name = await promptBox('新文件夹名称', '新文件夹');
        if (!name || !name.trim()) return;
        invoke('create_folder', { name: name.trim() }).then((fid) => {
          if (fid) invoke('move_tab_to_folder', { id: tab.id, folderId: fid });
        });
      }
    } },
  ];
  const active = activeTab();
  if (active && active.id !== tab.id) {
    const inSplit = state.split && state.split.includes(active.id) && state.split.includes(tab.id);
    items.push({ label: inSplit ? '退出分屏' : '与当前标签分屏', run: () => invoke('toggle_split', { id: tab.id }) });
  }
  if (state.split) {
    items.push({ label: '切换分屏方向', run: () => invoke('toggle_split_orientation') });
    items.push({ label: '退出分屏', key: 'Esc', run: () => invoke('exit_split') });
  }
  items.push({ sep: true });
  items.push({ label: '关闭标签页', key: 'Ctrl+W', run: () => invoke('close_tab_cmd', { id: tab.id }) });
  openCtxMenu(e, items, 200);
}

/** 书签栏右键：编辑书签栏（Edge 风格显示策略） */
function showBookmarkBarMenu(e) {
  const mode = bookmarkBarMode();
  const mark = (m) => (mode === m ? '✓ ' : '    ');
  openCtxMenu(e, [
    { label: `${mark('always')}始终显示`, run: () => { localStorage.setItem('bmBarMode', 'always'); applyBookmarkBar(); } },
    { label: `${mark('newtab')}仅在新标签页显示`, run: () => { localStorage.setItem('bmBarMode', 'newtab'); applyBookmarkBar(); } },
    { label: `${mark('never')}从不显示`, run: () => { localStorage.setItem('bmBarMode', 'never'); applyBookmarkBar(); } },
    { sep: true },
    { label: '收藏当前页', key: 'Ctrl+D', run: () => toggleStar() },
    { label: '在新标签页中全部打开', run: () => {
      const collect = (n, acc) => { acc.push(...n.bookmarks); for (const c of n.children.values()) collect(c, acc); return acc; };
      for (const b of collect(buildBookmarkTree(), [])) invoke('navigate_new_tab', { input: b.url }).catch(() => {});
    } },
    { sep: true },
    { label: '管理全部书签', run: () => openBookmarks() },
  ], 210);
}
bookmarkbarEl.addEventListener('contextmenu', (e) => {
  e.preventDefault();
  showBookmarkBarMenu(e);
});
// 单个书签：额外提供删除
document.addEventListener('contextmenu', (e) => {
  if (e.target.closest('.bm-item')) {
    e.preventDefault();
    const el = e.target.closest('.bm-item');
    const url = el.dataset.bmUrl;  // 必须用真实 url，title 是「文件夹/标题\nurl」拼接串（M8）
    if (!url) return;
    openCtxMenu(e, [
      { label: '打开书签', run: () => invoke('navigate', { input: url }) },
      { sep: true },
      { label: '删除书签', run: async () => {
        if (!(await confirmBox('确定删除这个书签？此操作不可撤销。', '删除书签', '删除'))) return;
        invoke('remove_bookmark', { url }).catch(() => chromeToast('删除书签失败，请重试'));
      } },
    ], 172);
  } else if (!e.target.closest('.tab') && !e.target.closest('.folder-chip')) {
    hideContextMenu();
  }
});

/* ==================== 浮层入口 ==================== */

const openFindbar = () => invoke('ui_action', { what: 'find' });
const openHistory = () => invoke('ui_action', { what: 'history' });
const openBookmarks = () => invoke('ui_action', { what: 'bookmarks' });
const openDownloads = () => invoke('ui_action', { what: 'downloads' });
const openPalette = () => invoke('ui_action', { what: 'palette' });
const openExtensions = () => invoke('open_internal', { page: 'extensions' });

/* ==================== 网页翻译 ====================
 * 页内沉浸式翻译：注入 IMMERSIVE_JS 到当前页面，双语对照展示，
 * 再次点击还原。不再跳转百度翻译页面。
 */
const TRANSLATE_TARGETS = [
  { label: '翻译成中文', to: 'zh' },
  { label: '翻译成英文', to: 'en' },
  { label: '翻译成日文', to: 'ja' },
  { label: '翻译成韩文', to: 'ko' },
];

async function translateCurrent(to) {
  const a = activeTab();
  if (!a || isInternal(a)) {
    chromeToast('内置页面无需翻译');
    return;
  }
  const ext = await extState('trawl-translate');
  if (!ext.enabled) {
    chromeToast('网页翻译插件未开启，请到扩展程序页打开');
    return;
  }
  let target = to;
  if (!target && ext.cfg.target) target = String(ext.cfg.target);
  try { target = target || localStorage.getItem('trawl-translate-to') || 'zh'; } catch { target = target || 'zh'; }
  invoke('toggle_translate', { id: a.id, to: target })
    .catch((err) => {
      reportError(`toggle_translate FAILED: ${String(err)}`);
      chromeToast('翻译失败，请重试');
    });
}

async function openTranslateMenu() {
  const a = activeTab();
  if (!a || isInternal(a)) {
    openCtxMenuAt(address.getBoundingClientRect().right - 180, 96,
      [{ label: '内置页面无需翻译', disabled: true }], 180);
    return;
  }
  const r = $('#btn-translate').getBoundingClientRect();
  // 默认目标语言优先取插件配置（扩展页「配置」面板），没有再回退 localStorage
  let def = 'zh';
  const ext = await extState('trawl-translate');
  if (!ext.enabled) {
    openCtxMenuAt(Math.round(r.right - 190), Math.round(r.bottom + 8),
      [{ label: '网页翻译插件未开启', disabled: true },
       { label: '前往扩展程序页开启', run: openExtensions }], 190);
    return;
  }
  if (ext.cfg.target) def = String(ext.cfg.target);
  try { def = localStorage.getItem('trawl-translate-to') || def; } catch {}
  const order = TRANSLATE_TARGETS.slice().sort((x, y) => (y.to === def) - (x.to === def));
  const items = order.map(t => ({
    label: t.label + (t.to === def ? '（默认）' : ''),
    run: () => translateCurrent(t.to),
  }));
  items.push({ sep: true });
  items.push({ label: '页内双语翻译 · 再点还原', disabled: true });
  openCtxMenuAt(Math.round(r.right - 190), Math.round(r.bottom + 8), items, 190);
}

/** 地址栏建议面板：位置严格贴住地址栏，宽度与地址栏一致 */
let qbShown = false;
let quickbarSel = -1;   // quickbar 键盘选中项（quickbar-sel 事件同步）
listen('quickbar-sel', (ev) => {
  quickbarSel = typeof ev.payload === 'number' ? ev.payload : -1;
});
function openQuickbar() {
  const r = $('#address-wrap').getBoundingClientRect();
  qbShown = true;
  quickbarSel = -1;
  invoke('open_quickbar', {
    x: Math.round(r.left),
    y: Math.round(r.bottom + 6),
    width: Math.round(r.width),
    height: 300,
  }).catch((err) => reportError(`open_quickbar FAILED: ${String(err)}`));
}

/** 「更多」菜单：右对齐触发按钮，向下展开 */
function openMoreMenu() {
  const r = $('#btn-more').getBoundingClientRect();
  const width = 252;
  invoke('open_menu', {
    x: Math.round(r.right - width),
    y: Math.round(r.bottom + 6),
    width,
    height: 640,
  }).catch((err) => reportError(`open_menu FAILED: ${String(err)}`));
}

/* ==================== 地址栏 ==================== */

function beginEdit(selectAll) {
  if (editing) return;
  editing = true;
  editTabId = state.activeTab;
  addrHint.classList.remove('hidden');
  // 进入编辑态时换成完整地址（内部页显示 trawl://xxx），否则编辑「新标签页」这种标题没意义
  const a = activeTab();
  address.value = a ? a.url : '';
  // 关掉「地址栏自动补全」后不主动弹建议，等真正开始输入再弹
  if (state.settings?.autocomplete !== false) openQuickbar();
  invoke('push_suggest', { q: selectAll ? '' : address.value }).catch(() => {});
  if (selectAll) address.select();
  // 兜底：点页面时 chrome 可能收不到 blur 事件，轮询一次焦点状态
  clearInterval(editPoll);
  editPoll = setInterval(() => {
    if (!editing) { clearInterval(editPoll); return; }
    if (!document.hasFocus()) endEdit();
  }, 350);
}

function endEdit() {
  editing = false;
  editTabId = null;
  qbShown = false;
  clearInterval(editPoll);
  addrHint.classList.add('hidden');
  // 退出编辑态要彻底回到「隐藏时」的样子：恢复精简地址、清掉选区、放开焦点
  address.value = displayUrl(activeTab());
  try { address.setSelectionRange(0, 0); } catch (_) {}
  if (document.activeElement === address) address.blur();
}

address.addEventListener('focus', () => beginEdit(true));
address.addEventListener('click', () => beginEdit(true));

address.addEventListener('input', (e) => {
  // 自动补全关闭时，第一下输入才把建议面板叫出来
  if (!qbShown) openQuickbar();
  invoke('push_suggest', { q: address.value }).catch(() => {});
});

/** 回车提交：地址栏当前内容 → 导航 */
function commitAddress() {
  const v = address.value.trim();
  endEdit();
  address.blur();
  invoke('close_overlay').catch(() => {});
  if (v) invoke('navigate', { input: v })
    .then(() => navPush(state.activeTab, v))
    .catch((err) => {
      reportError(`navigate FAILED: ${String(err)}`);
      chromeToast('打开失败，请检查网址或网络连接');
    });
}

/** 放弃编辑，恢复显示当前页地址 */
function cancelEdit() {
  endEdit();
  address.blur();
  invoke('close_overlay').catch(() => {});
}

// 回车 / Esc 统一在 document 层处理（见下方 keydown），
// 这样即使焦点被浮层 webview 打散到 body 上也能生效，此处不再重复绑定。

/* ==================== 收藏 ==================== */

function toggleStar() {
  const a = activeTab();
  if (!a || isInternal(a)) return;
  invoke('toggle_bookmark', { url: a.url, title: a.title });
}

/* ==================== 布局上报 ==================== */

function reportLayout() {
  const vertical = document.documentElement.dataset.vertical === '1';
  // 竖直标签栏时，tabstrip 变成左侧 188px 一列；工具栏/书签栏在其右侧。
  // 页面 webview 的内容区要向右让出这 188px，高度仍是 chrome 除标签栏以外的部分。
  const x = vertical ? 188 : 0;
  const chromeH = vertical
    ? Math.round($('#toolbar').offsetHeight + (bookmarkbarEl.classList.contains('hidden') ? 0 : bookmarkbarEl.offsetHeight) + 2)
    : chromeEl.offsetHeight;
  invoke('layout', {
    x,
    y: chromeH,
    width: Math.round(window.innerWidth - x),
    height: Math.round(window.innerHeight - chromeH),
  }).catch((err) => reportError(`layout FAILED: ${String(err)}`));
}

/* ==================== 事件绑定 ==================== */

$('#btn-newtab').onclick = () => invoke('new_tab');
$('#btn-tablist').onclick = () => invoke('open_tablist');
$('#btn-back').onclick = goBack;
$('#btn-fwd').onclick = goForward;
$('#btn-reload').onclick = () => {
  if (activeTab()?.loading) invoke('stop_loading');
  else invoke('nav_reload');
};
$('#btn-home').onclick = () => {
  const hp = state.settings?.homepage || 'trawl://newtab';
  if (hp.startsWith('trawl://')) invoke('open_internal', { page: hp.slice('trawl://'.length) });
  else invoke('navigate', { input: hp });
};
$('#btn-star').onclick = toggleStar;
$('#btn-translate').onclick = openTranslateMenu;
$('#btn-extensions').onclick = openExtensions;
$('#btn-history').onclick = openHistory;
$('#btn-dl').onclick = openDownloads;
$('#btn-more').onclick = openMoreMenu;
$('#win-min').onclick = () => invoke('win_minimize');
$('#win-max').onclick = () => invoke('win_toggle_maximize');
$('#win-close').onclick = () => invoke('win_close');

/* ---- 工具栏按钮右键：隐藏该按钮 / 打开外观设置 ----
 * 支持「增删」：下载、扩展、历史按钮都可以一键从工具栏移除，设置里再找回 */
function toolbarBtnMenu(e, key, name) {
  e.preventDefault();
  openCtxMenu(e, [
    {
      label: '隐藏「' + name + '」按钮',
      run: () => {
        const s = Object.assign({}, state.settings);
        s[key] = false;
        invoke('save_settings', { settings: s }).catch(() => {});
        chromeToast('已隐藏「' + name + '」按钮，可在 设置-外观 里找回');
      },
    },
    { label: '工具栏设置…', run: () => invoke('open_internal', { page: 'settings' }) },
  ], 186);
}
$('#btn-extensions').addEventListener('contextmenu', (e) => toolbarBtnMenu(e, 'showExtBtn', '扩展程序'));
$('#btn-history').addEventListener('contextmenu', (e) => toolbarBtnMenu(e, 'showHistoryBtn', '历史记录'));
$('#btn-dl').addEventListener('contextmenu', (e) => toolbarBtnMenu(e, 'showDlBtn', '下载'));

/* ---- 多标签切换：Ctrl+Tab / Ctrl+Shift+Tab（全局热键已注册）----
 * 也响应标签栏上的滚轮：标签放不下时滚动列表，放得下时直接切换标签 */
function cycleTab(d) {
  const tabs = wsTabs();
  if (tabs.length < 2) return;
  const idx = tabs.findIndex(t => t.id === state.activeTab);
  const base = idx >= 0 ? idx : 0;
  const next = tabs[(base + d + tabs.length) % tabs.length];
  if (next) invoke('activate_tab', { id: next.id });
}

/** 恢复最近关闭的标签：new_tab 命令没有 url 参数，先新建再导航到原地址 */
function reopenClosedTab() {
  invoke('reopen_tab').then((url) => {
    if (!url) { chromeToast('没有可恢复的标签'); return; }
    invoke('new_tab').then(() => invoke('navigate', { input: url }));
  }).catch((err) => reportError(`reopen_tab FAILED: ${String(err)}`));
}
listen('shortcut', (ev) => {
  const p = ev.payload;
  const what = typeof p === 'string' ? p : (p && p.what);
  if (what === 'tab_next') { cycleTab(1); return; }
  if (what === 'tab_prev') { cycleTab(-1); return; }
});
$('#tabstrip').addEventListener('wheel', (e) => {
  if (e.ctrlKey) return;
  const vertical = document.documentElement.dataset.vertical === '1';
  const tabsBox = $('#tabs');
  const overflow = vertical
    ? tabsBox.scrollHeight > tabsBox.clientHeight + 2
    : tabsBox.scrollWidth > tabsBox.clientWidth + 2;
  if (overflow) {
    if (vertical) tabsBox.scrollTop += e.deltaY;
    else tabsBox.scrollLeft += e.deltaY;
    e.preventDefault();
  } else {
    // 一屏放得下：滚轮直接切换标签（参考 Chrome 行为）
    cycleTab(e.deltaY > 0 ? 1 : -1);
    e.preventDefault();
  }
}, { passive: false });

// 无边框窗口：双击标签栏空白处由 Tauri 拖拽脚本自动最大化/还原，
// 不再额外新建标签页，避免「最大化 + 开新标签」同时发生。

// 点 chrome 空白处收起浮层（浮层是独立窗口，不会自己失焦关闭）
document.addEventListener('mousedown', (e) => {
  if (e.target.closest('#address-wrap')) return;          // 点地址栏内部不退出编辑
  if (editing) endEdit();                                  // 其它任何位置：立即退出并恢复原样
  if (e.target.closest('#btn-more') ||
      e.target.closest('#btn-tablist') || e.target.closest('#ws-pill')) return;
  invoke('close_overlay').catch(() => {});
});

// 点到页面区时事件落在标签页 webview 上，chrome 根本收不到 mousedown，
// 只会收到 blur —— 这里补一次：确认焦点真的没了才退出编辑态。
// 建议面板/菜单浮层抢占焦点的场景由 Rust 的 refocus 事件把焦点还回来，
// 所以延迟 220ms 再判断，避免面板一弹出就把编辑态关掉。
window.addEventListener('blur', () => {
  setTimeout(() => {
    if (editing && !document.hasFocus()) endEdit();
  }, 220);
});

listen('state', (ev) => { state = ev.payload; render(); reportLayout(); });

// 浮层（建议面板 / 更多菜单）挂载后会把焦点从地址框上打掉，
// Rust 侧把窗口焦点还回来之后通知这里，把光标重新放回输入框末尾，
// 否则「输一半停一下再按回车」就完全没反应。
// 页面被点击（由注入到每个标签页里的桥上报）：退出地址栏编辑态并收起建议面板。
// 不依赖 window blur —— 点页面时 chrome 常常不会真的失焦。
listen('page-pressed', () => {
  if (editing) endEdit();
  invoke('close_overlay').catch(() => {});
});

/* ==================== 内置插件（扩展程序页）联动 ==================== */
// 查询某个内置插件的开关与配置（读不到时按「开启 + 默认值」处理）
async function extState(id) {
  try {
    const list = await invoke('get_extensions');
    const it = (list || []).find(x => x.id === id);
    if (!it) return { enabled: true, cfg: {} };
    return { enabled: !!it.enabled, cfg: it.cfg || {} };
  } catch { return { enabled: true, cfg: {} }; }
}

let chromeToastTimer = null;
function chromeToast(msg) {
  const el = $('#chrome-toast');
  if (!el) return;
  el.textContent = msg;
  el.classList.add('on');
  clearTimeout(chromeToastTimer);
  chromeToastTimer = setTimeout(() => el.classList.remove('on'), 2200);
}
listen('ext-toast', (ev) => chromeToast(String(ev.payload)));

/* ---- 自绘询问框（替代原生 confirm/prompt，避免 WebView2 假死） ---- */
let askResolve = null;
function askBox(opts) {
  const o = Object.assign({ title: '确认', msg: '', okText: '确定', input: false, value: '' }, opts || {});
  const mask = $('#ask-mask');
  if (!mask) return Promise.resolve(o.input ? null : false);
  $('#ask-title').textContent = o.title;
  $('#ask-msg').textContent = o.msg || '';
  $('#ask-msg').style.display = o.msg ? '' : 'none';
  const input = $('#ask-input');
  input.classList.toggle('hidden', !o.input);
  input.value = o.value || '';
  $('#ask-ok').textContent = o.okText || '确定';
  mask.classList.remove('hidden');
  const done = (v) => {
    mask.classList.add('hidden');
    document.removeEventListener('keydown', onKey, true);
    const r = askResolve; askResolve = null;
    if (r) r(v);
  };
  // capture 阶段拦截：Esc 关对话框而不是关浮层/取消编辑，Enter 提交而不是导航
  const onKey = (e) => {
    if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); done(o.input ? null : false); }
    else if (e.key === 'Enter') { e.preventDefault(); e.stopPropagation(); done(o.input ? input.value : true); }
  };
  document.addEventListener('keydown', onKey, true);
  $('#ask-ok').onclick = () => done(o.input ? input.value : true);
  $('#ask-cancel').onclick = () => done(o.input ? null : false);
  mask.onclick = (e) => { if (e.target === mask) done(o.input ? null : false); };
  setTimeout(() => {
    const t = o.input ? input : $('#ask-ok');
    t.focus();
    if (o.input) input.select();
  }, 0);
  return new Promise((resolve) => { askResolve = resolve; });
}
const confirmBox = (msg, title, okText) => askBox({ title: title || '确认', msg, okText: okText || '确定' });
const promptBox = (title, value, msg) => askBox({ title, msg: msg || '', input: true, value: value || '', okText: '确定' });

/* ---- 关闭确认插件 ---- */
const cgMask = $('#close-guard');
listen('close-guard', (ev) => {
  const n = Number(ev.payload) || 0;
  $('#cg-desc').textContent = `当前工作区还有 ${n} 个标签页，关闭窗口会一起关掉。`;
  cgMask.classList.remove('hidden');
});
$('#cg-cancel').onclick = () => cgMask.classList.add('hidden');
$('#cg-ok').onclick = () => { cgMask.classList.add('hidden'); invoke('force_close_app').catch(() => {}); };

/* ---- 更新提示：启动检测到新版本时右上角滑入（Edge 式） ---- */
const ubBanner = $('#update-banner');
let ubVersion = '', ubUrl = '';
listen('update-available', (ev) => {
  const p = ev.payload || {};
  if (!p.has_update) return;
  ubVersion = p.latest || '';
  ubUrl = p.url || '';
  $('#ub-title').textContent = `Trawl 新版本 v${ubVersion} 可用`;
  $('#ub-desc').textContent = '新版已在云端就绪，可前往下载页获取安装包。';
  ubBanner.classList.remove('hidden');
  clearTimeout(ubBanner._t);
  ubBanner._t = setTimeout(() => ubBanner.classList.add('hidden'), 30000);
});
$('#ub-now').onclick = () => {
  if (ubUrl) invoke('open_external', { url: ubUrl }).catch(() => {});
  ubBanner.classList.add('hidden');
};
$('#ub-later').onclick = () => {
  if (ubVersion) invoke('skip_update', { version: ubVersion }).catch(() => {});
  ubBanner.classList.add('hidden');
};

listen('refocus', () => {
  if (!editing) return;
  address.focus();
  const n = address.value.length;
  try { address.setSelectionRange(n, n); } catch (_) {}
});
invoke('initial_state').then((s) => { state = s; render(); reportLayout(); })
  .catch((err) => reportError(`initial_state FAILED: ${String(err)}`));

listen('shortcut', (ev) => {
  const p = ev.payload;
  const what = typeof p === 'string' ? p : (p && p.what);
  if (what === 'history') openHistory();
  else if (what === 'find') openFindbar();
  else if (what === 'palette') openPalette();
  else if (what === 'address') { address.focus(); beginEdit(true); }
  else if (what === 'bookmark') toggleStar();
  else if (what === 'new_tab') invoke('new_tab');
  else if (what === 'back') goBack();
  else if (what === 'forward') goForward();
  else if (what === 'tablist') invoke('open_tablist');
  else if (what === 'close_tab') { const a = activeTab(); if (a) invoke('close_tab_cmd', { id: a.id }); }
  else if (what === 'reload') invoke('nav_reload');
  else if (what === 'downloads') openDownloads();
  else if (what === 'bookmarks') openBookmarks();
  else if (what === 'zoom_in') invoke('zoom_page', { delta: 0.1 });
  else if (what === 'zoom_out') invoke('zoom_page', { delta: -0.1 });
  else if (what === 'zoom_reset') invoke('zoom_page', { delta: 0 });
  else if (what === 'extensions') openExtensions();
  else if (what === 'translate') openTranslateMenu();
  else if (what === 'reopen_tab') reopenClosedTab();
});

document.addEventListener('keydown', (e) => {
  // 修饰键按平台判断：macOS 认 ⌘(metaKey)，Windows 认 Ctrl(ctrlKey)；
  // otherMod 排除另一个平台的修饰键，避免 ⌘A / Ctrl+A 这类编辑操作被当成浏览器快捷键。
  const modKey = IS_MAC ? e.metaKey : e.ctrlKey;
  const otherMod = IS_MAC ? e.ctrlKey : e.metaKey;
  // 地址栏建议面板是独立 webview，创建时会把 chrome 的 DOM 焦点打到 body 上，
  // 之后再打字/回车就全落空（表现为「输完网址按回车没反应」）。
  // 所以编辑态统一在 document 层兜住：先把焦点还给输入框，再接管回车 / Esc。
  if (editing) {
    if (document.activeElement !== address && !e.ctrlKey && !e.altKey && !e.metaKey) {
      address.focus();
      const n = address.value.length;
      try { address.setSelectionRange(n, n); } catch (_) {}
    }
    // 建议列表键盘选择：方向键转发给 quickbar 浮层（独立 webview，按键留在地址栏）
    if (qbShown && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
      e.preventDefault();
      try { window.__TAURI__.event.emit('quickbar-key', e.key); } catch (_) {}
      return;
    }
    if (e.key === 'Enter') {
      e.preventDefault();
      // 有键盘选中项时打开选中项（quickbar 自己消费），否则直接搜索/导航
      if (qbShown && quickbarSel >= 0) {
        try { window.__TAURI__.event.emit('quickbar-key', 'Enter'); } catch (_) {}
        endEdit();
        address.blur();
        invoke('close_overlay').catch(() => {});
        return;
      }
      commitAddress();
      return;
    }
    if (e.key === 'Escape') { e.preventDefault(); cancelEdit(); return; }
  }
  if (e.key === 'Escape') {
    hideContextMenu();
    invoke('close_overlay').catch(() => {});
    // 多选态下 Esc 先清空选择并给出可见反馈，而不是静默清掉
    if (selectedTabs.size) {
      selectedTabs.clear();
      render();
      chromeToast('已取消多选');
    }
    if (state.glanceOpen) { invoke('close_glance'); return; }
    if (state.split) { invoke('exit_split'); return; }
  }
  // 热键注册失败（被别的程序占用）时的兜底；正常情况 Rust 侧已注册全局热键，
  // 事件不会冒泡到这里，因此不会重复执行。
  if (modKey && !e.altKey && !otherMod) {
    if (e.shiftKey) {
      if (e.key === 'Delete' || e.key === 'Del') { invoke('open_internal', { page: 'settings#privacy-clear' }); e.preventDefault(); return; }
      if (e.key === 'B' || e.key === 'b') { openBookmarks(); e.preventDefault(); return; }
      if (e.key === 'P' || e.key === 'p') { openPalette(); e.preventDefault(); return; }
      // 恢复最近关闭的标签（全局热键 Ctrl+Shift+T 同款逻辑，复用 reopenClosedTab）
      if (e.key === 'T' || e.key === 't') {
        reopenClosedTab();
        e.preventDefault(); return;
      }
      // 原「标签列表」改绑 Ctrl/Cmd+Shift+L
      if (e.key === 'L' || e.key === 'l') { invoke('open_tablist'); e.preventDefault(); return; }
      if (e.key === 'E' || e.key === 'e') { openExtensions(); e.preventDefault(); return; }
    } else {
      const k = e.key.toLowerCase();
      if (k === 't') { invoke('new_tab'); e.preventDefault(); }
      if (k === 'n') { invoke('open_standalone_window', { incognito: false }); e.preventDefault(); }
      if (k === 'p') { invoke('print_page'); e.preventDefault(); }
      if (k === 'l') { address.focus(); beginEdit(true); e.preventDefault(); }
      if (k === 'k') { openPalette(); e.preventDefault(); }  // 与三处文案一致：Ctrl+K = 命令面板
      if (k === 'f') { openFindbar(); e.preventDefault(); }
      if (k === 'h') { openHistory(); e.preventDefault(); }
      if (k === 'd') { toggleStar(); e.preventDefault(); }
      if (k === 'r') { invoke('nav_reload'); e.preventDefault(); }
      if (k === 'w') { const a = activeTab(); if (a) invoke('close_tab_cmd', { id: a.id }); e.preventDefault(); }
      if (e.key === '=' || e.key === '+') { invoke('zoom_page', { delta: 0.1 }); e.preventDefault(); }
      if (e.key === '-') { invoke('zoom_page', { delta: -0.1 }); e.preventDefault(); }
      if (e.key === '0') { invoke('zoom_page', { delta: 0 }); e.preventDefault(); }
      if (e.key === 'Enter' && state.glanceOpen) { invoke('promote_glance'); e.preventDefault(); }
      if (/^[1-9]$/.test(e.key)) {
        const ws = state.workspaces[parseInt(e.key, 10) - 1];
        if (ws) invoke('switch_workspace_cmd', { id: ws.id });
      }
    }
  }
  if (e.altKey && e.key === 'ArrowLeft') { goBack(); e.preventDefault(); }
  if (e.altKey && e.key === 'ArrowRight') { goForward(); e.preventDefault(); }
});

window.addEventListener('resize', () => {
  reportLayout();
  // 书签栏折叠依赖可用宽度，缩放窗口后强制重算
  if (!bookmarkbarEl.classList.contains('hidden')) {
    bookmarkbarEl.dataset.sig = '';
    renderBookmarkBar();
  }
});
window.addEventListener('load', reportLayout);

// 「跟随系统」主题：系统切换深浅色时重新渲染
try {
  window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => {
    if (state.settings?.theme === 'auto') render();
  });
} catch {}
