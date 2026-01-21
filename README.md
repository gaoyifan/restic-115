# restic-115

Restic REST backend server backed by 115 Open Platform cloud storage. It implements the restic REST v2 API over HTTP and maps repository objects to 115 directories/files, with a local SQLite cache to keep listings and IDs fast across restarts.

## What it does

- Provides the restic REST v2 endpoints over HTTP (Axum).
- Stores repository data in 115 Open Platform storage under a configurable repo path.
- Caches directory and file metadata in SQLite and reuses it across runs.
- Auto-refreshes access tokens using the refresh token.
- Supports HTTP Range requests for efficient partial downloads.

## Quick start

1. Obtain 115 Open Platform access and refresh tokens.
2. Export environment variables and run the server:

```bash
export OPEN115_ACCESS_TOKEN=... 
export OPEN115_REFRESH_TOKEN=...
export OPEN115_REPO_PATH=/restic-backup
export LISTEN_ADDR=127.0.0.1:8000

cargo run --release
```

3. Initialize a restic repository against this backend:

```bash
export RESTIC_PASSWORD=your-password
restic -r rest:http://127.0.0.1:8000/ init
```

## Configuration

All options are available as CLI flags and environment variables.

- `OPEN115_ACCESS_TOKEN` (`--access-token`): Bearer token for `proapi.115.com`.
- `OPEN115_REFRESH_TOKEN` (`--refresh-token`): Refresh token for `passportapi.115.com`.
- `OPEN115_REPO_PATH` (`--repo-path`): Repository root path on 115. Default: `/restic-backup`.
- `LISTEN_ADDR` (`--listen-addr`): Server listen address. Default: `127.0.0.1:8000`.
- `RUST_LOG` (`--log-level`): Log level. Default: `info`.
- `OPEN115_API_BASE` (`--api-base`): 115 Open Platform API base URL. Default: `https://proapi.115.com`.
- `OPEN115_USER_AGENT` (`--user-agent`): User agent for 115 API calls. Default: `restic-115`.
- `OPEN115_CALLBACK_SERVER` (`--callback-server`): Callback server hint (documentation only).
- `OPEN115_FORCE_CACHE_REBUILD` (`--force-cache-rebuild`): Force cache warm-up on startup.
- `DB_PATH` (`--db-path`): SQLite DB path. Default: `cache-115.db`.

## Cache behavior

On startup the server checks the SQLite cache. If it is empty (or `OPEN115_FORCE_CACHE_REBUILD=true`), it warms the cache by listing the repository root, the standard restic directories, and all `data/xx` subdirectories. The cache is updated on uploads and deletes to keep restic requests fast and avoid extra API listing calls.

Additionally, download URLs from the 115 API are cached in-memory for 5 minutes to reduce API calls during file downloads. This improves performance when restic reads the same file multiple times or performs operations that require repeated access to the same objects.

## Docker

Build and run with Docker Compose:

```bash
export OPEN115_ACCESS_TOKEN=...
export OPEN115_REFRESH_TOKEN=...
export OPEN115_REPO_PATH=/restic-backup

docker compose up --build
```

The container stores the cache DB at `./cache/cache-115.db` via the volume in `docker-compose.yml`.

## API behavior notes

- `POST /?create=true` initializes the repository directories.
- `DELETE /` returns `501 Not Implemented` (repository deletion is not implemented).
- `GET/HEAD/POST /config` operates on the restic config object.
- `GET/HEAD/POST/DELETE /:type/:name` handles restic objects by type (`data`, `index`, `snapshots`, `keys`, `locks`).

## Tests

Tests that touch the real 115 API and restic CLI require credentials:

```bash
export OPEN115_ACCESS_TOKEN=...
export OPEN115_REFRESH_TOKEN=...

cargo test --tests
```

End-to-end tests also require `restic` in `PATH`.

## License

MIT
