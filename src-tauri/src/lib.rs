mod downloader;
mod downloads;
mod store;

use std::path::PathBuf;
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use downloader::download_book as engine_download;
use downloader::engine;
use downloader::{load_rule, search};
use serde::Serialize;
use store::{ShelfBook, TocItem};
use tauri::Emitter;
use tauri::Manager;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BookHit {
    source_id: u32,
    title: String,
    author: String,
    url: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenBook {
    title: String,
    author: String,
    cover: Option<String>,
    chapters: Vec<TocItem>,
    cached_chapters: usize,
    shelf_id: Option<String>,
}

/// 记住/恢复窗口大小与位置（无边框窗口靠它省得每次重调）
fn bounds_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|d| d.join("window.json"))
}

fn restore_bounds(app: &tauri::AppHandle) {
    let Some(path) = bounds_path(app) else { return };
    let Ok(raw) = std::fs::read(&path) else {
        return;
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) else {
        return;
    };
    let (Some(x), Some(y), Some(w), Some(h)) = (
        v["x"].as_i64(),
        v["y"].as_i64(),
        v["w"].as_u64(),
        v["h"].as_u64(),
    ) else {
        return;
    };
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.set_size(tauri::PhysicalSize::new(w as u32, h as u32));
        let _ = win.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
    }
}

#[tauri::command]
fn save_window_bounds(app: tauri::AppHandle, x: i32, y: i32, w: u32, h: u32) -> Result<(), String> {
    let Some(path) = bounds_path(&app) else {
        return Ok(());
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let json = serde_json::json!({ "x": x, "y": y, "w": w, "h": h });
    std::fs::write(path, json.to_string()).map_err(|e| e.to_string())
}

fn data_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path().app_data_dir().map_err(|e| e.to_string())
}

fn data_url(bytes: &[u8]) -> String {
    let mime = if bytes.starts_with(&[0xFF, 0xD8]) {
        "image/jpeg"
    } else if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        "image/png"
    } else if bytes.starts_with(b"GIF8") {
        "image/gif"
    } else if bytes.len() > 12 && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "image/jpeg"
    };
    format!("data:{};base64,{}", mime, STANDARD.encode(bytes))
}

#[tauri::command]
async fn search_books(keyword: String, source_id: Option<u32>) -> Result<Vec<BookHit>, String> {
    let mut hits: Vec<downloader::SearchResult> = vec![];
    let mut errors = vec![];
    // 22biqu.com 已变为域名停放页；规则保留作多页解析回归样例，但不再作为在线源搜索。
    for id in [1u32] {
        if source_id.is_some_and(|selected| selected != id) {
            continue;
        }
        let rule = load_rule(id)?;
        match search(&rule, &keyword).await {
            Ok(mut results) => hits.append(&mut results),
            Err(error) => errors.push(format!("{}: {}", rule.name, error)),
        }
    }
    if hits.is_empty() && !errors.is_empty() {
        return Err(errors.join("；"));
    }
    Ok(hits
        .into_iter()
        .map(|h| BookHit {
            source_id: h.source_id,
            title: h.title,
            author: h.author,
            url: h.url,
        })
        .collect())
}

/// 粗读图片尺寸：书源里混着分享按钮之类的小图，太小就当没有封面。
fn image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() > 24 && bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        return Some((w, h));
    }
    if bytes.len() > 10 && bytes.starts_with(b"GIF8") {
        let w = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
        let h = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
        return Some((w, h));
    }
    if bytes.len() > 4 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        let mut i = 2usize;
        while i + 9 < bytes.len() {
            if bytes[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = bytes[i + 1];
            let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
            if marker >= 0xC0
                && marker <= 0xCF
                && marker != 0xC4
                && marker != 0xC8
                && marker != 0xCC
            {
                let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
                let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
                return Some((w, h));
            }
            i += 2 + len;
        }
    }
    None
}

fn usable_cover(bytes: &[u8]) -> bool {
    match image_size(bytes) {
        Some((w, h)) => w >= 50 && h >= 60,
        None => bytes.len() > 2048,
    }
}

/// 把远程封面落盘缓存并转 data URL（同一张图只下一次）
async fn cached_cover(root: &std::path::Path, url: &str) -> Option<String> {
    if url.trim().is_empty() {
        return None;
    }
    let path = store::cover_path(root, url);
    if let Ok(bytes) = std::fs::read(&path) {
        if usable_cover(&bytes) {
            return Some(data_url(&bytes));
        }
        let _ = std::fs::remove_file(&path);
    }
    match engine::fetch_cover_bytes(url).await {
        Ok(bytes) => {
            if !usable_cover(&bytes) {
                return None;
            }
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, &bytes);
            Some(data_url(&bytes))
        }
        Err(_) => None,
    }
}

#[tauri::command]
async fn cover_data(app: tauri::AppHandle, url: String) -> Result<Option<String>, String> {
    let root = data_dir(&app)?;
    Ok(cached_cover(&root, &url).await)
}

/// 搜索结果没有封面：现场读一次详情页拿封面图，再转 data URL。
#[tauri::command]
async fn book_cover(
    app: tauri::AppHandle,
    source_id: u32,
    book_url: String,
) -> Result<Option<String>, String> {
    let root = data_dir(&app)?;
    let rule = load_rule(source_id)?;
    let detail = match engine::book_detail(&rule, &book_url).await {
        Ok(d) => d,
        Err(_) => return Ok(None),
    };
    match detail.cover {
        Some(url) => Ok(cached_cover(&root, &url).await),
        None => Ok(None),
    }
}

#[tauri::command]
async fn shelf_list(app: tauri::AppHandle) -> Result<Vec<ShelfBook>, String> {
    let root = data_dir(&app)?;
    Ok(store::load_shelf(&root).books)
}

#[tauri::command]
async fn shelf_add(
    app: tauri::AppHandle,
    source_id: u32,
    url: String,
    title: String,
    author: String,
    cover: Option<String>,
) -> Result<ShelfBook, String> {
    let root = data_dir(&app)?;
    let mut shelf = store::load_shelf(&root);
    let id = store::book_id(source_id, &url);
    if let Some(existing) = shelf.books.iter().find(|b| b.id == id) {
        return Ok(existing.clone());
    }

    let mut book = ShelfBook {
        id,
        source_id,
        url,
        title,
        author,
        cover,
        added_at: store::now_ms(),
        last_chapter: None,
        last_chapter_title: None,
        chapters_total: None,
    };

    // 搜索结果没有封面，加书架时顺手抓一次详情页
    if book.cover.is_none() || book.title.trim().is_empty() {
        if let Ok(rule) = load_rule(source_id) {
            if let Ok(detail) = engine::book_detail(&rule, &book.url).await {
                if book.cover.is_none() {
                    book.cover = detail.cover;
                }
                if book.title.trim().is_empty() {
                    book.title = detail.title.unwrap_or_else(|| "未知书名".to_string());
                }
                if book.author.trim().is_empty() {
                    book.author = detail.author.unwrap_or_default();
                }
            }
        }
    }

    shelf.books.insert(0, book.clone());
    store::save_shelf(&root, &shelf)?;
    Ok(book)
}

#[tauri::command]
async fn shelf_remove(app: tauri::AppHandle, id: String) -> Result<(), String> {
    let root = data_dir(&app)?;
    let mut shelf = store::load_shelf(&root);
    shelf.books.retain(|b| b.id != id);
    store::save_shelf(&root, &shelf)
}

#[tauri::command]
async fn shelf_progress(
    app: tauri::AppHandle,
    id: String,
    chapter: usize,
    chapter_title: String,
    chapters_total: Option<usize>,
) -> Result<(), String> {
    let root = data_dir(&app)?;
    let mut shelf = store::load_shelf(&root);
    if let Some(book) = shelf.books.iter_mut().find(|b| b.id == id) {
        book.last_chapter = Some(chapter);
        book.last_chapter_title = Some(chapter_title);
        if chapters_total.is_some() {
            book.chapters_total = chapters_total;
        }
        store::save_shelf(&root, &shelf)?;
    }
    Ok(())
}

/// 打开一本书：目录（带缓存）+ 基本信息和已缓存章节数
#[tauri::command]
async fn open_book(
    app: tauri::AppHandle,
    source_id: u32,
    book_url: String,
) -> Result<OpenBook, String> {
    let root = data_dir(&app)?;
    let id = store::book_id(source_id, &book_url);
    let rule = load_rule(source_id)?;

    let mut toc = store::read_toc(&root, &id).filter(|t| !t.chapters.is_empty());
    let stale = toc
        .as_ref()
        .map(|t| store::now_ms().saturating_sub(t.fetched_at) > 24 * 3600 * 1000)
        .unwrap_or(true);

    if toc.is_none() || stale {
        let chapters = engine::toc(&rule, &book_url).await?;
        if chapters.is_empty() {
            return Err("目录是空的，书源可能已失效".to_string());
        }
        let detail = engine::book_detail(&rule, &book_url)
            .await
            .unwrap_or_default();
        let fresh = store::CachedToc {
            title: detail.title.unwrap_or_else(|| "未知书名".to_string()),
            author: detail.author.unwrap_or_default(),
            cover: detail.cover,
            chapters: chapters
                .into_iter()
                .map(|(title, url)| TocItem { title, url })
                .collect(),
            fetched_at: store::now_ms(),
        };
        store::write_toc(&root, &id, &fresh)?;
        toc = Some(fresh);
    }
    let toc = toc.ok_or_else(|| "目录读取失败".to_string())?;

    let shelf = store::load_shelf(&root);
    let shelf_id = shelf
        .books
        .iter()
        .find(|b| b.id == id)
        .map(|b| b.id.clone());

    Ok(OpenBook {
        title: toc.title,
        author: toc.author,
        cover: toc.cover,
        chapters: toc.chapters,
        cached_chapters: store::cached_chapter_count(&root, &id),
        shelf_id,
    })
}

/// 读一章：本地有缓存直接返回，没有就抓下来同时写缓存（边看边缓存）
#[tauri::command]
async fn load_chapter(
    app: tauri::AppHandle,
    source_id: u32,
    book_url: String,
    chapter_url: String,
) -> Result<String, String> {
    let root = data_dir(&app)?;
    let id = store::book_id(source_id, &book_url);
    if let Some(text) = store::read_chapter(&root, &id, &chapter_url) {
        return Ok(text);
    }
    let rule = load_rule(source_id)?;
    let text = engine::chapter(&rule, &chapter_url).await?;
    let _ = store::write_chapter(&root, &id, &chapter_url, &text);
    Ok(text)
}

/// 预取后面几章，翻页时就不用等；失败的静默跳过。
#[tauri::command]
async fn prefetch_chapters(
    app: tauri::AppHandle,
    source_id: u32,
    book_url: String,
    chapter_urls: Vec<String>,
) -> Result<usize, String> {
    let root = data_dir(&app)?;
    let id = store::book_id(source_id, &book_url);
    let rule = load_rule(source_id)?;
    let mut done = 0usize;
    for url in chapter_urls.into_iter().take(5) {
        // 已经有缓存的不重复计数，前端用它累加「已缓存」
        if store::read_chapter(&root, &id, &url).is_some() {
            continue;
        }
        if let Ok(text) = engine::chapter(&rule, &url).await {
            let _ = store::write_chapter(&root, &id, &url, &text);
            done += 1;
        }
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    }
    Ok(done)
}

#[tauri::command]
async fn download_book(
    app: tauri::AppHandle,
    source_id: u32,
    book_url: String,
    book_name: String,
) -> Result<String, String> {
    let rule = load_rule(source_id)?;
    // 保存到 ~/Downloads/摸鱼小说/书名.txt，方便下次直接打开
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let dir = std::path::Path::new(&home)
        .join("Downloads")
        .join("摸鱼小说");
    let _ = tokio::fs::create_dir_all(&dir).await;
    let safe = sanitize(&book_name);
    let path = dir.join(format!("{}.txt", safe));

    let app2 = app.clone();
    let on_progress: Arc<dyn Fn(usize, usize) + Send + Sync> = Arc::new(move |done, total| {
        let _ = app2.emit(
            "download-progress",
            serde_json::json!({ "done": done, "total": total }),
        );
    });

    engine_download(&rule, &book_url, path.to_str().unwrap(), on_progress).await?;
    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
async fn read_file(path: String) -> Result<String, String> {
    tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| e.to_string())
}

/// 桌面文字模式：只留透明窗口里的文字。
///
/// 这里**不再**给窗口加 macOS 的 Sidebar 毛玻璃材质。vibrancy 是一层
/// 半透明的模糊底板，正文压在上面会发虚、发白（看起来像字在发光，
/// 对比度也掉一截）。现在普通模式用 CSS 的不透明米色纸面，摸鱼模式
/// 干脆全透明，两种模式都不需要这层材质。
#[tauri::command]
fn set_desktop_mode(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        if let Some(window) = app.get_webview_window("main") {
            window
                .set_effects(None::<tauri::utils::config::WindowEffectsConfig>)
                .map_err(|e| e.to_string())?;
            let _ = window.set_shadow(!enabled);
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, enabled);
    Ok(())
}

// 清洗文件名中的非法字符（/ \ : * ? " < > |）
fn sanitize(name: &str) -> String {
    let bad: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|'];
    name.chars()
        .map(|c| if bad.contains(&c) { '_' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

/// 当前平台：macos / windows / linux / android / ios。前端据此决定用桌面还是触屏交互。
#[tauri::command]
fn platform() -> &'static str {
    std::env::consts::OS
}

/// 桌面直接启动；安卓由 tauri 的 mobile_entry_point 调进同一个 run()。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();

    // 老板键只在桌面上有意义；安卓没有全局快捷键，插件也不该注册。
    #[cfg(desktop)]
    let builder = builder.plugin(
        tauri_plugin_global_shortcut::Builder::new()
            .with_shortcuts(["cmdorctrl+Backquote", "alt+Backquote"])
            .unwrap()
            .with_handler(|app, _shortcut, event| {
                if event.state == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                    if let Some(w) = app.get_webview_window("main") {
                        if w.is_visible().unwrap_or(false) {
                            let _ = w.hide();
                        } else {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                }
            })
            .build(),
    );

    builder
        .manage(downloads::Downloads::default())
        .invoke_handler(tauri::generate_handler![
            platform,
            search_books,
            cover_data,
            book_cover,
            shelf_list,
            shelf_add,
            shelf_remove,
            shelf_progress,
            open_book,
            load_chapter,
            prefetch_chapters,
            download_book,
            read_file,
            set_desktop_mode,
            downloads::download_start,
            downloads::download_pause,
            downloads::download_resume,
            downloads::download_cancel,
            downloads::download_status,
            downloads::download_all,
            downloads::fetch_article,
            save_window_bounds
        ])
        .setup(|app| {
            // 启动时不挂 macOS 毛玻璃：那层半透明底板会让正文发虚/发光。
            // 普通模式的纸面底色由 CSS 画，摸鱼模式要的是纯透明。
            #[cfg(target_os = "macos")]
            {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.set_effects(None::<tauri::utils::config::WindowEffectsConfig>);
                }
            }
            restore_bounds(app.handle());
            // 老板键已在插件注册：Cmd + ` 一键隐藏/显示
            let _ = app;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod cover_tests {
    use super::{image_size, usable_cover};

    fn jpeg_header(w: u16, h: u16) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        v.extend_from_slice(&[0u8; 14]); // APP0 负载
        v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&[0x03, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        v
    }

    #[test]
    fn reads_jpeg_and_png_dimensions() {
        assert_eq!(image_size(&jpeg_header(120, 150)), Some((120, 150)));
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&96u32.to_be_bytes());
        png.extend_from_slice(&128u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        assert_eq!(image_size(&png), Some((96, 128)));
    }

    #[test]
    fn rejects_share_button_sized_images() {
        assert!(!usable_cover(&jpeg_header(155, 24)));
        assert!(usable_cover(&jpeg_header(120, 150)));
    }
}
