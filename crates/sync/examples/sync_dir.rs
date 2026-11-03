//! Sync one local folder with `easyCAD/<root>/` in My Drive, once or in the
//! background. Uses the token `login` stored.
//!
//! ```text
//! cargo run -p easycad-sync --example sync_dir -- DIR [--root NAME] [--watch] [--memory]
//! ```
//! `--memory` runs against the in-memory remote (no network) for a dry look
//! at what a first pass would upload.

use std::path::PathBuf;
use std::sync::Arc;

use easycad_sync::auth::{Authenticator, ClientConfig, KeychainStore};
use easycad_sync::{
    BackgroundOptions, DriveClient, Engine, EventSink, MemoryRemote, RemoteStore, RootConfig, SyncConfig, SyncEvent,
    SyncHandle,
};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut dir: Option<PathBuf> = None;
    let mut root = "Documents".to_string();
    let (mut watch, mut memory) = (false, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--root" => root = args.next().unwrap_or(root),
            "--watch" => watch = true,
            "--memory" => memory = true,
            _ if dir.is_none() && !a.starts_with('-') => dir = Some(PathBuf::from(a)),
            _ => {
                eprintln!("usage: sync_dir DIR [--root NAME] [--watch] [--memory]");
                std::process::exit(2);
            }
        }
    }
    let Some(dir) = dir else {
        eprintln!("usage: sync_dir DIR [--root NAME] [--watch] [--memory]");
        std::process::exit(2);
    };
    let state = easycad_sync::config::data_dir().join("sync").join(format!("example-{root}.json"));
    let cfg = SyncConfig::new(vec![RootConfig::new(&root, &dir)], state);
    let remote: Arc<dyn RemoteStore> = if memory {
        Arc::new(MemoryRemote::new())
    } else {
        let client = ClientConfig::discover(None).unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1)
        });
        Arc::new(DriveClient::new(Arc::new(Authenticator::new(client, Arc::new(KeychainStore::default_entry())))))
    };
    let (sink, rx) = EventSink::channel();
    let engine = Engine::new(remote, cfg, sink.clone());
    let opts = BackgroundOptions { watch, ..Default::default() };
    let handle = SyncHandle::spawn(engine, sink, opts).expect("sync thread");
    for ev in rx {
        match ev {
            SyncEvent::Progress(p) if p.bytes_done == 0 => println!("[{}/{}] {:?} {}", p.index, p.total, p.kind, p.path),
            SyncEvent::Conflict(c) => println!("conflict: {} (remote version kept as {})", c.path, c.conflict_copy),
            SyncEvent::Error(e) => eprintln!("error: {}{}", e.path.map(|p| p + ": ").unwrap_or_default(), e.message),
            SyncEvent::AuthRequired { message } => {
                eprintln!("sign-in needed ({message}); run the login example");
                break;
            }
            SyncEvent::Status(s) => println!("status: {s:?}"),
            SyncEvent::Completed(r) => {
                println!(
                    "pass done: {} up, {} copied, {} down, {} deleted here, {} trashed on Drive, {} conflicts, {} errors",
                    r.uploaded, r.copied_remote, r.downloaded, r.deleted_local, r.deleted_remote, r.conflicts.len(), r.errors.len()
                );
                if !watch {
                    break;
                }
            }
            _ => {}
        }
    }
    handle.stop();
}
