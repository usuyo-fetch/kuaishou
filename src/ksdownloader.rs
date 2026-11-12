//! 方法 `ks-downloader`：问你自己在本机运行的 KS-Downloader（GPL-3.0，API 模式）要一个作品，
//! 插件不带它、不下载它、不装它。
//!
//! 接口形状来自它 1.6 版（和主分支 1.7 beta）的源码和 README（2026-11-12 用 `gh api` 读的，
//! 没有运行过它，没有对快手发过请求）：
//! - `python main.py api [--host H] [--port P]` 启动，端口默认 5557，主机默认 `0.0.0.0`（README 写
//!   「访问 http://127.0.0.1:5557/docs」）；没有访问令牌；FastAPI，`GET /openapi.json` 是接口文档，
//!   `info.title` 是 `KS-Downloader`，`info.version` 形如 `1.6.stable`。
//! - `POST /detail/`（带结尾的斜杠，不带会被 307 跳转），请求体 `{"text", "cookies", "proxy"}`，
//!   `text` 是作品链接（自动从文字里提取，短链由服务自己展开）；回答 `{"message", "params", "data", "time"}`，
//!   失败时 `data` 是 `null`、`message` 说原因（跟着服务的界面语言，中文或英文），状态码仍是 200。
//! - `data` 的字段：`detailID`、`photoType`（`视频`/`图片`，英文界面是 `Video`/`Image`）、`caption`、`coverUrl`、
//!   `duration`（`HH:MM:SS`）、`realLikeCount`、`viewCount`、`commentCount`、`shareCount`（没有的是 -1）、
//!   `timestamp`（服务所在机器的本地时间，`%Y-%m-%d_%H:%M:%S`，没有时是 `unknown`）、`authorID`、`name`、
//!   `download`（服务存库前把地址列表用空格连成一个字符串，所以回答里是字符串；也认数组）。
//!
//! 插件对服务只做这几件事：
//! - 请求体只有 `text`，不带 `cookies`、`proxy`：服务没收到 `cookies` 时用它自己配置里的那份网页 cookie
//!   （`DetailPage.request_url`），所以 README 和提示都写「它的设置里放一份不登录的网页 cookie」。
//! - 服务地址只能是本机地址（回环），请求不经宿主的代理，只连这一个地址。
//! - 作品的图片、视频由 [`share::deliver`] 下载，和 `share-page` 一样只去快手自己的媒体主机，身份也是同一个。
//!
//! 照源码推断、没有量过的：服务对手机页链接（`v.m.chenzhongtech.com/fw/photo/…`，短链展开后就是它）走的
//! 是「应用」那一支，`photoType` 写死 `图片`，`download` 只有图集的地址（单图时是封面），拿不到视频地址；
//! 所以 `图片` 又没有地址时，按「视频的地址没有给」交（`missing` 里写 `media:video`），让宿主去找别的方法补。

use std::fmt;
use std::io::Read;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

use jiff::civil::DateTime;
use jiff::tz::TimeZone;
use serde_json::{Map, Value};
use url::{Host, Url};
use usuyo_plugin_kit::http::{self, Client, Hosts, Policy};
use usuyo_plugin_kit::protocol::{
    Access, CallFailure, ErrorCode, Evidence, FetchHealth, FetchOutput, Need, OutputKind, Rfc3339,
    UpstreamVersion,
};

use crate::api::{Kuaishou, step};
use crate::canonical::identity;
use crate::ids::{Parsed, SHORT_LINK_HOST, canonical_url, parse_input};
use crate::page::{AuthorInfo, Post, VideoInfo, head};
use crate::share::{deliver, wanted_outputs, write};

/// 清单里的方法 id。
pub const METHOD: &str = "ks-downloader";
/// 清单里设置项 `endpoint` 的默认值，KS-Downloader 的 API 模式默认监听 5557。
pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:5557";
const ENDPOINT_KEY: &str = "endpoint";
const NEED_ENDPOINT: &str = "setting:endpoint";
const NEED_SERVICE: &str = "service:endpoint";

/// 服务取一页有自己的请求间隔和重试，一次要等一阵；但宿主在发过 `progress` 以后 120 秒没有新的就杀整次调用，
/// 所以比它短。
const SERVICE_TIMEOUT: Duration = Duration::from_secs(100);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// 一个作品的回答远小于这个数。
const ANSWER_BYTES: u64 = 8 << 20;
const SETTINGS_BYTES: u64 = 1 << 20;
/// 交给服务的文字最多这么长，和认链接时看的长度一样。
const MAX_INPUT: usize = 4096;
/// 图文最多取几张，和 `share-page` 一样。
const MAX_IMAGES: usize = 100;
const USER_AGENT: &str = "usuyo-fetch-kuaishou";
/// 服务没在运行时给用户的一句话，`health` 的检查也用它。
pub const SERVICE_DOWN: &str = "KS-Downloader 没有在运行：请先在它的文件夹里运行 python main.py api --host 127.0.0.1 启动它的 API 模式（默认端口 5557；它的设置里请放一份不登录的网页 cookie，不要放登录了你账号的），再重试";

/// 本机的 KS-Downloader。
pub struct Service {
    endpoint: Url,
    /// 等服务回答多久；测试里换成短的。
    timeout: Duration,
    /// 服务写的 `timestamp` 是它所在机器的本地时间，它在本机，所以用本机的时区。
    zone: TimeZone,
}

impl fmt::Debug for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Service")
            .field("endpoint", &self.endpoint.as_str())
            .finish_non_exhaustive()
    }
}

fn need(text: &str) -> Need {
    Need::try_from(text.to_owned()).expect("constant need")
}

fn missing(need_text: &str, message: String, hint: &str) -> CallFailure {
    CallFailure {
        hint: Some(hint.to_owned()),
        needs: vec![need(need_text)],
        ..CallFailure::new(ErrorCode::DependencyMissing, message)
    }
}

/// 服务答的东西读不懂：不是网络问题，也不是限流，宿主换下一种方法；回答的开头留给修插件的人。
fn changed(message: impl Into<String>, answer: &str) -> CallFailure {
    CallFailure {
        hint: Some("KS-Downloader 的回答和这个插件认得的不一样，插件要更新，或换别的方法".into()),
        ..CallFailure::new(ErrorCode::UpstreamChanged, message)
    }
    .with_detail(head(answer))
}

impl Service {
    /// 服务地址要是 `http://` 的本机地址（回环 IP 或 `localhost`），不带账号密码、路径、参数。
    pub fn new(endpoint: &str) -> Result<Self, CallFailure> {
        let bad = |why: &str| {
            missing(
                NEED_ENDPOINT,
                format!("the `{ENDPOINT_KEY}` setting is not usable: {why}"),
                "插件设置里的 KS-Downloader 服务地址要写成本机地址，例如 http://127.0.0.1:5557",
            )
        };
        let mut url = Url::parse(endpoint.trim()).map_err(|_| bad("not a URL"))?;
        if url.scheme() != "http" {
            return Err(bad("the scheme is not http"));
        }
        let local = match url.host() {
            Some(Host::Ipv4(ip)) => ip.is_loopback(),
            Some(Host::Ipv6(ip)) => ip.is_loopback(),
            Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
            None => false,
        };
        if !local {
            return Err(bad("the host is not a local address"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(bad("it carries a user name or password"));
        }
        if !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() {
            return Err(bad("it carries a path, query or fragment"));
        }
        url.set_path("");
        Ok(Self {
            endpoint: url,
            timeout: SERVICE_TIMEOUT,
            zone: TimeZone::system(),
        })
    }

    /// 读宿主交来的设置：`USUYO_SETTINGS`（`endpoint`）。文件读不出来的错误不写出文件内容。
    pub fn from_env() -> Result<Self, CallFailure> {
        let endpoint = match read_settings()?.as_ref().and_then(|s| s.get(ENDPOINT_KEY)) {
            None | Some(Value::Null) => DEFAULT_ENDPOINT.to_owned(),
            Some(Value::String(s)) if s.trim().is_empty() => DEFAULT_ENDPOINT.to_owned(),
            Some(Value::String(s)) => s.clone(),
            Some(_) => {
                return Err(missing(
                    NEED_ENDPOINT,
                    format!("the `{ENDPOINT_KEY}` setting is not a string"),
                    "插件设置里的 KS-Downloader 服务地址要写成本机地址，例如 http://127.0.0.1:5557",
                ));
            }
        };
        Self::new(&endpoint)
    }

    /// 换等服务回答的时长。
    pub fn with_timeout(self, timeout: Duration) -> Self {
        Self { timeout, ..self }
    }

    /// 换读 `timestamp` 用的时区。
    pub fn with_zone(self, zone: TimeZone) -> Self {
        Self { zone, ..self }
    }

    /// 服务地址，给日志和提示用。
    pub fn endpoint(&self) -> &str {
        self.endpoint.as_str().trim_end_matches('/')
    }

    /// 服务有没有在这个地址上监听：只连一下 TCP，不发请求。
    fn listening(&self) -> Result<(), std::io::Error> {
        let host = self.endpoint.host_str().unwrap_or_default();
        let port = self.endpoint.port_or_known_default().unwrap_or(80);
        let mut last = std::io::Error::other("the address has no socket address");
        for addr in (host, port).to_socket_addrs()? {
            match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
                Ok(_) => return Ok(()),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    fn require_listening(&self) -> Result<(), CallFailure> {
        self.listening().map_err(|e| {
            missing(
                NEED_SERVICE,
                format!("nothing answers at {}: {e}", self.endpoint()),
                SERVICE_DOWN,
            )
        })
    }

    /// 只连这一个地址、不经代理：回环地址由沙箱直接放行（协议 §3.2 的「网络出口」）。
    fn client(&self) -> Client {
        Client::new(USER_AGENT, Hosts::only(self.endpoint.host_str())).with_policy(Policy::strict())
    }

    fn url(&self, path: &str) -> Result<Url, CallFailure> {
        self.endpoint
            .join(path)
            .map_err(|e| CallFailure::new(ErrorCode::Internal, format!("service URL: {e}")))
    }

    /// 问 `text` 这个作品，回服务答的 `data` 对象。
    fn detail(&self, text: &str) -> Result<Value, CallFailure> {
        self.require_listening()?;
        let body = serde_json::json!({ "text": text });
        let mut response = self
            .client()
            .post(&self.url("/detail/")?, body.to_string())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .timeout(self.timeout)
            .allow_private_first_hop(true)
            .send()
            .map_err(service_error)?;
        let status = response.status();
        let raw = response.read_to_vec(ANSWER_BYTES).map_err(service_error)?;
        let answer = String::from_utf8_lossy(&raw);
        if status != 200 {
            return Err(changed(
                format!("KS-Downloader answered HTTP {status}"),
                &answer,
            ));
        }
        let parsed: Value = serde_json::from_str(&answer)
            .map_err(|_| changed("the answer is not JSON", &answer))?;
        let message = parsed
            .get("message")
            .and_then(Value::as_str)
            .ok_or_else(|| changed("the answer has no `message`", &answer))?;
        match parsed.get("data") {
            None => Err(changed("the answer has no `data`", &answer)),
            // 失败时 `data` 是 `null`，`message` 说原因；原因没有固定的编号，分不出删除、验证、网络
            Some(Value::Null) => Err(CallFailure::new(
                ErrorCode::NotFound,
                format!(
                    "KS-Downloader returned no data (`{}`)",
                    message.chars().take(200).collect::<String>()
                ),
            )
            .with_detail(head(&answer))),
            Some(data @ Value::Object(map)) if !map.is_empty() => Ok(data.clone()),
            Some(_) => Err(changed("`data` is not a non-empty object", &answer)),
        }
    }

    /// `GET /openapi.json` 里的 `info.title` 和 `info.version`；`health` 用它认出这是不是 KS-Downloader。
    fn about(&self) -> Result<(String, String), String> {
        let url = self.url("/openapi.json").map_err(|e| e.message)?;
        let mut response = self
            .client()
            .get(&url)
            .header("Accept", "application/json")
            .timeout(PROBE_TIMEOUT)
            .allow_private_first_hop(true)
            .send()
            .map_err(|e| e.to_string())?;
        if response.status() != 200 {
            return Err(format!("HTTP {}", response.status()));
        }
        let raw = response
            .read_to_vec(ANSWER_BYTES)
            .map_err(|e| e.to_string())?;
        let doc: Value = serde_json::from_slice(&raw).map_err(|_| "not JSON".to_owned())?;
        let field = |name: &str| {
            doc.pointer(&format!("/info/{name}"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("no info.{name}"))
        };
        Ok((field("title")?, field("version")?))
    }
}

fn service_error(e: http::Error) -> CallFailure {
    match e {
        e if e.is_refusal() => CallFailure::new(
            ErrorCode::Internal,
            format!("the request to KS-Downloader was refused: {e}"),
        ),
        http::Error::TooLarge(limit) => CallFailure::new(
            ErrorCode::UpstreamChanged,
            format!("KS-Downloader answered with more than {limit} bytes"),
        ),
        e => CallFailure::new(ErrorCode::Network, format!("KS-Downloader: {e}")),
    }
}

fn read_settings() -> Result<Option<Value>, CallFailure> {
    let Some(path) = std::env::var_os("USUYO_SETTINGS") else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .and_then(|f| f.take(SETTINGS_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|e| {
            CallFailure::new(
                ErrorCode::Internal,
                format!("cannot read USUYO_SETTINGS: {e}"),
            )
        })?;
    if bytes.len() as u64 > SETTINGS_BYTES {
        return Err(CallFailure::new(
            ErrorCode::Internal,
            "USUYO_SETTINGS is too large",
        ));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| CallFailure::new(ErrorCode::Internal, "USUYO_SETTINGS is not a JSON object"))
}

/// 一个作品：交给服务的文字原样是宿主给的那条链接（服务要链接里的 `userId`、`photoId` 参数，短链它自己展开）；
/// 身份用服务回答里的 `detailID` 照本插件认链接的代码核过，链接里已经有 id 时两边要一致。
pub fn fetch(
    api: &Kuaishou,
    service: &Service,
    input: &str,
    out: &Path,
    want: Option<&[OutputKind]>,
) -> Result<FetchOutput, CallFailure> {
    let wanted = wanted_outputs(want)?;
    let sent = input.trim();
    let unsupported = || CallFailure::new(ErrorCode::UnsupportedInput, "not a kuaishou post link");
    if sent.len() > MAX_INPUT {
        return Err(unsupported());
    }
    let known = match parse_input(sent, |h| h == SHORT_LINK_HOST) {
        Parsed::Post { id, .. } => Some(id),
        Parsed::ShortLink(_) => None,
        Parsed::Foreign => return Err(unsupported()),
    };

    step("KS-Downloader");
    let fetched_at = Rfc3339::from_timestamp(jiff::Timestamp::now());
    let data = service.detail(sent)?;
    let answer = data.to_string();
    let answered = post_of(&data, &service.zone, &answer)?;
    if known.as_ref().is_some_and(|k| *k != answered.id) {
        return Err(changed(
            "KS-Downloader answered a different post than the link",
            &answer,
        ));
    }
    // `detailID` 和链接里的 id 是同一种写法：路径里那一段
    identity(&answered.id)?;
    write(
        out,
        "ks-downloader.json",
        &serde_json::to_vec_pretty(&data).map_err(|e| {
            CallFailure::new(ErrorCode::Internal, format!("ks-downloader.json: {e}"))
        })?,
    )?;
    let evidence = Evidence {
        fetched_at,
        access: Access::PublicWeb,
        site: None,
        upstream: None,
        status: None,
    };
    let mut output = deliver(
        api,
        out,
        &wanted,
        &answered.id,
        &answered.post,
        "KS-Downloader 的回答",
        evidence,
    )?;
    if answered.video_unknown && output.missing.contains(&OutputKind::Video) {
        let note = "KS-Downloader 对手机页链接按图片处理，只给了封面";
        output.missing_reason = Some(match output.missing_reason.take() {
            Some(r) => format!("{r}；{note}"),
            None => note.to_owned(),
        });
    }
    Ok(output)
}

/// 回答里读出的作品。
struct Answered {
    id: String,
    post: Post,
    /// 写着是图片，却一个地址也没有：多半是视频（见模块说明）。
    video_unknown: bool,
}

fn text(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn post_of(data: &Value, zone: &TimeZone, answer: &str) -> Result<Answered, CallFailure> {
    let id = text(data, "detailID")
        .filter(|id| {
            matches!(
                parse_input(&canonical_url(id), |_| false),
                Parsed::Post { id: ref parsed, .. } if parsed == id
            )
        })
        .ok_or_else(|| changed("`detailID` is missing or not a post id", answer))?;
    let kind = text(data, "photoType").map(|t| t.to_ascii_lowercase());
    let is_video = match kind.as_deref() {
        Some("视频" | "video") => true,
        Some("图片" | "image") => false,
        _ => return Err(changed("`photoType` is not a known value", answer)),
    };
    let cover = text(data, "coverUrl");
    let urls = download_urls(data.get("download"))
        .ok_or_else(|| changed("`download` is not a string or an array of strings", answer))?;
    let (cover_url, images, video) = if is_video {
        (
            cover,
            Vec::new(),
            urls.first().map(|url| VideoInfo {
                url: url.clone(),
                duration_sec: text(data, "duration").and_then(|d| clock_seconds(&d)),
                width: None,
                height: None,
            }),
        )
    } else if urls.len() == 1 && cover.as_deref() == Some(urls[0].as_str()) {
        // 单图：服务把封面当唯一的一张图
        (None, urls, None)
    } else {
        (cover, urls, None)
    };
    let video_unknown = !is_video && images.is_empty() && cover_url.is_some();
    let author = text(data, "name").map(|name| AuthorInfo {
        name,
        eid: text(data, "authorID"),
        user_id: None,
    });
    let mut counts = Map::new();
    for (from, to) in [
        ("realLikeCount", "likeCount"),
        ("commentCount", "commentCount"),
        ("shareCount", "shareCount"),
        ("viewCount", "viewCount"),
    ] {
        // 服务把没有的数记成 -1
        if let Some(n) = data.get(from).and_then(Value::as_u64) {
            counts.insert(to.into(), n.into());
        }
    }
    Ok(Answered {
        id,
        post: Post {
            caption: text(data, "caption").unwrap_or_default(),
            author,
            published_ms: text(data, "timestamp").and_then(|t| local_ms(&t, zone)),
            cover_url,
            images,
            video,
            counts,
            numeric_id: None,
        },
        video_unknown,
    })
}

/// 服务存库前把地址用空格连成一个字符串；没连时是数组。别的写法（数字、对象）回 `None`。
fn download_urls(value: Option<&Value>) -> Option<Vec<String>> {
    let urls: Vec<String> = match value {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(s)) => s.split_whitespace().map(str::to_owned).collect(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|i| i.as_str().map(|s| s.trim().to_owned()))
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect(),
        Some(_) => return None,
    };
    Some(urls.into_iter().take(MAX_IMAGES).collect())
}

/// `HH:MM:SS` 换成秒；`00:00:00`（服务没有时长时的写法）当作没有。
fn clock_seconds(text: &str) -> Option<f64> {
    let mut parts = text.split(':').map(|p| p.parse::<u32>().ok());
    let (h, m, s) = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(())?;
    let total = h * 3600 + m * 60 + s;
    (total > 0).then_some(f64::from(total))
}

/// `%Y-%m-%d_%H:%M:%S`（服务所在机器的本地时间）换成 Unix 毫秒；`unknown` 和别的写法当作没有。
fn local_ms(text: &str, zone: &TimeZone) -> Option<i64> {
    let civil = DateTime::strptime("%Y-%m-%d_%H:%M:%S", text).ok()?;
    let zoned = zone.to_zoned(civil).ok()?;
    Some(zoned.timestamp().as_millisecond())
}

/// `health`：只看本机，不碰快手。服务没在监听、地址上答的不像 KS-Downloader，都只挡这个方法。
pub fn health(health: FetchHealth) -> FetchHealth {
    let methods = [METHOD];
    let service = match Service::from_env() {
        Ok(s) => s,
        Err(failure) => {
            eprintln!("{}", failure.message);
            return health.check(
                "KS-Downloader 设置",
                false,
                Some(
                    "插件设置里的 KS-Downloader 服务地址要写成本机地址，例如 http://127.0.0.1:5557"
                        .to_owned(),
                ),
                &methods,
            );
        }
    };
    let mut health = health;
    if service.listening().is_err() {
        return health.check(
            "KS-Downloader 服务",
            false,
            Some(format!("{SERVICE_DOWN}（地址 {}）", service.endpoint())),
            &methods,
        );
    }
    match service.about() {
        Ok((title, version)) if title == "KS-Downloader" => {
            health.upstream.push(UpstreamVersion {
                name: title,
                version: version.clone(),
            });
            health.check(
                "KS-Downloader 服务",
                true,
                Some(format!(
                    "{} 上有 KS-Downloader {version} 在运行",
                    service.endpoint()
                )),
                &methods,
            )
        }
        other => {
            let seen = other.map_or_else(|why| why, |(title, _)| title);
            health.check(
                "KS-Downloader 服务",
                false,
                Some(format!(
                    "{} 上答话的不像 KS-Downloader（{}）：请核对插件设置里的服务地址，并确认它是用 API 模式启动的",
                    service.endpoint(),
                    seen.chars().take(100).collect::<String>()
                )),
                &methods,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_and_local_time() {
        assert_eq!(clock_seconds("00:02:29"), Some(149.0));
        assert_eq!(clock_seconds("00:00:00"), None);
        assert_eq!(clock_seconds("1:2"), None);
        assert_eq!(clock_seconds("a:b:c"), None);
        assert_eq!(clock_seconds("00:00:01:00"), None);
        let utc = TimeZone::UTC;
        assert_eq!(
            local_ms("2026-10-01_12:00:00", &utc),
            Some(1_790_856_000_000)
        );
        assert_eq!(local_ms("unknown", &utc), None);
        assert_eq!(local_ms("2026-10-01 12:00:00", &utc), None);
    }

    #[test]
    fn download_shapes() {
        let urls = |v: Value| download_urls(Some(&v));
        assert_eq!(
            urls(Value::from("https://a/1.jpg https://a/2.jpg")),
            Some(vec!["https://a/1.jpg".into(), "https://a/2.jpg".into()])
        );
        assert_eq!(urls(Value::from(" ")), Some(vec![]));
        assert_eq!(
            urls(serde_json::json!(["https://a/1.jpg", ""])),
            Some(vec!["https://a/1.jpg".into()])
        );
        assert_eq!(urls(serde_json::json!([1])), None);
        assert_eq!(urls(serde_json::json!(5)), None);
        assert_eq!(download_urls(None), Some(vec![]));
    }
}
