# Repository Guidelines

## Project Structure & Module Organization
- `src/main.rs` boots the Axum server, config, logging, and cache warm-up.
- `src/restic/` implements the REST v2 API handlers and routing.
- `src/open115/` contains the Open115 client, auth/token refresh, API types, and SQLite cache access.
- `tests/` holds integration and end-to-end tests (`integration_test.rs`, `e2e_test.rs`).
- `docs/` includes API references and debugging notes; update when behavior or error mapping changes.
- Docker uses `docker-compose.yml` with cache data mounted under `cache/`.

## Build, Test, and Development Commands
- `cargo build` compiles the server in debug mode.
- `cargo run --release` runs the server locally (configure via `.env` or env vars).
- `just build` / `just build-release` mirror Cargo build commands.
- `just test` runs all tests with single-threaded execution.
- `just test-integration` / `just test-e2e` run targeted suites.
- `just test-e2e-100mb` runs the large-file case (requires `restic` CLI).
- `docker compose up --build` builds and runs the containerized server.

## Coding Style & Naming Conventions
- Keep HTTP handlers in `src/restic/` and Open115 API/cache logic in `src/open115/`.
- Prefer explicit `AppError` variants and structured logging via `tracing`.

## Testing Guidelines
- Tests use Rust’s built-in test harness and live in `tests/`.
- Integration/E2E tests require `OPEN115_ACCESS_TOKEN` and `OPEN115_REFRESH_TOKEN`.
- E2E tests also require `restic` in `PATH`; override timeout with `E2E_TIMEOUT_SECS`.
- Run all tests with `cargo test --tests`, or a suite with `cargo test --test e2e_test`.

## Commit & Pull Request Guidelines
- Follow Conventional Commits as used in history: `feat:`, `fix:`, `refactor:`, `chore:`.
- If cache semantics or API responses change, update relevant `docs/` notes.

## Configuration & Security
- Reference `.env.example` for required variables.
- Key env vars include `OPEN115_ACCESS_TOKEN`, `OPEN115_REFRESH_TOKEN`, `OPEN115_REPO_PATH`, `DB_PATH`, and `OPEN115_FORCE_CACHE_REBUILD`.
- Keep secrets out of git; `Justfile` loads `.env` automatically.
