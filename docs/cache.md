# Caching Strategy

The `restic-115` server maintains a local SQLite cache of the 115 repository structure to minimize API calls and improve performance. Restic operations often involve listing directories multiple times; without caching, this would result in excessive API requests and slow performance due to network latency.

## Architecture

- **Storage**: SQLite database (`cache-115.db` by default).
- **ORM**: `sea-orm`.
- **Schema**:
  - `file_nodes`: Stores directory and file metadata.
    - `file_id` (PK): 115 File ID.
    - `parent_id`: ID of the parent directory.
    - `name`: File name.
    - `is_dir`: Boolean.
    - `size`: File size in bytes.
    - `pick_code`: 115 pick code (used for downloads).

## Warmup Behavior

On server startup, the `warm_cache()` method ensures the local cache is populated.

### Resumable Warmup (Default)
The warmup process is designed to be **resumable**. It traverses the repository structure (Root -> keys/locks/snapshots/index -> data/00..ff). For each directory:
1.  **Check Cache**: It queries the database to see if **any** children exist for this directory ID (`cache_has_children`).
2.  **Hit**: If children exist, it assumes the directory is valid and skips the API call (reusing cached data).
3.  **Miss**: If no children exist, it fetches the file list from the 115 API, wipes any stale entries for that directory, and saves the new list.

This means if the server is interrupted during warmup, the next run will skip the already-fetched directories and continue from where it left off.

### Forced Rebuild
If `OPEN115_FORCE_CACHE_REBUILD=true` is set:
- The "Check Cache" step is skipped.
- Every directory is fetched fresh from the 115 API.
- Useful for fixing consistency issues if the cache gets out of sync with the cloud (e.g., external modification).

## Runtime Consistency

The cache is kept in sync during write operations:
- **Upload**: When a file is uploaded (`upload_file`), the new file metadata is inserted into the DB immediately after success.
- **Delete**: When a file is deleted (`delete_file`), the entry is removed from the DB immediately.
- **Directory Creation**: `create_directory` inserts the new directory record upon success.

## Query Logic

- **Listing (`list_files`)**: Always serves from the database. It does **not** fall back to the API if the DB is empty (assumes warmup handled it).
- **Finding Paths (`find_path_id`)**: Traverses the directory tree using cached directory listings.
