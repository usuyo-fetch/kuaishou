//! 方法 `share-page`：一条链接取一个作品（协议 §3.6、§3.7）。不带账号，不读宿主交来的凭据；
//! 只有跟短链时站点发的 cookie 会带到后面的跳转和手机页（`api.rs`），下载封面、图片、视频不带 cookie。
//!
//! 输出目录里的文件名都是固定的，不用网络上来的名字：
//! `init-state.json`（手机页里 `INIT_STATE` 的原文）、`meta.json`、`images/cover.<ext>`、
//! `images/001.<ext>`…（图文的图片）、`media/video.mp4`。
//!
//! 视频文件用的地址：页面给的 `photo.mainMvUrls[0].url`，原样请求，不改写。页面给了几个地址也只用第一个。
//! 下载到的字节原样存。

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use usuyo_plugin_kit::protocol::{
    Access, Author, CallFailure, Completeness, ErrorCode, Evidence, FetchOutput, ItemKind, Media,
    MediaKind, OutputKind, RelPath, Rfc3339, TextFormat,
};

use crate::api::{ApiError, Kuaishou, blocked, step};
use crate::canonical::{identity, locate};
use crate::page::{self, PageError, Post};

/// 本方法能给的输出，和清单里的 `outputs` 一致。
pub const OUTPUTS: [OutputKind; 4] = [
    OutputKind::Text,
    OutputKind::Images,
    OutputKind::Video,
    OutputKind::Meta,
];

const PAGE_HINT: &str = "快手的手机页没有给出作品数据，这种方法暂时取不到；可以换别的方法";
const IMAGE_BYTES: u64 = 20 << 20;
const IMAGE_TIMEOUT: Duration = Duration::from_secs(30);
const VIDEO_BYTES: u64 = 2 << 30;
const VIDEO_TIMEOUT: Duration = Duration::from_secs(20 * 60);
/// 一个图文最多存几张：平台的上限在几十张以内，再多的不取。
const MAX_IMAGES: usize = 100;
const TITLE_CHARS: usize = 100;

/// `--want` 的集合；没给是全要，要了本方法给不了的回 `invalid-args`。
pub(crate) fn wanted_outputs(
    want: Option<&[OutputKind]>,
) -> Result<BTreeSet<OutputKind>, CallFailure> {
    let wanted: BTreeSet<OutputKind> =
        want.map_or_else(|| OUTPUTS.into(), |w| w.iter().copied().collect());
    if let Some(other) = wanted.iter().find(|w| !OUTPUTS.contains(w)) {
        return Err(CallFailure::new(
            ErrorCode::InvalidArgs,
            format!("cannot give `{other}`"),
        ));
    }
    Ok(wanted)
}

/// 方法 `share-page`：读手机页里内嵌的数据。
pub fn fetch(
    api: &Kuaishou,
    input: &str,
    out: &Path,
    want: Option<&[OutputKind]>,
) -> Result<FetchOutput, CallFailure> {
    let wanted = wanted_outputs(want)?;
    let found = locate(api, input)?
        .ok_or_else(|| CallFailure::new(ErrorCode::UnsupportedInput, "not a kuaishou post link"))?;
    let id = found.id;

    let fetched_at = Rfc3339::from_timestamp(jiff::Timestamp::now());
    let html = api.mobile_page(&id, found.query.as_deref(), &found.cookies)?;
    let parsed = page::parse(&html, &id).map_err(page_failure)?;
    write(out, "init-state.json", parsed.raw.as_bytes())?;
    let evidence = Evidence {
        fetched_at,
        access: Access::PublicWeb,
        site: None,
        upstream: None,
        status: Some(200),
    };
    deliver(api, out, &wanted, &id, &parsed.post, "手机页", evidence)
}

/// 读出的作品变成输出：下载封面、图片、视频，写 `meta.json`，算出缺什么。
/// `source` 是数据从哪来的一个名词，写进 `missingReason`。
pub(crate) fn deliver(
    api: &Kuaishou,
    out: &Path,
    wanted: &BTreeSet<OutputKind>,
    id: &str,
    post: &Post,
    source: &str,
    evidence: Evidence,
) -> Result<FetchOutput, CallFailure> {
    let identity = identity(id)?;
    let mut output = FetchOutput {
        identity: Some(identity),
        kind: if post.is_image_post() {
            ItemKind::Post
        } else {
            ItemKind::Video
        },
        title: title(post),
        author: post.author.as_ref().map(|a| Author {
            name: a.name.clone(),
            platform_id: a.eid.clone().or_else(|| a.user_id.clone()),
            url: a
                .eid
                .as_ref()
                .map(|e| format!("https://www.kuaishou.com/profile/{e}")),
        }),
        published_at: post
            .published_ms
            .and_then(|t| jiff::Timestamp::from_millisecond(t).ok())
            .map(Rfc3339::from_timestamp),
        text: (!post.caption.is_empty()).then(|| post.caption.clone()),
        text_format: (!post.caption.is_empty()).then_some(TextFormat::Plain),
        html: None,
        images: Vec::new(),
        media: Vec::new(),
        platform_tags: Vec::new(),
        comments: None,
        completeness: Completeness::Partial,
        missing: Vec::new(),
        missing_reason: None,
        evidence,
        credentials_update: None,
    };

    // 这个作品本来没有的部分不算缺（协议 §3.7）：没有文字、没有封面也没有图、图文没有视频
    let mut absent: BTreeSet<OutputKind> = BTreeSet::new();
    // 交来一部分、还缺一部分的（几张图没下下来）：照样记缺
    let mut partial: BTreeSet<OutputKind> = BTreeSet::new();
    let mut reasons: Vec<String> = Vec::new();
    if output.text.is_none() {
        absent.insert(OutputKind::Text);
    }
    if output.title.is_none() && output.author.is_none() && output.published_at.is_none() {
        absent.insert(OutputKind::Meta);
    }
    if post.cover_url.is_none() && post.images.is_empty() {
        absent.insert(OutputKind::Images);
    } else if wanted.contains(&OutputKind::Images) {
        images(api, out, post, &mut output, &mut partial, &mut reasons)?;
    }
    if post.is_image_post() {
        absent.insert(OutputKind::Video);
    } else if wanted.contains(&OutputKind::Video) {
        match &post.video {
            None => {
                partial.insert(OutputKind::Video);
                reasons.push(format!("{source}没有给出视频地址"));
            }
            Some(video) => match download_video(api, out, &video.url) {
                Ok(()) => output.media.push(Media {
                    path: rel("media/video.mp4")?,
                    kind: MediaKind::Video,
                    mime: Some("video/mp4".into()),
                    lang: None,
                    duration_sec: video.duration_sec,
                    group: None,
                }),
                Err(e) => {
                    partial.insert(OutputKind::Video);
                    reasons.push(soft(e, "视频")?);
                }
            },
        }
    }

    write(
        out,
        "meta.json",
        &serde_json::to_vec_pretty(&page::meta(id, post))
            .map_err(|e| CallFailure::new(ErrorCode::Internal, format!("meta.json: {e}")))?,
    )?;

    let obtained = output.obtained();
    output.missing = wanted
        .iter()
        .filter(|kind| {
            !absent.contains(kind) && (partial.contains(kind) || !obtained.contains(kind))
        })
        .copied()
        .collect();
    output.completeness = if output.missing.is_empty() {
        Completeness::Full
    } else {
        Completeness::Partial
    };
    if !output.missing.is_empty() {
        output.missing_reason = Some(reasons.join("；"));
    }
    Ok(output)
}

/// 手机页读不出作品时的错误码：看不到的 → `not-found`；像验证页 → `blocked`；
/// 没有数据块、没有作品 → `upstream-changed`（量到的情况，见 `page.rs` 顶部）。
fn page_failure(err: PageError) -> CallFailure {
    match err {
        PageError::Unavailable {
            reason,
            message,
            detail,
        } => CallFailure {
            reason,
            ..CallFailure::new(ErrorCode::NotFound, message)
        }
        .with_detail(&detail),
        PageError::Verification { detail } => {
            blocked("the page looks like a verification page".into()).with_detail(&detail)
        }
        PageError::Missing { what, detail } => CallFailure {
            hint: Some(PAGE_HINT.into()),
            ..CallFailure::new(ErrorCode::UpstreamChanged, what)
        }
        .with_detail(&detail),
    }
}

fn title(post: &Post) -> Option<String> {
    let line = post.caption.lines().next()?.trim();
    (!line.is_empty()).then(|| line.chars().take(TITLE_CHARS).collect())
}

/// 封面（视频和图文都有时），再是图文的图片。一张下不下来记进 `partial`，原因进 `reasons`。
fn images(
    api: &Kuaishou,
    out: &Path,
    post: &Post,
    output: &mut FetchOutput,
    partial: &mut BTreeSet<OutputKind>,
    reasons: &mut Vec<String>,
) -> Result<(), CallFailure> {
    let jobs = post
        .cover_url
        .iter()
        .map(|u| ("images/cover".to_owned(), "封面".to_owned(), u))
        .chain(
            post.images
                .iter()
                .take(MAX_IMAGES)
                .enumerate()
                .map(|(i, u)| (format!("images/{:03}", i + 1), format!("图片 {}", i + 1), u)),
        );
    for (stem, label, raw) in jobs {
        match image(api, out, &stem, &label, raw) {
            Ok(path) => output.images.push(path),
            Err(e) => {
                partial.insert(OutputKind::Images);
                reasons.push(soft(e, &label)?);
            }
        }
    }
    Ok(())
}

fn image(
    api: &Kuaishou,
    out: &Path,
    stem: &str,
    label: &str,
    raw: &str,
) -> Result<RelPath, ApiError> {
    let url = api.endpoints().media_url(raw)?;
    let ext = url
        .path()
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .filter(|e| matches!(e.as_str(), "jpg" | "jpeg" | "png" | "webp" | "gif" | "avif"))
        .unwrap_or_else(|| "jpg".into());
    let name = format!("{stem}.{ext}");
    step(label);
    api.download(&url, &out.join(&name), IMAGE_BYTES, IMAGE_TIMEOUT, label)?;
    RelPath::try_from(name).map_err(ApiError::Shape)
}

fn download_video(api: &Kuaishou, out: &Path, raw: &str) -> Result<(), ApiError> {
    let url = api.endpoints().media_url(raw)?;
    step("视频");
    api.download(
        &url,
        &out.join("media/video.mp4"),
        VIDEO_BYTES,
        VIDEO_TIMEOUT,
        "视频",
    )?;
    Ok(())
}

/// 可选的一步出错时：硬错误（网络、限流、写盘）原样报出，其余的把原因记进 `missingReason`。
fn soft(err: ApiError, what: &str) -> Result<String, CallFailure> {
    if err.is_hard() {
        return Err(err.into());
    }
    Ok(format!("{what}：{err}"))
}

fn rel(path: &str) -> Result<RelPath, CallFailure> {
    RelPath::try_from(path).map_err(|e| CallFailure::new(ErrorCode::Internal, e))
}

pub(crate) fn write(out: &Path, name: &str, bytes: &[u8]) -> Result<(), CallFailure> {
    let dest = out.join(name);
    let io = |source| ApiError::Io {
        path: name.to_owned(),
        source,
    };
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    std::fs::write(&dest, bytes).map_err(io)?;
    Ok(())
}
