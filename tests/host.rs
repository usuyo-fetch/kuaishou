//! 真的二进制：不需要网络的子命令，以及清单经宿主的校验和调用。

use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;
use usuyo_plugin::protocol::{Access, Account, Canonicalized, ErrorCode, Identity, Login};
use usuyo_plugin::{
    CallLimits, CallOptions, FetchInput, FetchRequest, HostConfig, PluginError, PluginHost,
    discover,
};

const BIN: &str = env!("CARGO_BIN_EXE_usuyo-fetch-kuaishou");

/// 清空环境后运行，和宿主一样。
fn run(args: &[&str]) -> (i32, Value) {
    let output = Command::new(BIN).args(args).env_clear().output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json = if stdout.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&stdout).unwrap()
    };
    (output.status.code().unwrap(), json)
}

#[test]
fn canonicalize_needs_no_network_for_full_links() {
    let (code, out) = run(&[
        "canonicalize",
        "https://v.m.chenzhongtech.com/fw/photo/3x24h4gjvevux4w?cc=share_copylink",
    ]);
    assert_eq!(code, 0);
    assert_eq!(out["ok"], true);
    assert_eq!(out["match"], true);
    assert_eq!(out["identity"]["platform"], "kuaishou");
    assert_eq!(out["identity"]["contentId"], "3x24h4gjvevux4w");
    assert_eq!(
        out["identity"]["url"],
        "https://www.kuaishou.com/short-video/3x24h4gjvevux4w"
    );
    assert!(out.get("resolvedUrl").is_none());

    let (code, out) = run(&[
        "canonicalize",
        "https://example.com/short-video/3x24h4gjvevux4w",
    ]);
    assert_eq!((code, &out["match"]), (0, &Value::Bool(false)));
}

#[test]
fn health_does_not_touch_the_network() {
    for args in [&["health"][..], &["health", "--probe"][..]] {
        let (code, out) = run(args);
        assert_eq!(code, 0);
        assert_eq!(
            (out["ready"].as_bool(), out["protocol"].as_u64()),
            (Some(true), Some(1))
        );
        assert_eq!(out["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(out["login"], serde_json::json!([]));
        assert!(out["checks"].as_array().is_none_or(Vec::is_empty));
    }
}

#[test]
fn bad_usage_exits_2() {
    for args in [
        &[][..],
        &["nope"][..],
        &["fetch", "x"][..],
        &["fetch", "x", "--out", "/o"][..],
        &["sources"][..],
        &["canonicalize"][..],
    ] {
        let (code, out) = run(args);
        assert_eq!((code, out), (2, Value::Null), "{args:?}");
    }
}

#[test]
fn each_call_goes_to_its_method() {
    // 方法和调用对不上的回 invalid-args，不发请求
    for args in [
        &["sources", "--method", "share-page"][..],
        &["list", "kuaishou:likes", "--method", "iphone"][..],
        &[
            "fetch",
            "https://www.kuaishou.com/short-video/3x24h4gjvevux4w",
            "--out",
            "/o",
            "--method",
            "android",
        ][..],
    ] {
        let (code, out) = run(args);
        assert_eq!(
            (code, &out["error"]["code"]),
            (1, &Value::from("invalid-args")),
            "{args:?}"
        );
    }
    // 不是快手的链接：不发请求
    let tmp = tempfile::tempdir().unwrap();
    let (code, out) = run(&[
        "fetch",
        "https://example.com/x",
        "--out",
        tmp.path().to_str().unwrap(),
        "--method",
        "share-page",
    ]);
    assert_eq!(code, 1);
    assert_eq!(out["error"]["code"], "unsupported-input");
}

/// 清单装成宿主认的样子：`<root>/usuyo-fetch/kuaishou/<version>/`。
fn host() -> (tempfile::TempDir, PluginHost) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("plugins/usuyo-fetch/kuaishou/0.1.0");
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("usuyo-plugin.yaml");
    std::fs::copy(manifest, dir.join("usuyo-plugin.yaml")).unwrap();
    std::fs::copy(BIN, dir.join("usuyo-fetch-kuaishou")).unwrap();
    let found = discover(&tmp.path().join("plugins")).unwrap();
    assert!(found.rejected.is_empty(), "{:?}", found.rejected);
    let host = PluginHost::new(
        HostConfig {
            work_dir: tmp.path().join("work"),
            cache_dir: tmp.path().join("cache"),
            locale: usuyo_plugin::Locale::new("zh-CN"),
            limits: CallLimits::default(),
        },
        found.fetch,
    );
    (tmp, host)
}

#[tokio::test]
async fn the_manifest_loads_and_matches_kuaishou_links() {
    let (_tmp, host) = host();
    let plugin = &host.plugins()[0];
    assert_eq!(plugin.id().as_str(), "usuyo-fetch/kuaishou");
    assert_eq!(plugin.header.version.to_string(), env!("CARGO_PKG_VERSION"));
    let spec = &plugin.spec;
    // routes.md 第 8 节的每一行，顺序照清单
    let ids: Vec<&str> = spec.methods.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["share-page", "iphone", "android"]);
    let share = spec.method("share-page").unwrap();
    assert_eq!(
        (share.access, share.login, share.account),
        (Access::PublicWeb, Login::None, Account::None)
    );
    assert!(share.built && share.sites.is_empty() && share.capabilities.fetch.is_some());
    assert!(share.on_by_default());
    for (id, access, account) in [
        ("iphone", Access::VisionUi, Account::OwnPhone),
        ("android", Access::VisionUi, Account::OwnPhone),
    ] {
        let m = spec.method(id).unwrap();
        assert!(!m.built, "{id}");
        assert_eq!((m.access, m.account), (access, account), "{id}");
    }
    for hit in [
        "https://v.kuaishou.com/mkYOSR",
        "https://www.kuaishou.com/short-video/3x24h4gjvevux4w",
        "https://www.kuaishou.com/f/X-aqEn4ZUlPtlME2",
        "https://v.m.chenzhongtech.com/fw/photo/3x24h4gjvevux4w?cc=share_copylink",
    ] {
        let url = url::Url::parse(hit).unwrap();
        assert_eq!(host.candidates(&url).len(), 1, "{hit}");
    }
    let url = url::Url::parse("https://www.kuaishou.com/profile/3xdng3ssi9kdpk2").unwrap();
    assert!(host.candidates(&url).is_empty());

    // 经宿主调一次：输出要过宿主的 schema 校验
    let url = url::Url::parse("https://www.kuaishou.com/short-video/3x24h4gjvevux4w").unwrap();
    let canonical = host
        .canonicalize(host.candidates(&url)[0], &url, &CallOptions::default())
        .await
        .unwrap();
    let Canonicalized::Match {
        identity: Identity::Platform { content_id, .. },
        ..
    } = canonical
    else {
        panic!("{canonical:?}")
    };
    assert_eq!(content_id, "3x24h4gjvevux4w");
}

#[tokio::test]
async fn a_link_that_is_not_a_post_is_turned_away_through_the_host() {
    let (_tmp, host) = host();
    let plugin = &host.plugins()[0];
    // 主机名对得上 `match`，路径认不出：插件回 unsupported-input
    let url = url::Url::parse("https://www.kuaishou.com/short-video/a").unwrap();
    let request = FetchRequest {
        method: "share-page".into(),
        input: FetchInput::Url(url),
        want: None,
        input_file: None,
    };
    let err = host
        .fetch(plugin, request, &CallOptions::default())
        .await
        .unwrap_err();
    let PluginError::Call { failure, .. } = err else {
        panic!("{err:?}")
    };
    assert_eq!(failure.code, ErrorCode::UnsupportedInput);
}
