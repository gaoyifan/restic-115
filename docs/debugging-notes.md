# restic-115 Debug 记录与经验总结（持续更新）

本文档总结 `restic-115` 在实现/调试 Restic REST backend（以 115 Open Platform 作为底层存储）过程中遇到的问题、定位方法、已修复点以及尚未解决的阻塞点，方便后续接手者快速进入上下文。

> 适用范围：仓库当前实现（axum server + Open115Client）以及 `tests/e2e_test.rs` 的 `just test-e2e-100mb` 场景。

---

## 背景：E2E 失败长什么样

`just test-e2e-100mb` 主要流程：

- 启动本地 `restic-115` server（REST backend）
- `restic -r rest:http://127.0.0.1:<port>/ init`
- `restic backup` / `restore` / hash 校验

调试过程中遇到过的失败形态（按出现顺序）：

- `restic init` 报：`unexpected HTTP response (502): 502 Bad Gateway`
- 解决 502 后，遇到 `401 Unauthorized`（token 失效/刷新频控）
- 再之后遇到 `400 Bad Request`（我们 server 自己返回的 400）
- 修复 400 后，又遇到 `500 Internal Server Error`（我们 server 自己返回的 500）

关键经验：**先拿到 server 的真实错误体与日志**，再判断是 115 返回、还是我们自己的解析/逻辑问题。

---

## 经验 1：不要只看 HTTP 状态码，要看 115 的 “JSON code”

115 文件接口经常：

- HTTP 200
- JSON body 里 `code != 0` 表示错误（例如 token 无效、额度上限）

典型例子：

- `code=40140125/40140126/...`：access_token 无效/校验失败（需要 refresh）
- `code=406`：`已达到当前访问上限...`（额度/频控/配额类错误）

### 已修复：token 失效自动 refresh + retry

在 `src/open115/client.rs` 的 `get_json()` / `post_form_json()` 中：

- 先用当前 token 请求
- 如遇 HTTP 401 或 JSON code 属于 token 无效集合，则 refresh 后重试一次

并新增了 **token 写回本地**（可选）：

- `OPEN115_PERSIST_TOKENS=true`
- `OPEN115_TOKEN_STORE_PATH=.env`（默认 `.env`）

目的：减少每次启动都 refresh 的概率（但注意：115 refresh 本身也可能被频控）。

---

## 经验 2：502 不一定是网关问题，可能是我们把 115 错误统一映射成 502

早期 `AppError::Open115Api` 统一映射为 502，导致：

- 115 的额度上限（code=406）也变成 502
- restic 侧只看到 “Bad Gateway”，很难判断可否重试/退避

### 已修复：把 115 code=406 映射为 429

在 `src/error.rs`：

- `Open115Api{code=406}` → HTTP `429 Too Many Requests`
- 其他仍保留 502（上游失败）

意义：**语义更正确**，并且允许在 server 侧做退避重试。

---

## 经验 3：重试/指数退避必须在 server 侧做，不能在测试做

原因：

- 测试层重试会放大 API 调用次数，反而更容易触发配额上限
- 正确职责：server 应提供可恢复的语义（例如 429 可退避、上传最终一致性可等待）

### 已实现：对“频率限制/额度”类错误的指数回退重试（server 侧）

在 `src/open115/client.rs` 的 `get_json()` / `post_form_json()`：

- 如果解析到 JSON `code=406`（访问上限）或 `code=40140117`（刷新太频繁），则在有限次数内做指数回退重试（sleep 1s/2s/4s… 上限 60s）
- 如果遇到 HTTP `429 Too Many Requests`，同样进行指数回退重试

注意：这只能处理“短时的限流/额度恢复”，无法保证在长期额度不足时一定成功。

#### 2026-01 Update：退避参数与测试超时对齐

由于 `tests/e2e_test.rs` 对每个 `restic` 命令设置了 **5 分钟硬超时**（避免“卡住没反馈”），server 侧单次请求也需要避免“分钟级 sleep”：

- 当前实现把指数退避的 sleep 上限调小（避免单次请求等待分钟级）
- 重试次数也做了上限（避免在长期额度不足时无意义等待）

---

## 经验 4：115 API 返回结构并不稳定，要写“形态兼容”的解析

### 4.1 `refreshToken` 的 state 类型不稳定

`/open/refreshToken` 可能返回：

- `state: 0/1`（数字）
- 失败时 `data: {}`（空对象）

因此：

- `state` 需要兼容 bool/number/string
- `RefreshTokenData` 字段需要允许缺失（Option），再在成功路径上校验必需字段

### 4.2 `upload/get_token` 的 data 形态不稳定（导致过 400）

在 `POST /keys/<id>` 上传 key 过程中，会调用 115：

- `/open/upload/get_token`

调试中发现其返回的 `data` 可能是 **map/object**，而我们最初用 `Vec<UploadToken>` 解析导致：

- serde 失败：`invalid type: map, expected a sequence`
- 我们 server 直接返回 HTTP 400

#### 已修复

- `UploadTokenResponse.data` 改为 `serde_json::Value`
- 在 `get_upload_token()` 内部根据 `data` 的实际形态（array/object/nested）提取并反序列化为 `UploadToken`

---

## 仍然存在的问题（当前阻塞点）

### 问题 A：OSS 上传成功后，文件“可见化”有延迟（最终一致性）

目前 `upload_file()` 在 OSS `PUT` 成功后会 **直接返回成功**，不再尝试通过“列目录/查 file_id”来等待可见化（避免在上传路径触发大量目录列举，尤其是 data 的 hash 分桶目录）。

并且：如果该目录之前有缓存（例如 data 的 `{00..ff}` 子目录），会把对应目录缓存标记为 **dirty**。只有当 REST API 收到“列目录请求”（`GET /:type/`）时，才会重新列目录刷新缓存。

#### 2026-01 Update：用 OSS callback 结果提供“读后写一致性”（无需列目录）

OpenList 的实现表明：OSS PutObject/CompleteMultipartUpload 的 callback 返回体可能包含 `file_id`/`pick_code`/`cid` 等信息。

当前实现会：

- 尝试解析 OSS callback JSON（见 `src/open115/types.rs` 的 `OssCallbackResult`）
- 在进程内缓存 `cid + filename -> (file_id, pick_code, ...)` 的 **file hint**

这样 `GET/HEAD /data/<id>` 可以直接拿到 `pick_code` 去 downurl+download，从而尽量避开：

- 115 search 索引延迟导致的 “<data/...> does not exist”
- 为了“等可见”而被迫列目录（尤其是 data hash 子目录）

### 问题 B：根目录巨大导致 list_files 分页非常贵（配额上限）

我们已引入 `/open/ufile/search` 作为“按名查找”的 fast path，并尽量避免在 root 下做全量分页 list。

但在账号配额非常紧张时，即使调用量减少也可能触发 `code=406`。

#### 建议

- 默认尽量避免 root 下的大范围操作
- 尽量用 `search` 定位；必要时缓存目录 ID，并持久化（避免每次启动重复查找）
- 若 115 的限流策略允许，考虑在 server 内部为所有 115 调用统一加“预算/速率控制”（全局 semaphore + 最小间隔）

---

## 推荐的调试方法（可复用）

- **先单独跑 `restic init`**，避免 100MB 生成耗时干扰：

```bash
RESTIC_PASSWORD=debug restic -r rest:http://127.0.0.1:<port>/ init -v -v
```

- **开启 server 端 debug 日志**（尤其是 axum trace + Open115Client 内部错误）：
  - `RUST_LOG=debug`
  - 观察 `POST /?create=true`、`POST /keys/<id>` 等关键请求对应的日志

- **E2E 现在会打印 server 日志 + 5 分钟超时**：
  - `tests/e2e_test.rs` 会将 server stdout/stderr 打印到测试输出（不再是“Initializing repository...” 很久无反馈）
  - 每个 `restic` 子命令有 5 分钟硬超时（超时会 kill 并打印 stdout/stderr）

- **看到 400/500 时优先怀疑我们自己**：
  - 400 通常是 handler 参数/类型不匹配、JSON 反序列化失败、type 不识别
  - 500 通常是 OSS put 成功后的最终一致性、或我们把可恢复错误当 fatal

---

## 文件索引（定位入口）

- **REST backend 路由与行为**：`src/restic/handler.rs`
- **115 client（含 token refresh、search、upload/oss）**：`src/open115/client.rs`
- **115 response types（兼容 state、get_token/search 等）**：`src/open115/types.rs`
- **错误映射（Open115Api → 429/502 等）**：`src/error.rs`
- **E2E 测试**：`tests/e2e_test.rs`

---

## 下一步工作清单（建议）

- （已调整策略）上传成功后直接返回，并把相关目录缓存标记为 dirty；只在 list 端点触发重新列目录
- 给 115 API 调用加全局节流（可选）：并发上限 + 最小间隔 + 对 406 的退避
- 把关键的 115 JSON 响应样本（脱敏）记录在 `docs/`，便于未来更新解析逻辑

---

## 2026-01 行为变更摘要（便于快速对齐）

- **移除 warm cache / 预热**：启动不再触发列目录
- **读请求不创建目录**：`HEAD/GET/LIST/DELETE` 不再调用 `ensure_path()`（避免在配额紧张时无意义 mkdir）
- **上传不列目录**：OSS 上传成功后直接返回；data 的 hash 子目录不会在上传路径被列出
- **read-after-write**：优先用 OSS callback 回传的信息提供文件定位提示；必要时对非 data 目录允许小范围 list fallback
