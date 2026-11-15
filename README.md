# usuyo-fetch/kuaishou

快手的获取插件，供 [usuyo](https://github.com/usuyo/usuyo) 调用：按分享链接取回单条图文或视频。

| 功能 | 实现方式（方法 id） | 账号 | 风险层级 | 状态 |
|---|---|---|---|---|
| 按链接取单条内容（图文、视频） | `share-page` | 不使用 | `public-web`，默认开 | 已实现；2026-11-12 对着真实页面跑过四次（短链、不带参数的链接、网页版分享链接 `/f/`），取到视频或不可用的回答；图文、验证页没有量到 |
| 列出我的收藏 | `iphone` | 只在使用者手机的 App 内 | `vision-ui`，默认关 | 未实现 |
| 列出我的收藏 | `android` | 只在使用者手机的 App 内 | `vision-ui`，默认关 | 未实现 |
| 列出赞过的内容 | `iphone` | 只在使用者手机的 App 内 | `vision-ui`，默认关 | 未实现 |
| 列出赞过的内容 | `android` | 只在使用者手机的 App 内 | `vision-ui`，默认关 | 未实现 |

## 方法

### share-page

取单个作品：描述、作者、发布时间、计数、封面、图文的图片、视频、原始数据。读手机页 `v.m.chenzhongtech.com/fw/photo/<id>`（短链 `v.kuaishou.com/<码>` 跳去的那一页）里内嵌的 `INIT_STATE`，请求不带 cookie，不读宿主交来的凭据。桌面页 `www.kuaishou.com/short-video/<id>` 只有页面壳，不用。2026-11-12 不带 cookie 取一条短链和它的手机页都没有遇到验证页；频繁请求会出验证页。

### iphone

通过 iPhone 镜像在使用者手机的快手 App 内读收藏和赞过的作品，需要登录，只在使用者自己的手机上。未实现。

### android

通过安卓手机在使用者手机的快手 App 内读收藏和赞过的作品，需要登录，只在使用者自己的手机上。未实现。

分享页解析移植自 [parse-video](https://github.com/wujunwei928/parse-video)（MIT，Copyright (c) 2022 wujunwei928），字段名对照 [ParseHub](https://github.com/z-mio/ParseHub)（MIT，Copyright (c) 2024 梓澪）。

## 开发

`Cargo.toml` 里的 `usuyo-plugin`、`usuyo-plugin-kit` 是对 `https://github.com/usuyo/usuyo` 的 git 依赖，钉在固定的 `rev`。

本地开发时用旁边 `usuyo/` 的工作区代替它们：`usuyo-fetch/.cargo/config.toml`（不进任何插件仓库）写了下面的覆盖，cargo 从当前目录往上找这个文件，在插件文件夹里跑 cargo 就生效；从别处跑要加 `--config <这个文件>`。

```toml
[patch."https://github.com/usuyo/usuyo"]
usuyo-plugin = { path = "../usuyo/crates/usuyo-plugin" }
usuyo-plugin-kit = { path = "../usuyo/crates/usuyo-plugin-kit" }
```

`Cargo.lock` 记的是覆盖后的路径形式，所以 CI 不加 `--locked` 构建，cargo 只重新锁这两个 usuyo crate。
