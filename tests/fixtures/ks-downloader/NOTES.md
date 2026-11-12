# ks-downloader 夹具

NOT MEASURED：这里每一个文件都是手写的，内容是编的；没有运行过 KS-Downloader，没有对快手发过请求。
形状照 KS-Downloader 1.6（和主分支 1.7 beta）的源码写，2026-11-12 用 `gh api` 读的：

- `source/app/app.py`：`POST /detail/` 的处理，`ResponseModel`（`message`、`params`、`data`、`time`），
  `detail_one` 在存库后返回 `data`（存库时 `download` 被用空格连成字符串）。
- `source/model/response.py`、`source/model/detail.py`、`source/model/base.py`：请求体 `{text, cookies, proxy}`。
- `source/extract/extractor.py`：`data` 的字段（`photoType`、`detailID`、`caption`、`coverUrl`、`duration`、
  `realLikeCount`、`viewCount`、`shareCount`、`commentCount`、`timestamp`、`authorID`、`name`、`download`）。
- `locale/en_US/LC_MESSAGES/ks.po`：英文界面的 `Video`、`Image`、失败信息。

占位符：`{BASE}` 是测试起的本机假服务的地址，`{ID}` 是作品 id，`{LINK}` 是测试请求里的链接。

| 文件 | 说明 |
|---|---|
| `video.json` | 视频（网页那一支），`download` 是一个地址，有时长 |
| `images.json` | 图集（`download` 是用空格连起来的三个地址），`name` 退回成作者 id |
| `single-image.json` | 英文界面，单图：`download` 是数组，只有封面；`timestamp` 是 `unknown` |
| `cover-only.json` | 写着图片、没有任何地址：源码推断是手机页链接上的视频（见 `src/ksdownloader.rs` 顶部），没有量过 |
| `no-data.json`、`no-data-en.json` | `data` 为 `null`，`message` 说失败（中文、英文界面） |

测试里别的错误回答（HTTP 422、不是 JSON、字段类型不对）直接在测试代码里写。
