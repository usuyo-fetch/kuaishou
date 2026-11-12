# usuyo-fetch/kuaishou

快手的获取插件，供 [usuyo](https://github.com/usuyo/usuyo) 调用：按分享链接取回单条图文或视频。

| 方法 | 清单里的 id | 取得 | 账号 | 层级 | 现状 |
|---|---|---|---|---|---|
| 分享页 | `share-page` | 单个作品：描述、作者、发布时间、计数、封面、图文的图片、视频、原始数据 | 不使用 | `public-web` | 已实现；2026-11-12 对着真实页面跑过四次：短链、不带参数的链接、网页版分享链接 `/f/` 都取到视频或不可用的回答；图文、验证页没有量到 |
| KS-Downloader | `ks-downloader` | 单条图文、视频 | 不使用（一份不登录的网页 cookie） | `public-web` | 未实现 |
| iPhone 镜像 | `iphone` | 存过的：收藏、赞过 | 只在使用者手机的 App 内 | `vision-ui` | 未实现 |
| 安卓手机 | `android` | 存过的：收藏、赞过 | 只在使用者手机的 App 内 | `vision-ui` | 未实现 |
| MediaCrawler 的结果文件 | `mediacrawler` | 存下的：你自己跑 MediaCrawler 留下的快手帖子文件（json 或 jsonl，或把 data 文件夹压成的 zip）里每篇的链接、标题、作者昵称、发布时间；不读评论、CSV、SQLite；只列链接，内容交给本插件的其他方法去取 | 不使用（文件是使用者自己用登录了账号的 MediaCrawler 跑出来的，这种方法不联网） | `private-api`，默认关 | 已实现，只对着手写的文件测过，没有对着 MediaCrawler 真正的输出比对过 |

分享页解析移植自 [parse-video](https://github.com/wujunwei928/parse-video)（MIT，Copyright (c) 2022 wujunwei928），字段名对照 [ParseHub](https://github.com/z-mio/ParseHub)（MIT，Copyright (c) 2024 梓澪）。
