//! `ks-downloader` 方法：全部离线。本机起一个假的 KS-Downloader（形状照 `tests/fixtures/ks-downloader/NOTES.md`），
//! 不运行真的 KS-Downloader，不连快手。库函数的测试把服务和图片视频都指到本机；
//! 起插件进程的测试清空环境，代理指向一个关着的端口，万一哪条路径连了别处，只会失败。

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jiff::tz::TimeZone;
use serde_json::{Value, json};
use usuyo_fetch_kuaishou::api::{Endpoints, Kuaishou};
use usuyo_fetch_kuaishou::ksdownloader::{self, Service};
use usuyo_plugin::protocol::{
    Access, Account, CallFailure, Completeness, ErrorCode, FetchOutput, Login, OutputKind,
    PluginKind, PluginManifest,
};
use usuyo_plugin_kit::http::Policy;

const ID: &str = "3xsamplevideo001";
const JPEG: &[u8] = b"\xff\xd8\xff\xe0fake-jpeg";
const MP4: &[u8] = b"\x00\x00\x00\x18ftypmp42fake-video-bytes";
const BIN: &str = env!("CARGO_BIN_EXE_usuyo-fetch-kuaishou");
const DEAD_PROXY: &str = "http://127.0.0.1:9";

fn link() -> String {
    format!("https://www.kuaishou.com/short-video/{ID}")
}

fn fixture(name: &str, base: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/ks-downloader")
        .join(name);
    std::fs::read_to_string(path)
        .unwrap()
        .replace("{BASE}", base)
        .replace("{ID}", ID)
        .replace("{LINK}", &link())
}

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: String,
}

struct Reply {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    /// 回答之前先等多久。
    delay: Duration,
}

fn json_reply(body: impl Into<Vec<u8>>) -> Reply {
    Reply {
        status: 200,
        content_type: "application/json",
        body: body.into(),
        delay: Duration::ZERO,
    }
}

fn status_reply(status: u16, body: &str) -> Reply {
    Reply {
        status,
        ..json_reply(body)
    }
}

/// 本机的假 KS-Downloader，也兼当图片视频的主机：`/img/*`、`/media/*`。
struct FakeService {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl FakeService {
    fn start(route: impl Fn(&Seen, &str) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let route = Arc::new(route);
        let (log, served) = (seen.clone(), base.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let (route, log, base) = (route.clone(), log.clone(), served.clone());
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let mut parts = line.split_whitespace();
                    let method = parts.next().unwrap_or_default().to_owned();
                    let path = parts.next().unwrap_or("/").to_owned();
                    let mut headers = BTreeMap::new();
                    loop {
                        let mut header = String::new();
                        if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                            break;
                        }
                        if let Some((k, v)) = header.split_once(':') {
                            headers.insert(k.to_ascii_lowercase(), v.trim().to_owned());
                        }
                    }
                    let length: usize = headers
                        .get("content-length")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    let mut body = vec![0u8; length];
                    reader.read_exact(&mut body).ok();
                    let seen = Seen {
                        method,
                        path,
                        headers,
                        body: String::from_utf8_lossy(&body).into_owned(),
                    };
                    log.lock().unwrap().push(seen.clone());
                    let reply = route(&seen, &base);
                    std::thread::sleep(reply.delay);
                    let head = format!(
                        "HTTP/1.1 {} X\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        reply.status,
                        reply.content_type,
                        reply.body.len()
                    );
                    stream.write_all(head.as_bytes()).ok();
                    stream.write_all(&reply.body).ok();
                });
            }
        });
        Self { base, seen }
    }

    /// 答 `file` 这份样例，图片视频照常给。
    fn answering(file: &'static str) -> Self {
        Self::start(move |seen, base| media_or(seen, || json_reply(fixture(file, base))))
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn detail_requests(&self) -> Vec<Seen> {
        self.seen()
            .into_iter()
            .filter(|s| s.path == "/detail/")
            .collect()
    }

    fn media_requests(&self) -> Vec<Seen> {
        self.seen()
            .into_iter()
            .filter(|s| s.path.starts_with("/img/") || s.path.starts_with("/media/"))
            .collect()
    }

    fn service(&self) -> Service {
        Service::new(&self.base).unwrap().with_zone(TimeZone::UTC)
    }
}

fn media_or(seen: &Seen, otherwise: impl FnOnce() -> Reply) -> Reply {
    if seen.path.contains("missing") {
        status_reply(404, "")
    } else if seen.path.starts_with("/img/") {
        Reply {
            content_type: "image/jpeg",
            ..json_reply(JPEG)
        }
    } else if seen.path.starts_with("/media/") {
        Reply {
            content_type: "video/mp4",
            ..json_reply(MP4)
        }
    } else {
        otherwise()
    }
}

/// 一个没人监听的本机地址。
fn closed_base() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://{}", listener.local_addr().unwrap())
}

fn local_policy() -> Policy {
    Policy {
        lookup: |_, _| Err(std::io::Error::other("no DNS in tests")),
        public: |ip| ip == IpAddr::from([127, 0, 0, 1]),
        cookies_over_http: true,
        ..Policy::strict()
    }
}

fn api(base: &str) -> Kuaishou {
    Kuaishou::new(Endpoints::local(base, local_policy()))
}

fn run_with(
    fake: &FakeService,
    service: &Service,
    input: &str,
    out: &Path,
    want: Option<&[OutputKind]>,
) -> Result<FetchOutput, CallFailure> {
    ksdownloader::fetch(&api(&fake.base), service, input, out, want)
}

fn run(
    fake: &FakeService,
    input: &str,
    out: &Path,
    want: Option<&[OutputKind]>,
) -> Result<FetchOutput, CallFailure> {
    run_with(fake, &fake.service(), input, out, want)
}

fn failure_json(failure: &CallFailure) -> Value {
    serde_json::to_value(failure).unwrap()
}

/// 输出目录里的文件，相对路径。
fn files(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, base: &Path, into: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, base, into);
            } else {
                into.push(
                    path.strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    let mut found = Vec::new();
    walk(dir, dir, &mut found);
    found.sort();
    found
}

#[test]
fn video_post_through_the_service() {
    let fake = FakeService::answering("video.json");
    let out = tempfile::tempdir().unwrap();
    let got = run(&fake, &link(), out.path(), None).unwrap();

    // 一次请求，POST /detail/，体里只有 text；没有 cookie、令牌，没有 cookies、proxy 字段
    let requests = fake.detail_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, "POST");
    let body: Value = serde_json::from_str(&request.body).unwrap();
    assert_eq!(body, json!({"text": link()}));
    for header in ["cookie", "authorization"] {
        assert!(!request.headers.contains_key(header), "{header}");
    }

    let value = serde_json::to_value(&got).unwrap();
    // 和 share-page 同一个身份
    assert_eq!(
        value["identity"],
        json!({"platform": "kuaishou", "contentId": ID, "url": format!("https://www.kuaishou.com/short-video/{ID}")})
    );
    assert_eq!(value["kind"], "video");
    assert_eq!(value["title"], "三台磨豆机，同一支豆子。 #手冲");
    assert_eq!(value["text"], "三台磨豆机，同一支豆子。 #手冲");
    assert_eq!(value["author"]["name"], "咖啡日常");
    assert_eq!(value["author"]["platformId"], "3xauthorsample01");
    assert_eq!(value["publishedAt"], "2026-10-01T12:00:00Z");
    assert_eq!(value["completeness"], "full");
    assert_eq!(value["missing"], json!([]));
    assert_eq!(value["images"], json!(["images/cover.jpg"]));
    assert_eq!(value["media"][0]["path"], "media/video.mp4");
    assert_eq!(value["media"][0]["kind"], "video");
    assert_eq!(value["media"][0]["durationSec"], 149.0);
    assert_eq!(value["evidence"]["access"], "public-web");
    assert!(value["evidence"].get("site").is_none());
    assert_eq!(got.completeness, Completeness::Full);

    // 文件名是固定的；视频字节原样
    assert_eq!(
        files(out.path()),
        [
            "images/cover.jpg",
            "ks-downloader.json",
            "media/video.mp4",
            "meta.json"
        ]
    );
    assert_eq!(
        std::fs::read(out.path().join("media/video.mp4")).unwrap(),
        MP4
    );
    // 留下的只有 data：请求的回显（params）不进输出目录
    let raw: Value =
        serde_json::from_slice(&std::fs::read(out.path().join("ks-downloader.json")).unwrap())
            .unwrap();
    assert_eq!(raw["detailID"], ID);
    assert!(raw.get("params").is_none());
    let meta: Value =
        serde_json::from_slice(&std::fs::read(out.path().join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["counts"]["likeCount"], 1234);
    assert_eq!(meta["counts"]["viewCount"], 56789);
    assert_eq!(meta["counts"]["commentCount"], 17);
    // 没有的数（-1）不记
    assert!(meta["counts"].get("shareCount").is_none());
}

#[test]
fn the_identity_is_the_one_share_page_gives() {
    let fake = FakeService::answering("video.json");
    let out = tempfile::tempdir().unwrap();
    let got = run(&fake, &link(), out.path(), None).unwrap();
    assert_eq!(
        got.identity,
        Some(usuyo_fetch_kuaishou::canonical::identity(ID).unwrap())
    );
}

#[test]
fn image_post_with_three_pictures() {
    let fake = FakeService::answering("images.json");
    let out = tempfile::tempdir().unwrap();
    let got = run(&fake, &link(), out.path(), None).unwrap();
    let value = serde_json::to_value(&got).unwrap();
    assert_eq!(value["kind"], "post");
    assert_eq!(
        value["images"],
        json!([
            "images/cover.jpg",
            "images/001.jpg",
            "images/002.jpg",
            "images/003.jpg"
        ])
    );
    assert_eq!(value["media"], json!([]));
    // 作者名退回成 id 时照写
    assert_eq!(value["author"]["name"], "3xauthorsample02");
    assert_eq!(value["publishedAt"], "2026-10-02T08:15:30Z");
    assert_eq!(value["completeness"], "full");
    // 图集本来没有视频：不算缺
    assert_eq!(value["missing"], json!([]));
    assert_eq!(fake.media_requests().len(), 4);
}

#[test]
fn a_single_picture_and_an_unknown_time() {
    let fake = FakeService::answering("single-image.json");
    let out = tempfile::tempdir().unwrap();
    let got = run(&fake, &link(), out.path(), None).unwrap();
    let value = serde_json::to_value(&got).unwrap();
    assert_eq!(value["kind"], "post");
    // 封面就是那一张图，只下一次
    assert_eq!(value["images"], json!(["images/001.jpg"]));
    assert_eq!(fake.media_requests().len(), 1);
    assert!(value.get("publishedAt").is_none());
    assert!(value.get("text").is_none());
    assert_eq!(value["completeness"], "full");
}

#[test]
fn a_picture_without_any_address_is_a_video_the_service_could_not_give() {
    let fake = FakeService::answering("cover-only.json");
    let out = tempfile::tempdir().unwrap();
    let got = run(&fake, &link(), out.path(), None).unwrap();
    let value = serde_json::to_value(&got).unwrap();
    assert_eq!(value["completeness"], "partial");
    assert_eq!(value["missing"], json!(["media:video"]));
    let reason = value["missingReason"].as_str().unwrap();
    assert!(reason.contains("KS-Downloader"), "{reason}");
    assert!(reason.contains("视频"), "{reason}");
    assert_eq!(value["images"], json!(["images/cover.jpg"]));
}

#[test]
fn only_what_was_wanted_is_downloaded() {
    let fake = FakeService::answering("video.json");
    let out = tempfile::tempdir().unwrap();
    let got = run(
        &fake,
        &link(),
        out.path(),
        Some(&[OutputKind::Text, OutputKind::Meta]),
    )
    .unwrap();
    assert!(fake.media_requests().is_empty());
    assert_eq!(got.completeness, Completeness::Full);
    assert!(got.images.is_empty() && got.media.is_empty());
    assert_eq!(got.text.as_deref(), Some("三台磨豆机，同一支豆子。 #手冲"));
    // 本方法给不了的输出：invalid-args，不发请求
    let other = tempfile::tempdir().unwrap();
    let err = run(&fake, &link(), other.path(), Some(&[OutputKind::Comments])).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgs);
    assert_eq!(fake.detail_requests().len(), 1);
}

#[test]
fn a_picture_that_cannot_be_downloaded_is_reported_missing() {
    let fake = FakeService::start(|seen, base| {
        media_or(seen, || {
            json_reply(fixture("images.json", base).replace("/img/a_1.jpg", "/img/missing.jpg"))
        })
    });
    let out = tempfile::tempdir().unwrap();
    let got = run(&fake, &link(), out.path(), None).unwrap();
    assert_eq!(got.completeness, Completeness::Partial);
    assert_eq!(got.missing, [OutputKind::Images]);
    assert!(got.missing_reason.as_deref().unwrap().contains("图片 2"));
    // 三张里下到了两张，加封面
    assert_eq!(got.images.len(), 3);
}

#[test]
fn a_video_on_a_host_outside_the_media_hosts_is_not_requested() {
    let fake = FakeService::start(|seen, base| {
        media_or(seen, || {
            json_reply(fixture("video.json", base).replace(
                &format!("{base}/media/video.mp4"),
                "https://evil.example/a.mp4",
            ))
        })
    });
    let out = tempfile::tempdir().unwrap();
    let got = run(&fake, &link(), out.path(), None).unwrap();
    assert_eq!(got.missing, [OutputKind::Video]);
    assert!(got.media.is_empty());
    assert!(
        fake.media_requests()
            .iter()
            .all(|r| r.path.starts_with("/img/"))
    );
}

#[test]
fn a_short_link_goes_to_the_service_unchanged() {
    let fake = FakeService::answering("video.json");
    let out = tempfile::tempdir().unwrap();
    let text = "https://v.kuaishou.com/mkYOSR 「手冲咖啡」复制此消息，打开【快手】直接观看！";
    let got = run(&fake, text, out.path(), None).unwrap();
    // 身份来自回答里的 detailID；插件没有自己展开短链，请求只到了假服务
    assert_eq!(
        serde_json::to_value(&got).unwrap()["identity"]["contentId"],
        ID
    );
    let requests = fake.detail_requests();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_str(&requests[0].body).unwrap();
    assert_eq!(body["text"], text);
    assert!(fake.seen().iter().all(|r| r.path == "/detail/"
        || r.path.starts_with("/img/")
        || r.path.starts_with("/media/")));
}

#[test]
fn a_phone_page_link_keeps_its_parameters() {
    // 服务取手机页链接要里面的 userId、photoId，所以原样交给它
    let fake = FakeService::answering("video.json");
    let out = tempfile::tempdir().unwrap();
    let input = format!(
        "https://v.m.chenzhongtech.com/fw/photo/{ID}?userId=3xauthorsample01&photoId={ID}&cc=share_copylink"
    );
    run(&fake, &input, out.path(), None).unwrap();
    let body: Value = serde_json::from_str(&fake.detail_requests()[0].body).unwrap();
    assert_eq!(body["text"], input);
}

#[test]
fn the_english_interface_of_the_service_is_understood() {
    // `Image` 在 single-image.json 里；这里把视频的 `视频` 换成 `Video`
    let fake = FakeService::start(|seen, base| {
        media_or(seen, || {
            json_reply(fixture("video.json", base).replace("\"视频\"", "\"Video\""))
        })
    });
    let out = tempfile::tempdir().unwrap();
    let got = run(&fake, &link(), out.path(), None).unwrap();
    assert_eq!(got.media.len(), 1);
}

#[test]
fn a_post_other_than_the_link_is_refused() {
    let fake = FakeService::answering("video.json");
    let out = tempfile::tempdir().unwrap();
    let other = "https://www.kuaishou.com/short-video/3xsomeotherpost1";
    let err = run(&fake, other, out.path(), None).unwrap_err();
    assert_eq!(err.code, ErrorCode::UpstreamChanged);
    assert!(err.detail.is_some());
    assert!(fake.media_requests().is_empty());
    assert!(files(out.path()).is_empty());
}

#[test]
fn a_link_that_is_not_kuaishou_sends_nothing() {
    let fake = FakeService::answering("video.json");
    let out = tempfile::tempdir().unwrap();
    for input in [
        "https://example.com/short-video/3xsamplevideo001",
        "https://www.kuaishou.com/profile/3xdng3ssi9kdpk2",
        "not a link",
        "",
    ] {
        let err = run(&fake, input, out.path(), None).unwrap_err();
        assert_eq!(err.code, ErrorCode::UnsupportedInput, "{input}");
    }
    let err = run(&fake, &"a".repeat(5000), out.path(), None).unwrap_err();
    assert_eq!(err.code, ErrorCode::UnsupportedInput);
    assert!(fake.seen().is_empty());
}

#[test]
fn the_service_is_not_running() {
    let base = closed_base();
    let service = Service::new(&base).unwrap();
    let fake = FakeService::answering("video.json");
    let out = tempfile::tempdir().unwrap();
    let err = run_with(&fake, &service, &link(), out.path(), None).unwrap_err();
    let value = failure_json(&err);
    assert_eq!(value["code"], "dependency-missing");
    assert_eq!(value["needs"], json!(["service:endpoint"]));
    let hint = value["hint"].as_str().unwrap();
    assert!(
        hint.contains("没有在运行") && hint.contains("python main.py api --host 127.0.0.1"),
        "{hint}"
    );
    assert!(fake.seen().is_empty());
}

#[test]
fn error_answers_of_the_service() {
    let out = tempfile::tempdir().unwrap();
    // 失败时 data 是 null：说不清原因，一律 not-found，原文进 detail；中文、英文界面一样
    for file in ["no-data.json", "no-data-en.json"] {
        let fake = FakeService::answering(file);
        let err = run(&fake, &link(), out.path(), None).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound, "{file}");
        assert!(err.detail.as_deref().unwrap().contains("\"data\": null"));
    }
    // 其余都是插件认不出的回答：upstream-changed，不是网络、限流，宿主不会干等
    let cases: [(&str, u16, &str); 10] = [
        (
            "422",
            422,
            r#"{"detail":[{"loc":["body","text"],"msg":"Field required"}]}"#,
        ),
        ("500", 500, "Internal Server Error"),
        ("404", 404, r#"{"detail":"Not Found"}"#),
        ("not json", 200, "<html>hello</html>"),
        (
            "no message",
            200,
            r#"{"data":{"detailID":"3xsamplevideo001"}}"#,
        ),
        ("no data", 200, r#"{"message":"x"}"#),
        ("data is a list", 200, r#"{"message":"x","data":[1]}"#),
        ("empty data", 200, r#"{"message":"x","data":{}}"#),
        (
            "unknown type",
            200,
            r#"{"message":"x","data":{"detailID":"3xsamplevideo001","photoType":"直播","download":""}}"#,
        ),
        (
            "download is a number",
            200,
            r#"{"message":"x","data":{"detailID":"3xsamplevideo001","photoType":"视频","download":5}}"#,
        ),
    ];
    for (name, status, body) in cases {
        let fake = FakeService::start(move |_, _| status_reply(status, body));
        let err = run(&fake, &link(), out.path(), None).unwrap_err();
        assert_eq!(err.code, ErrorCode::UpstreamChanged, "{name}");
        assert!(err.detail.is_some(), "{name}");
        assert!(files(out.path()).is_empty(), "{name}");
    }
    // 没有 detailID、detailID 不像作品 id
    for body in [
        r#"{"message":"x","data":{"photoType":"视频","download":""}}"#,
        r#"{"message":"x","data":{"detailID":"Unknown","photoType":"视频","download":""}}"#,
        r#"{"message":"x","data":{"detailID":"../../x","photoType":"视频","download":""}}"#,
    ] {
        let fake = FakeService::start(move |_, _| status_reply(200, body));
        let err = run(&fake, &link(), out.path(), None).unwrap_err();
        assert_eq!(err.code, ErrorCode::UpstreamChanged, "{body}");
    }
}

#[test]
fn a_service_that_never_answers_times_out() {
    let fake = FakeService::start(|_, _| Reply {
        delay: Duration::from_millis(1500),
        ..json_reply("{}")
    });
    let service = fake.service().with_timeout(Duration::from_millis(300));
    let out = tempfile::tempdir().unwrap();
    let err = run_with(&fake, &service, &link(), out.path(), None).unwrap_err();
    assert_eq!(err.code, ErrorCode::Network);
}

#[test]
fn a_huge_answer_is_not_read_to_the_end() {
    let fake = FakeService::start(|_, _| {
        let mut body = br#"{"message":"x","data":{"caption":""#.to_vec();
        body.resize(9 << 20, b'a');
        json_reply(body)
    });
    let out = tempfile::tempdir().unwrap();
    let err = run(&fake, &link(), out.path(), None).unwrap_err();
    assert_eq!(err.code, ErrorCode::UpstreamChanged);
}

#[test]
fn only_local_addresses_are_accepted() {
    for bad in [
        "http://192.168.1.5:5557",
        "http://example.com:5557",
        "https://127.0.0.1:5557",
        "http://user:pw@127.0.0.1:5557",
        "http://127.0.0.1:5557/api",
        "http://127.0.0.1:5557/?a=1",
        "not a url",
        "http://0.0.0.0:5557",
    ] {
        let err = Service::new(bad).unwrap_err();
        let value = failure_json(&err);
        assert_eq!(value["code"], "dependency-missing", "{bad}");
        assert_eq!(value["needs"], json!(["setting:endpoint"]), "{bad}");
        assert!(!value.to_string().contains("pw@"), "{bad}");
    }
    for good in [
        "http://127.0.0.1:5557",
        "http://127.0.0.1:5557/",
        "http://localhost:5557",
        "http://[::1]:5557",
    ] {
        assert!(Service::new(good).is_ok(), "{good}");
    }
}

// ---- 起插件进程：设置经宿主给的文件传入

fn plugin(args: &[&str], settings: Option<&Value>, dir: &Path) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.env_clear()
        .env("USUYO_PROTOCOL", "1")
        .env("USUYO_LOCALE", "zh-CN")
        .env("HTTP_PROXY", DEAD_PROXY)
        .env("HTTPS_PROXY", DEAD_PROXY)
        .env("ALL_PROXY", DEAD_PROXY)
        .args(args);
    if let Some(s) = settings {
        let path = dir.join("settings.json");
        std::fs::write(&path, s.to_string()).unwrap();
        cmd.env("USUYO_SETTINGS", path);
    }
    cmd.output().unwrap()
}

fn envelope(out: &Output) -> Value {
    let stdout = String::from_utf8(out.stdout.clone()).unwrap();
    assert_eq!(
        stdout.lines().count(),
        1,
        "stdout is one JSON value: {stdout}"
    );
    serde_json::from_str(&stdout).unwrap()
}

#[test]
fn the_process_reads_the_endpoint_setting_and_gives_text_and_meta() {
    let fake = FakeService::answering("video.json");
    let dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let output = plugin(
        &[
            "fetch",
            &link(),
            "--out",
            out.path().to_str().unwrap(),
            "--method",
            "ks-downloader",
            "--want",
            "text,meta",
        ],
        Some(&json!({"endpoint": fake.base})),
        dir.path(),
    );
    assert!(output.status.success(), "{output:?}");
    let value = envelope(&output);
    assert_eq!(value["ok"], true);
    assert_eq!(value["identity"]["contentId"], ID);
    assert_eq!(value["text"], "三台磨豆机，同一支豆子。 #手冲");
    assert_eq!(value["completeness"], "full");
    assert_eq!(fake.detail_requests().len(), 1);
    assert!(fake.media_requests().is_empty());
}

#[test]
fn the_process_answers_dependency_missing_without_a_service_or_with_a_bad_setting() {
    let dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let args = [
        "fetch",
        &link(),
        "--out",
        out.path().to_str().unwrap(),
        "--method",
        "ks-downloader",
    ];
    let output = plugin(&args, Some(&json!({"endpoint": closed_base()})), dir.path());
    assert_eq!(output.status.code(), Some(1));
    let value = envelope(&output);
    assert_eq!(value["error"]["code"], "dependency-missing");
    assert_eq!(value["error"]["needs"], json!(["service:endpoint"]));

    // 设置写坏了：不是本机地址、不是字符串
    for bad in [json!("http://example.com:5557"), json!(5)] {
        let value = envelope(&plugin(&args, Some(&json!({"endpoint": bad})), dir.path()));
        assert_eq!(value["error"]["code"], "dependency-missing");
        assert_eq!(value["error"]["needs"], json!(["setting:endpoint"]));
    }
    // 设置文件坏了：不把内容写出来
    std::fs::write(dir.path().join("settings.json"), "{oops-secret").unwrap();
    let output = Command::new(BIN)
        .env_clear()
        .env("USUYO_SETTINGS", dir.path().join("settings.json"))
        .args(args)
        .output()
        .unwrap();
    let value = envelope(&output);
    assert_eq!(value["error"]["code"], "internal");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("oops-secret"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("oops-secret"));
    assert!(files(out.path()).is_empty());
}

#[test]
fn health_names_the_method_it_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let doc = |title: &str| {
        json!({"openapi": "3.1.0", "info": {"title": title, "version": "1.6.stable"}}).to_string()
    };
    let checks = |value: &Value| value["checks"].as_array().unwrap().clone();

    // 服务在运行
    let fake = FakeService::start({
        let ok = doc("KS-Downloader");
        move |seen, _| {
            if seen.path == "/openapi.json" {
                json_reply(ok.clone())
            } else {
                status_reply(404, "")
            }
        }
    });
    let value = envelope(&plugin(
        &["health"],
        Some(&json!({"endpoint": fake.base})),
        dir.path(),
    ));
    assert_eq!(
        (value["ok"].clone(), value["ready"].clone()),
        (json!(true), json!(true))
    );
    assert_eq!(
        value["upstream"],
        json!([{"name": "KS-Downloader", "version": "1.6.stable"}])
    );
    assert!(checks(&value).iter().all(|c| c["ok"] == true));
    // 检查只问了接口文档，没有请求作品
    assert!(fake.detail_requests().is_empty());

    // 服务没在运行：只挡 ks-downloader，一句中文说怎么启动
    let value = envelope(&plugin(
        &["health", "--probe"],
        Some(&json!({"endpoint": closed_base()})),
        dir.path(),
    ));
    assert_eq!(
        value["ready"], true,
        "a blocked method does not block the plugin"
    );
    let all = checks(&value);
    let bad: Vec<&Value> = all.iter().filter(|c| c["ok"] == false).collect();
    assert_eq!(bad.len(), 1);
    assert_eq!(bad[0]["methods"], json!(["ks-downloader"]));
    let message = bad[0]["message"].as_str().unwrap();
    assert!(
        message.contains("没有在运行") && message.contains("python main.py api --host 127.0.0.1"),
        "{message}"
    );

    // 地址上是别的程序
    let other = FakeService::start(move |_, _| json_reply(doc("Something else")));
    let value = envelope(&plugin(
        &["health"],
        Some(&json!({"endpoint": other.base})),
        dir.path(),
    ));
    let all = checks(&value);
    let bad = all.iter().find(|c| c["ok"] == false).unwrap();
    assert!(
        bad["message"]
            .as_str()
            .unwrap()
            .contains("不像 KS-Downloader")
    );
    assert_eq!(bad["methods"], json!(["ks-downloader"]));

    // 设置里不是本机地址
    let value = envelope(&plugin(
        &["health"],
        Some(&json!({"endpoint": "http://example.com:5557"})),
        dir.path(),
    ));
    assert_eq!(value["checks"][0]["ok"], false);
    assert_eq!(value["checks"][0]["methods"], json!(["ks-downloader"]));
    assert_eq!(value["ready"], true);
}

#[test]
fn manifest_declares_the_method_the_way_the_code_calls_it() {
    let text = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("usuyo-plugin.yaml"),
    )
    .unwrap();
    let parsed = PluginManifest::from_yaml(&text).unwrap();
    assert!(
        parsed.unknown_fields.is_empty(),
        "{:?}",
        parsed.unknown_fields
    );
    let m = parsed.value;
    let PluginKind::Fetch(spec) = &m.kind else {
        panic!("not a fetch plugin");
    };
    let method = spec.method(ksdownloader::METHOD).unwrap();
    assert!(method.built && method.sites.is_empty() && method.visitors.is_empty());
    assert_eq!(
        (method.access, method.login, method.account),
        (Access::PublicWeb, Login::None, Account::None)
    );
    assert!(method.on_by_default());
    assert!(method.risk.is_some());
    let fetch = method.capabilities.fetch.as_ref().unwrap();
    assert!(
        fetch.outputs.contains(&OutputKind::Video) && fetch.outputs.contains(&OutputKind::Text)
    );
    // 设置的名字和默认值同代码一致；没有密钥设置（KS-Downloader 没有访问令牌）
    let endpoint = m
        .header
        .settings
        .iter()
        .find(|s| s.key == "endpoint")
        .unwrap();
    assert_eq!(
        endpoint.default,
        Some(Value::String(ksdownloader::DEFAULT_ENDPOINT.into()))
    );
    assert!(m.header.settings.iter().all(|s| !s.secret));
    let hosts = &m.header.permissions.network.hosts;
    assert!(hosts.iter().any(|h| h == "${setting.endpoint}"));
}
