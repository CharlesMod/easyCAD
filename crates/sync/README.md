# easycad-sync

Google Drive sync for easyCAD: OAuth2 for an installed app (PKCE, loopback
redirect, refresh token in the OS keychain), a Drive v3 client, and a local-first
three-way sync engine. The app uses it through `crates/app/src/cloud.rs`; the
design is in `TODO.md`.

## Google setup (once per Google project)

`scripts/setup-google.sh <project-id>` creates the project and enables the Drive
API, then prints the console pages for the rest:

1. Google Auth Platform ▸ **Branding**: app name, support e-mail.
2. **Audience**: External. While the app is in *Testing*, add every Google account
   that will sign in under *Test users* (sign-in is refused otherwise) and expect
   refresh tokens to expire after 7 days. *Publish app* removes both limits; with
   only the `drive.file` scope and no logo it needs no Google review.
3. **Data access**: add the scope `https://www.googleapis.com/auth/drive.file`.
4. **Clients** ▸ Create client ▸ *Desktop app* ▸ download the JSON.
5. Install it where easyCAD looks for it:
   `cargo run -p easycad-sync --example login -- --client-file ~/Downloads/client_secret_….json`
   (copies it to `~/.config/easycad/google_client.json`, mode 0600, and signs in).

Lookup order for the client: `--client-file`, `EASYCAD_GOOGLE_CLIENT_FILE`,
`<config>/easycad/google_client.json`, `EASYCAD_GOOGLE_CLIENT_ID` +
`EASYCAD_GOOGLE_CLIENT_SECRET`. Never commit the file.

## Try it

```
cargo run -p easycad-sync --example login                 # browser sign-in, prints the account
cargo run -p easycad-sync --example sync_dir -- ~/tmp/s   # one pass of a folder to easyCAD/Documents
cargo run -p easycad-sync --example login -- --sign-out   # revoke and forget the token
cargo test -p easycad-sync                                # offline tests (fake remote, wiremock)
```
