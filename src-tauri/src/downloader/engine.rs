use rand::Rng;
use regex::Regex;
use scraper::{ElementRef, Html, Selector};
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Semaphore;
use url::Url;

use crate::downloader::rule::Rule;

pub struct SearchResult {
    pub source_id: u32,
    pub title: String,
    pub author: String,
    pub url: String,
}

const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36";
const MAX_TOC_PAGES: usize = 500;
const MAX_CHAPTER_PAGES: usize = 100;

// 全局复用一个客户端：连接池、DNS 缓存都在阅读时反复用得上。
static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

fn client() -> reqwest::Client {
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .user_agent(UA)
                // 小说源可直连；macOS 系统代理对该 HTTP 站点会返回 502。
                .no_proxy()
                .timeout(std::time::Duration::from_secs(20))
                .connect_timeout(std::time::Duration::from_secs(8))
                .build()
                .expect("构建 HTTP 客户端失败")
        })
        .clone()
}

fn resolve(base: &str, href: &str) -> String {
    if let Ok(u) = Url::parse(href) {
        return u.to_string();
    }
    if let Ok(b) = Url::parse(base) {
        if let Ok(u) = b.join(href) {
            return u.to_string();
        }
    }
    href.to_string()
}

async fn fetch_html(cli: &reqwest::Client, url: &str) -> Result<String, String> {
    cli.get(url)
        .send()
        .await
        .map_err(|e| format!("请求失败 {url}: {e}"))?
        .error_for_status()
        .map_err(|e| format!("页面返回错误 {url}: {e}"))?
        .text()
        .await
        .map_err(|e| format!("读取页面失败 {url}: {e}"))
}

// 翻页规则既可能指向 <a href="...">，也可能指向 <option value="...">。
fn element_url(el: ElementRef<'_>, base: &str) -> Option<String> {
    let raw = el
        .value()
        .attr("href")
        .or_else(|| el.value().attr("value"))?
        .trim();
    if raw.is_empty() || raw == "#" || raw.to_ascii_lowercase().starts_with("javascript:") {
        return None;
    }
    Some(resolve(base, raw))
}

fn page_urls(doc: &Html, selector: &str, base: &str) -> Result<Vec<String>, String> {
    let sel = Selector::parse(selector).map_err(|e| format!("无效的翻页选择器 {selector}: {e}"))?;
    Ok(doc
        .select(&sel)
        .filter_map(|el| element_url(el, base))
        .collect())
}

// 解析 search.data 模板（"{searchkey: %s}" / "{searchkey: %s, Submit: 搜索}"）为表单键值
fn search_form(data: &str, kw: &str) -> Vec<(String, String)> {
    let mut out = vec![];
    let re = Regex::new(r#"([\w]+)\s*:\s*"?([^,}"]+)"?"#).unwrap();
    for c in re.captures_iter(data) {
        let k = c[1].to_string();
        let v = c[2].trim().to_string();
        if v == "%s" {
            out.push((k, kw.to_string()));
        } else {
            out.push((k, v));
        }
    }
    if out.is_empty() {
        out.push(("searchkey".to_string(), kw.to_string()));
    }
    out
}

pub async fn search(rule: &Rule, kw: &str) -> Result<Vec<SearchResult>, String> {
    let s = rule
        .search
        .as_ref()
        .ok_or_else(|| "该源不支持搜索".to_string())?;
    let cli = client();
    let body = s
        .data
        .as_ref()
        .map(|d| search_form(d, kw))
        .unwrap_or_default();
    let request = if s.method.eq_ignore_ascii_case("get") {
        cli.get(&s.url).query(&body)
    } else {
        cli.post(&s.url).form(&body)
    };
    let html = request
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| format!("搜索请求失败: {e}"))?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let doc = Html::parse_document(&html);

    let res_sel =
        Selector::parse(s.result.as_deref().unwrap_or("body")).map_err(|e| e.to_string())?;
    let name_sel =
        Selector::parse(s.book_name.as_deref().unwrap_or("a")).map_err(|e| e.to_string())?;
    let author_sel = s.author.as_deref().and_then(|a| Selector::parse(a).ok());

    let mut out = vec![];
    for row in doc.select(&res_sel) {
        let name_el = row.select(&name_sel).next();
        let title = name_el
            .map(|e| e.text().collect::<String>())
            .unwrap_or_default();
        if title.trim().is_empty() {
            continue;
        }
        let href = name_el.and_then(|e| e.value().attr("href").map(|h| resolve(&rule.url, h)));
        let author = author_sel
            .as_ref()
            .and_then(|sel| row.select(sel).next())
            .map(|e| e.text().collect::<String>())
            .unwrap_or_default();
        if let Some(href) = href {
            out.push(SearchResult {
                source_id: rule.id,
                title: title.trim().to_string(),
                author: author.trim().to_string(),
                url: href,
            });
        }
    }
    Ok(out)
}

/// 抓一个普通网页（导入文章用）
pub async fn fetch_page(url: &str) -> Result<String, String> {
    fetch_html(&client(), url).await
}

/// 目录（复用全局客户端）
pub async fn toc(rule: &Rule, book_url: &str) -> Result<Vec<(String, String)>, String> {
    toc_with_client(&client(), rule, book_url).await
}

/// 单章正文（带重试）
pub async fn chapter(rule: &Rule, chapter_url: &str) -> Result<String, String> {
    chapter_with_retry(&client(), rule, chapter_url).await
}

/// 详情页信息：书名 / 作者 / 封面地址
#[derive(Debug, Clone, Default)]
pub struct BookDetail {
    pub title: Option<String>,
    pub author: Option<String>,
    pub cover: Option<String>,
}

pub async fn book_detail(rule: &Rule, book_url: &str) -> Result<BookDetail, String> {
    let html = fetch_html(&client(), book_url).await?;
    let doc = Html::parse_document(&html);
    let pick = |selector: Option<&str>| -> Option<String> {
        let sel = Selector::parse(selector?).ok()?;
        let text = doc.select(&sel).next()?.text().collect::<String>();
        let text = text.trim().to_string();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    };

    let (title_sel, author_sel, cover_sel) = match rule.book.as_ref() {
        Some(b) => (b.title.as_deref(), b.author.as_deref(), b.cover.as_deref()),
        None => (None, None, None),
    };

    let author = pick(author_sel).map(|text| {
        // 「作    者：天蚕土豆」→「天蚕土豆」
        match text.rsplit_once('：').or_else(|| text.rsplit_once(':')) {
            Some((_, name)) if !name.trim().is_empty() => name.trim().to_string(),
            _ => text,
        }
    });

    let cover = cover_sel.and_then(|selector| {
        let sel = Selector::parse(selector).ok()?;
        let el = doc.select(&sel).next()?;
        el.value()
            .attr("src")
            .or_else(|| el.value().attr("data-src"))
            .map(|s| resolve(book_url, s.trim()))
    });

    Ok(BookDetail {
        title: pick(title_sel),
        author,
        cover,
    })
}

/// 下载图片字节（封面用；同样直连）
pub async fn fetch_cover_bytes(url: &str) -> Result<Vec<u8>, String> {
    let resp = client()
        .get(url)
        .send()
        .await
        .map_err(|e| format!("封面请求失败: {e}"))?
        .error_for_status()
        .map_err(|e| format!("封面返回错误: {e}"))?;
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("封面文件过大".to_string());
    }
    Ok(bytes.to_vec())
}

async fn toc_with_client(
    cli: &reqwest::Client,
    rule: &Rule,
    book_url: &str,
) -> Result<Vec<(String, String)>, String> {
    let t = rule.toc.as_ref().ok_or_else(|| "无目录规则".to_string())?;
    let item_sel = Selector::parse(t.item.as_deref().unwrap_or("a")).map_err(|e| e.to_string())?;
    let offset = t.offset.unwrap_or(0);
    let next_selector = if t.pagination {
        Some(
            t.next_page
                .as_deref()
                .ok_or_else(|| "目录启用了翻页，但未配置 nextPage".to_string())?,
        )
    } else {
        None
    };

    let mut out = vec![];
    let mut chapter_urls = HashSet::new();
    let mut visited = HashSet::new();
    let mut scheduled = HashSet::from([book_url.to_string()]);
    let mut pending = VecDeque::from([book_url.to_string()]);
    let mut item_index = 0;

    while let Some(page_url) = pending.pop_front() {
        if !visited.insert(page_url.clone()) {
            continue;
        }
        if visited.len() > MAX_TOC_PAGES {
            return Err(format!("目录翻页超过安全上限 {MAX_TOC_PAGES} 页"));
        }

        let html = fetch_html(cli, &page_url).await?;
        let doc = Html::parse_document(&html);
        for a in doc.select(&item_sel) {
            if item_index < offset {
                item_index += 1;
                continue;
            }
            item_index += 1;
            let title = a.text().collect::<String>();
            if let Some(href) = a.value().attr("href") {
                let chapter_url = resolve(&page_url, href);
                if chapter_urls.insert(chapter_url.clone()) {
                    out.push((title.trim().to_string(), chapter_url));
                }
            }
        }

        if let Some(selector) = next_selector {
            for url in page_urls(&doc, selector, &page_url)? {
                if scheduled.insert(url.clone()) {
                    pending.push_back(url);
                }
            }
        }
    }
    Ok(out)
}

async fn chapter_with_client(
    cli: &reqwest::Client,
    rule: &Rule,
    chap_url: &str,
) -> Result<String, String> {
    let c = rule
        .chapter
        .as_ref()
        .ok_or_else(|| "无正文规则".to_string())?;
    let sel = Selector::parse(&c.content).map_err(|e| e.to_string())?;
    let next_selector = if c.pagination {
        Some(
            c.next_page
                .as_deref()
                .ok_or_else(|| "正文启用了翻页，但未配置 nextPage".to_string())?,
        )
    } else {
        None
    };
    let mut parts = vec![];
    let mut visited = HashSet::new();
    let mut scheduled = HashSet::from([chap_url.to_string()]);
    let mut pending = VecDeque::from([chap_url.to_string()]);

    while let Some(page_url) = pending.pop_front() {
        if !visited.insert(page_url.clone()) {
            continue;
        }
        if visited.len() > MAX_CHAPTER_PAGES {
            return Err(format!("正文翻页超过安全上限 {MAX_CHAPTER_PAGES} 页"));
        }

        let html = fetch_html(cli, &page_url).await?;
        let doc = Html::parse_document(&html);
        let el = doc
            .select(&sel)
            .next()
            .ok_or_else(|| format!("未找到正文: {page_url}"))?;
        let content = clean(&el.inner_html(), c);
        if !content.is_empty() {
            parts.push(content);
        }

        if let Some(selector) = next_selector {
            for url in page_urls(&doc, selector, &page_url)? {
                if scheduled.insert(url.clone()) {
                    pending.push_back(url);
                }
            }
        }
    }

    Ok(parts.join("\n"))
}

async fn chapter_with_retry(
    cli: &reqwest::Client,
    rule: &Rule,
    chap_url: &str,
) -> Result<String, String> {
    let mut last_error = String::new();
    for attempt in 1..=3 {
        match chapter_with_client(cli, rule, chap_url).await {
            Ok(content) => return Ok(content),
            Err(error) => {
                last_error = error;
                if attempt < 3 {
                    tokio::time::sleep(std::time::Duration::from_millis(300 * attempt)).await;
                }
            }
        }
    }
    Err(last_error)
}

// 正文清洗：删广告标签 → <br>/</p> 转换行 → 去标签 → 按行去广告正则
fn clean(html: &str, c: &crate::downloader::rule::ChapterRule) -> String {
    let mut s = html.to_string();

    if let Some(ft) = &c.filter_tag {
        for tag in ft.split_whitespace() {
            let re = Regex::new(&format!(r#"<{0}[^>]*>.*?</{0}>"#, tag)).unwrap();
            s = re.replace_all(&s, "").to_string();
            let re2 = Regex::new(&format!(r#"<{0}[^>]*/>"#, tag)).unwrap();
            s = re2.replace_all(&s, "").to_string();
        }
    }

    let br_re = Regex::new(r#"<br\s*/?>"#).unwrap();
    s = br_re.replace_all(&s, "\n").to_string();
    let p_re = Regex::new(r#"</p>"#).unwrap();
    s = p_re.replace_all(&s, "\n").to_string();

    let tag_re = Regex::new(r#"<[^>]+>"#).unwrap();
    s = tag_re.replace_all(&s, "").to_string();
    s = decode_entities(&s);

    let filter = c.filter_txt.as_ref().and_then(|f| Regex::new(f).ok());
    let mut lines: Vec<String> = s
        .split('\n')
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if let Some(fr) = &filter {
        lines.retain(|l| !fr.is_match(l));
    }
    lines.join("\n")
}

fn decode_entities(s: &str) -> String {
    let s = s
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&nbsp;", " ")
        .replace("&#39;", "'");
    let num = Regex::new(r"&#(\d+);").unwrap();
    let hex = Regex::new(r"&#x([0-9a-fA-F]+);").unwrap();
    let s = num.replace_all(&s, |c: &regex::Captures| {
        let n: u32 = c[1].parse().unwrap_or(0);
        std::char::from_u32(n)
            .map(|c| c.to_string())
            .unwrap_or_default()
    });
    let s = hex.replace_all(&s, |c: &regex::Captures| {
        let n = u32::from_str_radix(&c[1], 16).unwrap_or(0);
        std::char::from_u32(n)
            .map(|c| c.to_string())
            .unwrap_or_default()
    });
    s.to_string()
}

// 下载整本：并发抓取各章，每完成一章通过 on_progress(done, total) 回传进度
pub async fn download_book(
    rule: &Rule,
    book_url: &str,
    out_path: &str,
    on_progress: Arc<dyn Fn(usize, usize) + Send + Sync>,
) -> Result<(), String> {
    let cli = client();
    let links = toc_with_client(&cli, rule, book_url).await?;
    let total = links.len();
    if total == 0 {
        return Err("目录为空".to_string());
    }
    let sem = Arc::new(Semaphore::new(4));
    let done = Arc::new(AtomicUsize::new(0));
    let mut handles = vec![];

    for (i, (title, url)) in links.iter().enumerate() {
        let permit = sem.clone().acquire_owned().await.unwrap();
        let rule = rule.clone();
        let title = title.clone();
        let url = url.clone();
        let done = done.clone();
        let on_progress = on_progress.clone();
        let cli = cli.clone();
        // 随机延迟在 spawn 之前算好：rand::thread_rng 不是 Send，不能跨 await 留在 future 里
        let delay = {
            let mut rng = rand::thread_rng();
            rng.gen_range(200u64..400u64)
        };
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            let _ = tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            let content = chapter_with_retry(&cli, &rule, &url).await;
            let n = done.fetch_add(1, Ordering::SeqCst) + 1;
            on_progress(n, total);
            (i, title, content)
        }));
    }

    let mut chapters: Vec<(usize, String, String)> = vec![];
    let mut failures = vec![];
    for h in handles {
        match h.await {
            Ok((i, title, Ok(content))) => chapters.push((i, title, content)),
            Ok((_, title, Err(error))) => failures.push(format!("{title}: {error}")),
            Err(error) => failures.push(format!("下载任务异常: {error}")),
        }
    }
    if !failures.is_empty() {
        let preview = failures
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join("；");
        return Err(format!(
            "书源“{}”有 {} 章下载失败（已自动重试）：{}",
            rule.name,
            failures.len(),
            preview
        ));
    }
    chapters.sort_by_key(|c| c.0);

    let mut out = String::new();
    for (_, title, content) in &chapters {
        out.push_str(&format!("\n\n{}\n\n", title));
        out.push_str(content);
    }
    tokio::fs::write(out_path, out)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn mock_site(pages: HashMap<&'static str, &'static str>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for _ in 0..pages.len() {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0; 4096];
                let size = socket.read(&mut request).await.unwrap();
                let first_line = String::from_utf8_lossy(&request[..size])
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                let path = first_line.split_whitespace().nth(1).unwrap_or("/");
                let (status, body) = pages
                    .get(path)
                    .map(|body| ("200 OK", *body))
                    .unwrap_or(("404 Not Found", "missing"));
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        format!("http://{address}")
    }

    #[test]
    fn pagination_accepts_anchor_and_option_urls() {
        let doc = Html::parse_document(
            r#"<a class="next" href="page-2.html">下一页</a>
               <select><option value="/book/page-3.html">3</option></select>"#,
        );
        assert_eq!(
            page_urls(
                &doc,
                "a.next, option",
                "https://example.com/book/page-1.html"
            )
            .unwrap(),
            vec![
                "https://example.com/book/page-2.html",
                "https://example.com/book/page-3.html",
            ]
        );
    }

    #[test]
    fn pagination_ignores_non_navigation_values() {
        let doc = Html::parse_document(
            r##"<a href="#">空</a><a href="javascript:void(0)">脚本</a><option value="">空</option>"##,
        );
        assert!(page_urls(&doc, "a, option", "https://example.com/")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn search_form_parses_quoted_and_unquoted_values() {
        assert_eq!(
            search_form(r#"{searchkey: "%s", Submit: 搜索}"#, "斗破苍穹"),
            vec![
                ("searchkey".to_string(), "斗破苍穹".to_string()),
                ("Submit".to_string(), "搜索".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn traverses_paginated_toc_and_chapter() {
        let base = mock_site(HashMap::from([
            (
                "/book/index.html",
                r#"<div class="toc"><a href="c1.html">第一章</a></div>
                   <select class="pages"><option value="index.html">1</option><option value="page2.html">2</option></select>"#,
            ),
            (
                "/book/page2.html",
                r#"<div class="toc"><a href="c2.html">第二章</a></div>
                   <select class="pages"><option value="index.html">1</option><option value="page2.html">2</option></select>"#,
            ),
            (
                "/book/c1.html",
                r#"<div id="content">正文一<br>上半页</div><a id="next" href="c1-2.html">下一页</a>"#,
            ),
            (
                "/book/c1-2.html",
                r#"<div id="content">正文一<br>下半页</div>"#,
            ),
        ]))
        .await;
        let rule: Rule = serde_json::from_value(serde_json::json!({
            "id": 99,
            "url": base,
            "name": "测试源",
            "toc": {
                "item": ".toc a",
                "pagination": true,
                "nextPage": ".pages option"
            },
            "chapter": {
                "content": "#content",
                "pagination": true,
                "nextPage": "#next"
            }
        }))
        .unwrap();
        let cli = client();
        let book_url = format!("{base}/book/index.html");
        let links = toc_with_client(&cli, &rule, &book_url).await.unwrap();
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].0, "第一章");
        assert_eq!(links[1].0, "第二章");

        let content = chapter_with_client(&cli, &rule, &links[0].1).await.unwrap();
        assert_eq!(content, "正文一\n上半页\n正文一\n下半页");
    }

    // 联网用例：验证真实书源的目录 / 正文 / 详情页选择器是否还对得上
    // 跑法：cargo test --bin moyu-reader live_xiangshu -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_xiangshu_toc_chapter_and_cover() {
        let rule: Rule = serde_json::from_str(include_str!("../rules/xiangshu.json")).unwrap();
        let book = "http://www.ibiqugu.net/7/7877/";

        let chapters = super::toc(&rule, book).await.expect("目录抓取失败");
        assert!(chapters.len() > 10, "目录条数太少: {}", chapters.len());

        let (title, url) = chapters.first().unwrap();
        let text = super::chapter(&rule, url).await.expect("正文抓取失败");
        assert!(text.chars().count() > 100, "正文太短");

        let detail = super::book_detail(&rule, book)
            .await
            .expect("详情页抓取失败");
        assert!(detail.cover.is_some(), "没拿到封面");
        assert!(detail.title.is_some(), "没拿到书名");

        println!(
            "章节数={} 首章={} 正文字数={} 书名={:?} 作者={:?} 封面={:?}",
            chapters.len(),
            title,
            text.chars().count(),
            detail.title,
            detail.author,
            detail.cover
        );
    }
}
