//! 对着本机的假服务器跑：短链展开、手机页（视频、图文、已删除、验证页、没有数据）、媒体主机、
//! 错误码映射、输出目录里有什么。不碰真网络。
//!
//! `fixtures/` 里的页面都是手写的，内容是编的。`share_video.html`、`share_no_data.html`、`desktop_shell.html`
//! 照 2026-11-12 量到的真实页面的形状写（`INIT_STATE` 的几项、`photo` 的字段；桌面页只有 `__APOLLO_STATE__` 的壳）；
//! 已删除（`result` 223）的那一项照真实的回答写（经宿主量到，见 `src/page.rs`）；图文（`ext_params.atlas`）、
//! 验证页（`result` 2001）、私密（20113）照 parse-video、ParseHub 的字段写，没有在真实的回答上量过。

mod support;

use std::net::IpAddr;
use std::path::Path;

use support::{Request, Response, Server, fixture};
use usuyo_fetch_kuaishou::api::{Endpoints, Kuaishou};
use usuyo_fetch_kuaishou::canonical::canonicalize;
use usuyo_fetch_kuaishou::share;
use usuyo_plugin::protocol::{
    Access, CallFailure, Canonicalized, Completeness, ErrorCode, Identity, ItemKind, MediaKind,
    NotFoundReason, OutputKind,
};
use usuyo_plugin_kit::http::Policy;

const VIDEO: &str = "3xsamplevideo001";
const IMAGES: &str = "3xsampleimages01";
const REMOVED: &str = "3xsampleremoved1";
const NO_DATA: &str = "3xsamplenodata01";
const VERIFICATION: &str = "3xsampleverify01";
const DESKTOP: &str = "3xsampledesktop01";

/// 只在测试里用的 id：各自触发一种服务器行为。
const NO_PLAY: &str = "3xtestnoplay0001";
const BAD_COVER: &str = "3xtestbadcover001";
const BAD_IMAGE: &str = "3xtestbadimage001";
const EVIL_VIDEO: &str = "3xtestevilvideo01";
const PRIVATE: &str = "3xtestprivate0001";
const OTHER_POST: &str = "3xtestotherpost01";
const JSON_BLOCK: &str = "3xtestjsonblock01";
const HTTP_403: &str = "3xtesthttp403001";
const HTTP_429: &str = "3xtesthttp429001";
const HTTP_500: &str = "3xtesthttp500001";
const HTTP_302: &str = "3xtesthttp302001";
const HTTP_302_CAPTCHA: &str = "3xtesthttp302cap1";

fn local_policy() -> Policy {
    Policy {
        lookup: |_, _| {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no DNS in tests",
            ))
        },
        public: |ip| ip == IpAddr::from([127, 0, 0, 1]),
        cookies_over_http: true,
        ..Policy::strict()
    }
}

fn html(name: &str, base: &str) -> Response {
    Response {
        status: 200,
        headers: vec![("Content-Type", "text/html; charset=utf-8".into())],
        body: fixture(name, base).into_bytes(),
    }
}

fn body(text: String) -> Response {
    Response {
        status: 200,
        headers: vec![],
        body: text.into_bytes(),
    }
}

/// 手机页：按 id 给不同的页面。
fn mobile_page(id: &str, base: &str) -> Response {
    let as_id = |text: String| text.replace(VIDEO, id);
    match id {
        VIDEO => html("share_video.html", base),
        IMAGES => html("share_images.html", base),
        REMOVED => html("share_removed.html", base),
        NO_DATA => html("share_no_data.html", base),
        VERIFICATION => html("share_verification.html", base),
        DESKTOP => html("desktop_shell.html", base),
        PRIVATE => body(
            fixture("share_removed.html", base)
                .replace("获取失败，作品可能已被删除或尚未上传", "作品是私密的")
                .replace("223", "20113"),
        ),
        JSON_BLOCK => body(r#"{"result":2}"#.into()),
        NO_PLAY => body(as_id(fixture("share_video.html", base)).replace(
            r#""mainMvUrls":[{"cdn":"__BASE__","url":"__BASE__/play/v1?pkg=1&sign=a"},{"cdn":"__BASE__","url":"__BASE__/play/v2?pkg=1"}],"#
                .replace("__BASE__", base)
                .as_str(),
            "",
        )),
        BAD_COVER => body(as_id(fixture("share_video.html", base)).replace("/img/cover.jpg", "/img/missing.jpg")),
        BAD_IMAGE => body(
            fixture("share_images.html", base)
                .replace(IMAGES, id)
                .replace("/ufile/atlas/a_1.jpg", "/ufile/atlas/missing.jpg"),
        ),
        // 视频地址先跳到名单外的主机
        EVIL_VIDEO => body(as_id(fixture("share_video.html", base)).replace("/play/v1?pkg=1&sign=a", "/play/evil")),
        // 页面答的是另一个作品：`share_info` 里的 id 对不上
        OTHER_POST => html("share_video.html", base),
        HTTP_403 => Response::status(403).header("Content-Type", "text/html"),
        HTTP_429 => Response::status(429).header("Retry-After", "30"),
        HTTP_500 => Response::status(500),
        HTTP_302 => Response::redirect("https://www.kuaishou.com/short-video/3xtesthttp302001"),
        HTTP_302_CAPTCHA => Response::redirect("https://captcha.zt.kuaishou.com/verify?next=1"),
        _ => Response::status(404),
    }
}

/// 一个什么都有的快手。
fn kuaishou(req: &Request, base: &str) -> Response {
    if let Some(id) = req.path.strip_prefix("/fw/photo/") {
        return mobile_page(id, base);
    }
    match req.path.as_str() {
        // 短链：量到的形状，跳到手机页并带着分享参数
        "/mkYOSR" => Response::redirect(&format!(
            "https://v.m.chenzhongtech.com/fw/photo/{VIDEO}?cc=share_copylink&photoId={VIDEO}&shareToken=X-sample"
        )),
        "/f/X-aqEn4ZUlPtlME2" => Response::redirect(&format!(
            "https://www.kuaishou.com/short-video/{VIDEO}?fid=1&cc=share_copylink"
        )),
        "/s1" => Response::redirect("/s2"),
        "/s2" => Response::redirect(&format!(
            "https://v.m.chenzhongtech.com/fw/photo/{VIDEO}?cc=share_copylink"
        )),
        "/to-user" => Response::redirect("https://www.kuaishou.com/profile/3xdng3ssi9kdpk2"),
        "/to-off" => Response::redirect(&format!("https://example.com/short-video/{VIDEO}")),
        "/loop" => Response::redirect("/loop"),
        "/play/v1" => Response::redirect("/cdn/video.mp4"),
        "/play/evil" => Response::redirect("https://evil.example/v.mp4"),
        "/cdn/video.mp4" => Response::bytes(b"VIDEOBYTES").header("Content-Type", "video/mp4"),
        "/img/cover.jpg" => Response::bytes(b"COVER"),
        "/ufile/atlas/a_0.jpg" => Response::bytes(b"IMG1"),
        "/ufile/atlas/a_1.jpg" => Response::bytes(b"IMG2"),
        _ => Response::status(404),
    }
}

fn api_for(server: &Server) -> Kuaishou {
    Kuaishou::new(Endpoints::local(&server.base, local_policy()))
}

fn failure<T: std::fmt::Debug>(result: Result<T, CallFailure>) -> CallFailure {
    result.expect_err("expected a failure")
}

fn post_url(id: &str) -> String {
    format!("https://www.kuaishou.com/short-video/{id}")
}

/// 输出目录里的全部文件，相对路径，排好序。
fn files(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                out.push(
                    path.strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn content_id(identity: &Option<Identity>) -> String {
    match identity {
        Some(Identity::Platform {
            platform,
            content_id,
            url,
        }) => {
            assert_eq!(platform.as_str(), "kuaishou");
            assert_eq!(url.as_deref(), Some(post_url(content_id).as_str()));
            content_id.clone()
        }
        other => panic!("{other:?}"),
    }
}

fn paths(server: &Server) -> Vec<String> {
    server.paths()
}

#[test]
fn a_video_post_gives_text_author_counts_cover_and_the_video_file() {
    let server = Server::start(kuaishou);
    let api = api_for(&server);
    let tmp = tempfile::tempdir().unwrap();
    let out = share::fetch(&api, &post_url(VIDEO), tmp.path(), None).unwrap();

    assert_eq!(content_id(&out.identity), VIDEO);
    assert_eq!(out.kind, ItemKind::Video);
    assert_eq!(out.title.as_deref(), Some("手冲咖啡三段式注水"));
    assert_eq!(
        out.text.as_deref(),
        Some("手冲咖啡三段式注水\n第一段 40 克闷蒸 30 秒 #手冲 #咖啡日常")
    );
    let author = out.author.as_ref().unwrap();
    assert_eq!(
        (
            author.name.as_str(),
            author.platform_id.as_deref(),
            author.url.as_deref()
        ),
        (
            "咖啡日常",
            Some("3xsampleauthor01"),
            Some("https://www.kuaishou.com/profile/3xsampleauthor01")
        )
    );
    assert_eq!(
        out.published_at.as_ref().unwrap().as_str(),
        "2024-10-01T12:00:00Z"
    );
    assert_eq!(
        out.images.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
        ["images/cover.jpg"]
    );
    assert_eq!(
        std::fs::read(tmp.path().join("images/cover.jpg")).unwrap(),
        b"COVER"
    );
    // 视频：页面给的第一个地址，经一次跳转到 CDN
    assert_eq!(out.media.len(), 1);
    assert_eq!(
        (out.media[0].kind, out.media[0].path.as_str()),
        (MediaKind::Video, "media/video.mp4")
    );
    assert_eq!(out.media[0].duration_sec, Some(183.0));
    assert_eq!(
        std::fs::read(tmp.path().join("media/video.mp4")).unwrap(),
        b"VIDEOBYTES"
    );
    assert_eq!(out.completeness, Completeness::Full);
    assert!(out.missing.is_empty() && out.missing_reason.is_none());
    assert_eq!(out.evidence.access, Access::PublicWeb);
    assert_eq!(
        (out.evidence.site.as_deref(), out.evidence.status),
        (None, Some(200))
    );

    assert_eq!(
        files(tmp.path()),
        [
            "images/cover.jpg",
            "init-state.json",
            "media/video.mp4",
            "meta.json"
        ]
    );
    // 原始数据原样存：就是页面里 `INIT_STATE` 的那一段
    let page = fixture("share_video.html", &server.base);
    let raw = std::fs::read_to_string(tmp.path().join("init-state.json")).unwrap();
    assert!(page.contains(&raw) && raw.starts_with('{') && raw.ends_with('}'));
    let meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(tmp.path().join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["photoId"], VIDEO);
    assert_eq!(meta["numericPhotoId"], "5196000000000000001");
    assert_eq!(meta["kind"], "video");
    assert_eq!(meta["counts"]["likeCount"], 1234);
    assert_eq!(meta["counts"]["commentCount"], 56);
    assert_eq!(meta["counts"]["forwardCount"], 7);
    assert_eq!(meta["counts"]["viewCount"], 4567);
    assert_eq!(meta["author"]["eid"], "3xsampleauthor01");
    assert_eq!(meta["author"]["userId"], "534000001");
    assert_eq!(meta["timestampMs"], 1727784000000_i64);
    assert_eq!(meta["video"]["width"], 720);

    // 手机页用手机 UA，没有分享参数时不带参数；没有一个请求带 cookie
    let requests = server.requests();
    let page = requests
        .iter()
        .find(|r| r.path.starts_with("/fw/photo/"))
        .unwrap();
    assert!(page.headers["user-agent"].contains("iPhone"));
    assert!(page.query.is_empty());
    assert!(requests.iter().all(|r| r.cookie().is_none()));
    // 页面给了两个视频地址，只用第一个，地址原样
    assert!(!paths(&server).contains(&"/play/v2".to_owned()));
    let video = requests.iter().find(|r| r.path == "/play/v1").unwrap();
    assert_eq!(video.query["sign"], "a");
}

#[test]
fn an_image_post_gives_the_cover_and_every_image_and_no_video() {
    let server = Server::start(kuaishou);
    let api = api_for(&server);
    let tmp = tempfile::tempdir().unwrap();
    let out = share::fetch(&api, &post_url(IMAGES), tmp.path(), None).unwrap();
    assert_eq!(content_id(&out.identity), IMAGES);
    assert_eq!(out.kind, ItemKind::Post);
    assert_eq!(
        out.images.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
        ["images/cover.jpg", "images/001.jpg", "images/002.jpg"]
    );
    assert_eq!(
        std::fs::read(tmp.path().join("images/001.jpg")).unwrap(),
        b"IMG1"
    );
    assert!(out.media.is_empty());
    // 图文本来没有视频：不算缺
    assert_eq!(out.completeness, Completeness::Full);
    assert!(out.missing.is_empty());
    assert!(
        !paths(&server).iter().any(|p| p.starts_with("/play")),
        "{:?}",
        paths(&server)
    );
    let meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(tmp.path().join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["kind"], "images");
}

#[test]
fn links_in_every_accepted_shape_reach_the_same_post() {
    let server = Server::start(kuaishou);
    let api = api_for(&server);
    for input in [
        format!("https://www.kuaishou.com/short-video/{VIDEO}"),
        format!("https://www.kuaishou.com/short-video/{VIDEO}?fid=1&cc=share_copylink"),
        format!("https://v.m.chenzhongtech.com/fw/photo/{VIDEO}?cc=share_copylink"),
        format!("https://v.m.chenzhongtech.com/fw/long-video/{VIDEO}"),
        format!("看这个 https://www.kuaishou.com/short-video/{VIDEO} 复制此消息，打开【快手】"),
        // 短链（本机的 /mkYOSR 算短链主机）、几跳的短链、网页版分享链接 /f/
        format!("{}/mkYOSR", server.base),
        format!("{}/s1", server.base),
        format!("{}/f/X-aqEn4ZUlPtlME2", server.base),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let out = share::fetch(
            &api,
            &input,
            tmp.path(),
            Some(&[OutputKind::Text, OutputKind::Meta]),
        )
        .unwrap_or_else(|e| panic!("{input}: {e:?}"));
        assert_eq!(content_id(&out.identity), VIDEO, "{input}");
    }
}

#[test]
fn the_share_query_goes_to_the_mobile_page_unchanged() {
    let server = Server::start(kuaishou);
    let api = api_for(&server);
    let tmp = tempfile::tempdir().unwrap();
    share::fetch(
        &api,
        &format!("{}/mkYOSR", server.base),
        tmp.path(),
        Some(&[OutputKind::Text]),
    )
    .unwrap();
    let requests = server.requests();
    // 短链一次，手机页一次，手机页带着分享参数
    assert_eq!(
        paths(&server),
        ["/mkYOSR".to_owned(), format!("/fw/photo/{VIDEO}")]
    );
    assert_eq!(requests[1].query["cc"], "share_copylink");
    assert_eq!(requests[1].query["shareToken"], "X-sample");
    assert!(requests.iter().all(|r| r.cookie().is_none()));
    assert!(requests[0].headers["user-agent"].contains("Macintosh"));
}

#[test]
fn canonicalize_expands_short_links_and_keeps_the_resolved_url() {
    let server = Server::start(kuaishou);
    let api = api_for(&server);
    let Canonicalized::Match {
        identity,
        resolved_url,
    } = canonicalize(&api, &format!("{}/mkYOSR", server.base)).unwrap()
    else {
        panic!("no match")
    };
    assert_eq!(content_id(&Some(identity)), VIDEO);
    let resolved = resolved_url.unwrap();
    assert!(
        resolved.starts_with(&format!(
            "https://v.m.chenzhongtech.com/fw/photo/{VIDEO}?cc=share_copylink"
        )),
        "{resolved}"
    );
    // 展开只请求短链，不请求作品页
    assert_eq!(paths(&server), ["/mkYOSR"]);

    // 完整链接不发请求
    let quiet = Server::start(kuaishou);
    let out = canonicalize(&api_for(&quiet), &post_url(VIDEO)).unwrap();
    assert!(matches!(
        out,
        Canonicalized::Match {
            resolved_url: None,
            ..
        }
    ));
    assert!(quiet.requests().is_empty());
}

#[test]
fn a_short_link_that_leaves_kuaishou_or_is_not_a_post_is_not_a_match() {
    let server = Server::start(kuaishou);
    let api = api_for(&server);
    for path in ["/to-user", "/to-off", "/gone"] {
        let out = canonicalize(&api, &format!("{}{path}", server.base));
        match path {
            "/gone" => assert_eq!(failure(out).code, ErrorCode::NotFound),
            _ => assert!(matches!(out.unwrap(), Canonicalized::NoMatch), "{path}"),
        }
    }
    // 跳到别的主机就停：不去请求那台主机
    assert!(!paths(&server).iter().any(|p| p.contains("example")));
    let f = failure(canonicalize(&api, &format!("{}/loop", server.base)));
    assert_eq!(f.code, ErrorCode::UpstreamChanged);
    // 不是快手的链接，不发请求
    let quiet = Server::start(kuaishou);
    let out = canonicalize(
        &api_for(&quiet),
        "https://example.com/short-video/3xsamplevideo001",
    );
    assert!(matches!(out.unwrap(), Canonicalized::NoMatch));
    assert!(quiet.requests().is_empty());
}

#[test]
fn want_limits_the_downloads() {
    let server = Server::start(kuaishou);
    let api = api_for(&server);
    let tmp = tempfile::tempdir().unwrap();
    let out = share::fetch(
        &api,
        &post_url(VIDEO),
        tmp.path(),
        Some(&[OutputKind::Text, OutputKind::Meta]),
    )
    .unwrap();
    assert_eq!(out.completeness, Completeness::Full);
    assert!(out.images.is_empty() && out.media.is_empty());
    assert!(
        paths(&server).iter().all(|p| p.starts_with("/fw/photo/")),
        "{:?}",
        paths(&server)
    );
    // 只给清单里没有的输出，回 invalid-args，不发请求
    let quiet = Server::start(kuaishou);
    let f = failure(share::fetch(
        &api_for(&quiet),
        &post_url(VIDEO),
        tmp.path(),
        Some(&[OutputKind::Audio]),
    ));
    assert_eq!(f.code, ErrorCode::InvalidArgs);
    assert!(quiet.requests().is_empty());
}

#[test]
fn a_cover_or_an_image_that_does_not_come_is_missing_with_a_reason() {
    let server = Server::start(kuaishou);
    let api = api_for(&server);
    let tmp = tempfile::tempdir().unwrap();
    let out = share::fetch(&api, &post_url(BAD_COVER), tmp.path(), None).unwrap();
    // 封面没下来、视频下来了：图片缺，其余照交
    assert_eq!(out.completeness, Completeness::Partial);
    assert_eq!(out.missing, [OutputKind::Images]);
    assert!(out.missing_reason.as_deref().unwrap().contains("封面"));
    assert_eq!(out.media.len(), 1);
    assert!(out.text.is_some());

    // 三张里下到两张：交来一部分，照样记缺
    let tmp = tempfile::tempdir().unwrap();
    let out = share::fetch(&api, &post_url(BAD_IMAGE), tmp.path(), None).unwrap();
    assert_eq!(out.completeness, Completeness::Partial);
    assert_eq!(out.missing, [OutputKind::Images]);
    assert!(out.missing_reason.as_deref().unwrap().contains("图片 2"));
    assert_eq!(out.images.len(), 2);
}

#[test]
fn a_video_on_a_host_off_the_list_is_missing_and_the_rest_is_kept() {
    let server = Server::start(kuaishou);
    let api = api_for(&server);
    let tmp = tempfile::tempdir().unwrap();
    let out = share::fetch(&api, &post_url(EVIL_VIDEO), tmp.path(), None).unwrap();
    assert_eq!(out.completeness, Completeness::Partial);
    assert_eq!(out.missing, [OutputKind::Video]);
    assert!(out.missing_reason.as_deref().unwrap().contains("视频"));
    assert!(out.media.is_empty());
    assert_eq!(out.images.len(), 1);
    assert!(!files(tmp.path()).contains(&"media/video.mp4".to_owned()));
}

#[test]
fn a_video_post_without_an_address_is_missing_the_video() {
    let server = Server::start(kuaishou);
    let api = api_for(&server);
    let tmp = tempfile::tempdir().unwrap();
    let out = share::fetch(&api, &post_url(NO_PLAY), tmp.path(), None).unwrap();
    assert_eq!(out.completeness, Completeness::Partial);
    assert_eq!(out.missing, [OutputKind::Video]);
    assert_eq!(
        out.missing_reason.as_deref(),
        Some("手机页没有给出视频地址")
    );
}

#[test]
fn a_removed_or_private_post_is_not_found_with_the_reason_and_the_start_of_the_answer() {
    let server = Server::start(kuaishou);
    let api = api_for(&server);
    let tmp = tempfile::tempdir().unwrap();
    let f = failure(share::fetch(&api, &post_url(REMOVED), tmp.path(), None));
    assert_eq!(f.code, ErrorCode::NotFound);
    assert_eq!(f.reason, Some(NotFoundReason::Deleted));
    assert!(f.message.contains("223") && f.message.contains("已被删除"));
    let detail = f.detail.as_deref().unwrap();
    // 出错的那一项排在页面文字后面，不被前面的站点配置挤掉
    assert!(
        detail.contains(r#""result":223"#) && detail.contains("已被删除"),
        "{detail}"
    );

    let f = failure(share::fetch(&api, &post_url(PRIVATE), tmp.path(), None));
    assert_eq!(f.code, ErrorCode::NotFound);
    assert_eq!(f.reason, Some(NotFoundReason::Private));
    assert!(f.detail.as_deref().unwrap().contains("私密"));
}

#[test]
fn a_verification_page_is_blocked_with_the_start_of_the_answer() {
    let server = Server::start(kuaishou);
    let api = api_for(&server);
    let tmp = tempfile::tempdir().unwrap();
    let f = failure(share::fetch(
        &api,
        &post_url(VERIFICATION),
        tmp.path(),
        None,
    ));
    assert_eq!(f.code, ErrorCode::Blocked);
    let detail = f.detail.as_deref().unwrap();
    assert!(
        detail.contains("2001") && detail.contains("captcha"),
        "{detail}"
    );
    // 整页是 `{"result":2}`
    let f = failure(share::fetch(&api, &post_url(JSON_BLOCK), tmp.path(), None));
    assert_eq!(f.code, ErrorCode::Blocked);
    assert!(f.detail.as_deref().unwrap().contains(r#""result":2"#));
    // 状态码 403，或跳去验证页
    let f = failure(share::fetch(&api, &post_url(HTTP_403), tmp.path(), None));
    assert_eq!(f.code, ErrorCode::Blocked);
    let f = failure(share::fetch(
        &api,
        &post_url(HTTP_302_CAPTCHA),
        tmp.path(),
        None,
    ));
    assert_eq!(f.code, ErrorCode::Blocked);
    assert!(f.message.contains("captcha.zt.kuaishou.com"));
}

#[test]
fn a_page_without_the_data_is_upstream_changed_with_the_start_of_the_answer() {
    let server = Server::start(kuaishou);
    let api = api_for(&server);
    let tmp = tempfile::tempdir().unwrap();
    // 有 `INIT_STATE`，没有带 `photo` 的那一项
    let f = failure(share::fetch(&api, &post_url(NO_DATA), tmp.path(), None));
    assert_eq!(f.code, ErrorCode::UpstreamChanged);
    assert!(f.message.contains("photo"), "{}", f.message);
    assert!(f.detail.as_deref().unwrap().contains("result"));
    // 桌面页的壳（量到的形状）：没有 `INIT_STATE`
    let f = failure(share::fetch(&api, &post_url(DESKTOP), tmp.path(), None));
    assert_eq!(f.code, ErrorCode::UpstreamChanged);
    assert!(f.message.contains("INIT_STATE"));
    assert!(f.detail.as_deref().unwrap().contains("无法连接网络"));
    // 页面答的是另一个作品
    let f = failure(share::fetch(&api, &post_url(OTHER_POST), tmp.path(), None));
    assert_eq!(f.code, ErrorCode::UpstreamChanged);
    assert!(f.message.contains("different post"));
    assert!(f.hint.is_some());
    // 失败时输出目录里没有留下作品文件
    assert!(!files(tmp.path()).iter().any(|f| f.starts_with("media/")));
}

#[test]
fn status_codes_map_to_error_codes() {
    let server = Server::start(kuaishou);
    let api = api_for(&server);
    let tmp = tempfile::tempdir().unwrap();
    let f = failure(share::fetch(&api, &post_url(HTTP_429), tmp.path(), None));
    assert_eq!((f.code, f.retry_after), (ErrorCode::RateLimited, Some(30)));
    let f = failure(share::fetch(&api, &post_url(HTTP_500), tmp.path(), None));
    assert_eq!(f.code, ErrorCode::Network);
    let f = failure(share::fetch(&api, &post_url(HTTP_302), tmp.path(), None));
    assert_eq!(f.code, ErrorCode::UpstreamChanged);
    let f = failure(share::fetch(
        &api,
        &post_url("3xtestmissing0001"),
        tmp.path(),
        None,
    ));
    assert_eq!(f.code, ErrorCode::NotFound);
}

#[test]
fn a_link_that_is_not_a_post_is_turned_away_without_a_request() {
    let server = Server::start(kuaishou);
    let api = api_for(&server);
    let tmp = tempfile::tempdir().unwrap();
    for input in [
        "https://example.com/short-video/3xsamplevideo001",
        "https://www.kuaishou.com/profile/3xdng3ssi9kdpk2",
        "not a link",
    ] {
        let f = failure(share::fetch(&api, input, tmp.path(), None));
        assert_eq!(f.code, ErrorCode::UnsupportedInput, "{input}");
    }
    assert!(server.requests().is_empty());
}
