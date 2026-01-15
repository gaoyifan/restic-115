# 115 开放平台秒传行为补充说明

本文档记录了关于 115 开放平台文件秒传（Fast Upload）路径中 `pick_code` 返回行为的验证结果，用于补充官方文档（`/docs/115-api/`）中未提及的细节。

## 验证结论

根据 2026-01-15 的实际接口验证：

1. **秒传路径返回 `pick_code`**：当调用 `/open/upload/init` 接口且返回 `status: 2`（表示秒传成功）时，响应 JSON 数据中包含 `pick_code` 字段。
2. **下载有效性**：该秒传返回的 `pick_code` 与普通上传返回的 `pick_code` 具有同等效力。
3. **闭环测试**：使用秒传获取的 `pick_code` 调用 `/open/ufile/downurl` 接口，可以正常获取下载地址并完整下载文件，下载内容与原文件完全一致。

## 典型响应示例（秒传路径）

```json
{
    "state": true,
    "code": 0,
    "message": "",
    "data": {
        "status": 2,
        "file_id": "3342177983093201749",
        "pick_code": "cq8zap7l323bbddre",
        "bucket": "",
        "object": "",
        "target": "",
        "callback": []
    }
}
```

## 开发建议

在实现文件存储逻辑时，如果需要在上传后立即记录下载凭据（如用于 Restic 的缓存读取或下发下载任务），可以直接从 `/open/upload/init` 的 `status: 2` 响应中提取 `pick_code`，无需额外调用文件列表或搜索接口来获取。
