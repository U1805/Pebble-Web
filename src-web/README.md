# Pebble Web 对齐说明

本文只记录 `src-web` 与 `src-tauri` 之间的**有意差异**和**已确认缺口**。

默认规则如下。

- 未列出的功能默认要求与 `src-tauri` 业务语义对齐。
- Web 可以使用不同 transport，但命令参数、返回值、持久化语义、远端写回、恢复语义和事件语义应等价。
- 不因为浏览器 transport 不同而复制一套业务规则。
- P2 的 Web 专属能力不属于当前对齐范围。
- 多用户、管理页、PWA、Web Push 和 Web 专属安全审计暂不在本文记录为缺失。

## 上游兼容补丁

- `src-tauri` 和共享 crates 保持与 QingJ01/Pebble 上游同步。
- `src-web/patch/` 统一隔离测试中确认、但尚未由上游修复的问题
- `src-web/src` 只保留调用这些补丁的最小接入点，避免以后同步上游时把 Web 修复误认为业务语义分叉。
- 若补丁与上游公开 issue 的行为和根因明确对应，文件名使用 `issueNNN_` 前缀（issue ID 不足三位时前置补 `0`，例如 `issue026_`）；仅症状相似但根因不同的补丁不关联 issue 编号。

当前补丁如下。

| 补丁 | 上游行为 | Web 修复 |
|---|---|---|
| `drafts.rs` | 账户没有 Drafts 文件夹时，本地草稿不关联任何文件夹，页面刷新后无法再访问 | 创建并复用 `__local_drafts__`，保证本地草稿始终具有可见的 Drafts 归属 |
| `outgoing.rs` | 本地 Outbox/Sent 邮件以空 `thread_id` 写入，导致已发送目录在会话视图中为空，回复也不能加入原会话 | 写入外发占位记录前按共享线程规则计算 `thread_id` |
| `archive.rs` | IMAP 没有远端 Archive 时仍移动到 `__local_archive__`，服务器 INBOX 原件随后会被同步为第二条活动记录 | 拒绝没有服务器 Archive 目标的 IMAP 单封和批量归档，避免静默制造重复邮件 |
| `batch_delete.rs` | 批量删除在远端移入 Trash 后仍统一软删除本地记录，pending 重放也会把已经位于 Trash 的普通删除误作本地软删除；离线永久删除还可能提前清除本地记录 | 操作前保存源目录语义，普通删除及重放均幂等移动到 Trash 并保持可见，只有原本位于 Trash 且远端确认成功的邮件才硬删除 |
| `folders.rs` | 本地占位系统目录不会在同角色远端目录出现后移除，无序角色查询可能继续命中 `__local_*`，导致远端写回被错误降级或拒绝 | Web 解析系统目录时优先 provider-backed 目录，仅在没有远端目录时使用本地回退 |
| `issue026_imap_move.rs` | [issue #26](https://github.com/QingJ01/Pebble/issues/26) 暴露了 MOVE 后远端身份改变、本地仍保留旧 `remote_id` 的问题；上游修复覆盖 Outlook，但 IMAP MOVE 后仍继续保留源邮箱 UID，目标邮箱分配新 UID 后，下次同步会把同一邮件导入为第二条活动记录 | 将同一远端身份更新原则扩展到 IMAP：移动前后核对目标邮箱，按 `Message-ID` 或唯一新增 UID 更新本地 `remote_id`；若并发同步已写入权威目标记录并造成身份冲突，则隐藏失效源记录 |
| `issue051_cloud_sync_locales.ts` | 上游为 [issue #51](https://github.com/QingJ01/Pebble/issues/51) 增加自动 WebDAV 备份后，界面使用了未写入中英文 locale 的文案键，中文界面显示英文 fallback | 在 Web i18n 入口合并缺失的自动备份配置、校验和结果文案 |
| `sidebar_empty_accounts.ts` | 多账户场景中，两个账户都尚无文件夹时 Sidebar 会在账户间反复自动切换，最终触发 React 最大更新深度错误 | Web 构建时把空文件夹回退收敛到稳定的“全部邮箱”，并在上游代码变化时要求重新审查补丁 |
| `gmail_oauth.rs` | Gmail OAuth 授权请求未声明离线访问，Google 可以只返回短期 access token 而不返回 refresh token，令牌过期后账户无法继续同步 | Gmail 授权请求增加 `access_type=offline` 和 `prompt=consent`，确保首次授权和重新授权都取得 refresh token |

SMTP 回复邮件缺少 `Message-ID` / `References`、回复后未设置 IMAP `\Answered`，以及 IMAP IDLE 偶发漏掉首次事件，当前都位于共享邮件实现中。为避免复制 SMTP/IMAP transport，本轮不在 Web 层覆盖；待上游修复后直接随共享 crates 同步。

## 桌面专属功能

以下功能依赖桌面操作系统或 Tauri 宿主。

Web 不实现对应桌面行为。

| 功能 | Tauri 命令或能力 | Web 行为 |
|---|---|---|
| 开机启动 | `get_autostart_enabled` | 返回浏览器平台不可用状态 |
| 设置开机启动 | `set_autostart_enabled` | 明确报告浏览器不支持 |
| 托盘菜单文案 | `set_tray_menu_labels` | 浏览器无托盘，不执行桌面操作 |
| 启动时 mailto 队列 | `take_pending_mailto_urls` | 浏览器不维护桌面 mailto 启动队列 |
| 默认邮件客户端设置 | `open_default_mail_settings` | 浏览器不能修改操作系统默认邮件客户端 |
| 原生标题栏主题同步 | `sync_titlebar_theme` | 浏览器使用页面自身样式 |
| 原生窗口控制与窗口事件 | `getCurrentWindow()` 的 `show` / `hide` / `close` / `minimize` / `toggleMaximize` / `onCloseRequested` / `onFocusChanged` | 浏览器拥有自己的窗口 chrome。Web CSS 隐藏上游 `TitleBar`，这些 Tauri window shim 不执行桌面窗口操作 |
| 托盘注意力标记 | `clear_notification_attention` 对应的 tray attention | 浏览器没有系统托盘，状态固定为无 tray attention |

实现位置主要在：

```text
src-web/frontend/runtime/desktop.ts
src-web/frontend/tauri/window.ts
```

## 浏览器等价实现

以下能力与桌面端目标相同，但 Web 使用浏览器或 HTTP 等价实现。

### Profile storage namespace

Tauri 根据桌面 profile 和迁移后的 app data 目录生成 namespace。

当前 Web 为单用户服务，返回稳定的 `web` namespace，使共享前端的浏览器本地存储仍然具备稳定作用域。

多用户属于后续 Web 专属能力，不在当前桌面功能重构范围。

实现位置：

```text
src-web/src/profile.rs
```

### 数据加密密钥

Tauri 通过 `CryptoService::init()` 从操作系统 credential store 读取或创建数据加密密钥。

Web 服务不依赖桌面 credential store。

Web 先读取 `PEBBLE_ENCRYPTION_KEY`，其次读取数据目录中的 `encryption.key`。

如果两者都不存在，Web 生成新密钥并保存到数据目录，再通过 `CryptoService::from_key()` 使用同一套加密格式。

实现位置：

```text
src-web/src/crypto.rs
src-web/src/state.rs
```

### 命令 transport

Tauri 使用 IPC。

Web 使用：

```text
POST /api/v1/command/{name}
```

Web transport 只转换顶层 command 参数名。

嵌套 DTO 继续遵循对应 Rust 类型自己的 Serde 规则。

HTTP 401 只表示 Pebble Web 登录会话无效。

邮件账户的 IMAP/SMTP/OAuth 认证错误仍然作为普通 command 业务错误返回，不能触发 Web 会话退出。

Tauri 直接序列化 `PebbleError`，Web 则把同一业务错误映射为 HTTP 状态与 JSON body。

Web 必须保留 `PebbleError` 变体中的原始 `message`，使共享前端的 `extractErrorMessage()` 与桌面端得到相同文本。

实现位置：

```text
src-web/frontend/runtime/invoke.ts
src-web/src/command_router.rs
```

### 事件

Tauri 使用 Tauri event bus。

Web 使用经过鉴权的 WebSocket。

浏览器 shim 保持 `listen()` 的事件名和 payload 形态兼容。只要仍有 listener 和有效 Web 登录 token，认证前或认证后的普通传输断线都会重连。服务端 4001 鉴权失败会结束会话且不重连。

实现位置：

```text
src-web/frontend/runtime/events.ts
src-web/frontend/tauri/event.ts
src-web/src/events.rs
src-web/src/realtime/
```

### Realtime preference

Tauri 在桌面进程存活期间把 realtime preference 应用到当前账户的长驻同步 worker。

Web 服务可能在浏览器打开之前已经启动，因此除保持与 Tauri 相同的 `realtime`、`balanced`、`battery`、`manual` 间隔语义外，还把当前 preference 加密持久化到服务端。

服务重启后会先恢复该 preference，再启动对应账户的长驻同步 worker。

`manual` 继续表示不启动后台轮询。

实现位置：

```text
src-web/src/commands/sync_cmd.rs
src-web/src/sync_runtime.rs
```

### 系统通知

Tauri 使用操作系统通知。

Web 使用 Browser Notification API。

Web 只复刻桌面端已有的用户通知语义：新邮件和 snooze 到期。`mail:error` 等普通应用事件不会被 Web 额外升级成系统通知。

共享的 `mail:new` 和 `mail:unsnoozed` payload 保持与 Tauri 一致。服务端另发 Web-only `web:notification` 事件传递浏览器通知正文和可选点击目标。

新邮件通知保持桌面端的 `Pebble - New Mail` 标题，并允许点击后打开对应邮件。snooze 到期通知保持 `Pebble - Snoozed Message` 标题，不增加桌面端不存在的邮件点击目标。

桌面端进程启动后默认启用通知 gate。浏览器不能在没有用户授权的情况下获得 Notification 权限，因此新的 Web profile 会先写入关闭偏好，用户主动启用并授权后才打开 runtime gate。权限拒绝或后续失效时，Web 会把关闭状态同步回共享 UI 偏好。

`get_notification_status` 在 Web 返回 `platform: "web"` 和 `app_id: null`。测试通知使用浏览器专用成功或权限错误文案。

Browser Notification 需要至少有一个已打开且已授权的 Pebble Web 页面接收服务端事件。浏览器完全关闭时的后台推送属于 Web Push/PWA 范围，当前不纳入桌面功能对齐。

实现位置：

```text
src-web/frontend/runtime/notifications.ts
src-web/src/commands/indexing.rs
src-web/src/snooze_watcher.rs
```

### OAuth 浏览器流程

Tauri 使用桌面 OAuth 回调流程。

Web 使用浏览器 popup、HTTP callback、PKCE 和一次性 state。

Web 的 pending state 只接受 5 分钟内到达的 callback。服务端在 callback 到达后保存 `processing / success / error` 流程状态。前端在 5 分钟点或 popup 提前关闭时查询该状态。callback 未到达时 command 超时或取消，已经到达时继续等待 token exchange 和账户写入，并返回服务端最终结果。

popup 关闭或超时判定会原子撤销仍处于 pending 的 state。callback 与撤销通过同一 pending 锁竞争，避免前端已返回取消或超时后服务器又创建账户。

共享账户页面对 Gmail 和 Outlook 调用 `complete_oauth_flow`。

Web 的 `add_account` 只接受 IMAP 和 POP3，因为该命令写入 IMAP/SMTP 凭据。

Web 不允许该命令创建 Gmail 或 Outlook 账户，避免生成 provider 与 auth data 格式不匹配的账户。

OAuth token、proxy 更新和 refresh 使用相同的账户级锁，避免并发覆盖凭据。

debug/test 构建可设置 `PEBBLE_OAUTH_TEST_BASE_URL`，把 Gmail 和 Outlook 的
`authorize`、`token`、`userinfo` 请求指向独立可控测试服务。release 构建忽略该变量，
继续固定使用 Google 与 Microsoft 官方端点。

实现位置：

```text
src-web/src/oauth.rs
src-web/frontend/runtime/oauth.ts
```

### 文件下载

Tauri 可以返回本地文件路径。

浏览器不能直接访问服务器文件系统。

Web 使用鉴权 HTTP 下载和 Blob 保存。浏览器不能指定或读取真实的操作系统保存目录，因此 Web 忽略 `save_to` 的目录部分，但保留它的 basename 作为 `anchor.download` 建议文件名。

Tauri 返回实际保存路径。Web 返回浏览器建议下载文件名，供共享 UI 表示下载完成。浏览器可能自行处理重名文件，因此 Web 不能报告最终磁盘路径。`attachment:download-progress` 通过浏览器本地事件保持相同 payload。

Web 下载端点还要求源文件位于 Pebble 附件目录内，避免 HTTP 服务读取任意服务器文件。该安全边界比桌面进程更严格。

实现位置：

```text
src-web/src/commands/attachments.rs
src-web/frontend/runtime/attachments.ts
```

### Compose 附件上传

Tauri 可以读取本地文件路径或字节。

Web 使用 `multipart/form-data` 上传到服务端暂存区，再进入与桌面端等价的 durable attachment/send 流程。

实现位置：

```text
src-web/src/commands/attachments.rs
src-web/frontend/runtime/attachments.ts
```

### 背景图片导入

Tauri 可以直接接收本地二进制参数。

Web 使用 multipart，避免把大文件扩展成 JSON number array。

背景图片写入和删除继续要求 Web 登录。共享前端通过 CSS URL 直接读取图片，浏览器不能为该请求附加 Bearer header，因此 Web 的图片读取端点不要求 JWT，只允许读取背景目录中的随机 `background-{id}.{ext}` 文件。

实现位置：

```text
src-web/src/commands/appearance.rs
src-web/frontend/runtime/attachments.ts
```

### 外部链接

Tauri 使用系统 opener。

Web 对 HTTP(S) 使用带 `noopener,noreferrer` 的 `window.open()`，对 `mailto:` 使用当前页面导航。

两端都只允许：

```text
http
https
mailto
```

浏览器在 `noopener` 成功打开新上下文时也可以返回 `null`，因此 Web 不能通过 `window.open()` 返回值可靠区分正常打开和 popup block。Web 保留安全的 `noopener,noreferrer`，只把同步 JavaScript 异常作为 command 错误返回。

### 应用版本

桌面端版本来自上游应用版本。

Web 构建时从根 `package.json` 读取同一版本，而不是使用 `src-web` crate 的技术版本。

实现位置：

```text
src-web/build.rs
```

### 诊断日志

Tauri 的 `read_app_log` 读取桌面进程写入的 `pebble.log`。

Web 使用同一命令读取服务进程写入的数据目录 `logs/pebble-web.log`。

两端保持相同的日志尾部读取、大小上限、缺失文件和返回结构语义。

实现位置：

```text
src-web/src/main.rs
src-web/src/config.rs
src-web/src/commands/diagnostics.rs
```

## 应实现但未实现

本节只记录已经通过 Tauri/Web 对照确认的桌面功能缺口。

当前审计继续进行。

若本节为空，不代表永远不存在缺口。

每次上游同步后都应重新检查：

```text
src-tauri/src/commands/**
src-tauri/src/realtime/**
src-tauri/src/events.rs
src-tauri/src/lib.rs
```

发现缺口后，应先记录到本节，再实现和移除记录。

## 当前不纳入对齐范围

以下项目属于后续 Web 专属产品能力，不属于当前“重构桌面版功能”工作。

```text
多用户
管理页
PWA
Web Push
Web 专属安全审计页
Web 专属部署管理功能
```

这些功能不能用于判断当前 Tauri/Web 功能对齐是否完成。
