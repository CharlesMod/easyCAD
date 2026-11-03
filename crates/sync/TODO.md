# easycad-sync: status and design

A workspace member since 2026-11-03; the app uses it (`crates/app/src/cloud.rs`,
see `docs/GUI.md` ▸ Google Drive sync). Setup for a new Google project:
`README.md` here and `scripts/setup-google.sh`.

## Modules

| module | what |
|---|---|
| `auth/` | `ClientConfig` (console JSON lookup), `pkce`, `TokenStore` (`KeychainStore`, `MemoryStore`), `Authenticator` (loopback sign-in, single-flight refresh, rotation, `invalid_grant`, revoke), the `TokenProvider` trait |
| `drive.rs` | `DriveClient`: folders, appProperties identity, paginated list, multipart / resumable upload, copy, trash, changes, `about` |
| `memory.rs` | `MemoryRemote`: the in-memory fake (offline switch, interrupted upload, stats, edits behind the engine's back) |
| `config.rs` | `SyncConfig`, `RootConfig` (include / ignore globs), default roots and folders, device name |
| `state.rs` | the JSON state DB (base entries, stat cache, pending sessions, root id, page token) |
| `local.rs` | scan with the stat cache, md5 / sha256, temp names, local trash, blob / doc.json path rules |
| `plan.rs` | the three-way plan and conflict names |
| `engine.rs` | `Engine::sync_once` and `remote_changed` |
| `background.rs` | `SyncHandle` (thread + runtime, watcher, change polling, backoff, auth pause) |

Tests (`cargo test -p easycad-sync`, no network): unit tests per module; `tests/engine.rs`
(new local, new remote, edits both ways, doc and file conflicts, deletes, blob dedupe,
rename, interrupted upload resumed after a restart, offline then reconnect, safety
stops, recreated Drive folder, lost state DB, the background handle with the
watcher); `tests/auth.rs` (refresh and rotation, invalid_grant, 5xx, invalidate,
revoke, loopback sign-in end to end, denied consent); `tests/drive.rs` (pagination,
backoff on 503 / rate-limit 403, 401 refresh, retries exhausted, resumable from the
server's offset, expired session).

Examples: `cargo run -p easycad-sync --example login -- [--client-file F] [--sign-out]`,
`cargo run -p easycad-sync --example sync_dir -- DIR [--root NAME] [--watch] [--memory]`.

## Design (implemented)

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

- The Google project is in Testing mode: only listed test users can sign in and
  refresh tokens expire after 7 days. Publish it (no review needed for `drive.file`
  without a logo) before other people use it.
- Local deletes by sync go to `<root>/.cwsync-trash/<date>/`, which nothing empties yet.
- A Drive 403 `storageQuotaExceeded` is reported per file; the GUI shows only the last error.
- Changes are polled from one token for all roots; a change outside `easyCAD/` the app
  can see (none with `drive.file`) would only cost an extra pass.
- No end-to-end test against the real Drive in CI (needs an account); the manual check
  is the `login` and `sync_dir` examples.
