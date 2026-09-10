# Pebble Web 模块文档

项目定位、功能列表、Docker 部署和许可证见[仓库介绍](../.github/README.zh-CN.md)。本文提供 `src-web/` 的源码运行方式、配置和目录导航。

## 文档

- [开发与维护指南](DEVELOPMENT.md)：架构边界、上游同步、编码、验证和部署维护。
- [兼容性与差异台账](COMPATIBILITY.md)：Tauri/Web 有意差异、兼容补丁和已确认缺口。

## 从源码运行

以下命令都在仓库根目录执行。需要 Rust stable、Node.js 和 pnpm，以及当前平台所需的 Rust 原生构建依赖；Linux 容器构建依赖可参考 [Dockerfile](Dockerfile)，完整桌面构建另需 Tauri 系统依赖。

```bash
pnpm install --frozen-lockfile
pnpm exec tsc --noEmit -p src-web/tsconfig.json
pnpm exec vite build --config src-web/vite.config.ts
cargo build --locked -p pebble-web
```

按[仓库部署说明](../.github/README.zh-CN.md#部署)和[环境变量示例](.env.example)设置进程环境。本地开发可设置 `PEBBLE_DATA_DIR=./.notes/web-data`、`PEBBLE_STATIC_DIR=./dist`，其中 `.notes/` 已被 Git 和 Docker 构建上下文忽略。然后运行：

```bash
cargo run --locked -p pebble-web
```

Web 服务从进程环境读取配置，不会自动加载 `.env` 文件。上面运行的是构建后的静态前端；修改前端后重新执行 Web 构建并刷新页面。当前根目录 `pnpm dev` 启动的是 Tauri，默认 Vite 开发配置也没有 Web API 代理；不要把它们当作 Web 开发服务。

## 服务配置

登录必填配置见仓库部署说明，其余服务变量如下：

| 变量 | 用途与默认值 |
| --- | --- |
| `PEBBLE_PORT` | 服务端口，默认 `8080` |
| `PEBBLE_DATA_DIR` | 数据目录，服务默认 `/data`；本地示例使用相对路径 |
| `PEBBLE_STATIC_DIR` | 静态前端目录，默认 `./dist`；容器使用 `/app/dist` |
| `PEBBLE_SYNC_INTERVAL` | 同步管理器的基础间隔，默认 `300` 秒；实际后台行为还受 realtime preference 控制 |
| `PEBBLE_ENCRYPTION_KEY` | 可选，64 个十六进制字符；未设置时读取或创建数据目录中的 `encryption.key` |
| `PEBBLE_OAUTH_REDIRECT_URL` | 使用 OAuth 时配置为浏览器可访问的 `/api/v1/oauth/callback` 完整 URL |

Gmail/Outlook 的 OAuth 客户端配置见 [.env.example](.env.example)。Gmail Web 部署使用 **Web application** 类型的 OAuth 客户端，回调 URI 必须与部署配置完全匹配；不要直接套用根 README 的桌面 OAuth 配置。

## 目录

```text
src-web/
├── README.md          # 模块介绍、运行方式和文档导航
├── DEVELOPMENT.md     # 开发与维护指南
├── COMPATIBILITY.md   # 差异、补丁和已确认缺口
├── src/               # Axum 服务、命令适配和后台任务
├── frontend/          # Web 入口、Tauri shim 和浏览器 runtime
├── patch/             # 经确认的上游问题兼容补丁
├── tsconfig.json      # 包含 Web runtime、shim 和补丁的类型检查
├── vite.config.ts     # Web 入口与模块 alias
├── Dockerfile
└── docker-compose.yaml
```
