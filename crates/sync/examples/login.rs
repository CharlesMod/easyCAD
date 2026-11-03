//! Sign in to Google (browser, PKCE, loopback) and store the refresh token in
//! the OS keychain, then show the account and the `easyCAD/` folder.
//!
//! ```text
//! cargo run -p easycad-sync --example login -- [--client-file PATH] [--sign-out] [--list]
//! ```
//! A `--client-file` is copied to `<config>/easycad/google_client.json` (0600)
//! on success, so it is needed only once. `--list` prints what is in
//! `easyCAD/Documents` on Drive.

use std::path::PathBuf;
use std::sync::Arc;

use easycad_sync::auth::{client, Authenticator, ClientConfig, KeychainStore};
use easycad_sync::{DriveClient, RemoteStore};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let mut client_file: Option<PathBuf> = None;
    let mut sign_out = false;
    let mut list = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--client-file" => client_file = args.next().map(PathBuf::from),
            "--sign-out" => sign_out = true,
            "--list" => list = true,
            _ => {
                eprintln!("usage: login [--client-file PATH] [--sign-out] [--list]");
                std::process::exit(2);
            }
        }
    }
    let cfg = match ClientConfig::discover(client_file.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let auth = Arc::new(Authenticator::new(cfg, Arc::new(KeychainStore::default_entry())));
    if sign_out {
        match auth.sign_out().await {
            Ok(()) => println!("signed out; the token is revoked and removed from the keychain"),
            Err(e) => eprintln!("sign-out: {e}"),
        }
        return;
    }
    if !auth.is_signed_in().await.unwrap_or(false) {
        println!("opening the browser for Google sign-in…");
        if let Err(e) = auth.sign_in().await {
            eprintln!("sign-in failed: {e}");
            std::process::exit(1);
        }
        if let Some(f) = &client_file {
            match client::install(f) {
                Ok(p) => println!("client file installed at {}", p.display()),
                Err(e) => eprintln!("could not install the client file: {e}"),
            }
        }
    }
    let drive = DriveClient::new(auth.clone());
    match drive.account().await {
        Ok(a) => println!("signed in as {}", a.email.or(a.name).unwrap_or_else(|| "?".into())),
        Err(e) => {
            eprintln!("Drive: {e}");
            std::process::exit(1);
        }
    }
    match drive.root_id("Documents").await {
        Ok(_) => println!("My Drive ▸ easyCAD ▸ Documents is ready"),
        Err(e) => eprintln!("Drive folder: {e}"),
    }
    if list {
        match drive.list("Documents").await {
            Ok(mut files) => {
                files.sort_by(|a, b| a.path.cmp(&b.path));
                for f in files {
                    let when = f.modified_time.map(|t| t.to_rfc3339()).unwrap_or_default();
                    println!("  easyCAD/Documents/{}  {} bytes  {when}", f.path, f.size);
                }
            }
            Err(e) => eprintln!("list: {e}"),
        }
    }
}
