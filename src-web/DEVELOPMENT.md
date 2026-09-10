# Pebble Web 开发与维护指南

本文面向开发人员和 Agent，说明项目边界和维护方法。运行配置见 [README](README.md)，平台差异、补丁和已确认缺口见 [COMPATIBILITY](COMPATIBILITY.md)。归档计划与旧 TODO 只解释历史决策，不能覆盖当前代码和现行文档。

## 维护目标与范围

唯一代码上游是 [QingJ01/Pebble](https://github.com/QingJ01/Pebble)。项目直接使用当前上游的 Rust crates 和 React 前端，把平台差异集中在 `src-web/`。旧 Pebble-Web 只曾提供早期实现参考，不是当前契约来源。

长期可合并上游比 Web 代码完全独立更重要。上游业务逻辑变化时，先接受新逻辑，再更新 Web 适配；不要保留旧业务实现来回避冲突。

项目面向一名用户管理自己的邮箱，不计划成为通用 Webmail 服务或服务器控制面板。多用户、用户管理页、Web 专属安全审计页、部署管理页和 Web Push 已从当前范围移除。正常安全日志与部署安全要求仍然保留。

主要个人邮件工作流已有 Web 实现。命令存在、编译成功或差异台账为空，都不能证明业务语义全部等价；每次上游同步后仍需重新审计。

## 修改分类

编码前先判断改动属于哪一类：

| 类别 | 目的 | 主要位置 |
| --- | --- | --- |
| Tauri/Web 对齐 | 与当前桌面业务语义一致 | `src-web/src/`、`src-web/frontend/` |
| 浏览器等价实现 | 通过不同平台 API 达到相同业务目标 | `frontend/runtime/`、`frontend/tauri/` |
| 上游兼容补丁 | 修复经测试确认、尚未由上游修复的问题 | `src-web/patch/` |
| Web 专属产品功能 | 移动端布局、可选 PWA 等 | Web 专属文件，不进入共享业务代码 |

开始修改前明确：当前 Tauri 行为、共享 crate 已有能力、是否确需修改上游目录、是否会产生第二套业务规则、需要更新的差异记录，以及验证业务结果的方法。

## 架构与代码组织

### 共享目录

`crates/`、`src/` 和 `src-tauri/` 以上游为准，默认不加入 Web 专用改动。不要复制核心 crate、完整 React 前端或 SMTP/IMAP transport。

根 Cargo workspace 直接包含 `src-web`，Web crate 通过路径依赖复用共享核心。`tests/` 主要保留上游前端测试；Web 专属回归应尽量放在 `src-web/` 对应模块附近。

### Rust 服务

`src-web/src/` 尽量按 `src-tauri/src/` 的路径组织，把桌面实现当作维护索引。例如上游修改 `commands/messages/flags.rs`，就检查 Web 的对应文件。

Web 独有基础设施可以独立组织，包括 `auth.rs`、`config.rs`、`crypto.rs`、`error.rs`、`oauth.rs`、`sync_runtime.rs` 和 `browser_notifications.rs`。不要为了内部整洁而重新设计一套不便与 Tauri 对照的目录结构。

### 前端平台边界

共享 `src/` 尽量不知道 Web 的存在。Web 构建通过 [vite.config.ts](vite.config.ts) 将 `src/main.tsx` 对 `./App` 的引用替换为 `frontend/ui/WebApp.tsx`，并配置模块 alias：

```text
@tauri-apps/api/core
@tauri-apps/api/event
@tauri-apps/api/app
@tauri-apps/api/path
@tauri-apps/api/window
@/lib/i18n
```

Tauri API 指向 `frontend/tauri/` 的 shim，再调用 `frontend/runtime/`。WebApp 负责登录、通知和浏览器环境包装；Web CSS 也从这个边界进入。

在共享 `src/lib/platform/` 维护平台选择层的早期方案已放弃。不要重新向共享组件加入大量 `if web`，也不要为少量重复进行大规模跨层重构。

## Command 与 Event 契约

桌面使用 Tauri IPC，Web 使用 `POST /api/v1/command/{name}`。Web 不设计第二套业务 REST 模型；同一业务动作保持相同命令名、参数含义和返回语义。

每个命令至少核对：名称、必填/可选参数及默认值、返回值、错误文本与类别、SQLite 持久化、远端写回、pending operation、失败恢复、后台 worker、并发和事件。

transport 只将顶层 command 参数名从 camelCase 转成 snake_case。嵌套 DTO 遵循其 Rust 类型的 Serde 规则。优先复用上游名字和类型，不创建同义业务 DTO；Web 专用 DTO 只用于 transport、认证或浏览器边界。

Web 的事件通过鉴权 WebSocket 传输，shim 保持 Tauri 风格的 `listen()`。事件名和 payload 以 Tauri 为准；envelope 元数据不能自动并入 payload，例如不能追加桌面 payload 没有的 `account_id`。

平台不存在的开机启动、托盘、原生窗口和默认邮件客户端设置可以明确返回不支持或执行安全 NOOP。不得用隐藏功能或“伪成功 NOOP”处理平台无关的业务功能。具体行为以 [COMPATIBILITY](COMPATIBILITY.md) 为准。

## Web 运行边界

### 会话与安全

当前服务为单用户部署，JWT 的 `sub` 固定为 `admin`。登录密码和签名密钥来自 `PEBBLE_PASSWORD`、`PEBBLE_JWT_SECRET`。登录后使用 Bearer token 调用业务 command；公开健康检查和登录是明确的例外。

HTTP 401 只表示 Web 会话无效。IMAP、SMTP 和 OAuth 认证失败是邮件业务错误，不能触发 Web 退出登录。错误映射应保留共享 `PebbleError` 的原始消息。

新增实现必须遵守这些边界：

- 业务 command 默认鉴权，公开资源例外必须明确限制范围。
- 不记录密码、完整 OAuth token/secret，普通诊断日志不记录邮件正文。
- 上传和下载限制在受控数据目录，不信任客户端提供的本地路径。
- OAuth state 一次性使用且有过期时间；回调、取消、超时不能竞态创建账户。
- 使用高熵 JWT secret，替换默认密码；正式远程部署使用 HTTPS。
- 不给应用 Docker socket 或其他宿主机管理权限。

### 数据与加密

服务默认数据目录为 `/data`，可通过 `PEBBLE_DATA_DIR` 修改。主要内容包括 `pebble.db`、`attachments/`、`index/`、`logs/`、背景图片和 `encryption.key`。

数据格式复用共享 `CryptoService`。Web 优先读取 `PEBBLE_ENCRYPTION_KEY`，然后读取数据目录的 `encryption.key`，都不存在时生成并持久化新密钥。Web 不依赖桌面 credential store，也不把 Web 密钥读取逻辑写入 `pebble-crypto`。

### 后台任务

服务端承担同步、实时 worker、pending 重放、索引更新、Snooze、规则、OAuth refresh 和自动 WebDAV 备份。它们按配置运行，不能依赖浏览器页面是否打开。

Web 将 realtime preference 加密持久化，重启后先恢复偏好再启动同步 worker。`manual` 仍表示不启动后台轮询。启动时搜索恢复/重建先于同步和 pending worker，以避免索引被并发清空。

Browser Notification 只复刻新邮件和 Snooze 到期通知，要求至少一个已打开且获授权的页面；普通 `mail:error` 不自动升级为系统通知。页面全部关闭后没有系统通知是预期行为，不以 Web Push 补齐。

### OAuth 与文件

Gmail/Outlook 使用 popup、HTTP callback、PKCE 和一次性 state。token refresh、代理更新和账户凭据更新必须保持账户级并发保护。普通 `add_account` 只创建 IMAP/POP3 账户，OAuth 账户经 `complete_oauth_flow` 创建。

上传使用 multipart，进入受控暂存区和 durable attachment 流程，不把大文件扩展成 JSON number array。下载使用鉴权 HTTP 与 Blob；浏览器只能得到建议文件名，不能报告真实磁盘保存路径。

OAuth 取消/超时、文件边界、通知权限及其他平台细节统一维护在 [COMPATIBILITY](COMPATIBILITY.md)，不要在多份文档重复维护完整规则。

## 上游兼容补丁与差异台账

`patch/` 只保存经过测试确认的上游问题，不承载一般 Web 适配或产品需求。新增补丁应满足：问题来自上游行为、Web 正常使用需要修复、接入足够薄、有复现或测试证据，并已登记在 [COMPATIBILITY](COMPATIBILITY.md)。上游修复同一根因后优先移除本地补丁。

关联上游公开 issue 时使用 `issueNNN_` 前缀；只有症状相似、根因不同的补丁不关联 issue 编号。当前补丁的准确文件名、原因和处置以差异台账为准，不在本指南维护第二份列表。

共享 SMTP/IMAP 的已知问题也在差异台账记录。不要为修复它们复制 transport；若必须在 Web 修复，先证明可以保持很薄的补丁。

差异台账只记录补丁、桌面专属功能、浏览器等价实现和已确认但未实现的对齐缺口。发现缺口先登记，修复后移除。正常等价功能和历史任务列表不进入台账。

## 上游同步

长期分支为 `upstream` 和 `web`：前者跟随原作者 Pebble，后者保留 Web 适配。remote 与 branch 是不同概念，执行前检查 `git remote -v`、`git branch -vv`，不能把 fork 的 `origin/upstream` 自动当成原作者最新提交。

1. 确认工作区状态，记录旧上游、旧 Web 和本次目标 SHA，固定同步范围。
2. 获取原作者 Pebble 的上游提交，校验祖先关系，将上游跟踪分支 fast-forward 到目标。
3. 从 Web 创建同步分支；可在被忽略的 `.worktrees/` 下创建独立 worktree。
4. 将上游 merge 到同步分支，保留历史；冲突时优先保留上游业务逻辑，再适配 Web。
5. 对照变更的 Tauri 路径更新 `src-web/`，逐项核对补丁是否仍然必要。
6. 运行针对变化的回归及完整验证，检查旧数据升级和失败恢复。
7. 更新差异台账和必要升级说明，再审查并合入 `web`。

同步时至少检查：

```text
src-tauri/src/lib.rs
src-tauri/src/commands/**
src-tauri/src/realtime/**
src-tauri/src/events.rs
src/lib/api.ts
crates/pebble-mail/**
crates/pebble-store/**
crates/pebble-oauth/**
crates/pebble-rules/**
数据库 migration、Cargo feature 与锁文件
```

核对 command 的增删、参数默认值、返回值和错误，event 名及 payload，worker 生命周期，持久化/远端写回，OAuth refresh 和并发。没有文本冲突也需要语义审计；共享前端和 crates 自动更新，不代表 Web 命令实现自动获得新的桌面行为。

个人执行计划和本地笔记放在被忽略的 `.notes/`，不提交到仓库。新 worktree 不会自动获得主工作目录里被忽略的笔记，需从原路径读取。旧阶段编号、未勾选 TODO 和早期旧 Pebble-Web 文档不作为当前任务依据。

## 测试与构建

以下命令在仓库根目录执行。先记录基线，区分已有失败与新引入的失败。根据变更选择有业务意义的回归，再完成所需检查。

```bash
cargo check --locked -p pebble-web --all-targets
cargo test --locked -p pebble-web --all-targets
cargo build --locked -p pebble-web
pnpm test
pnpm exec tsc --noEmit
pnpm exec vite build --config src-web/vite.config.ts
```

完整上游同步还应检查共享核心和桌面宿主：

```bash
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets
pnpm build:frontend
pnpm build
```

桌面构建需要当前平台的 Tauri 系统依赖；其他平台打包由对应环境验证。根 `pnpm build:frontend` 使用桌面 Vite 配置，不能代替显式 Web 构建。

根 `tsconfig.json` 当前只 include `src/`，`tsc --noEmit` 不覆盖所有 Web TS 文件。修改 Web shim/runtime 时需确保受影响代码有有效的类型检查；Vite 转译成功不是完整 Web 类型检查证据。

Web Rust 测试位于模块内，已有 Web 前端补丁测试也位于 `src-web/patch/`。静态 command 名称检查只能发现接口名称缺口，重要变化还需验证 runtime、SQLite、远端调用次数和失败恢复；没有受控账户时不能把模拟测试称为真实收发验证。

不能执行的检查明确标记“未执行”，不要将代码阅读写成编译或测试通过。不要在行为修复中混入无关格式调整。

## 部署与升级维护

当前仓库已包含 Docker 多阶段构建、非 root 运行用户、Compose 数据卷、健康检查和 GHCR 发布配置，不再把这些列为尚未实现。

从源码构建本地镜像：

```bash
docker build -f src-web/Dockerfile -t pebble-web:local .
```

[Docker workflow](../.github/workflows/docker.yml) 在匹配版本 tag 或手动触发时构建并发布 amd64/arm64 镜像，检查请求版本与根 `package.json` 一致，生成版本标签和 `latest`。配置存在不等于某次构建、发布和部署已经验收。

[现有 CI](../.github/workflows/ci.yml) 面向 `master` 的 push/PR，执行 workspace Rust 检查、前端测试和桌面打包。它没有为 `web` 分支配置自动 push/PR 验证；Docker workflow 也不能代替完整测试。同步验收必须确认所需检查实际运行。

Web 应用显示版本由 `build.rs` 从根 `package.json` 读取；Web crate 的技术版本可以不同。升级时核对 workflow 的手动版本默认值与应用版本，避免过期示例。

部署验收检查健康接口、登录、静态资源、WebSocket、数据卷权限与重启恢复。远程反向代理应提供 HTTPS 并支持 WebSocket。应用本身不管理 Docker、宿主机服务、Nginx 或 Caddy。

数据库升级前停止服务，保存一致的完整数据卷快照、配置、旧镜像 digest 和对应加密密钥；使用外置 `PEBBLE_ENCRYPTION_KEY` 时另行保管它。设置备份不包含邮件正文和附件，不能代替数据卷快照。

先用隔离的旧数据副本演练升级，防止测试实例同步或重放真实邮件操作。回退使用旧镜像、升级前数据快照和对应密钥，不假设新 schema 可直接供旧程序使用；回退不能撤销已发生的远端邮件操作，升级后的新数据需另行保全。

## 后续移动端与 PWA 边界

P2 只保留移动端使用相关工作。共享前端虽有少量窄屏规则，App Shell、Sidebar、Inbox、Settings 和 StatusBar 仍以桌面布局为主；当前 `frontend/ui/web.css` 只隐藏桌面 TitleBar。

优先仅修改 Web CSS，使约 390–430px 宽度下的主要流程可操作。允许用 `!important` 和 `:has()` 覆盖上游 inline style；可尝试 48px 图标侧栏、详情打开时隐藏列表、窄屏设置布局、看板横向滚动、`100dvh` 和安全区 inset，Compose 继续利用已有响应式规则。

不为手机复制 React 页面或大改共享 `src/`。纯 CSS 无法使核心邮件流程可靠使用时，暂停 PWA 工作。

移动端布局可接受后，才考虑最小 PWA：Manifest、图标、standalone display、安装支持及静态资源 Service Worker。Service Worker 只缓存静态资源，不缓存登录响应、邮件 API、正文或其他敏感业务数据。Web Push 不在该范围。

## 新参与者阅读顺序

1. [README](README.md) 和本指南。
2. [COMPATIBILITY](COMPATIBILITY.md)。
3. `vite.config.ts`、`frontend/runtime/invoke.ts`、`frontend/runtime/events.ts`。
4. `src/command_router.rs`、`src/state.rs`、`src/sync_runtime.rs`。
5. `patch/` 和当前任务对应的 `src-tauri` 实现。

好的修改让上游目录保持原样、Web 差异足够薄、业务结果一致，并留下能解释行为的验证证据。若短期修复显著增加下一次上游合并的成本，应重新评估实现。
