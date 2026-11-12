//! 读手机页 `v.m.chenzhongtech.com/fw/photo/<id>`（手机 UA）里的数据。
//!
//! 数据在 `<script>window.INIT_STATE = {…}</script>` 里：一个对象，键是被移位过的接口名（不去解它），
//! 值里有一项同时带 `result` 和 `photo`，作品就在那一项的 `photo` 里。
//!
//! 来源（MIT 许可证）：
//! - 移植 wujunwei928/parse-video `parser/kuaishou.go` 的页面解析（`INIT_STATE`、`result`、`photo`、
//!   `caption`、`mainMvUrls`、`coverUrls`、`ext_params.atlas`）。MIT License，Copyright (c) 2022 wujunwei928。
//! - 字段名和图集的几种写法对照 z-mio/ParseHub `src/parsehub/provider_api/kuaishou.py` 的页面解析。
//!   MIT License，Copyright (c) 2024 梓澪。
//!
//! 许可证全文在上面两个项目的仓库里，两份的版权声明也写在 README 末行。
//! 没有移植：ParseHub 的 GraphQL 接口和它自带的 cookie、桌面页的 `__APOLLO_STATE__`（量到不带作品）、
//! `/fw/long-video/` 改写成 `/fw/photo/`（这里的请求本来就走 `fw/photo`）。
//!
//! 2026-11-12 对着真实页面量过，三个请求，都不带 cookie：
//! - 桌面 UA 取 `www.kuaishou.com/short-video/<id>`：回 200，页面是个壳，只有 `__APOLLO_STATE__`
//!   （站点配置：`visionConfig`、`visionStartup`），没有作品，也没有 `INIT_STATE`；
//! - 桌面 UA 取 `v.kuaishou.com/<短码>`（parse-video 的 issue #57 里贴的）：302 到
//!   `https://v.m.chenzhongtech.com/fw/photo/<id>?cc=share_copylink&…&shareToken=…`；
//! - 手机 UA 取那个落地地址：回 200，`INIT_STATE` 在，里面五项都带 `result: 1`，其中一项有 `photo`、`counts`、`fid`：
//!   `photo.caption`、`userName`、`userEid`（路径里的 id 那种字符串）、`userId`（数字）、`timestamp`（毫秒）、
//!   `duration`（毫秒）、`likeCount`、`commentCount`、`forwardCount`、`viewCount`、`mainMvUrls[].url`（`*.kwimgs.com`）、
//!   `coverUrls[].url`（`*.yximgs.com`）、`photoType: "VIDEO"`、`share_info`（含 `photoId=<路径里的 id>`），
//!   `photo.photoId` 是十九位数字，和路径里的 id 不是一个东西。JSON 严格模式能读。
//!
//! 同一天经宿主（`usuyo add`）又跑了四次，都不带 cookie：
//! - `v.kuaishou.com/<码>`（上面那条短链）：展开成 `fw/photo/<id>?<分享参数>`，再请求它：有作品数据，视频、封面都下下来了；
//! - `www.kuaishou.com/short-video/<id>`（没有分享参数）：直接请求 `fw/photo/<id>`：有作品数据，和短链是同一条；
//! - `www.kuaishou.com/f/<码>`：展开后宿主记下的链接是 `www.kuaishou.com/short-video/<id>?cc=share_wxms&kpf=PC_WEB&…`，
//!   再请求 `fw/photo/<id>`（带着这些参数）：有作品数据；
//! - parse-video 的 issue #46 里的 `short-video/<id>`：`INIT_STATE` 的一项是 `{"result":223,"error_msg":"获取失败，作品可能已被删除或尚未上传",…}`，
//!   没有 `photo`；页面文字是「找不到该作品，为你推荐了热门作品」。这就是下面认的「作品不可用」。
//!   宿主随后换通用网页插件存了桌面页的 HTML，那份不是这个插件取的。
//!
//! 视频地址在 `*.kwimgs.com` 或 `*.kwaicdn.com`，封面在 `*.yximgs.com`。
//!
//! 没有量过、照上游源码写的：图文的 `ext_params.atlas`（`cdn[]`、`list[]`）、
//! `result` 为 20113（ParseHub 的 PR #167 和 223 一起当「作品已被删除」）、`result` 的文字说私密时的写法、
//! 验证页（`result` 2001 带 `captchaConfig`，parse-video 的 issue #36；整页是 `{"result":2}` 的 JSON，
//! ParseHub 的 `_is_blocked_payload`）。测试夹具里这几种是手写的，内容是编的。

use serde_json::{Map, Value, json};
use usuyo_plugin_kit::protocol::NotFoundReason;

/// 手机页上读出的一个作品。
#[derive(Debug, Clone)]
pub struct Post {
    pub caption: String,
    pub author: Option<AuthorInfo>,
    /// 发布时间，Unix 毫秒。
    pub published_ms: Option<i64>,
    pub cover_url: Option<String>,
    /// 图文的图片地址，一张一项；视频作品是空的。
    pub images: Vec<String>,
    /// 视频的地址，照页面给的 `photo.mainMvUrls[0].url`。
    pub video: Option<VideoInfo>,
    /// 写进 `meta.json` 的计数。
    pub counts: Map<String, Value>,
    /// 页面数据里的数字 id（`photo.photoId`），写进 `meta.json`。
    pub numeric_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AuthorInfo {
    pub name: String,
    /// `photo.userEid`：个人主页链接里的那一段。
    pub eid: Option<String>,
    /// `photo.userId`：数字。
    pub user_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct VideoInfo {
    pub url: String,
    pub duration_sec: Option<f64>,
    pub width: Option<u64>,
    pub height: Option<u64>,
}

impl Post {
    /// 有 `images` 的是图文，没有视频文件。
    pub fn is_image_post(&self) -> bool {
        !self.images.is_empty()
    }
}

/// 读不出作品的几种情况，每种带一段给失败日志的原文开头。
#[derive(Debug)]
pub enum PageError {
    /// 页面像验证页。
    Verification { detail: String },
    /// 页面说这条看不到。
    Unavailable {
        reason: Option<NotFoundReason>,
        message: String,
        detail: String,
    },
    /// 没有 `INIT_STATE`，或里面没有作品：页面改版，或平台这次没给。
    Missing { what: String, detail: String },
}

/// 一页读完的结果：作品，加上原样的数据块。
pub struct Parsed {
    pub post: Post,
    /// `INIT_STATE` 的原文，存成输出目录里的文件。
    pub raw: String,
}

pub fn parse(html: &str, id: &str) -> Result<Parsed, PageError> {
    // 整页是一个 JSON 对象（`{"result":2}`）：被拦的路由
    if let Some(body) = html
        .trim()
        .starts_with('{')
        .then(|| serde_json::from_str::<Value>(html.trim()).ok())
        .flatten()
        && let Some(code) = body["result"].as_i64()
    {
        return Err(entry_failure(&body, code, head(html).to_owned()));
    }
    let Some((raw, root)) = init_state(html) else {
        let text = visible_text(html);
        let lower = text.to_lowercase();
        let detail = if text.is_empty() {
            head(html).to_owned()
        } else {
            text
        };
        return Err(
            if html.contains("kwaiCaptcha") || lower.contains("验证") || lower.contains("captcha")
            {
                PageError::Verification { detail }
            } else {
                PageError::Missing {
                    what: "the page has no `INIT_STATE` block".into(),
                    detail,
                }
            },
        );
    };
    // 数据块的开头：页面文字加 JSON 开头，给失败日志
    let detail = || format!("{}\n{}", visible_text(html), head(raw));
    let entries: Vec<&Value> = root
        .as_object()
        .into_iter()
        .flat_map(Map::values)
        .filter(|e| e["result"].is_i64())
        .collect();
    // 带 `photo` 的那一项是作品；`result` 不是 1 就是平台在说这条取不到
    if let Some(entry) = entries.iter().find(|e| e["photo"].is_object()) {
        let code = entry["result"].as_i64().unwrap_or_default();
        if code != 1 {
            return Err(entry_failure(entry, code, entry_detail(html, entry)));
        }
        // `share_info` 里有 `photoId=<路径里的 id>`：对不上就是页面答了别的作品
        if let Some(info) = entry["photo"]["share_info"].as_str()
            && let Some(answered) = share_info_id(info)
            && answered != id
        {
            return Err(PageError::Missing {
                what: "the page answered for a different post".into(),
                detail: detail(),
            });
        }
        return Ok(Parsed {
            post: from_entry(entry),
            raw: raw.to_owned(),
        });
    }
    if let Some(entry) = entries.iter().find(|e| e["result"].as_i64() != Some(1)) {
        let code = entry["result"].as_i64().unwrap_or_default();
        return Err(entry_failure(entry, code, entry_detail(html, entry)));
    }
    Err(PageError::Missing {
        what: "`INIT_STATE` has no entry with `photo`".into(),
        detail: detail(),
    })
}

/// 失败日志里的原文开头：页面文字加出错的那一项（`INIT_STATE` 的开头是一大段站点配置，出错的那一项常在后面）。
fn entry_detail(html: &str, entry: &Value) -> String {
    format!("{}\n{}", visible_text(html), head(&entry.to_string()))
}

/// `result` 不是 1 的一项：验证、作品不可用，或不认识的码。
fn entry_failure(entry: &Value, code: i64, detail: String) -> PageError {
    let message = text_of(&entry["error_msg"]);
    let lower = message.to_lowercase();
    if entry.get("captchaConfig").is_some()
        || matches!(code, 2 | 2001 | 400002)
        || lower.contains("captcha")
        || message.contains("验证")
    {
        return PageError::Verification { detail };
    }
    if matches!(code, 223 | 20113) {
        // 两个码都按「已删除」记，文字里说私密的记私密；223 的文字量到过，私密的写法是猜的
        let reason =
            if message.contains("私密") || message.contains("仅自己") || lower.contains("private")
            {
                NotFoundReason::Private
            } else {
                NotFoundReason::Deleted
            };
        return PageError::Unavailable {
            reason: Some(reason),
            message: format!("the page reports result {code}: {message}")
                .trim_end_matches([' ', ':'])
                .to_owned(),
            detail,
        };
    }
    PageError::Missing {
        what: format!("the page reports result {code}: {message}")
            .trim_end_matches([' ', ':'])
            .to_owned(),
        detail,
    }
}

/// `share_info` 是 `userId=…&photoId=…` 这样的查询串。
fn share_info_id(info: &str) -> Option<&str> {
    info.split('&')
        .find_map(|pair| pair.strip_prefix("photoId="))
        .filter(|id| !id.is_empty())
}

/// 作品所在的那一项（带 `photo` 的）里的字段。
pub fn from_entry(entry: &Value) -> Post {
    let photo = &entry["photo"];
    let mut counts = Map::new();
    for key in ["likeCount", "commentCount", "forwardCount", "viewCount"] {
        if let Some(n) = photo[key].as_u64() {
            counts.insert(key.into(), n.into());
        }
    }
    let images = atlas_images(entry);
    let width = photo["width"].as_u64().filter(|w| *w > 0);
    let height = photo["height"].as_u64().filter(|h| *h > 0);
    Post {
        caption: photo["caption"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_owned(),
        author: photo["userName"]
            .as_str()
            .filter(|n| !n.trim().is_empty())
            .map(|name| AuthorInfo {
                name: name.to_owned(),
                eid: non_empty(text_of(&photo["userEid"])),
                user_id: non_empty(text_of(&photo["userId"])),
            }),
        published_ms: photo["timestamp"].as_i64().filter(|t| *t > 0),
        cover_url: first_url(&photo["coverUrls"]).or_else(|| first_url(&photo["webpCoverUrls"])),
        images,
        // 视频文件用的地址：`mainMvUrls` 的第一项，不改写
        video: first_url(&photo["mainMvUrls"]).map(|url| VideoInfo {
            url,
            // `duration` 的单位是毫秒（实测 149799 对应两分半）
            duration_sec: photo["duration"]
                .as_f64()
                .filter(|d| *d > 0.0)
                .map(|ms| ms / 1000.0),
            width,
            height,
        }),
        counts,
        numeric_id: non_empty(text_of(&photo["photoId"])),
    }
}

/// 图集：`photo.ext_params.atlas`，或同一项里的 `atlas`（ParseHub 两种都认）。
/// 图片地址是 `https://<cdn[0]>/<list[i]>`；`list` 里已经是完整地址的照用。
fn atlas_images(entry: &Value) -> Vec<String> {
    [&entry["photo"]["ext_params"]["atlas"], &entry["atlas"]]
        .into_iter()
        .filter(|a| a.is_object())
        .find_map(|atlas| {
            let list: Vec<&str> = atlas["list"]
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .filter(|p| !p.trim().is_empty())
                .collect();
            let cdn = atlas["cdn"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(Value::as_str)
                .or_else(|| atlas["cdn"].as_str())
                .or_else(|| atlas["cdnList"][0]["cdn"].as_str())
                .map(|c| c.trim().trim_end_matches('/'));
            let urls: Vec<String> = list
                .iter()
                .filter_map(|path| image_url(cdn, path.trim()))
                .collect();
            (!urls.is_empty()).then_some(urls)
        })
        .unwrap_or_default()
}

fn image_url(cdn: Option<&str>, path: &str) -> Option<String> {
    if path.starts_with("http://") || path.starts_with("https://") || path.starts_with("//") {
        return Some(path.to_owned());
    }
    let cdn = cdn.filter(|c| !c.is_empty())?;
    let cdn = if cdn.starts_with("http://") || cdn.starts_with("https://") {
        cdn.to_owned()
    } else {
        format!("https://{cdn}")
    };
    Some(format!("{cdn}/{}", path.trim_start_matches('/')))
}

/// `[{cdn, url}]`（实测的形状）或字符串数组里的第一个地址。
fn first_url(list: &Value) -> Option<String> {
    list.as_array()?.iter().find_map(|item| {
        item.as_str()
            .or_else(|| item["url"].as_str())
            .filter(|u| !u.trim().is_empty())
            .map(str::to_owned)
    })
}

pub fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

/// 找 `window.INIT_STATE =` 后面的第一个 JSON 值，返回它的原文和解析结果。
fn init_state(html: &str) -> Option<(&str, Value)> {
    let marker = "window.INIT_STATE";
    let rest = html[html.find(marker)? + marker.len()..].trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let mut values = serde_json::Deserializer::from_str(rest).into_iter::<Value>();
    let value = values.next()?.ok()?;
    Some((&rest[..values.byte_offset()], value))
}

/// 失败日志里的原文开头：`CallFailure::with_detail` 还会再截到 4096 字节。
pub fn head(text: &str) -> &str {
    let mut end = text.len().min(3000);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// 页面上看得见的文字：去掉 `<script>`、`<style>` 和标签，空白并成一个空格，最多 300 个字符。
pub fn visible_text(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        out.push(' ');
        let tag = &rest[open + 1..];
        let name: String = tag
            .chars()
            .take_while(char::is_ascii_alphabetic)
            .collect::<String>()
            .to_ascii_lowercase();
        let skip_to = if name == "script" || name == "style" {
            let close = format!("</{name}");
            tag.to_ascii_lowercase().find(&close).map(|i| open + 1 + i)
        } else {
            None
        };
        let from = skip_to.unwrap_or(open);
        rest = match rest[from..].find('>') {
            Some(end) => &rest[from + end + 1..],
            None => "",
        };
    }
    out.push_str(rest);
    out.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(300)
        .collect()
}

/// `meta.json` 里的作品部分。
pub fn meta(id: &str, post: &Post) -> Value {
    let mut meta = json!({
        "photoId": id,
        "kind": if post.is_image_post() { "images" } else { "video" },
        "counts": post.counts,
    });
    if let Some(n) = &post.numeric_id {
        meta["numericPhotoId"] = json!(n);
    }
    if let Some(a) = &post.author {
        meta["author"] = json!({"name": a.name, "eid": a.eid, "userId": a.user_id});
    }
    if let Some(t) = post.published_ms {
        meta["timestampMs"] = t.into();
    }
    if let Some(v) = &post.video {
        meta["video"] =
            json!({"width": v.width, "height": v.height, "durationSec": v.duration_sec});
    }
    meta
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_text_skips_scripts_and_tags() {
        let html = "<html><head><title>t</title><script>var a='<b>';</script><style>p{}</style></head><body><p>作品不见了</p> <p>请稍后</p></body></html>";
        assert_eq!(visible_text(html), "t 作品不见了 请稍后");
    }

    #[test]
    fn init_state_stops_at_the_end_of_the_value() {
        let html = r#"<script>window.INIT_STATE = {"a":{"b":[1,2]},"c":"}"}</script><p>x</p>"#;
        let (raw, value) = init_state(html).unwrap();
        assert_eq!(raw, r#"{"a":{"b":[1,2]},"c":"}"}"#);
        assert_eq!(value["a"]["b"][1], 2);
        assert!(init_state("<p>no data</p>").is_none());
        assert!(init_state("window.INIT_STATE = nonsense").is_none());
    }

    #[test]
    fn atlas_addresses() {
        let entry = json!({"photo": {"ext_params": {"atlas": {
            "cdn": ["js2.a.yximgs.com", "bd.a.yximgs.com"],
            "list": ["/ufile/atlas/a_0.webp", "ufile/atlas/a_1.webp", "https://p5.a.yximgs.com/c.jpg"],
        }}}});
        assert_eq!(
            atlas_images(&entry),
            [
                "https://js2.a.yximgs.com/ufile/atlas/a_0.webp",
                "https://js2.a.yximgs.com/ufile/atlas/a_1.webp",
                "https://p5.a.yximgs.com/c.jpg"
            ]
        );
        // 同一项里的 atlas，cdn 写成 cdnList
        let entry = json!({"photo": {}, "atlas": {"cdnList": [{"cdn": "js2.a.yximgs.com"}], "list": ["/a.jpg"]}});
        assert_eq!(atlas_images(&entry), ["https://js2.a.yximgs.com/a.jpg"]);
        // 没有 cdn 又不是完整地址的丢掉
        let entry = json!({"photo": {"ext_params": {"atlas": {"list": ["/a.jpg"]}}}});
        assert!(atlas_images(&entry).is_empty());
        assert!(atlas_images(&json!({"photo": {}})).is_empty());
    }

    #[test]
    fn share_info_gives_the_path_id() {
        assert_eq!(
            share_info_id("userId=3xdng3ssi9kdpk2&photoId=3xhe42v3qfzepmy"),
            Some("3xhe42v3qfzepmy")
        );
        assert_eq!(share_info_id("userId=1"), None);
    }
}
