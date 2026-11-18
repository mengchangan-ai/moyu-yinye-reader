// 摸鱼阅读 · 前端逻辑（浏览器里可预览；在 Tauri 里启用书源、书架与原生窗口能力）
const $ = (s) => document.querySelector(s);

function showRuntimeError(message) {
  const flow = document.querySelector("#flow");
  if (flow) flow.textContent = `界面初始化失败：${message}`;
  const pos = document.querySelector("#pos");
  if (pos) pos.textContent = "运行错误";
}
window.addEventListener("error", (e) => showRuntimeError(e.message || "未知错误"));
window.addEventListener("unhandledrejection", (e) =>
  showRuntimeError(e.reason?.message || String(e.reason || "未知异步错误"))
);

const TAURI = typeof window !== "undefined" && window.__TAURI__ ? window.__TAURI__ : null;
const runningInTauri = !!(TAURI && TAURI.core && TAURI.core.invoke);

function invoke(command, args) {
  if (!runningInTauri) throw new Error("该功能需要在桌面 App 内使用");
  return TAURI.core.invoke(command, args);
}

const state = {
  chapters: [], // 本地：{title, lines}；在线：{title, url, lines?, loading?, error?}
  chapter: 0,
  page: 0,
  pages: 1,
  anchor: 0, // 当前页首行在这一章里的行号：窗口缩放后靠它回到原地
  reanchor: false, // 下一次 render 是否按 anchor 重新定位
  dir: "v", // v = 上下翻页，h = 左右翻页
  desk: false,
  book: null, // 在线书：{sourceId, bookUrl, title, author, cover, shelfId, cached, total}
  totalLines: 0,
  view: "home",
  platform: "", // macos / windows / linux / android / ios
  mobile: false, // 手机上用触摸翻页、不挂桌面窗口逻辑
  settings: { fontSize: 16, fg: "#000000" },
};

const SAMPLE = `第三章  江南
清晨的雾还没散，码头边停着一只乌篷船。
“你真的要走？”她站在石阶上，声音很轻。
他没回头，只是把包袱往肩上提了提。
// 注：此处为两人分别的高潮
船夫撑篙，水波一圈圈荡开。
“江南好，只怕一去不回。”
风里带着水汽，也带着说不清的离别。
……
（未完待续）`;

const CHAP_RE = /^\s*(第[一二三四五六七八九十百千零0-9]+[章回卷部节]|Chapter\s*\d+)/i;

const escapeHtml = (s) =>
  String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));

// ---------- 文本 ----------
function splitLines(text) {
  const rows = String(text)
    .replace(/\r\n?/g, "\n")
    .split("\n")
    .map((l) => l.replace(/\s+$/, ""));
  while (rows.length && rows[0].trim() === "") rows.shift();
  while (rows.length && rows[rows.length - 1].trim() === "") rows.pop();
  return rows;
}

// 整本 txt 切章；没有章节标记就当单章
function parseBook(text) {
  const lines = String(text).replace(/\r\n?/g, "\n").split("\n");
  const chapters = [];
  let cur = { title: "正文", lines: [] };
  for (const raw of lines) {
    const line = raw.replace(/\s+$/, "");
    if (CHAP_RE.test(line) && line.trim().length < 60) {
      if (cur.lines.length > 0) chapters.push(cur);
      cur = { title: line.trim(), lines: [] };
    } else if (line.trim() === "") {
      if (cur.lines.length > 0 && cur.lines[cur.lines.length - 1] !== "") cur.lines.push("");
    } else {
      cur.lines.push(line);
    }
  }
  if (cur.lines.length > 0 || chapters.length === 0) chapters.push(cur);
  return chapters;
}

// 纯阅读样式：不再按「标题 / 对话 / 注释」上色
const classOf = () => "";
void CHAP_RE;

const lineHtml = (line) => `<div class="line ${classOf(line)}">${escapeHtml(line) || " "}</div>`;

// ---------- 排版 / 分页 ----------
const COL_GAP = 56;

function contentBox() {
  const c = $("#content");
  const cs = getComputedStyle(c);
  return {
    w: c.clientWidth - parseFloat(cs.paddingLeft) - parseFloat(cs.paddingRight),
    h: c.clientHeight - parseFloat(cs.paddingTop) - parseFloat(cs.paddingBottom),
    lineHeight: parseFloat(cs.lineHeight) || 26,
  };
}

// 分页结果缓存：只有窗口尺寸 / 字号 / 章节内容变了才重新计算
let pageCache = null;
function invalidateLayout() {
  pageCache = null;
}

function layoutKey(ch) {
  const box = contentBox();
  return [state.chapter, ch.lines.length, Math.round(box.w), Math.round(box.h), state.settings.fontSize].join(":");
}

// 上下翻页：按**真实渲染高度**切页，不再用 floor(高度 / 行高) 估行数。
// 原来按「一行一个自然段」估算，可小说里一个自然段就是一行、长段会折成 2~3 个视觉行，
// 估出来的行数偏多，页面底部被裁掉，翻页就漏字；窗口一缩窄折算得更多，漏得更明显。
//
// anchorLine 给定时（窗口缩放/换字号/开关设置条之后），就从那一行开始重新切：
// 往前倒着切出之前的页、往后正着切出之后的页，这样那一行正好停在页首，
// 前后翻页仍然首尾相接，既不回退也不漏字。
function verticalPageStarts(ch, anchorLine) {
  const sig = layoutKey(ch);
  if (pageCache && pageCache.sig === sig) return pageCache;

  const flow = $("#flow");
  flow.removeAttribute("style");
  flow.innerHTML = ch.lines.map(lineHtml).join("");

  const els = flow.children;
  const n = els.length;
  const box = contentBox();
  let starts = [0];
  let anchorPage = 0;

  if (n > 0 && box.h > 0) {
    const firstTop = els[0].offsetTop;
    const tops = new Array(n);
    const heights = new Array(n);
    for (let i = 0; i < n; i++) {
      tops[i] = els[i].offsetTop - firstTop;
      heights[i] = els[i].offsetHeight;
    }
    const fits = (from, to) => tops[to] + heights[to] - tops[from] <= box.h;

    const forward = (from) => {
      const out = [];
      let i = from;
      while (i < n) {
        out.push(i);
        let j = i;
        while (j + 1 < n && fits(i, j + 1)) j++;
        i = j + 1;
      }
      return out;
    };
    // 从 anchor 往前倒着切，返回升序的页首列表（最后一页截止到 anchor）
    const backward = (anchor) => {
      const out = [];
      let end = anchor;
      while (end > 0) {
        let s = end - 1;
        while (s > 0 && fits(s - 1, end - 1)) s--;
        out.unshift(s);
        end = s;
      }
      return out;
    };

    if (anchorLine > 0 && anchorLine < n) {
      const back = backward(anchorLine);
      starts = back.concat(forward(anchorLine));
      anchorPage = back.length;
    } else {
      starts = forward(0);
    }
  }

  pageCache = { sig, starts, anchorPage };
  return pageCache;
}

function renderVertical(ch) {
  const content = $("#content");
  content.classList.remove("dir-h");
  const flow = $("#flow");

  if (state.reanchor) invalidateLayout();
  const layout = verticalPageStarts(ch, state.reanchor ? state.anchor : null);
  const starts = layout.starts;
  state.pages = Math.max(1, starts.length);

  // 窗口/字号变了以后按「首行」重新定位，页码本身已经指向别处了
  if (state.reanchor) {
    state.reanchor = false;
    state.page = layout.anchorPage;
  }
  if (state.page >= starts.length) state.page = starts.length - 1;
  if (state.page < 0) state.page = 0;

  const from = starts[state.page];
  const to = state.page + 1 < starts.length ? starts[state.page + 1] : ch.lines.length;
  flow.removeAttribute("style");
  flow.innerHTML = ch.lines.slice(from, to).map(lineHtml).join("");
  state.anchor = from;
  $("#gutter").innerHTML = "";
}

function columnOf(el, flow, step) {
  return Math.round((el.offsetLeft - flow.offsetLeft) / step);
}

function columnFirstLine(flow, page, step) {
  const els = flow.children;
  for (let i = 0; i < els.length; i++) if (columnOf(els[i], flow, step) >= page) return i;
  return Math.max(0, els.length - 1);
}

function renderHorizontal(ch) {
  const content = $("#content");
  content.classList.add("dir-h");
  const flow = $("#flow");
  flow.innerHTML = ch.lines.map(lineHtml).join("");
  $("#gutter").innerHTML = "";

  const box = contentBox();
  const step = box.w + COL_GAP;
  flow.style.height = box.h + "px";
  flow.style.columnWidth = box.w + "px";
  flow.style.columnGap = COL_GAP + "px";

  const total = Math.max(1, Math.ceil((flow.scrollWidth + COL_GAP) / step));
  state.pages = total;

  if (state.reanchor) {
    state.reanchor = false;
    const el = flow.children[Math.min(state.anchor, flow.children.length - 1)];
    state.page = el ? columnOf(el, flow, step) : 0;
  }
  if (state.page >= total) state.page = total - 1;
  if (state.page < 0) state.page = 0;

  flow.style.transform = `translateX(${-state.page * step}px)`;
  state.anchor = columnFirstLine(flow, state.page, step);
}

function chapterOffset(index) {
  let n = 0;
  for (let i = 0; i < index; i++) n += state.chapters[i].lines ? state.chapters[i].lines.length : 0;
  return n;
}

function flashTurn() {
  const el = state.dir === "h" ? $("#content") : $("#flow");
  el.classList.remove("turn");
  void el.offsetWidth;
  el.classList.add("turn");
}

function render() {
  if (state.view !== "reader") return;
  const ch = state.chapters[state.chapter];
  const flow = $("#flow");
  if (!ch) {
    flow.innerHTML = "";
    $("#gutter").innerHTML = "";
    return;
  }
  if (ch.lines == null) {
    renderPlaceholder(ch);
    updateStatus(ch);
    return;
  }
  if (state.dir === "h") renderHorizontal(ch);
  else renderVertical(ch);
  updateStatus(ch);
  saveProgress();
  if (state.turn) {
    state.turn = false;
    flashTurn();
  }
}

function renderPlaceholder(ch) {
  const flow = $("#flow");
  $("#gutter").innerHTML = "";
  if (ch.error) {
    flow.innerHTML = `<div class="state-line err">载入失败：${escapeHtml(ch.error)}<span class="retry">点这里重试</span></div>`;
    const retry = flow.querySelector(".retry");
    if (retry)
      retry.onclick = () => {
        ch.error = null;
        render();
        loadChapter(state.chapter);
      };
    return;
  }
  flow.innerHTML = `<div class="state-line"><span class="spinner"></span>正在载入本章…</div>`;
}

function updateStatus(ch) {
  const n = state.chapters.length;
  let pct;
  if (state.book) {
    pct = Math.round(((state.chapter + state.page / state.pages) / Math.max(1, n)) * 100);
  } else if (state.totalLines && ch.lines) {
    pct = Math.round(
      ((chapterOffset(state.chapter) + (state.page / state.pages) * ch.lines.length) / state.totalLines) * 100
    );
  } else {
    pct = 0;
  }
  $("#prog").textContent = `${Math.min(100, Math.max(0, pct))}%`;
  $("#chapterInfo").textContent = `章 ${state.chapter + 1} / ${n}`;
  $("#pageInfo").textContent = `页 ${state.page + 1} / ${state.pages}`;
  const posEl = $("#pos");
  if (posEl) posEl.textContent = "";
  $("#chapterChip").textContent = state.book
    ? `${state.book.title}${ch.title ? " · " + ch.title : ""}`
    : ch.title || "正文";
  $("#chapterHead").innerHTML = `<strong>${escapeHtml(ch.title || "正文")}</strong>${
    state.book ? " · " + escapeHtml(state.book.title) : ""
  }`;
  syncDrawerCurrent();
  updateCacheInfo();
}

function updateCacheInfo() {
  const el = $("#cacheInfo");
  if (!state.book) {
    el.hidden = true;
    return;
  }
  el.hidden = false;
  el.textContent = state.book.cached > 0 ? `已缓存 ${state.book.cached} 章` : "边看边缓存";
}

// ---------- 加载 ----------
function loadLocal(text) {
  state.book = null;
  let hash = 2166136261;
  for (let i = 0; i < text.length; i++) hash = Math.imul(hash ^ text.charCodeAt(i), 16777619);
  state.localProgressKey = `local:${hash >>> 0}:${text.length}`;
  state.chapters = parseBook(text);
  state.totalLines = state.chapters.reduce((a, c) => a + c.lines.length, 0);
  state.chapter = 0;
  state.page = 0;
  state.anchor = 0;
  state.reanchor = false;
  const saved = readProgress(state.localProgressKey);
  if (saved) applySavedPosition(saved, state.chapters.length);
  buildToc();
  render();
}

// 用「首行」恢复阅读位置：页码只对当时的窗口大小成立，窗口一变就指到别处
function applySavedPosition(saved, chaptersLength) {
  if (!saved) return;
  state.chapter = Math.min(Math.max(0, Number(saved.chapter) || 0), Math.max(0, chaptersLength - 1));
  state.page = Math.max(0, Number(saved.page) || 0);
  state.anchor = Math.max(0, Number(saved.anchor) || 0);
  state.reanchor = saved.anchor != null;
}

async function loadChapter(i) {
  const ch = state.chapters[i];
  if (!ch || ch.lines != null || !ch.url) return;
  if (ch.loading) return ch.loading;
  ch.error = null;
  ch.loading = (async () => {
    try {
      const text = await invoke("load_chapter", {
        sourceId: state.book.sourceId,
        bookUrl: state.book.bookUrl,
        chapterUrl: ch.url,
      });
      ch.lines = splitLines(text);
      if (ch.lines.length === 0) ch.lines = ["（本章没有正文）"];
    } catch (e) {
      ch.error = String((e && e.message) || e);
    } finally {
      ch.loading = null;
    }
  })();
  await ch.loading;
  if (i === state.chapter) render();
  if (!ch.error) {
    prefetchFrom(i);
    saveProgress();
  }
}

function prefetchFrom(i) {
  if (!state.book) return;
  const urls = [];
  for (let k = i + 1; k <= i + 3 && k < state.chapters.length; k++) {
    const c = state.chapters[k];
    if (c.url && c.lines == null) urls.push(c.url);
  }
  if (!urls.length) return;
  invoke("prefetch_chapters", {
    sourceId: state.book.sourceId,
    bookUrl: state.book.bookUrl,
    chapterUrls: urls,
  })
    .then((n) => {
      if (state.book && n > 0) {
        state.book.cached += n;
        updateCacheInfo();
      }
    })
    .catch(() => {});
}

let progressTimer = null;
function progressKey() {
  return state.book ? `online:${state.book.sourceId}:${state.book.bookUrl}` : state.localProgressKey;
}
function readProgress(key) {
  try { return JSON.parse(localStorage.getItem(`yinye.progress:${key}`) || "null"); }
  catch { return null; }
}
function saveProgress() {
  const key = progressKey();
  if (key) {
    try {
      localStorage.setItem(
        `yinye.progress:${key}`,
        JSON.stringify({ chapter: state.chapter, page: state.page, anchor: state.anchor })
      );
    } catch { /* 存储不可用时仍保留书架进度 */ }
  }
  if (!state.book || !state.book.shelfId) return;
  clearTimeout(progressTimer);
  const ch = state.chapters[state.chapter];
  const payload = {
    id: state.book.shelfId, chapter: state.chapter,
    chapterTitle: ch?.title || "", chaptersTotal: state.chapters.length,
  };
  const shelfBook = shelfBooks.find(b => b.id === payload.id);
  if (shelfBook) { shelfBook.lastChapter = payload.chapter; shelfBook.lastChapterTitle = payload.chapterTitle; }
  progressTimer = setTimeout(() => {
    invoke("shelf_progress", payload).catch(() => {});
  }, 900);
}

// ---------- 翻页 ----------
async function nextPage() {
  if (state.page + 1 < state.pages) {
    state.page++;
    state.turn = true;
    render();
    return;
  }
  if (state.chapter + 1 < state.chapters.length) {
    state.chapter++;
    state.page = 0;
    state.turn = true;
    render();
    await loadChapter(state.chapter);
    render();
  }
}

async function prevPage() {
  if (state.page > 0) {
    state.page--;
    state.turn = true;
    render();
    return;
  }
  if (state.chapter > 0) {
    state.chapter--;
    await loadChapter(state.chapter);
    state.page = 1e9; // render 会夹到最后一页
    render();
  }
}

async function gotoChapter(i) {
  const target = Math.max(0, Math.min(state.chapters.length - 1, i));
  state.chapter = target;
  state.page = 0;
  state.anchor = 0;
  state.reanchor = false;
  state.turn = true;
  render();
  await loadChapter(target);
  render();
}

// ---------- 目录（左侧抽屉） ----------
let tocOpen = false;

function buildToc() {
  const list = $("#tocList");
  list.innerHTML = state.chapters
    .map((c, i) => `<div class="drawer-item" data-i="${i}">${escapeHtml(c.title)}</div>`)
    .join("");
  list.querySelectorAll(".drawer-item").forEach((d) => {
    d.onclick = () => gotoChapter(+d.dataset.i);
  });
  $("#tocCount").textContent = state.chapters.length ? `${state.chapters.length} 章` : "";
  syncDrawerCurrent();
}

function setDrawer(open) {
  tocOpen = typeof open === "boolean" ? open : !tocOpen;
  const d = $("#tocDrawer");
  d.hidden = false;
  d.classList.toggle("open", tocOpen);
  if (tocOpen) {
    // 内容层滑一下，宽度过渡交给 CSS
    const inner = d.querySelector(".drawer-list");
    if (inner) {
      inner.style.animation = "none";
      void inner.offsetWidth;
      inner.style.animation = "drawer-in 0.22s ease";
    }
    syncDrawerCurrent(true);
  }
  // 正文宽度变了，等过渡结束重新分页；按首行定位，别让页码跳走
  clearTimeout(setDrawer._t);
  setDrawer._t = setTimeout(() => {
    invalidateLayout();
    state.reanchor = true;
    render();
  }, 260);
}

function syncDrawerCurrent(forceScroll) {
  const list = $("#tocList");
  if (!list) return;
  const cur = list.querySelector(".drawer-item.on");
  const next = list.querySelector(`.drawer-item[data-i="${state.chapter}"]`);
  if (cur && cur !== next) cur.classList.remove("on");
  if (next) {
    next.classList.add("on");
    if (tocOpen && (forceScroll || !next.dataset.seen)) {
      next.dataset.seen = "1";
      next.scrollIntoView({ block: "center" });
    }
  }
}

$("#tocBtn").onclick = (e) => {
  e.stopPropagation();
  setDrawer();
};
$("#tocClose").onclick = () => setDrawer(false);

// ---------- 导入（文件 / 粘贴文章 / 链接文章） ----------
$("#importBtn").onclick = (e) => {
  e.stopPropagation();
  $("#importMenu").hidden = !$("#importMenu").hidden;
};
document.addEventListener("click", (e) => {
  if (!$("#importMenu").hidden && !e.target.closest("#importMenu") && !e.target.closest("#importBtn")) {
    $("#importMenu").hidden = true;
  }
});
document.querySelectorAll("#importMenu button").forEach((b) => {
  b.onclick = () => {
    $("#importMenu").hidden = true;
    if (b.dataset.import === "file") $("#fileInput").click();
    else openArticleModal(b.dataset.import);
  };
});
$("#fileInput").onchange = (e) => {
  const f = e.target.files[0];
  if (!f) return;
  const r = new FileReader();
  r.onload = () => {
    loadLocal(r.result);
    showReader();
    render();
  };
  r.readAsText(f, "utf-8");
};
window.addEventListener("dragover", (e) => e.preventDefault());
window.addEventListener("drop", (e) => {
  e.preventDefault();
  const f = e.dataTransfer.files[0];
  if (f && /\.txt$/i.test(f.name)) {
    const r = new FileReader();
    r.onload = () => {
      loadLocal(r.result);
      showReader();
      render();
    };
    r.readAsText(f, "utf-8");
  }
});

// ---------- 键盘 ----------
document.addEventListener("keydown", (e) => {
  const inInput = e.target.tagName === "INPUT" || e.target.tagName === "TEXTAREA";

  if ((e.metaKey || e.ctrlKey) && (e.key === "f" || e.key === "F")) {
    e.preventDefault();
    showHome(true);
    return;
  }
  if ((e.metaKey || e.ctrlKey) && e.shiftKey && (e.key === "m" || e.key === "M")) {
    e.preventDefault();
    toggleDesk();
    return;
  }
  if (e.key === "Escape") {
    if (state.desk) { toggleDesk(false); return; }
    if (state.view === "reader" && tocOpen) { setDrawer(false); return; }
    if (state.view === "home" && homeMode === "search") { $("#homeBack").click(); return; }
  }
  if (inInput) return;
  if (state.view !== "reader") return;
  if ((e.metaKey || e.ctrlKey) && ["+", "=", "-", "0"].includes(e.key)) {
    e.preventDefault();
    setFont(e.key === "0" ? 16 : state.settings.fontSize + (e.key === "-" ? -1 : 1));
    return;
  }
  if (e.ctrlKey && e.key === "ArrowDown") { e.preventDefault(); gotoChapter(state.chapter + 1); return; }
  if (e.ctrlKey && e.key === "ArrowUp") { e.preventDefault(); gotoChapter(state.chapter - 1); return; }
  if ((e.metaKey || e.ctrlKey) && (e.key === "t" || e.key === "T")) { e.preventDefault(); setDrawer(); return; }

  if (e.key === " " || e.key === "PageDown" || e.key === "ArrowDown" || e.key === "ArrowRight") {
    e.preventDefault();
    nextPage();
  } else if (e.key === "PageUp" || e.key === "ArrowUp" || e.key === "ArrowLeft") {
    e.preventDefault();
    prevPage();
  }
});

// 滚轮翻页
let wheelLock = 0;
$("#content").addEventListener(
  "wheel",
  (e) => {
    e.preventDefault();
    const now = Date.now();
    if (now - wheelLock < 220) return;
    const dy = e.deltaY, dx = e.deltaX;
    if (Math.abs(dy) < 2 && Math.abs(dx) < 2) return;
    wheelLock = now;
    if (dy > 0 || dx > 0) nextPage();
    else prevPage();
  },
  { passive: false }
);

// ---------- 主题 / 设置 ----------
$("#gearBtn").onclick = () => {
  $("#controls").hidden = !$("#controls").hidden;
  // 设置条一开一关，正文高度就变了：重新分页并停在原来那一行
  invalidateLayout();
  state.reanchor = true;
  requestAnimationFrame(render);
};


$("#fontMinus").onclick = () => setFont(state.settings.fontSize - 1);
$("#fontPlus").onclick = () => setFont(state.settings.fontSize + 1);
function setFont(px) {
  state.settings.fontSize = Math.max(8, Math.min(48, px));
  applyFont();
  invalidateLayout();
  state.reanchor = true;
  render();
  saveSettings();
}
function applyFont() {
  const base = state.settings.fontSize;
  const lh = Math.round(base * 1.62);
  document.documentElement.style.setProperty("--font-size", base + "px");
  document.documentElement.style.setProperty("--line-height", lh + "px");
  $("#fontInfo").textContent = `字号 ${base}`;
}

document.querySelectorAll("#dirSeg .seg-btn").forEach((b) => {
  b.onclick = () => setDir(b.dataset.dir);
});

// 字色：空字符串 = 跟随主题
function setFg(color) {
  state.settings.fg = color || "";
  if (/^#[0-9a-f]{6}$/i.test(color || "")) {
    const rgb = [1, 3, 5].map(i => parseInt(color.slice(i, i + 2), 16) / 255);
    const light = rgb[0] * .2126 + rgb[1] * .7152 + rgb[2] * .0722 > .5;
    document.body.dataset.theme = light ? "dark" : "light";
  } else document.body.dataset.theme = "glass";
  if (color) document.body.style.setProperty("--reader-fg", color);
  else document.body.style.removeProperty("--reader-fg");
  const picker = $("#fgColor");
  if (picker && color) picker.value = /^#[0-9a-f]{6}$/i.test(color) ? color : picker.value;
  document.querySelectorAll("#fgSwatches .swatch").forEach((b) => {
    b.classList.toggle("active", (b.dataset.fg || "") === state.settings.fg);
  });
  saveSettings();
}
document.querySelectorAll("#fgSwatches .swatch").forEach((b) => {
  b.onclick = () => setFg(b.dataset.fg);
});
$("#fgColor").oninput = (e) => setFg(e.target.value);
function setDir(dir) {
  state.dir = dir;
  document.querySelectorAll("#dirSeg .seg-btn").forEach((b) => b.classList.toggle("active", b.dataset.dir === dir));
  // 上下↔左右切换时留在同一行，不要跳回章首
  invalidateLayout();
  state.reanchor = true;
  render();
  saveSettings();
}

function saveSettings() {
  try {
    localStorage.setItem("moyu.settings", JSON.stringify({ ...state.settings, dir: state.dir }));
  } catch { /* 忽略 */ }
}
function restoreSettings() {
  try {
    const s = JSON.parse(localStorage.getItem("moyu.settings") || "{}");
    if (s.fontSize) state.settings.fontSize = s.fontSize;
    if (s.dir) state.dir = s.dir;
    document.querySelectorAll("#dirSeg .seg-btn").forEach((b) => b.classList.toggle("active", b.dataset.dir === state.dir));
    applyFont();
    setFg(typeof s.fg === "string" && s.fg ? s.fg : "#000000");
  } catch { /* 忽略 */ }
}

// ---------- 桌面文字模式 ----------
async function toggleDesk(force) {
  const on = typeof force === "boolean" ? force : !state.desk;
  if (on === state.desk) return;
  if (on && state.view !== "reader") {
    toast("先在书架里打开一本书");
    return;
  }
  state.desk = on;
  document.body.classList.toggle("desk", on);
  $("#deskExit").hidden = !on;
  applyFont();
  if (on) {
    const chip = $("#deskExit");
    chip.classList.add("hint");
    clearTimeout(chip._t);
    chip._t = setTimeout(() => chip.classList.remove("hint"), 2600);
  }
  if (runningInTauri) {
    try {
      await invoke("set_desktop_mode", { enabled: on });
    } catch (e) {
      console.warn("切换桌面模式失败", e);
    }
  }
  // 摸鱼模式把标题栏/工具栏/状态栏都藏了，正文区变高：重新分页、停在原来那一行
  invalidateLayout();
  state.reanchor = true;
  requestAnimationFrame(() => {
    render();
    if (runningInTauri && on) {
      try { TAURI.window.getCurrentWindow().setFocus(); } catch { /* 忽略 */ }
    }
  });
}
$("#desktopModeBtn").onclick = () => toggleDesk();
$("#deskExit").onclick = () => toggleDesk(false);

// ---------- 原生窗口（仅桌面） ----------
function setupDesktopWindow() {
  if (!runningInTauri) return;
  const win = TAURI.window.getCurrentWindow();
  document.querySelectorAll(".lights span").forEach((el) => {
    el.onclick = async (e) => {
      e.stopPropagation();
      try {
        if (el.dataset.act === "close") await win.close();
        else if (el.dataset.act === "min") await win.minimize();
        else await win.toggleMaximize();
      } catch (err) {
        console.error("窗口操作失败", err);
      }
    };
  });
  const bar = $(".titlebar");
  if (bar)
    bar.addEventListener("dblclick", (e) => {
      if (e.target.closest(".lights")) return;
      win.toggleMaximize().catch(() => {});
    });
}

// ---------- 书架主页 ----------
let shelfBooks = [];
const coverCache = new Map();
let homeMode = "shelf"; // shelf | search
let searchResults = [];
let lastKeyword = "";

function toast(msg, ms = 2400) {
  const el = $("#toast");
  el.textContent = msg;
  el.hidden = false;
  clearTimeout(el._t);
  el._t = setTimeout(() => (el.hidden = true), ms);
}

function showHome(focusSearch) {
  state.view = "home";
  document.body.dataset.view = "home";
  $("#homeView").hidden = false;
  $("#readerView").hidden = true;
  $("#statusbar").hidden = true;
  $("#backBtn").hidden = true;
  $("#tocBtn").hidden = true;
  $("#desktopModeBtn").hidden = true;
  setDrawer(false);
  renderHome();
  if (focusSearch) setTimeout(() => $("#homeSearch").focus(), 40);
}

function showReader() {
  state.view = "reader";
  document.body.dataset.view = "reader";
  $("#homeView").hidden = true;
  $("#readerView").hidden = false;
  $("#statusbar").hidden = false;
  $("#backBtn").hidden = false;
  $("#tocBtn").hidden = false;
  $("#desktopModeBtn").hidden = state.mobile; // 手机上「摸鱼模式」没有意义
  invalidateLayout(); // 从书架回到阅读时正文区尺寸已变，旧分页作废
  requestAnimationFrame(render);
}

$("#backBtn").onclick = () => showHome();
$("#homeBack").onclick = () => {
  homeMode = "shelf";
  renderHome();
};

function bookCard(item, i, mode) {
  const inShelf = shelfBooks.some((b) => b.url === item.url);
  let act = "";
  if (mode === "search") {
    act = inShelf
      ? `<button class="card-act" disabled title="已在书架">✓</button>`
      : `<button class="card-act" data-act="add" title="加入书架">＋</button>`;
  } else {
    act = `<button class="card-act" data-act="del" title="移出书架">×</button>`;
  }
  const meta =
    mode === "search"
      ? escapeHtml(item.author || "佚名")
      : item.lastChapterTitle
      ? "读到 " + escapeHtml(item.lastChapterTitle)
      : escapeHtml(item.author || "未开始");
  return `<article class="book-card" data-i="${i}" data-mode="${mode}" data-url="${escapeHtml(item.url || "")}">
    <div class="book-cover">
      <span>${escapeHtml((item.title || "书").slice(0, 1))}</span>
      <img alt="" />
      ${act}
    </div>
    <div class="book-name">${escapeHtml(item.title)}</div>
    <div class="book-meta">${meta}</div>
  </article>`;
}

function bindCards(mode) {
  $("#bookGrid").querySelectorAll(".book-card").forEach((card) => {
    const i = +card.dataset.i;
    const item = mode === "search" ? searchResults[i] : shelfBooks[i];
    if (!item) return;
    const img = card.querySelector("img");

    if (mode === "shelf") {
      if (item.cover) fillCover(img, item.cover);
    } else {
      loadSearchCover(img, item);
    }

    card.onclick = (e) => {
      if (e.target.closest(".card-act")) return;
      if (mode === "shelf") {
        openOnlineBook({
          sourceId: item.sourceId,
          url: item.url,
          title: item.title,
          author: item.author,
          cover: item.cover,
          lastChapter: item.lastChapter || 0,
        });
      } else {
        openOnlineBook({ ...item, lastChapter: 0 });
      }
    };

    const add = card.querySelector('[data-act="add"]');
    if (add) add.onclick = (e) => {
      e.stopPropagation();
      addToShelf(add, item);
    };
    const del = card.querySelector('[data-act="del"]');
    if (del) del.onclick = (e) => {
      e.stopPropagation();
      removeFromShelf(item);
    };
  });
}

function renderHome() {
  const grid = $("#bookGrid");
  const empty = $("#homeEmpty");
  const searching = homeMode === "search";
  $("#homeTitle").textContent = searching ? `搜索结果 · ${lastKeyword}` : "书架";
  $("#homeBack").hidden = !searching;

  const list = searching ? searchResults : shelfBooks;
  grid.innerHTML = list.map((item, i) => bookCard(item, i, homeMode)).join("");
  empty.hidden = list.length > 0;
  if (!searching && !list.length) {
    empty.innerHTML = `<div class="home-empty-icon">📚</div><p>书架还是空的</p><p class="dim">上面搜一本书，点封面右上角的「＋」加入书架</p>`;
  }
  bindCards(homeMode);
}

function fillCover(imgEl, remoteUrl) {
  if (!remoteUrl || !imgEl) return;
  const cached = coverCache.get(remoteUrl);
  if (cached) {
    imgEl.src = cached;
    imgEl.classList.add("has");
    return;
  }
  invoke("cover_data", { url: remoteUrl })
    .then((data) => {
      if (!data) return;
      coverCache.set(remoteUrl, data);
      imgEl.onerror = () => imgEl.classList.remove("has");
      imgEl.src = data;
      imgEl.classList.add("has");
    })
    .catch(() => {});
}

async function loadSearchCover(img, item) {
  if (!img || !runningInTauri) return;
  try {
    const data = await invoke("book_cover", { sourceId: item.sourceId, bookUrl: item.url });
    if (!data) return;
    img.onerror = () => img.classList.remove("has");
    img.src = data;
    img.classList.add("has");
  } catch { /* 没封面就用首字占位 */ }
}

async function doSearch() {
  const kw = $("#homeSearch").value.trim();
  if (!kw) return;
  lastKeyword = kw;
  homeMode = "search";
  searchResults = [];
  $("#homeTitle").textContent = `搜索结果 · ${kw}`;
  $("#homeBack").hidden = false;
  $("#homeEmpty").hidden = true;
  $("#bookGrid").innerHTML = `<div class="home-loading"><span class="spinner"></span>正在搜索「${escapeHtml(kw)}」…</div>`;
  if (!runningInTauri) {
    $("#bookGrid").innerHTML = `<div class="home-loading">搜索需要在桌面 App 内使用</div>`;
    return;
  }
  try {
    searchResults = await invoke("search_books", { keyword: kw, sourceId: Number($("#sourceSelect").value) });
    if (!searchResults.length) {
      $("#bookGrid").innerHTML = "";
      $("#homeEmpty").hidden = false;
      $("#homeEmpty").innerHTML = `<div class="home-empty-icon">🔍</div><p>没搜到，换个关键词试试</p><p class="dim">当前书源：香书小说</p>`;
      return;
    }
    renderHome();
    // 封面按顺序慢慢补
    const limit = Math.min(searchResults.length, 12);
    for (let i = 0; i < limit; i++) {
      const img = $(`#bookGrid .book-card[data-i="${i}"] img`);
      if (img) await loadSearchCover(img, searchResults[i]);
      await new Promise((r) => setTimeout(r, 120));
    }
  } catch (e) {
    $("#bookGrid").innerHTML = `<div class="home-loading">搜索失败：${escapeHtml(String(e))}</div>`;
  }
}

$("#homeSearchBtn").onclick = doSearch;
$("#homeSearch").addEventListener("keydown", (e) => {
  if (e.key === "Enter") doSearch();
});

async function refreshShelf() {
  if (!runningInTauri) return;
  try {
    shelfBooks = await invoke("shelf_list");
  } catch { /* 忽略 */ }
  if (homeMode === "shelf" && state.view === "home") renderHome();
}

async function addToShelf(btn, r) {
  btn.disabled = true;
  btn.textContent = "…";
  try {
    await invoke("shelf_add", {
      sourceId: r.sourceId,
      url: r.url,
      title: r.title,
      author: r.author || "",
      cover: null,
    });
    btn.textContent = "✓";
    await refreshShelf();
    toast(`《${r.title}》已加入书架`);
  } catch (e) {
    btn.textContent = "＋";
    btn.disabled = false;
    toast("加入书架失败：" + e);
  }
}

async function removeFromShelf(book) {
  try {
    await invoke("shelf_remove", { id: book.id });
    toast(`已把《${book.title}》移出书架`);
    await refreshShelf();
  } catch (e) {
    toast("移除失败：" + e);
  }
}

// ---------- 在线阅读 ----------
async function openOnlineBook(book) {
  if (!runningInTauri) {
    toast("在线阅读需要在桌面 App 内使用");
    return;
  }
  showReader();
  $("#flow").innerHTML = `<div class="state-line"><span class="spinner"></span>正在取目录…</div>`;
  $("#gutter").innerHTML = "";
  try {
    const info = await invoke("open_book", { sourceId: book.sourceId, bookUrl: book.url });
    state.book = {
      sourceId: book.sourceId,
      bookUrl: book.url,
      title: info.title || book.title || "未知书名",
      author: info.author || book.author || "",
      cover: info.cover || book.cover || null,
      shelfId: info.shelfId || null,
      cached: info.cachedChapters || 0,
      total: info.chapters.length,
    };
    state.chapters = info.chapters.map((c) => ({ title: c.title, url: c.url }));
    state.totalLines = 0;
    const saved = readProgress(progressKey());
    if (saved) applySavedPosition(saved, state.chapters.length);
    else {
      state.chapter = Math.min(Math.max(0, Number(book.lastChapter) || 0), state.chapters.length - 1);
      state.page = 0;
      state.anchor = 0;
      state.reanchor = false;
    }
    state.pages = 1;
    buildToc();
    render();
    await loadChapter(state.chapter);
    render();
    toast(`《${state.book.title}》共 ${state.chapters.length} 章`);
  } catch (e) {
    $("#flow").innerHTML = `<div class="state-line err">打开失败：${escapeHtml(String(e))}</div>`;
    toast("打开失败：" + e);
  }
}


// ---------- 文章导入（粘贴 / 链接） ----------
let articleMode = "paste";

function openArticleModal(mode) {
  articleMode = mode;
  $("#modalTitle").textContent = mode === "link" ? "从链接导入文章" : "粘贴文章 / 文本";
  $("#linkField").hidden = mode !== "link";
  $("#articleFetch").hidden = mode !== "link";
  $("#articleText").value = "";
  $("#articleUrl").value = "";
  $("#articleTitle").value = "";
  $("#articleModal").hidden = false;
  setTimeout(() => (mode === "link" ? $("#articleUrl") : $("#articleText")).focus(), 30);
}
function closeArticleModal() {
  $("#articleModal").hidden = true;
}
$("#modalClose").onclick = closeArticleModal;
$("#articleCancel").onclick = closeArticleModal;

$("#articleFetch").onclick = async () => {
  const url = $("#articleUrl").value.trim();
  if (!url) {
    toast("先填文章链接");
    return;
  }
  $("#articleFetch").disabled = true;
  $("#articleFetch").textContent = "抓取中…";
  try {
    const art = await invoke("fetch_article", { url });
    $("#articleText").value = art.text;
    if (art.title) $("#articleTitle").value = art.title;
    toast(`抓到 ${art.text.length} 字`);
  } catch (e) {
    toast("抓取失败：" + e);
  } finally {
    $("#articleFetch").disabled = false;
    $("#articleFetch").textContent = "从链接抓取正文";
  }
};

$("#articleOk").onclick = () => {
  const text = $("#articleText").value.trim();
  const title = $("#articleTitle").value.trim();
  if (!text) {
    toast("正文是空的");
    return;
  }
  closeArticleModal();
  importArticle(title, text);
};

function importArticle(title, text) {
  loadLocal(text);
  showReader();
  render();
  if (title && state.chapters.length) {
    if (state.chapters.length === 1) {
      state.chapters[0].title = title;
      buildToc();
      render();
    } else {
      toast(`已按章节识别出 ${state.chapters.length} 章`);
    }
  }
  toast(`已导入${title ? "《" + title + "》" : "文章"}，共 ${state.chapters.length} 章`);
}

// ---------- 任意位置拖动窗口 + 八向拉伸 + 记住大小（仅桌面） ----------
function setupDesktopDrag() {
  if (!runningInTauri) return;
  const win = TAURI.window.getCurrentWindow();
  let dragStart = null;

  document.addEventListener("mousedown", (e) => {
    if (e.button !== 0) return;
    if (e.target.closest("button, input, textarea, select, a, [data-tauri-drag-region], .library, .drawer, .modal, .import-menu, .rz")) return;
    dragStart = { x: e.clientX, y: e.clientY };
  });
  document.addEventListener("mousemove", (e) => {
    if (!dragStart) return;
    if (Math.hypot(e.clientX - dragStart.x, e.clientY - dragStart.y) > 5) {
      dragStart = null;
      win.startDragging().catch(() => {});
    }
  });
  document.addEventListener("mouseup", () => {
    dragStart = null;
  });

  document.querySelectorAll(".rz").forEach((el) => {
    el.addEventListener("mousedown", (e) => {
      e.preventDefault();
      e.stopPropagation();
      try {
        win.startResizeDragging(el.dataset.rz);
      } catch (err) {
        console.warn("拉伸窗口失败", err);
      }
    });
  });

  let boundsTimer = null;
  const saveBounds = async () => {
    try {
      const pos = await win.outerPosition();
      const size = await win.outerSize();
      if (!pos || !size) return;
      invoke("save_window_bounds", {
        x: Math.round(pos.x),
        y: Math.round(pos.y),
        w: Math.round(size.width),
        h: Math.round(size.height),
      }).catch(() => {});
    } catch { /* 忽略 */ }
  };
  const queueBounds = () => {
    clearTimeout(boundsTimer);
    boundsTimer = setTimeout(saveBounds, 700);
  };
  window.addEventListener("resize", queueBounds);
  document.addEventListener("mouseup", queueBounds);
  setTimeout(saveBounds, 1200);
}

// ---------- 手机：手指滑动 / 点击翻页 ----------
function setupTouchPaging() {
  const content = $("#content");
  let start = null;
  let lastTap = 0;

  const pageAt = (x, y) => {
    const w = window.innerWidth;
    if (x < w * 0.32) prevPage();
    else if (x > w * 0.68) nextPage();
    else if (state.view === "reader") toggleChrome();
  };

  content.addEventListener(
    "touchstart",
    (e) => {
      if (e.touches.length !== 1) return;
      const t = e.touches[0];
      start = { x: t.clientX, y: t.clientY, time: Date.now() };
    },
    { passive: true }
  );

  content.addEventListener(
    "touchend",
    (e) => {
      if (!start) return;
      const t = e.changedTouches[0];
      const dx = t.clientX - start.x;
      const dy = t.clientY - start.y;
      const moved = Math.max(Math.abs(dx), Math.abs(dy));
      const s = start;
      start = null;
      if (moved >= 40) {
        // 滑动：以主方向为准，上下/左右都能翻
        if (Math.abs(dx) > Math.abs(dy)) dx < 0 ? nextPage() : prevPage();
        else dy < 0 ? nextPage() : prevPage();
        return;
      }
      if (moved > 12) return;
      const now = Date.now();
      if (now - lastTap < 260) return; // 双击不重复翻
      lastTap = now;
      pageAt(t.clientX, t.clientY);
      void s;
    },
    { passive: true }
  );
}

// 手机上把工具栏/状态栏收起来，纯文字更沉浸；轻点屏幕中间再唤出
function toggleChrome() {
  const hidden = document.body.classList.toggle("bare");
  const bar = $(".toolbar");
  const status = $("#statusbar");
  if (bar) bar.hidden = hidden;
  if (status && state.view === "reader") status.hidden = hidden;
  invalidateLayout();
  state.reanchor = true;
  requestAnimationFrame(render);
}

// 安卓返回键由 MainActivity 转进来：能退一层就退一层，返回 true 表示已处理。
window.__backAction = () => {
  if (tocOpen) { setDrawer(false); return true; }
  if (state.desk) { toggleDesk(false); return true; }
  if (state.view === "reader") { showHome(); return true; }
  return false;
};

// ---------- 平台 ----------
async function detectPlatform() {
  let p = "";
  if (runningInTauri) {
    try { p = await invoke("platform"); } catch { p = ""; }
  }
  if (!p) {
    const ua = navigator.userAgent || "";
    p = /Android/i.test(ua) ? "android" : /iPhone|iPad|iPod/i.test(ua) ? "ios" : "desktop";
  }
  state.platform = p;
  state.mobile = p === "android" || p === "ios";
  document.body.dataset.platform = p;

  if (state.mobile) {
    // 手机默认字号大一点、方向保持上下（竖屏一屏一页最自然）
    // 手机第一次打开：默认字号给大一点（手机上 16 偏小）。
    // 用独立标记判断，因为 restoreSettings 已经往 localStorage 写过 fontSize 了。
    let inited = null;
    try { inited = localStorage.getItem("moyu.mobileInit"); } catch { /* 忽略 */ }
    if (!inited) {
      setFont(17);
      setDir("v");
      try { localStorage.setItem("moyu.mobileInit", "1"); } catch { /* 忽略 */ }
    }
    setupTouchPaging();
    // 手机上标题栏/摸鱼模式/窗口热区都没意义
    document.querySelectorAll(".titlebar, .rz, .desk-drag, .desk-exit").forEach((el) => {
      el.style.display = "none";
    });
    const deskBtn = $("#desktopModeBtn");
    if (deskBtn) deskBtn.hidden = true;
  } else {
    setupDesktopWindow();
    setupDesktopDrag();
  }
  requestAnimationFrame(render);
}

// ---------- 启动 ----------
let resizeTimer = null;
window.addEventListener("resize", () => {
  clearTimeout(resizeTimer);
  // 缩放时窗宽/窗高一直在变，重排后原来的页码已经指向别处：按「首行」重新定位
  resizeTimer = setTimeout(() => {
    invalidateLayout();
    state.reanchor = true;
    render();
  }, 90);
});

restoreSettings();
loadLocal(SAMPLE);
// 打开就进书城：有书架就落在书架，没有就落在搜索
showHome();
if (runningInTauri) {
  refreshShelf().then(() => renderHome()).catch(() => {});
}
detectPlatform();

