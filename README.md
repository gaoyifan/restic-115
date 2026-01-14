# restic-115

基于 Rust/Axum 实现的 **Restic REST Backend Server**，底层存储对接 **115 网盘开放平台（Open Platform）**。该项目让 restic 可以用 `rest:` 仓库协议把数据存到 115 网盘。

## 架构概览

```
restic CLI  <-- Restic REST API(v2) -->  restic-115  <-- 115 Open Platform -->  115 云盘
                                                  \--(upload)--> Aliyun OSS(STS) + callback
```

- **Restic 端协议**：实现 `reference/restic/doc/REST_backend.rst` 描述的 REST 后端（本项目按 **v2** 结构返回列表）。
- **115 侧能力**：
  - 文件列表/创建文件夹/删除/获取下载地址：`https://proapi.115.com/*`
  - 刷新 token：`https://passportapi.115.com/open/refreshToken`
  - 上传：`/open/upload/init` + `/open/upload/get_token` + **阿里云 OSS PutObject（带 callback/callback_var）**

## 鉴权说明（无 AppID/AppSecret）

你不需要自己申请 AppSecret；可以参考 OpenList 的做法，借助 OpenList 提供的 callback server 完成鉴权并获得：

- `access_token`
- `refresh_token`

建议使用 OpenList 在线 Token 工具，选择 **“115 网盘 (OAuth2) 跳转登录”**，并勾选“使用 OpenList 提供的参数”，回调地址会自动使用：

- `https://api.oplist.org/115cloud/callback`

拿到 token 后，把它们作为环境变量传给本项目（见下文）。

## Restic REST API 支持范围

实现的端点（与 restic 交互所需的核心集合）：

- `POST /?create=true`：初始化仓库目录结构
- `HEAD/GET/POST /config`：config 文件
- `GET /:type/`：列出某类文件（v2 返回：`[{name,size}]`）
- `HEAD/GET/POST/DELETE /:type/:name`：单文件的存在性/下载（含 Range）/上传/删除

`type` 支持：

- `data`
- `keys`
- `locks`
- `snapshots`
- `index`

## 目录映射规则（115 内部存储布局）

仓库根目录由 `OPEN115_REPO_PATH` 控制，默认 `/restic-backup`。

- `config`：存放在仓库根目录（文件名固定为 `config`）
- 其余类型：存放在 `/{repo_path}/{type}/`
- `data`：采用 2 字符前缀分桶，降低单目录文件数：
  - `/{repo_path}/data/{name前2字符}/{name}`

## 运行

### 环境变量

| 变量 | 说明 | 默认值 |
|---|---|---|
| `OPEN115_ACCESS_TOKEN` | 115 access_token | 必填 |
| `OPEN115_REFRESH_TOKEN` | 115 refresh_token | 必填 |
| `OPEN115_REPO_PATH` | 115 上的仓库根路径 | `/restic-backup` |
| `LISTEN_ADDR` | 监听地址 | `127.0.0.1:8000` |
| `RUST_LOG` | 日志等级 | `info` |
| `OPEN115_API_BASE` | 115 Open API base | `https://proapi.115.com` |
| `OPEN115_USER_AGENT` | 请求 UA | `restic-115` |
| `OPEN115_CALLBACK_SERVER` | 获取 token 的回调提示 | `https://api.oplist.org/115cloud/callback` |

### 启动服务

```bash
cargo run --release
```

### 使用 Restic

```bash
export RESTIC_PASSWORD="your-secure-password"
restic -r rest:http://127.0.0.1:8000/ init
restic -r rest:http://127.0.0.1:8000/ backup /path/to/files
restic -r rest:http://127.0.0.1:8000/ snapshots
restic -r rest:http://127.0.0.1:8000/ restore latest --target /path/to/restore
```

## 测试

本项目参考 `reference/restic-123pan` 的测试结构，包含：

- **Integration tests**：直接调用 115 API，覆盖 refresh / mkdir / list / upload / downurl+download / delete
  - `tests/integration_test.rs`
- **E2E tests**：启动本服务后调用本机 `restic` CLI 做 init/backup/restore
  - `tests/e2e_test.rs`
  - 额外提供 `test_e2e_100mb`：生成 ~100MB 随机不可压缩数据，验证备份/恢复与 hash 一致

### 使用 Just

根目录提供 `Justfile`：

- `just test`
- `just test-integration`
- `just test-e2e`
- `just test-e2e-100mb`（只跑 100MB 大规模 E2E；需要本机安装 `restic`）

## 已知限制/注意事项

- 115 上传使用 OSS STS + callback 的流程；如果 115 API 返回结构发生变动，可能需要调整 `src/open115/client.rs` 中对 `upload/init` 与 callback 字段的提取逻辑。
- delete 在 Restic 侧按“幂等删除”处理：即使云端已不存在也返回 HTTP 200（符合 restic 常见行为）。
- **上传路径不会列目录**：OSS 上传成功后直接返回成功，并把对应目录缓存标记为 dirty；只有当 REST API 收到“列目录请求”（`GET /:type/`）时，才会重新列目录刷新缓存。
- **Token 刷新与持久化**：默认只在内存中刷新 token；如果你希望“刷新后写回本地”，可设置：
  - `OPEN115_PERSIST_TOKENS=true`：启用将刷新后的 `OPEN115_ACCESS_TOKEN`/`OPEN115_REFRESH_TOKEN` 写回文件
  - `OPEN115_TOKEN_STORE_PATH=.env`：写入目标文件路径（默认 `.env`）
  - 注意：写回会覆盖文件中对应 key 的值；请确保该文件不被提交到 git（通常应在 `.gitignore` 中）。

## 参考内容（文档/实现）

本项目的设计与实现主要参考了以下内容（均已随仓库 vendoring 在 `reference/` 或 `docs/` 下，便于离线查阅）：

- **Restic REST Backend 规范**：定义了 REST 后端的接口形态与行为（尤其是 `GET /{type}/` 在 v1/v2 的返回差异、Range 下载等）。
  - `reference/restic/doc/REST_backend.rst`
- **115 开放平台文档（文件/鉴权/上传）**：用于确认 115 的基础域名、刷新 token、文件列表/下载地址/删除/新建文件夹、以及 “init + STS + OSS callback” 上传流程。
  - `docs/115/`
- **OpenList 的 115 Open 驱动实现**：用于理解只依赖 `access_token + refresh_token` 的使用方式、下载链接获取（pick_code/downurl）、以及上传走 OSS callback 的总体流程。
  - `reference/OpenList/drivers/115_open/`
- **restic-123pan 参考项目**：本项目的 Rust 工程结构、axum handler 组织方式、以及 integration/e2e 测试框架主要借鉴该项目，并将云盘 client 从 123pan 适配为 115。
  - `reference/restic-123pan/`