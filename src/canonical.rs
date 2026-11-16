//! `canonicalize`：认出快手作品的身份。`fetch` 用同一个函数，所以两边给的身份一样。

use usuyo_plugin_kit::protocol::{CallFailure, Canonicalized, ErrorCode, Identity, PlatformId};

use crate::api::{CookieJar, Kuaishou};
use crate::ids::{Parsed, canonical_url, parse_input};

/// 输入指向的作品；输入是短链时还带着展开后的完整链接。
pub struct Located {
    pub id: String,
    /// 分享链接上的参数：取手机页时原样带上。
    pub query: Option<String>,
    pub resolved_url: Option<String>,
    /// 展开短链时站点发的 cookie；取手机页时带上。不是短链就是空的。
    pub cookies: CookieJar,
}

/// 认出输入是哪个作品。`None` 是「不是快手的作品链接」，不是错误。
/// 只有短链会发请求，只跟随重定向，并且只在快手自己的主机之间；跳转里站点发的 cookie 记在 `cookies` 里。
pub fn locate(api: &Kuaishou, input: &str) -> Result<Option<Located>, CallFailure> {
    let endpoints = api.endpoints();
    match parse_input(input, |h| endpoints.is_short_link_host(h)) {
        Parsed::Post { id, query } => Ok(Some(Located {
            id,
            query,
            resolved_url: None,
            cookies: CookieJar::default(),
        })),
        Parsed::Foreign => Ok(None),
        Parsed::ShortLink(url) => {
            let (expanded, cookies) = api.expand_short_link(url)?;
            Ok(
                match parse_input(expanded.as_str(), |h| endpoints.is_short_link_host(h)) {
                    Parsed::Post { id, query } => Some(Located {
                        id,
                        query,
                        resolved_url: Some(expanded.into()),
                        cookies,
                    }),
                    Parsed::ShortLink(_) | Parsed::Foreign => None,
                },
            )
        }
    }
}

pub fn identity(id: &str) -> Result<Identity, CallFailure> {
    let platform =
        PlatformId::try_from("kuaishou").map_err(|e| CallFailure::new(ErrorCode::Internal, e))?;
    Ok(Identity::Platform {
        platform,
        content_id: id.to_owned(),
        url: Some(canonical_url(id)),
    })
}

pub fn canonicalize(api: &Kuaishou, input: &str) -> Result<Canonicalized, CallFailure> {
    Ok(match locate(api, input)? {
        Some(found) => Canonicalized::Match {
            identity: identity(&found.id)?,
            resolved_url: found.resolved_url,
        },
        None => Canonicalized::NoMatch,
    })
}
