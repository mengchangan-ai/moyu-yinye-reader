pub mod engine;
pub mod rule;

use rule::Rule;

pub use engine::{download_book, search, SearchResult};

// 内嵌书源规则（编译期包含，免运行时依赖）；id 对应书源编号
pub fn load_rule(id: u32) -> Result<Rule, String> {
    let s = match id {
        1 => include_str!("../rules/xiangshu.json"),
        9 => include_str!("../rules/22biqu.json"),
        _ => return Err(format!("未知书源 id={}", id)),
    };
    serde_json::from_str::<Rule>(s).map_err(|e| e.to_string())
}
