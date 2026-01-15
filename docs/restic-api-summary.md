## Restic REST API v2 接口逻辑总结

本文总结 `src/restic/handler.rs` 中各接口的关键流程，重点描述如何映射到
Open115 的目录/文件操作与下载行为。当前实现采用**全量缓存（Warm Cache）**策略，读操作完全基于内存缓存。

### POST `/?create=true`
- 校验查询参数 `create=true`，否则返回 400。
- 调用 `Open115Client::init_repository()` 创建仓库根目录结构。
- 成功返回 200。

### DELETE `/`
- 未实现，固定返回 501。

### HEAD `/config`
- 只读：从内存缓存查找目录。
- 通过 `find_type_dir_id(Config)` 找到 config 目录 ID。
- 用 `find_file` 在缓存中查询 `config` 文件。
- 找到则返回 200 + `Content-Length`，否则 404。

### GET `/config`
- 只读：从内存缓存查找。
- 目录解析与 HEAD 相同；获取到文件后调用 `download_file(pick_code, None)`。
- 返回 200 + `Content-Type: application/octet-stream` + `Content-Length`。

### POST `/config`
- 读取请求体（上限 1GiB）。
- 通过 `get_type_dir_id(Config)` 确保目录存在（必要时创建并更新缓存）。
- 调用 `upload_file(...)` 上传为 `config`。
- 上传成功后，`upload_file` 会自动更新内存 `files_cache`，确保后续读操作满足 Read-after-Write。
- 返回 200。

### GET `/:type/`
- 校验 `type`（data、keys、locks、snapshots、index），拒绝 `config`。
- `data` 类型：调用 `list_all_data_files()` 遍历缓存中的所有 hash 子目录。
- 其他类型：用 `find_type_dir_id` 找目录，不存在则返回空列表。
- 仅通过内存 `files_cache` 返回文件 `{name, size}`，并使用 v2 content type。

### HEAD `/:type/:name`
- 只读：从内存缓存查找。
- `data`：用 `find_data_file_dir_id(name)` 定位 hash 目录。
- 其他：用 `find_type_dir_id(type)` 获取目录。
- 用 `find_file` 在缓存中查找文件。
- 找到则返回 200 + `Content-Length`，否则 404。

### GET `/:type/:name`
- 只读：从内存缓存查找。
- 目录/文件解析逻辑同 HEAD。
- 支持单段 Range：
  - 解析 `Range: bytes=...`（支持 suffix 与 open-ended 形式）。
  - 非法范围 -> 400；不可满足 -> 416 + `Content-Range: bytes */size`。
  - 可满足 -> `download_file(pick_code, Some((start, end)))`，
    返回 206 + `Content-Range` + `Accept-Ranges` + `Content-Length`。
- 无 Range：`download_file(pick_code, None)`，返回 200 + `Accept-Ranges`。

### POST `/:type/:name`
- 读取请求体（上限 1GiB）。
- `data`：`get_data_file_dir_id(name)` 解析/创建 hash 子目录（维护缓存）。
- 其他：`get_type_dir_id(type)` 解析/创建类型目录（维护缓存）。
- 调用 `upload_file(...)` 上传。
- `upload_file` 在秒传或 OSS 上传成功后立即更新内存 `files_cache`。
- 返回 200。

### DELETE `/:type/:name`
- 只读查找：从内存缓存定位文件。
- `data`：`find_data_file_dir_id(name)`，不存在则直接返回 200。
- 其他：`find_type_dir_id(type)`，不存在则直接返回 200。
- 存在时：用 `find_file` 获取文件并删除。
- `delete_file(...)` 执行删除后，立即从内存 `files_cache` 中移除该项。
- 无论文件是否存在，最终都返回 200（符合 Restic 幂等删除预期）。

### 函数逻辑与 API 映射汇总

本节汇总 `Open115Client` 中各核心函数的逻辑实现及其对应的 115 开放平台 API。

| 函数名 | 核心逻辑摘要 | 调用 115 API / 说明 |
| :--- | :--- | :--- |
| **`init_repository`** | 初始化仓库：递归创建 Restic 必需的目录结构（config, data, keys 等）。 | 调用 `ensure_path`。 |
| **`ensure_path`** | 路径确保：递归检查并创建路径中缺失的目录。 | 调用 `create_directory`。 |
| **`create_directory`** | 创建目录：在指定父 ID 下创建文件夹。若已存在，则通过 `find_file` 返回已有 ID。 | `POST /open/folder/add` |
| **`find_path_id`** | 路径解析：将字符串路径转换为 115 目录 ID。 | **优先查缓存**，不命中时逐级调用 `find_file`。 |
| **`get_type_dir_id`** | 类型路径获取（带创建）：获取指定 Restic 类型（如 index）的目录 ID。 | 调用 `ensure_path`。 |
| **`find_type_dir_id`** | 类型路径查找：在缓存中定位指定类型的目录 ID。 | 调用 `find_path_id`。 |
| **`get_data_file_dir_id`** | Data 子路径获取（带创建）：根据文件 hash 前两位获取/创建分级子目录。 | 调用 `ensure_path`。 |
| **`find_data_file_dir_id`** | Data 子路径查找：定位 Data 分级子目录 ID。 | 调用 `find_path_id`。 |
| **`find_file`** | 精确查找：在 `files_cache` 中查找指定父目录下同名的文件/目录。 | **纯内存操作**（依赖 `warm_cache` 或写入时的实时更新）。 |
| **`download_file`** | 文件下载：获取 115 内部下载地址并执行下载。支持 Range 分片下载。 | 1. `POST /open/ufile/downurl` <br> 2. `GET {download_url}` |
| **`upload_file`** | 文件上传：1. 调用 init 尝试秒传；2. 处理 Sign Check（若需要）；3. 失败则直传 OSS。 | 1. `POST /open/upload/init` <br> 2. `POST /open/upload/get_token` <br> 3. 阿里云 OSS `PutObject` |
| **`delete_file`** | 幂等删除：请求 115 删除文件，并同步更新内存缓存。 | `POST /open/ufile/delete` |
| **`list_all_data_files`** | Data 遍历：从内存缓存汇总 `data/00` 到 `data/ff` 下的所有文件。 | **纯内存操作**。 |
| **`warm_cache`** | 缓存预热：启动时递归拉取整个 Repo 目录树并填充内存缓存。 | 驱动 `fetch_files_from_api`。 |
| **`fetch_files_from_api`** | 分页拉取：从 115 获取指定目录下的全量文件列表。 | `GET /open/ufile/files` (分页 Loop) |
