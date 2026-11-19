# Live reload (dev installs)

A running easyCAD restarts itself into a newly installed build and restores its
session, so UI and behaviour changes apply without the user quitting the app.

## Applying a change

`scripts/dev-install.sh` builds `cargo build --release -p easycad` in the repo it
is run from and installs it atomically (copy to `~/.local/bin/.easycad.new`, then
`mv -f` over `~/.local/bin/easycad`; the running process's file is never written
into). It prints the sha installed. Set `EASYCAD_OCCT_DIR` for the prebuilt OCCT.

## What the app does

1. At startup it remembers the path it was started from (`current_exe`, with
   Linux's ` (deleted)` suffix removed) and that file's inode, size and mtime.
2. A background thread polls it every second. A file with a different stamp, the
   executable bit set and an unchanged stamp for 300 ms counts as a finished build.
3. The UI then waits until the app is idle (`reload::idle`): no mouse button down,
   no drag, no focused text field, no dialog / prompt / wizard / export open, no
   pending debounced edit, and not in sketch or drawing mode (their draft state is
   not serialisable). While waiting it shows one toast saying why.
4. It writes `$XDG_DATA_HOME/easycad/reload/<pid>.json` plus
   `<pid>.doc.ecad` (the whole document, unsaved edits included), also a normal
   recovery copy when the document is dirty, and `exec`s the new binary with
   `--resume <json>` (same PID, so the window manager sees the same process).
5. The new process loads the document, restores camera, display mode, parts mode,
   browser/build-plate panels, hidden bodies, document path and dirty state, and
   window size / position / maximised state, deletes the session files, and shows
   "Updated to <sha> (was <sha>) - session restored".

Not carried over: the undo history (it lives inside the document type), selection,
and uncommitted sketch / drawing / dialog state (reload waits for those to close).

If the session file cannot be read, the app starts normally, the recovery copy is
made an orphan so the usual "recover unsaved work" offer lists it, and the session
files stay on disk. If `exec` fails, the app warns and stops watching.

## Switches

* Settings > Updates > "Auto-reload on new build" (default on).
* `EASYCAD_NO_AUTORELOAD=1` disables the watcher entirely.

The running build is shown in Settings > Updates: version, git short sha (`+` =
uncommitted changes at build time) and build time (`build.rs`).

## Data files

Settings and process profiles are read at start and written by the app; they are
not reloaded when edited externally (not done: small gain, risk of fighting the
app's own writes).
