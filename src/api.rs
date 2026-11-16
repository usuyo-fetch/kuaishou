//! 快手的 HTTP 请求：手机页、展开短链、下载封面图片和视频。不读宿主交来的凭据。
//! 展开短链时站点在跳转里用 `Set-Cookie` 发的 cookie（`did` 一类）记在这一次调用的内存里（`CookieJar`），
//! 只带给后面的短链跳转和手机页；不落盘，不进日志和报错，下载封面、图片、视频不带。
//! 请求都经 `usuyo_plugin_kit::http`：每种请求只连自己那几台主机，只连公网地址。
//! 手机页和短链不跟随跳转（自己一跳一跳读）；只有下载媒体跟随，每一跳都要在媒体主机名单里。

use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use url::Url;
use usuyo_plugin_kit::http::{self, Client, Cookie, Hosts, Policy};
use usuyo_plugin_kit::protocol::{CallFailure, ErrorCode, NotFoundReason, PluginEvent};

use crate::ids::{Parsed, SHORT_LINK_HOST, WEB_HOST, is_kuaishou_host, parse_input};

/// 手机 Safari 的 UA：手机页（`fw/photo/<id>`）用它取到了作品数据（2026-11-12 实测，没有带 cookie）。
const PAGE_USER_AGENT: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 26_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Mobile/15E148 Safari/604.1";
/// 短链和媒体用普通桌面浏览器的 UA。
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36";
const MEDIA_REFERER: &str = "https://www.kuaishou.com/";
/// 作品数据所在的手机页的主机。
const PAGE_HOST: &str = "v.m.chenzhongtech.com";
/// 手机页回答上限（实测约 180 KB）。
const PAGE_BYTES: u64 = 8 << 20;
const PAGE_TIMEOUT: Duration = Duration::from_secs(20);
const SHORT_LINK_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_SHORT_LINK_HOPS: usize = 4;
/// 媒体地址一般跳一次到 CDN。
const MEDIA_REDIRECTS: u32 = 5;
/// 封面、图片、视频只从这些域名的子域下载，和清单的 `network.hosts` 一致。
/// 2026-11-12 量到：视频在 `*.kwimgs.com`（一条）或 `*.kwaicdn.com`（另一条，第一次只列了前者，
/// 视频没下下来，`missingReason` 里写了主机名），封面在 `*.yximgs.com`，头像在 `*.kwimgs.com`；
/// 补上 `*.kwaicdn.com` 以后没有再对着平台下载过，只有 `media_hosts` 测试管着它。
/// 图文的图片主机来自页面数据里的 `atlas.cdn`，没有量过，不在名单上的主机会记进 `missingReason`，照它补。
const MEDIA_HOST_SUFFIXES: [&str; 3] = [".kwimgs.com", ".yximgs.com", ".kwaicdn.com"];

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("network: {0}")]
    Network(String),
    #[error("HTTP {status}")]
    Http {
        status: u16,
        retry_after: Option<u64>,
        /// 3xx 的 `Location`。
        location: Option<String>,
    },
    #[error("unexpected response: {0}")]
    Shape(String),
    #[error("a response is larger than {limit} bytes")]
    TooLarge { limit: u64 },
    #[error("the host `{0}` is not one of the allowed media hosts")]
    ForbiddenHost(String),
    /// 连之前的检查没过：主机不在这种请求的名单里，或地址不是公网。
    #[error("refused to connect: {0}")]
    Refused(String),
    #[error("cannot write `{path}`: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    /// 平台回了，但不是能用的回答：带着回答的开头，宿主记进失败日志。
    #[error("{error}")]
    Answered {
        error: Box<ApiError>,
        answer: String,
    },
}

impl ApiError {
    /// 要停下来报错的：网络、写盘、限流。其余的在可选的那一步里只记进 `missing`，主体照样交出去。
    pub fn is_hard(&self) -> bool {
        match self {
            ApiError::Network(_) | ApiError::Io { .. } => true,
            ApiError::Http { status, .. } => *status == 429,
            ApiError::Shape(_)
            | ApiError::TooLarge { .. }
            | ApiError::ForbiddenHost(_)
            | ApiError::Refused(_) => false,
            ApiError::Answered { error, .. } => error.is_hard(),
        }
    }

    fn answered(self, raw: &[u8]) -> ApiError {
        let end = raw.len().min(4096);
        ApiError::Answered {
            error: Box::new(self),
            answer: String::from_utf8_lossy(&raw[..end]).into_owned(),
        }
    }
}

impl From<ApiError> for CallFailure {
    fn from(err: ApiError) -> CallFailure {
        failure(&err)
    }
}

fn failure(err: &ApiError) -> CallFailure {
    let message = match err {
        ApiError::Http {
            status,
            location: Some(l),
            ..
        } => format!("HTTP {status} redirecting to {l}"),
        _ => err.to_string(),
    };
    match err {
        ApiError::Network(_) => CallFailure::new(ErrorCode::Network, message),
        ApiError::Http {
            status,
            retry_after,
            location,
        } => match status {
            403 | 412 => blocked(message),
            // 跳去验证页的算被拦；别的跳转是页面改了
            300..=399
                if location.as_deref().is_some_and(|l| {
                    let l = l.to_ascii_lowercase();
                    l.contains("verify") || l.contains("captcha")
                }) =>
            {
                blocked(message)
            }
            404 => CallFailure::new(ErrorCode::NotFound, message),
            410 => CallFailure {
                reason: Some(NotFoundReason::Deleted),
                ..CallFailure::new(ErrorCode::NotFound, message)
            },
            429 => CallFailure {
                retry_after: *retry_after,
                hint: Some("快手提示请求太频繁，请过一会儿再试".into()),
                ..CallFailure::new(ErrorCode::RateLimited, message)
            },
            500..=599 => CallFailure::new(ErrorCode::Network, message),
            _ => CallFailure::new(ErrorCode::UpstreamChanged, message),
        },
        ApiError::Shape(_)
        | ApiError::TooLarge { .. }
        | ApiError::ForbiddenHost(_)
        | ApiError::Refused(_) => CallFailure::new(ErrorCode::UpstreamChanged, message),
        ApiError::Io { .. } => CallFailure::new(ErrorCode::Internal, message),
        ApiError::Answered { error, answer } => failure(error).with_detail(answer),
    }
}

pub fn blocked(message: String) -> CallFailure {
    CallFailure {
        hint: Some("快手出现了验证页；请稍后再试，也可以先在浏览器里打开快手完成验证".into()),
        ..CallFailure::new(ErrorCode::Blocked, message)
    }
}

/// 要连的主机和地址策略。正式运行用 `production`；测试把它指到本机的假服务器。
#[derive(Debug, Clone)]
pub struct Endpoints {
    page_base: Url,
    /// 测试用的本机主机：算短链主机，也算允许的媒体主机，媒体链接不升级成 https。
    local_host: Option<String>,
    policy: Policy,
}

impl Endpoints {
    pub fn production() -> Self {
        Self {
            page_base: Url::parse(&format!("https://{PAGE_HOST}/")).expect("constant URL"),
            local_host: None,
            policy: Policy::from_host(),
        }
    }

    /// 测试用：`base` 是 `http://127.0.0.1:<端口>`，`policy` 让这个本机地址算公网。
    pub fn local(base: &str, policy: Policy) -> Self {
        let page_base = Url::parse(base).expect("local base URL");
        let local_host = page_base.host_str().map(str::to_owned);
        Self {
            page_base,
            local_host,
            policy,
        }
    }

    pub fn is_short_link_host(&self, host: &str) -> bool {
        host == SHORT_LINK_HOST || self.local_host.as_deref() == Some(host)
    }

    fn page_host(&self) -> &str {
        self.page_base.host_str().unwrap_or_default()
    }

    fn is_media_host(&self, host: &str) -> bool {
        self.local_host.as_deref() == Some(host)
            || MEDIA_HOST_SUFFIXES
                .iter()
                .any(|s| host.len() > s.len() && host.ends_with(s))
    }

    fn client(&self, user_agent: &str, hosts: &[&str]) -> Client {
        let local = self.local_host.as_deref();
        Client::new(user_agent, Hosts::only(hosts.iter().copied().chain(local)))
            .with_policy(self.policy)
    }

    /// 页面给的媒体地址：补 scheme、升级成 https、核对主机。
    pub fn media_url(&self, raw: &str) -> Result<Url, ApiError> {
        let raw = raw.trim();
        let full = if raw.starts_with("//") {
            format!("https:{raw}")
        } else {
            raw.to_owned()
        };
        let mut url = Url::parse(&full)
            .map_err(|_| ApiError::Shape("a media address is not a URL".into()))?;
        let host = url
            .host_str()
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if !self.is_media_host(&host) {
            return Err(ApiError::ForbiddenHost(host));
        }
        let local = self.local_host.as_deref() == Some(host.as_str());
        // 清单的 `network.hosts` 没写端口，只放行默认端口
        if !local && url.port().is_some() {
            return Err(ApiError::ForbiddenHost(format!(
                "{host}:{}",
                url.port().unwrap_or_default()
            )));
        }
        if url.scheme() == "http" && !local {
            url.set_scheme("https")
                .map_err(|()| ApiError::Shape("a media address cannot use https".into()))?;
        }
        if url.scheme() != "https" && !local {
            return Err(ApiError::Shape("a media address is not http(s)".into()));
        }
        Ok(url)
    }
}

fn transport(e: http::Error) -> ApiError {
    match e {
        http::Error::TooLarge(limit) => ApiError::TooLarge { limit },
        e if e.is_refusal() => ApiError::Refused(e.to_string()),
        e => ApiError::Network(e.to_string()),
    }
}

fn status_error(response: &http::Response) -> ApiError {
    ApiError::Http {
        status: response.status(),
        retry_after: response
            .header("retry-after")
            .and_then(|v| v.trim().parse().ok()),
        location: response.header("location").map(str::to_owned),
    }
}

/// 一次调用里站点发的 cookie：名字、值、能收到它的主机和路径。值不进 `Debug`。
#[derive(Default)]
pub struct CookieJar(Vec<StoredCookie>);

struct StoredCookie {
    name: String,
    value: String,
    /// 没有 `Domain` 时只有发它的主机收得到（`host_only`）；有就是它和它的子域。
    domain: String,
    host_only: bool,
    path: String,
}

impl std::fmt::Debug for CookieJar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CookieJar({} cookies, <redacted>)", self.0.len())
    }
}

impl CookieJar {
    /// 记下 `from` 这个请求的回答里的一条 `Set-Cookie`。`Domain` 不是发它的主机或其上级的丢掉；
    /// 读不出名字的丢掉。
    fn store(&mut self, from: &Url, set_cookie: &str) {
        let host = from.host_str().unwrap_or_default().to_ascii_lowercase();
        let mut parts = set_cookie.split(';');
        let Some((name, value)) = parts.next().and_then(|p| p.split_once('=')) else {
            return;
        };
        let (name, value) = (name.trim(), value.trim());
        if name.is_empty() {
            return;
        }
        let mut domain = None;
        let mut path = None;
        for attr in parts {
            let (key, val) = attr.split_once('=').unwrap_or((attr, ""));
            let val = val.trim();
            match key.trim().to_ascii_lowercase().as_str() {
                "domain" => {
                    domain = Some(val.trim_start_matches('.').to_ascii_lowercase());
                }
                "path" if val.starts_with('/') => path = Some(val.to_owned()),
                _ => {}
            }
        }
        let (domain, host_only) = match domain.filter(|d| !d.is_empty()) {
            Some(d) if domain_matches(&host, &d) => (d, false),
            Some(_) => return,
            None => (host, true),
        };
        let path = path.unwrap_or_else(|| default_path(from.path()));
        self.0
            .retain(|c| !(c.name == name && c.domain == domain && c.path == path));
        self.0.push(StoredCookie {
            name: name.to_owned(),
            value: value.to_owned(),
            domain,
            host_only,
            path,
        });
    }

    /// 发给 `url` 的 `Cookie` 请求头：主机和路径对得上的那几条；一条都没有就是 `None`。
    /// 只给这一个主机，别的主机的那一跳不带（`usuyo_plugin_kit` 的 `Cookie` 也只认这一个名字）。
    fn cookie_for(&self, url: &Url) -> Option<Cookie> {
        let host = url.host_str()?.to_ascii_lowercase();
        let pairs: Vec<String> = self
            .0
            .iter()
            .filter(|c| {
                let host_ok = if c.host_only {
                    host == c.domain
                } else {
                    domain_matches(&host, &c.domain)
                };
                host_ok && path_matches(url.path(), &c.path)
            })
            .map(|c| format!("{}={}", c.name, c.value))
            .collect();
        (!pairs.is_empty()).then(|| Cookie::new(pairs.join("; "), [host]))
    }
}

/// `host` 是 `domain` 本身或它的子域。
fn domain_matches(host: &str, domain: &str) -> bool {
    host == domain
        || host
            .strip_suffix(domain)
            .is_some_and(|rest| rest.ends_with('.'))
}

fn default_path(request_path: &str) -> String {
    match request_path.rfind('/') {
        Some(0) | None => "/".to_owned(),
        Some(i) => request_path[..i].to_owned(),
    }
}

fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    request_path == cookie_path
        || request_path
            .strip_prefix(cookie_path)
            .is_some_and(|rest| cookie_path.ends_with('/') || rest.starts_with('/'))
}

pub struct Kuaishou {
    endpoints: Endpoints,
}

impl Kuaishou {
    pub fn new(endpoints: Endpoints) -> Self {
        Self { endpoints }
    }

    pub fn endpoints(&self) -> &Endpoints {
        &self.endpoints
    }

    /// 取手机页 `fw/photo/<id>` 的 HTML；`query` 是分享链接上的参数，原样带上；
    /// `cookies` 是展开短链时站点发的 cookie，主机对得上的才带（直接给作品链接时是空的）。
    /// 不跟跳转，状态不是 2xx 就报错，带着回答的开头。
    pub fn mobile_page(
        &self,
        id: &str,
        query: Option<&str>,
        cookies: &CookieJar,
    ) -> Result<String, ApiError> {
        let mut url = self
            .endpoints
            .page_base
            .join(&format!("fw/photo/{id}"))
            .map_err(|e| ApiError::Shape(format!("bad page URL: {e}")))?;
        url.set_query(query);
        step("手机页");
        let client = self
            .endpoints
            .client(PAGE_USER_AGENT, &[self.endpoints.page_host()]);
        let mut request = client
            .get(&url)
            .header(
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .timeout(PAGE_TIMEOUT);
        if let Some(cookie) = cookies.cookie_for(&url) {
            request = request.cookie(&cookie);
        }
        let mut response = request.send().map_err(transport)?;
        if !(200..300).contains(&response.status()) {
            let e = status_error(&response);
            let head = response.read_to_vec(4096).unwrap_or_default();
            return Err(e.answered(&head));
        }
        let body = response.read_to_vec(PAGE_BYTES).map_err(transport)?;
        Ok(String::from_utf8_lossy(&body).into_owned())
    }

    /// 展开短链：一跳一跳读 `Location`，只请求短链主机（`v.kuaishou.com`、`www.kuaishou.com`）。
    /// 落到作品页（或别的主机）就停，不请求作品页。
    /// 跳转只许落在快手自己的主机上：别的主机原样交回调用方，由它当作「不是快手的链接」。
    /// 每一跳回答里的 `Set-Cookie` 记进返回的 `CookieJar`，带到后面主机对得上的跳转里。
    pub fn expand_short_link(&self, start: Url) -> Result<(Url, CookieJar), ApiError> {
        let mut jar = CookieJar::default();
        let mut current = start;
        for _ in 0..MAX_SHORT_LINK_HOPS {
            let host = current
                .host_str()
                .map(str::to_ascii_lowercase)
                .unwrap_or_default();
            if !(self.endpoints.is_short_link_host(&host) || host == WEB_HOST) {
                return Ok((current, jar));
            }
            let client = self
                .endpoints
                .client(USER_AGENT, &[SHORT_LINK_HOST, WEB_HOST]);
            let mut request = client.get(&current).timeout(SHORT_LINK_TIMEOUT);
            if let Some(cookie) = jar.cookie_for(&current) {
                request = request.cookie(&cookie);
            }
            let mut response = request.send().map_err(transport)?;
            for set_cookie in response.headers("set-cookie") {
                jar.store(&current, set_cookie);
            }
            let status = response.status();
            if !(300..400).contains(&status) {
                if !(200..300).contains(&status) {
                    let e = status_error(&response);
                    let head = response.read_to_vec(4096).unwrap_or_default();
                    return Err(e.answered(&head));
                }
                return Ok((current, jar));
            }
            let location = response
                .header("location")
                .ok_or_else(|| ApiError::Shape("a redirect without `Location`".into()))?;
            let next = current
                .join(location)
                .map_err(|_| ApiError::Shape("`Location` is not a URL".into()))?;
            if !matches!(next.scheme(), "http" | "https") {
                return Ok((next, jar));
            }
            // 只有快手自己的主机才继续；落在作品页上就停，作品页不在这里请求，交回去认 id
            let next_host = next.host_str().unwrap_or_default();
            if !is_kuaishou_host(next_host) && !self.endpoints.is_short_link_host(next_host) {
                return Ok((next, jar));
            }
            match parse_input(next.as_str(), |h| self.endpoints.is_short_link_host(h)) {
                Parsed::ShortLink(_) => current = next,
                Parsed::Post { .. } | Parsed::Foreign => return Ok((next, jar)),
            }
        }
        Err(ApiError::Shape("too many redirects".into()))
    }

    /// 把一个文件流式存到 `dest`，最多 `limit` 字节。跟随跳转，每一跳都要在媒体主机名单里。
    /// 不带 cookie（`CookieJar` 里的也不带）。每秒向宿主报一次进度。
    pub fn download(
        &self,
        url: &Url,
        dest: &Path,
        limit: u64,
        timeout: Duration,
        label: &str,
    ) -> Result<u64, ApiError> {
        let response = self
            .endpoints
            .client(USER_AGENT, &MEDIA_HOST_SUFFIXES)
            .get(url)
            .header("Referer", MEDIA_REFERER)
            .follow_redirects(MEDIA_REDIRECTS)
            .timeout(timeout)
            .send()
            .map_err(transport)?;
        if !(200..300).contains(&response.status()) {
            return Err(status_error(&response));
        }
        let total = response
            .header("content-length")
            .and_then(|v| v.parse::<u64>().ok());
        if total.is_some_and(|t| t > limit) {
            return Err(ApiError::TooLarge { limit });
        }
        let io = |source| ApiError::Io {
            path: dest.display().to_string(),
            source,
        };
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let file = std::fs::File::create_new(dest).map_err(io)?;
        let written = copy_limited(response.into_reader(limit), file, limit, total, label);
        if written.is_err() {
            // 写了一半的文件不留在输出目录里：宿主会把没被引用的文件也原样收下。
            // 清理失败不改变这次调用的结果，调用失败时整个目录本来就会被丢掉。
            let _ = std::fs::remove_file(dest);
        }
        written.map_err(|e| match e {
            CopyError::Api(e) => e,
            CopyError::Io(source) => io(source),
        })
    }
}

enum CopyError {
    Api(ApiError),
    Io(std::io::Error),
}

/// 把响应体流式写进文件，`reader` 读出超过 `limit` 字节时报错，每秒报一次进度。
fn copy_limited(
    mut reader: impl Read,
    mut file: std::fs::File,
    limit: u64,
    total: Option<u64>,
    label: &str,
) -> Result<u64, CopyError> {
    let mut buffer = vec![0u8; 64 << 10];
    let mut written = 0u64;
    let mut last_report = Instant::now();
    loop {
        let n = reader.read(&mut buffer).map_err(|e| {
            CopyError::Api(if e.kind() == std::io::ErrorKind::FileTooLarge {
                ApiError::TooLarge { limit }
            } else {
                ApiError::Network(e.to_string())
            })
        })?;
        if n == 0 {
            break;
        }
        written += n as u64;
        file.write_all(&buffer[..n]).map_err(CopyError::Io)?;
        if last_report.elapsed() >= Duration::from_secs(1) {
            progress(written, total, label);
            last_report = Instant::now();
        }
    }
    file.flush().map_err(CopyError::Io)?;
    progress(written, total, label);
    Ok(written)
}

/// 开始一步之前告诉宿主：一步一步慢下来时，宿主照样收得到 progress，不按「没进度」杀掉进程。
pub(crate) fn step(label: &str) {
    let event = PluginEvent::Progress {
        done: None,
        total: None,
        unit: None,
        message: Some(label.into()),
    };
    eprintln!("{}", event.stderr_line());
}

fn progress(done: u64, total: Option<u64>, label: &str) {
    let event = PluginEvent::Progress {
        done: Some(done as f64),
        total: total.map(|t| t as f64),
        unit: Some("B".into()),
        message: Some(label.into()),
    };
    eprintln!("{}", event.stderr_line());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_hosts() {
        let e = Endpoints::production();
        let ok = |s: &str| e.media_url(s).map(|u| u.to_string());
        assert_eq!(
            ok("http://tymov2.a.kwimgs.com/upic/2025/a.mp4?pkg=1").unwrap(),
            "https://tymov2.a.kwimgs.com/upic/2025/a.mp4?pkg=1"
        );
        assert!(ok("https://p5.a.yximgs.com/upic/a.jpg").is_ok());
        assert!(ok("https://v23-3.kwaicdn.com/upic/a.mp4").is_ok());
        assert!(ok("//js2.a.yximgs.com/ufile/atlas/a.webp").is_ok());
        assert!(matches!(
            ok("https://p5.a.yximgs.com:8443/a"),
            Err(ApiError::ForbiddenHost(_))
        ));
        for bad in [
            "https://kwimgs.com/a",
            "https://evilkwimgs.com/a",
            "https://kwimgs.com.evil.test/a",
            "https://127.0.0.1/a",
        ] {
            assert!(matches!(ok(bad), Err(ApiError::ForbiddenHost(_))), "{bad}");
        }
        assert!(matches!(
            ok("ftp://a.kwimgs.com/a"),
            Err(ApiError::Shape(_))
        ));
        assert!(matches!(ok("nonsense"), Err(ApiError::Shape(_))));
    }

    fn jar_with(from: &str, set_cookie: &str) -> CookieJar {
        let mut jar = CookieJar::default();
        jar.store(&Url::parse(from).unwrap(), set_cookie);
        jar
    }

    fn sent(jar: &CookieJar, to: &str) -> bool {
        jar.cookie_for(&Url::parse(to).unwrap()).is_some()
    }

    #[test]
    fn cookies_go_only_to_the_hosts_they_belong_to() {
        // 有 Domain：它和子域收得到，别的快手主机、CDN 收不到
        let jar = jar_with(
            "https://v.kuaishou.com/mkYOSR",
            "did=web_x; Domain=.kuaishou.com; Path=/",
        );
        assert!(sent(&jar, "https://www.kuaishou.com/short-video/1"));
        assert!(sent(&jar, "https://v.kuaishou.com/a"));
        assert!(!sent(&jar, "https://v.m.chenzhongtech.com/fw/photo/1"));
        assert!(!sent(&jar, "https://p5.a.yximgs.com/a.jpg"));
        assert!(!sent(&jar, "https://evilkuaishou.com/a"));
        // 没有 Domain：只有发它的主机
        let jar = jar_with("https://v.kuaishou.com/mkYOSR", "did=web_x");
        assert!(sent(&jar, "https://v.kuaishou.com/b"));
        assert!(!sent(&jar, "https://www.kuaishou.com/b"));
        // Domain 不是发它的主机的上级：丢掉
        let jar = jar_with("https://v.kuaishou.com/x", "did=web_x; Domain=example.com");
        assert!(!sent(&jar, "https://example.com/"));
        assert!(!sent(&jar, "https://v.kuaishou.com/"));
        // 路径
        let jar = jar_with("https://v.kuaishou.com/x", "a=1; Path=/fw");
        assert!(sent(&jar, "https://v.kuaishou.com/fw/photo/1"));
        assert!(!sent(&jar, "https://v.kuaishou.com/fwx"));
    }

    #[test]
    fn cookies_join_replace_and_stay_out_of_debug() {
        let mut jar = jar_with("https://v.kuaishou.com/x", "did=old");
        jar.store(&Url::parse("https://v.kuaishou.com/y").unwrap(), "kpf=H5");
        jar.store(&Url::parse("https://v.kuaishou.com/z").unwrap(), "did=new");
        jar.store(
            &Url::parse("https://v.kuaishou.com/z").unwrap(),
            "=nameless",
        );
        let cookie = jar
            .cookie_for(&Url::parse("https://v.kuaishou.com/a").unwrap())
            .unwrap();
        assert_eq!(format!("{cookie:?}"), "Cookie(<redacted>)");
        let stored: Vec<String> = jar
            .0
            .iter()
            .map(|c| format!("{}={}", c.name, c.value))
            .collect();
        assert_eq!(stored, ["kpf=H5", "did=new"]);
        assert!(!format!("{jar:?}").contains("new"));
    }

    #[test]
    fn failure_mapping() {
        let http = |status, location: Option<&str>| {
            CallFailure::from(ApiError::Http {
                status,
                retry_after: Some(7),
                location: location.map(str::to_owned),
            })
        };
        assert_eq!(http(403, None).code, ErrorCode::Blocked);
        assert_eq!(http(412, None).code, ErrorCode::Blocked);
        assert_eq!(http(429, None).retry_after, Some(7));
        assert_eq!(http(502, None).code, ErrorCode::Network);
        assert_eq!(http(404, None).code, ErrorCode::NotFound);
        assert_eq!(http(410, None).reason, Some(NotFoundReason::Deleted));
        assert_eq!(
            http(
                302,
                Some("https://www.kuaishou.com/short-video/3x24h4gjvevux4w")
            )
            .code,
            ErrorCode::UpstreamChanged
        );
        assert_eq!(
            http(302, Some("https://captcha.zt.kuaishou.com/x")).code,
            ErrorCode::Blocked
        );
    }
}
