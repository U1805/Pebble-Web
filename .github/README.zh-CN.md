<p align="center">
  <img src="../src/assets/app-icon.png" alt="Pebble logo" width="120">
</p>

<h1 align="center">Pebble Web</h1>

<p align="center">
  一个本地优先的网页邮件客户端，让收件箱更安静、更清晰，也更可控。
</p>

<p align="center">
  <a href="README.md">English</a>
  ·
  <a href="https://github.com/QingJ01/Pebble">Pebble 上游</a>
  ·
  <a href="https://github.com/QingJ01/Pebble-Web">官方 Pebble Web</a>
  ·
  <a href="../src-web/README.md">Web 文档</a>
  ·
  <a href="../LICENSE">AGPL-3.0</a>
</p>

## 项目定位

本仓库是 [QingJ01/Pebble](https://github.com/QingJ01/Pebble) 的长期 Web fork。

- **唯一代码上游**：[QingJ01/Pebble](https://github.com/QingJ01/Pebble)。核心业务、React 页面和桌面端行为以它为准。
- **官方网页版**：[QingJ01/Pebble-Web](https://github.com/QingJ01/Pebble-Web)。它由 Pebble 原作者 QingJ01 维护。
- **本仓库的方向**：直接在当前 Pebble 代码之上维护独立 Web 适配层，尽可能降低长期合并上游更新的成本。

本项目不是 Pebble 的重新实现，也不会复制并长期维护另一套核心 crate 或完整前端。

如果你需要 Pebble 桌面版的完整介绍、截图和使用说明，请阅读保留自上游的[根目录 README](../README.md)。

## 为什么维护这个 Fork

桌面版 Pebble 和 Web 版都在持续发展。若 Web 项目复制核心代码与完整前端，两边会逐渐产生业务差异，每次升级都需要重复迁移和修复。

本项目选择另一条维护路线：

1. 直接使用 Pebble 当前的 Rust 核心 crate 与当前的 React 前端。
2. 将 HTTP、WebSocket、浏览器 API 和 Web 鉴权集中在 `src-web/`， 语义对齐当前 Tauri 实现。
3. 上游发生变化时，优先接受上游业务逻辑，再更新 Web 适配。

## 当前能力

当前 Web 适配覆盖主要个人邮件工作流，包括：

- IMAP、POP3、Gmail OAuth 和 Outlook OAuth 账户。
- 邮件同步、文件夹、线程、搜索和消息状态管理。
- SMTP/OAuth 发送、回复、全部回复、转发和草稿。
- 浏览器附件上传与下载。
- 联系人、标签、可信发件人和规则。
- Snooze、Kanban 和翻译。
- 本地备份、WebDAV 和诊断日志。
- 浏览器通知与实时 WebSocket 事件。

浏览器中不存在的桌面概念不会伪装成业务成功，例如系统托盘、开机启动、原生窗口控制和设置系统默认邮件客户端。

更精确的桌面/Web 差异及实现见[兼容性台账](../src-web/COMPATIBILITY.md)。架构边界、上游同步和验证方法见[开发与维护指南](../src-web/DEVELOPMENT.md)。

## 部署

下载 Compose 配置：

```bash
curl -fsSLO https://raw.githubusercontent.com/U1805/Pebble-Web/web/src-web/docker-compose.yaml
```

启动前编辑 `docker-compose.yaml`，至少设置 `PEBBLE_PASSWORD` 和不少于 32 个字符的 `PEBBLE_JWT_SECRET`。OAuth 环境变量仅在使用 Gmail 或 Outlook 时配置。

启动服务：

```bash
docker-compose up -d
```

随后在浏览器访问 <http://localhost:8080>。

## 上游同步原则

仓库长期维护两个方向明确的分支：

- `upstream`：以 fast-forward 方式跟随 `QingJ01/Pebble`。
- `web`：包含全部 Web 适配。

同步时将 `upstream` 合并到 `web`。发生冲突时默认保留上游业务逻辑，然后重新适配 `src-web/`；不通过保留旧业务代码来规避冲突。

每次上游同步应重点核对：

- Tauri command 的名称、参数和返回值。
- Tauri event 的名称与 payload。
- 邮件、存储、OAuth、规则和数据库 migration 的变化。
- Web 服务、共享前端、桌面端和最终部署构建。

## 贡献

欢迎针对 Web 适配、上游兼容和回归测试提交 issue 或 pull request。修改时请遵守以下边界：

- 先检查当前 Pebble 上游实现，不根据旧代码猜测接口。
- 不复制核心 crate，不复制完整前端。
- Web 专用代码优先放入 `src-web/`。
- 命令参数、返回值、持久化副作用和事件语义与 Tauri 对齐。
- 保持提交主题单一，并为行为变化补充相应测试。

## 致谢与许可

感谢 [QingJ01](https://github.com/QingJ01) 创建并持续维护 Pebble 及官方 Pebble Web。

本项目沿用 Pebble 的 [GNU Affero General Public License v3.0](../LICENSE)。
