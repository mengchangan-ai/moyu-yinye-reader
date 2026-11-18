use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 书架上的一本书。封面只存远程地址，展示时再转成 data URL，避免把二进制塞进 json。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShelfBook {
    pub id: String,
    pub source_id: u32,
    pub url: String,
    pub title: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub cover: Option<String>,
    #[serde(default)]
    pub added_at: u64,
    #[serde(default)]
    pub last_chapter: Option<usize>,
    #[serde(default)]
    pub last_chapter_title: Option<String>,
    #[serde(default)]
    pub chapters_total: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shelf {
    #[serde(default)]
    pub books: Vec<ShelfBook>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 稳定的短 id：同一本书（同源同 url）永远得到同一个 id，重复加入书架不会出现两条。
pub fn book_id(source_id: u32, url: &str) -> String {
    let mut h = DefaultHasher::new();
    source_id.hash(&mut h);
    url.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn hash_str(s: &str) -> String {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    format!("{:016x}", h.finish())
}

pub fn shelf_path(root: &Path) -> PathBuf {
    root.join("shelf.json")
}

pub fn load_shelf(root: &Path) -> Shelf {
    match std::fs::read(shelf_path(root)) {
        Ok(bytes) => serde_json::from_slice::<Shelf>(&bytes).unwrap_or(Shelf { books: vec![] }),
        Err(_) => Shelf { books: vec![] },
    }
}

pub fn save_shelf(root: &Path, shelf: &Shelf) -> Result<(), String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(shelf).map_err(|e| e.to_string())?;
    // 先写临时文件再改名，避免写一半掉电把书架写坏
    let tmp = root.join("shelf.json.tmp");
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, shelf_path(root)).map_err(|e| e.to_string())
}

/// 一本书的本地缓存目录：appdata/books/<id>/
pub fn book_dir(root: &Path, id: &str) -> PathBuf {
    root.join("books").join(id)
}

pub fn cover_path(root: &Path, url: &str) -> PathBuf {
    root.join("covers").join(format!("{}.img", hash_str(url)))
}

pub fn chapter_path(root: &Path, book: &str, chapter_url: &str) -> PathBuf {
    book_dir(root, book).join(format!("{}.txt", hash_str(chapter_url)))
}

pub fn read_chapter(root: &Path, book: &str, chapter_url: &str) -> Option<String> {
    std::fs::read_to_string(chapter_path(root, book, chapter_url)).ok()
}

pub fn write_chapter(root: &Path, book: &str, chapter_url: &str, text: &str) -> Result<(), String> {
    let dir = book_dir(root, book);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(chapter_path(root, book, chapter_url), text).map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedToc {
    pub title: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub cover: Option<String>,
    pub chapters: Vec<TocItem>,
    pub fetched_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TocItem {
    pub title: String,
    pub url: String,
}

pub fn toc_path(root: &Path, book: &str) -> PathBuf {
    book_dir(root, book).join("toc.json")
}

pub fn read_toc(root: &Path, book: &str) -> Option<CachedToc> {
    let bytes = std::fs::read(toc_path(root, book)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn write_toc(root: &Path, book: &str, toc: &CachedToc) -> Result<(), String> {
    let dir = book_dir(root, book);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec(toc).map_err(|e| e.to_string())?;
    std::fs::write(toc_path(root, book), json).map_err(|e| e.to_string())
}

pub fn cached_chapter_count(root: &Path, book: &str) -> usize {
    std::fs::read_dir(book_dir(root, book))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map(|x| x == "txt").unwrap_or(false))
                .count()
        })
        .unwrap_or(0)
}
