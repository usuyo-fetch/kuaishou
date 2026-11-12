# usuyo-fetch/kuaishou

快手的获取插件，供 [usuyo](https://github.com/usuyo/usuyo) 调用：按分享链接取回单条图文或视频。

| 方法 | 清单里的 id | 取得 | 账号 | 层级 | 现状 |
|---|---|---|---|---|---|
| 分享页 | `share-page` | 单个作品：描述、作者、发布时间、计数、封面、图文的图片、视频、原始数据 | 不使用 | `public-web` | 已实现；2026-11-12 对着真实页面跑过四次：短链、不带参数的链接、网页版分享链接 `/f/` 都取到视频或不可用的回答；图文、验证页没有量到 |
| KS-Downloader | `ks-downloader` | 单条图文、视频：问你自己在本机运行的 KS-Downloader，拿它给的文字、作者、时间、计数、封面、图片和视频地址，图片和视频由本插件下载 | 不使用（插件不给它 cookie；它自己的设置里要放一份不登录的网页 cookie） | `public-web` | 已实现，只对着手写的假服务测过，没有运行过真的 KS-Downloader |
| iPhone 镜像 | `iphone` | 存过的：收藏、赞过 | 只在使用者手机的 App 内 | `vision-ui` | 未实现 |
| 安卓手机 | `android` | 存过的：收藏、赞过 | 只在使用者手机的 App 内 | `vision-ui` | 未实现 |
| MediaCrawler 的结果文件 | `mediacrawler` | 存下的：你自己跑 MediaCrawler 留下的快手帖子文件（json 或 jsonl，或把 data 文件夹压成的 zip）里每篇的链接、标题、作者昵称、发布时间；不读评论、CSV、SQLite；只列链接，内容交给本插件的其他方法去取 | 不使用（文件是使用者自己用登录了账号的 MediaCrawler 跑出来的，这种方法不联网） | `private-api`，默认关 | 已实现，只对着手写的文件测过，没有对着 MediaCrawler 真正的输出比对过 |

## KS-Downloader 方法

调用你自己装、自己运行的 [KS-Downloader](https://github.com/JoeanAmier/KS-Downloader)（GPL-3.0）：插件不带它、不下载它、不安装它，只经本机 HTTP 问它，所以插件本身仍是 MIT。

- 你要先启动它的 API 模式：`python main.py api --host 127.0.0.1`（端口默认 5557；它的 `--host` 默认是 `0.0.0.0`，会让局域网里的别人也连得上，所以建议写 `127.0.0.1`）。
- 它的设置里要放一份**不登录**的网页 cookie（它的 README 说无需登录）；不要放登录了你账号的 cookie。插件自己不给它任何 cookie 或账号信息。
- 服务地址在插件设置里（`endpoint`，默认 `http://127.0.0.1:5557`），只接受本机地址。
- 调用：`POST <endpoint>/detail/`，请求体只有 `{"text": <链接>}`；回答里的图片、视频地址由本插件下载，只去快手自己的媒体主机，文件名和 `share-page` 一样。身份和 `share-page` 是同一个。
- `health` 只连本机：服务没在运行、或那个地址上答话的不是 KS-Downloader 时，只挡 `ks-downloader` 这一种方法，一句话说怎么启动。
- 它的失败回答没有固定的编号，分不出删除、验证、网络：`data` 为空一律回 `not-found`，原文放在 `detail`；读不懂的回答回 `upstream-changed`。
- 接口、字段来自它 1.6 的源码和 README，没有运行过它；对手机页链接（短链展开后就是）它按源码走「图片」那一支，拿不到视频地址，插件把这种回答记成缺视频，交给别的方法补。

分享页解析移植自 [parse-video](https://github.com/wujunwei928/parse-video)（MIT，Copyright (c) 2022 wujunwei928），字段名对照 [ParseHub](https://github.com/z-mio/ParseHub)（MIT，Copyright (c) 2024 梓澪）。
