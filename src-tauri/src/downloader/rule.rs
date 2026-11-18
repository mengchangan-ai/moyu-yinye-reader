use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub id: u32,
    pub url: String,
    pub name: String,
    #[serde(default)]
    pub search: Option<SearchRule>,
    #[serde(default)]
    pub toc: Option<TocRule>,
    #[serde(default)]
    pub chapter: Option<ChapterRule>,
    #[serde(default)]
    pub book: Option<BookRule>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BookRule {
    /// 详情页封面图选择器（取 img 的 src / data-src）
    #[serde(default)]
    pub cover: Option<String>,
    /// 详情页书名选择器
    #[serde(default)]
    pub title: Option<String>,
    /// 详情页作者选择器（文本里带「作者：」前缀会自动去掉）
    #[serde(default)]
    pub author: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SearchRule {
    pub url: String,
    #[serde(default = "default_method")]
    pub method: String,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub book_name: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TocRule {
    #[serde(default)]
    pub item: Option<String>,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub pagination: bool,
    #[serde(default)]
    pub next_page: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ChapterRule {
    pub content: String,
    #[serde(default)]
    pub filter_txt: Option<String>,
    #[serde(default)]
    pub filter_tag: Option<String>,
    #[serde(default)]
    pub pagination: bool,
    #[serde(default)]
    pub next_page: Option<String>,
}

fn default_method() -> String {
    "get".to_string()
}
