//! 认链接：从一条链接或用户粘贴的分享文字里找出快手作品的 id。不发请求。
//!
//! 认的形状：
//! - `v.kuaishou.com/<短码>`、`www.kuaishou.com/f/<短码>`：App 和网页的分享链接，要展开才知道 id
//! - `www.kuaishou.com/short-video/<id>`
//! - `<子域>.chenzhongtech.com/fw/photo/<id>`、`.../fw/long-video/<id>`：分享链接跳去的手机页
//!   （2026-11-12 量到 `v.kuaishou.com` 短链跳到 `v.m.chenzhongtech.com/fw/photo/<id>?<分享参数>`）
//!
//! id 是路径里的那一段（小写字母加数字，十五个字符上下，例如 `3x…`），不是页面数据里 `photo.photoId` 的那串数字。
//! 主机整个比对，不取「路径最后一段」。

use url::Url;

/// 短码最长多少：分享链接里的短码只有十来个字符，留足余量。
const MAX_CODE: usize = 48;
const MIN_ID: usize = 6;
const MAX_ID: usize = 32;
/// 粘贴的文字最多看这么多字节。
const MAX_TEXT: usize = 4096;
/// 分享参数最长多少：超过的不带。
const MAX_QUERY: usize = 2048;

pub const SHORT_LINK_HOST: &str = "v.kuaishou.com";
pub const WEB_HOST: &str = "www.kuaishou.com";

#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    /// 作品 id；`query` 是链接上的分享参数（手机页请求时原样带上）。
    Post {
        id: String,
        query: Option<String>,
    },
    /// 要展开的短链。
    ShortLink(Url),
    Foreign,
}

/// 输入是一条链接，或带链接的分享文字（「…… https://v.kuaishou.com/xxxx 复制此消息，打开【快手】直接观看！」）。
/// `is_short_host` 判断主机是不是短链主机（测试里把本机也算进去）。
pub fn parse_input(input: &str, is_short_host: impl Fn(&str) -> bool) -> Parsed {
    let Some(url) = find_url(input) else {
        return Parsed::Foreign;
    };
    let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
        return Parsed::Foreign;
    };
    let segments: Vec<&str> = url
        .path()
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let post = |id: &str| {
        if is_id(id) {
            Parsed::Post {
                id: id.to_owned(),
                query: url
                    .query()
                    .filter(|q| !q.is_empty() && q.len() <= MAX_QUERY)
                    .map(str::to_owned),
            }
        } else {
            Parsed::Foreign
        }
    };
    let short = |code: &str| {
        if is_code(code) {
            Parsed::ShortLink(url.clone())
        } else {
            Parsed::Foreign
        }
    };
    match (host.as_str(), segments.as_slice()) {
        (WEB_HOST | "kuaishou.com", ["short-video", id]) => post(id),
        (h, ["f", code]) if is_short_host(h) || h == WEB_HOST || h == "kuaishou.com" => short(code),
        (h, ["fw", "photo" | "long-video", id]) if is_mobile_page_host(h) => post(id),
        (h, [code]) if is_short_host(h) => short(code),
        _ => Parsed::Foreign,
    }
}

/// 整个输入是一条链接（没有空白）就用它；不然取文字里第一个 `http(s)://` 起到空白或非 ASCII 字符为止的一段。
fn find_url(input: &str) -> Option<Url> {
    let text = &input[..floor(input, MAX_TEXT)];
    let text = text.trim();
    // 带空白的是分享文字：整段交给 `Url::parse` 会把空白后面的字都当成路径
    if !text.contains(char::is_whitespace)
        && let Ok(url) = Url::parse(text)
        && matches!(url.scheme(), "http" | "https")
    {
        return Some(url);
    }
    let start = ["https://", "http://"]
        .iter()
        .filter_map(|p| text.find(p))
        .min()?;
    let rest = &text[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_graphic() || matches!(c, '"' | '\'' | '<' | '>'))
        .unwrap_or(rest.len());
    let candidate = rest[..end].trim_end_matches(['.', ',', ';', ')', ']', '!']);
    Url::parse(candidate)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
}

fn floor(text: &str, max: usize) -> usize {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn is_id(id: &str) -> bool {
    (MIN_ID..=MAX_ID).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn is_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= MAX_CODE
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// 规范链接：只用于去重和显示，视频和图文用同一种写法。
pub fn canonical_url(id: &str) -> String {
    format!("https://{WEB_HOST}/short-video/{id}")
}

/// 是不是快手自己的主机：展开短链时，跳转只许落在这些主机上。
pub fn is_kuaishou_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    ["kuaishou.com", "chenzhongtech.com"]
        .iter()
        .any(|d| host == *d || host.strip_suffix(d).is_some_and(|p| p.ends_with('.')))
}

fn is_mobile_page_host(host: &str) -> bool {
    host.strip_suffix("chenzhongtech.com")
        .is_some_and(|p| p.ends_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Parsed {
        parse_input(input, |h| h == SHORT_LINK_HOST)
    }

    fn post(id: &str, query: Option<&str>) -> Parsed {
        Parsed::Post {
            id: id.into(),
            query: query.map(str::to_owned),
        }
    }

    #[test]
    fn post_links() {
        for (input, query) in [
            ("https://www.kuaishou.com/short-video/3x24h4gjvevux4w", None),
            (
                "https://www.kuaishou.com/short-video/3x24h4gjvevux4w/?fid=1&cc=share_copylink",
                Some("fid=1&cc=share_copylink"),
            ),
            (
                "https://v.m.chenzhongtech.com/fw/photo/3x24h4gjvevux4w?cc=share_copylink&photoId=3x24h4gjvevux4w",
                Some("cc=share_copylink&photoId=3x24h4gjvevux4w"),
            ),
            (
                "https://v.m.chenzhongtech.com/fw/long-video/3x24h4gjvevux4w",
                None,
            ),
            ("  http://kuaishou.com/short-video/3x24h4gjvevux4w\n", None),
        ] {
            assert_eq!(parse(input), post("3x24h4gjvevux4w", query), "{input}");
        }
    }

    #[test]
    fn share_text() {
        let text = "https://v.kuaishou.com/mkYOSR 「手冲咖啡」复制此消息，打开【快手】直接观看！";
        let Parsed::ShortLink(url) = parse(text) else {
            panic!("{:?}", parse(text))
        };
        assert_eq!(url.as_str(), "https://v.kuaishou.com/mkYOSR");
        assert_eq!(
            parse("看这个 https://www.kuaishou.com/short-video/3x24h4gjvevux4w，真好"),
            post("3x24h4gjvevux4w", None)
        );
        assert_eq!(parse("没有链接 mkYOSR"), Parsed::Foreign);
    }

    #[test]
    fn short_links() {
        for input in [
            "https://v.kuaishou.com/mkYOSR",
            "https://v.kuaishou.com/mkYOSR/",
            "https://www.kuaishou.com/f/X-aqEn4ZUlPtlME2",
        ] {
            assert!(matches!(parse(input), Parsed::ShortLink(_)), "{input}");
        }
        for input in [
            "https://v.kuaishou.com/",
            "https://v.kuaishou.com/a/b",
            "https://v.kuaishou.com/a%2Fb",
            "https://www.kuaishou.com/f/",
            "https://www.kuaishou.com/f/a/b",
        ] {
            assert_eq!(parse(input), Parsed::Foreign, "{input}");
        }
    }

    #[test]
    fn foreign() {
        for input in [
            "https://www.kuaishou.com/profile/3xdng3ssi9kdpk2",
            "https://www.kuaishou.com/short-video/",
            "https://www.kuaishou.com/short-video/a",
            "https://www.kuaishou.com/short-video/3x24h4gjvevux4w/extra",
            "https://www.kuaishou.com/short-video/3x24h4gjvevux4w_x",
            "https://evil.example/short-video/3x24h4gjvevux4w",
            "https://www.kuaishou.com.evil.example/short-video/3x24h4gjvevux4w",
            "https://notkuaishou.com/short-video/3x24h4gjvevux4w",
            "https://evil.example/fw/photo/3x24h4gjvevux4w",
            "https://www.kuaishou.com/fw/photo/3x24h4gjvevux4w",
            "https://v.m.chenzhongtech.com/fw/user/3x24h4gjvevux4w",
            "ftp://www.kuaishou.com/short-video/3x24h4gjvevux4w",
            "not a url",
            "",
        ] {
            assert_eq!(parse(input), Parsed::Foreign, "{input}");
        }
    }

    #[test]
    fn kuaishou_hosts() {
        assert!(is_kuaishou_host("v.m.chenzhongtech.com"));
        assert!(is_kuaishou_host("V.kuaishou.com"));
        assert!(is_kuaishou_host("kuaishou.com"));
        assert!(!is_kuaishou_host("evilkuaishou.com"));
        assert!(!is_kuaishou_host("kuaishou.com.evil.test"));
    }
}
