use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::downloader::{engine, load_rule};
use crate::store;

/// 整本下载任务：多条 worker 抢同一个下标，暂停只是让 worker 空转等待。
pub struct Job {
    pub id: String,
    pub total: usize,
    pub done: AtomicUsize,
    pub next: AtomicUsize,
    pub paused: AtomicBool,
    pub cancelled: AtomicBool,
    pub finished: AtomicBool,
    pub failed: AtomicUsize,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobStatus {
    pub id: String,
    pub done: usize,
    pub total: usize,
    pub failed: usize,
    pub paused: bool,
    pub finished: bool,
}

#[derive(Default)]
pub struct Downloads(pub Mutex<HashMap<String, Arc<Job>>>);

const WORKERS: usize = 4;

fn status(job: &Job) -> JobStatus {
    JobStatus {
        id: job.id.clone(),
        done: job.done.load(Ordering::SeqCst),
        total: job.total,
        failed: job.failed.load(Ordering::SeqCst),
        paused: job.paused.load(Ordering::SeqCst),
        finished: job.finished.load(Ordering::SeqCst),
    }
}

fn emit(app: &AppHandle, job: &Job) {
    let _ = app.emit("download-progress", status(job));
}

#[tauri::command]
pub async fn download_start(
    app: AppHandle,
    state: tauri::State<'_, Downloads>,
    source_id: u32,
    book_url: String,
    chapter_urls: Vec<String>,
) -> Result<JobStatus, String> {
    let rule = load_rule(source_id)?;
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let book = store::book_id(source_id, &book_url);
    let id = format!("{}-{}", book, store::now_ms());

    let total = chapter_urls.len();
    let job = Arc::new(Job {
        id: id.clone(),
        total,
        done: AtomicUsize::new(0),
        next: AtomicUsize::new(0),
        paused: AtomicBool::new(false),
        cancelled: AtomicBool::new(false),
        finished: AtomicBool::new(false),
        failed: AtomicUsize::new(0),
    });
    state
        .0
        .lock()
        .map_err(|_| "下载队列被占用".to_string())?
        .insert(id.clone(), job.clone());

    for _ in 0..WORKERS.min(total.max(1)) {
        let app = app.clone();
        let job = job.clone();
        let rule = rule.clone();
        let root = root.clone();
        let book = book.clone();
        let urls = chapter_urls.clone();
        tokio::spawn(async move {
            loop {
                if job.cancelled.load(Ordering::SeqCst) {
                    return;
                }
                if job.paused.load(Ordering::SeqCst) {
                    tokio::time::sleep(std::time::Duration::from_millis(180)).await;
                    continue;
                }
                let i = job.next.fetch_add(1, Ordering::SeqCst);
                if i >= urls.len() {
                    return;
                }
                let url = urls[i].clone();
                if store::read_chapter(&root, &book, &url).is_some() {
                    job.done.fetch_add(1, Ordering::SeqCst);
                    emit(&app, &job);
                    continue;
                }
                match engine::chapter(&rule, &url).await {
                    Ok(text) => {
                        let _ = store::write_chapter(&root, &book, &url, &text);
                        job.done.fetch_add(1, Ordering::SeqCst);
                    }
                    Err(_) => {
                        job.failed.fetch_add(1, Ordering::SeqCst);
                        job.done.fetch_add(1, Ordering::SeqCst);
                    }
                }
                emit(&app, &job);

                // 全部章节都领完了，最后一个收尾的 worker 标记完成
                if job.next.load(Ordering::SeqCst) >= urls.len()
                    && job.done.load(Ordering::SeqCst) >= urls.len()
                {
                    job.finished.store(true, Ordering::SeqCst);
                    emit(&app, &job);
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            }
        });
    }

    // 已经全有缓存时直接把任务标成完成
    if total == 0 {
        job.finished.store(true, Ordering::SeqCst);
    }
    emit(&app, &job);
    Ok(status(&job))
}

#[tauri::command]
pub fn download_pause(state: tauri::State<'_, Downloads>, id: String) -> Result<(), String> {
    if let Some(job) = state
        .0
        .lock()
        .map_err(|_| "下载队列被占用".to_string())?
        .get(&id)
    {
        job.paused.store(true, Ordering::SeqCst);
    }
    Ok(())
}

#[tauri::command]
pub fn download_resume(state: tauri::State<'_, Downloads>, id: String) -> Result<(), String> {
    if let Some(job) = state
        .0
        .lock()
        .map_err(|_| "下载队列被占用".to_string())?
        .get(&id)
    {
        job.paused.store(false, Ordering::SeqCst);
    }
    Ok(())
}

#[tauri::command]
pub fn download_cancel(state: tauri::State<'_, Downloads>, id: String) -> Result<(), String> {
    let mut guard = state.0.lock().map_err(|_| "下载队列被占用".to_string())?;
    if let Some(job) = guard.get(&id) {
        job.cancelled.store(true, Ordering::SeqCst);
        job.finished.store(true, Ordering::SeqCst);
    }
    guard.remove(&id);
    Ok(())
}

#[tauri::command]
pub fn download_status(
    state: tauri::State<'_, Downloads>,
    id: String,
) -> Result<Option<JobStatus>, String> {
    let guard = state.0.lock().map_err(|_| "下载队列被占用".to_string())?;
    Ok(guard.get(&id).map(|j| status(j)))
}

#[tauri::command]
pub fn download_all(state: tauri::State<'_, Downloads>) -> Result<Vec<JobStatus>, String> {
    let guard = state.0.lock().map_err(|_| "下载队列被占用".to_string())?;
    Ok(guard.values().map(|j| status(j)).collect())
}

/// 从链接抓一篇文章：按常见正文容器找，找不到就挑文本最长的块。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Article {
    pub title: String,
    pub text: String,
}

#[tauri::command]
pub async fn fetch_article(url: String) -> Result<Article, String> {
    use scraper::{Html, Selector};
    let html = engine::fetch_page(&url).await?;
    let doc = Html::parse_document(&html);

    let title = Selector::parse("title")
        .ok()
        .and_then(|s| {
            doc.select(&s)
                .next()
                .map(|e| e.text().collect::<String>().trim().to_string())
        })
        .unwrap_or_else(|| "导入的文章".to_string());

    let candidates = [
        "article",
        "main",
        "#content",
        ".article-content",
        ".article_content",
        ".rich_media_content",
        "#js_content",
        ".post-content",
        ".content",
        "div",
    ];
    let mut best = String::new();
    for sel in candidates {
        let Ok(selector) = Selector::parse(sel) else {
            continue;
        };
        for el in doc.select(&selector) {
            let text = el.text().collect::<Vec<_>>().join("\n");
            let text = tidy(&text);
            if text.chars().count() > best.chars().count() {
                best = text;
            }
        }
        if best.chars().count() > 400 {
            break;
        }
    }
    if best.chars().count() < 50 {
        return Err("没抓到正文，这个页面可能是动态加载的".to_string());
    }
    Ok(Article { title, text: best })
}

fn tidy(raw: &str) -> String {
    let mut out = String::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_string()
}
