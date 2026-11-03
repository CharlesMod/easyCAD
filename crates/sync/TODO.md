# easycad-sync: work in progress (on hold)

Google sign-in and Drive sync were tabled before this crate was finished.
It is **not** in the root workspace `members`. Its `Cargo.toml` has its own
`[workspace]` table, so `cargo build` at the repo root never pulls in its
network dependencies (reqwest, keyring, notify). To work on it on its own:
`cd crates/sync && cargo clippy --all-targets`. When it is ready, remove the
`[workspace]` table, switch `version`/`edition`/`license` to `.workspace = true`,
and add `crates/sync` to the root `members`.

## Done

- `Cargo.toml`: the planned dependency set (reqwest 0.12 with rustls,
  keyring 3 with native macOS/Windows backends and the pure-Rust
  `async-secret-service` backend on Linux, so libdbus is not needed; notify 8,
  tokio, wiremock in dev). It resolves and builds.
- `src/error.rs`: `AuthError`, `RemoteError`, `SyncError`. Auth and network
  failures are values, never panics.
- `src/remote.rs`: the `RemoteStore` trait (root_id, list, get, download,
  multipart upload, begin_resumable/resume_upload, server-side copy, trash,
  start_page_token, changes_since), plus `RemoteFile`, `FileMeta` and `UploadRequest`.
- `src/events.rs`: `SyncEvent` (Status, Progress, Conflict, AuthRequired,
  Error, Completed), `SyncStatus`, `ConflictInfo`, `SyncReport`, and
  `EventSink` (an std mpsc channel with an optional GUI repaint waker).
- Compiles clean except for one dead-code warning (`SyncReport::merge`,
  which the engine will use).

## Design (agreed, not implemented)

**Auth.** OAuth2 authorization code + PKCE (S256) with a loopback redirect
`http://127.0.0.1:<random port>`; opens the system browser with the `open`
crate. Scope is `https://www.googleapis.com/auth/drive.file` only, which is
least privilege and avoids Google app verification. Use `access_type=offline` and
`prompt=consent` so Google returns a refresh token. The refresh token lives in the OS
keychain (`keyring`, called in `spawn_blocking`), never in plaintext. The access
token is cached in memory and refreshed 60 s before expiry with a single-flight
lock. Save the token again when Google rotates it. `invalid_grant`
deletes the token and fires an auth-required callback/event. A 5xx or
a network error keeps the token and reports offline. After a Drive 401, invalidate the access token
and retry once. `sign_out()` POSTs to the revoke endpoint, then deletes the
keychain entry even if the revoke fails.

**Client credentials.** Load the Google console's downloaded file
(`client_secret_<id>.apps.googleusercontent.com.json`, top-level key `installed`
with client_id, client_secret, auth_uri, token_uri). Lookup order: `--client-file`
path, env `EASYCAD_GOOGLE_CLIENT_FILE`, `<config_dir>/easycad/google_client.json`,
then env `EASYCAD_GOOGLE_CLIENT_ID` + `EASYCAD_GOOGLE_CLIENT_SECRET`. The login
example copies a `--client-file` into the config dir (0600) on success, so it only
needs to be passed once.

**Drive layout.** Create or find `easyCAD/` in My Drive. Each sync root
(Documents, Parts Library, Settings, all configurable) maps to a subfolder of it.
Real subfolders mirror the local tree so Drive is browsable, but files are
*identified* by `appProperties`: `cw_root`, the path split across
`cw_p0..cw_pN` (Drive caps each key+value at 124 bytes), `cw_dev`, `cw_doc`
(doc_id) and `cw_rev`. One paginated query per root:
`appProperties has { key='cw_root' and value='<root>' } and trashed=false`.
Track `md5Checksum`, `headRevisionId`, `version` and `modifiedTime`. Use multipart
upload up to 5 MB and resumable above it (8 MiB chunks, resume via `Content-Range: bytes */N`
and 308 responses; the session URI is persisted in the state DB). Retry 429,
5xx and rate-limit 403 with jittered exponential backoff. Delete means trash,
never a hard delete.

**Sync engine (local-first).** State DB is a JSON file written atomically. It holds,
per root and path, the last-synced remote id/md5/revision and the local
md5/size/mtime (the stat cache avoids rehashing big STEP files), plus pending
resumable sessions, the root folder id, and the changes `startPageToken`.
`sync_once()` builds a three-way plan (local now vs last-synced vs remote now),
then runs it in phases: conflict copies, blobs, other files, `doc.json`, deletes last.
- Equal md5 on both sides counts as synced, whatever the base says. This makes
  a lost or reset state DB and a lost upload response safe.
- Blobs (`*.ecad/blobs/<sha256>`) never conflict. If a blob already exists on the remote by md5,
  it is copied server-side instead of re-uploaded (this also makes renames cheap).
  Verify sha256 against the name on download. A blob deletion propagates
  only when the other side's `doc.json` is unchanged or absent; otherwise the blob
  is restored. Skip a `doc.json` upload if any of its blobs failed.
- A `doc.json` changed on both sides is never overwritten. The remote doc
  (doc.json + its blobs) is written to `<name> (conflict <device> <YYYY-MM-DD HHMMSS>).ecad`,
  then local is pushed to the original path, a `Conflict` event is emitted, and the copy syncs
  as a new doc. The copy keeps the same `doc_id`, and the GUI decides later. Other files
  get the same treatment, as `<stem> (conflict ...).<ext>`.
- Deletions propagate only when the other side is unchanged. A modified-vs-deleted
  pair restores the modified file. Rename is handled as delete plus add.
- Before updating or trashing a remote file, re-`get` it and skip the action if it
  changed. Before replacing a local file, re-stat it.
- Atomic local writes: download to `.cwsync-tmp-*` in the same dir, verify md5, rename.
  Ignore temp files (`.cwsync-*`, `*.tmp`, `*.step.tmp`, `*.part`, `~$*`, `.~lock.*`,
  `.DS_Store`, `Thumbs.db`) and user globs.
- Safety stops: a missing local root that has base entries; a side that is entirely empty
  while many deletes are planned; a changed root folder id resets the base instead of deleting.

**Background mode.** A tokio task with a `notify` watcher on each root (debounced 2 s),
polling `changes` from the saved `startPageToken` every 60 s, plus a periodic full pass.
On offline, emit `Status(Offline)` and back off (up to 5 min). On auth-required, pause until
`SyncHandle::resume()`. `SyncHandle` has sync methods (`sync_now`, `pause`, `resume`, `stop`)
so it suits a non-async GUI.

## Left to do

1. `src/auth/`: client-JSON loading, PKCE, loopback server, token refresh, `TokenStore`
   (keychain and in-memory), `sign_out`.
2. `src/drive.rs`: `DriveClient` implementing `RemoteStore` as above.
3. `src/memory.rs`: an in-memory `RemoteStore` fake (offline toggle, mid-upload interruption,
   transfer stats).
4. `src/state.rs`, `src/local.rs` (scan, ignore, hashing, atomic write), `src/plan.rs`,
   `src/engine.rs`, `src/config.rs` (sync roots, device name), `src/background.rs`.
5. Tests (no network): new local, new remote, both edited (conflict), deleted on one
   side, blob dedupe, rename, interrupted upload resume, offline then reconnect (two
   engines sharing one fake); PKCE (RFC 7636 vector); token refresh, invalid_grant
   and revoke against wiremock; a loopback login end to end against wiremock; Drive
   pagination, backoff and resumable against wiremock.
6. `examples/login.rs` (`--client-file`, `--sign-out`) and `examples/sync_dir.rs`.
7. `README.md` and `scripts/setup-google.sh`. The script runs `gcloud projects create <id>` and
   `gcloud services enable drive.googleapis.com --project <id>`, then prints console URLs
   for the steps that have no gcloud command for a personal Gmail account: Google
   Auth Platform, then Branding, then Audience (External, Testing, add yourself as a test user),
   then Clients (Desktop app), then Download JSON.
8. Integrate into the workspace (see top).
