# Building easyCAD

## Release packages for all three platforms (run on a Mac)

```bash
scripts/build-release.sh            # macOS + Linux + Windows
scripts/build-release.sh macos      # or any subset: macos linux windows
```

Output in `dist/`:

| file | contents |
|---|---|
| `easyCAD-<ver>-<sha>-macos-universal.zip` | `easyCAD.app` (arm64 + x86_64, ad-hoc signed) |
| `easyCAD-<ver>-<sha>-linux-x86_64.zip` | `easycad` binary, `.desktop` file, README with runtime deps |
| `easyCAD-<ver>-<sha>-windows-x86_64.zip` | `easyCAD.exe` (static, no DLLs, no console window) |
| `easyCAD-<ver>-<sha>-all-platforms.zip` | the three zips above |

Every package also carries `LICENSE` and `THIRD-PARTY-NOTICES.html` (written by
`scripts/notices.sh`: each Rust crate's license plus OpenCascade's LGPL exception).

### One-time setup on the Mac
1. Xcode Command Line Tools: `xcode-select --install`
2. Homebrew packages: `brew install cmake`
3. Rust: https://rustup.rs (the script adds the macOS targets itself)
4. Docker Desktop (for the Linux and Windows builds), running.
5. cargo-about, for the license notices every package carries:
   `cargo install --locked cargo-about --features cli`

### How it builds
- **macOS:** native `cargo build --release` for each arch in `MACOS_ARCHS`, then `lipo` into a
  universal binary inside `easyCAD.app`, with an ad-hoc `codesign`. Minimum macOS 11.
- **Linux:** inside a Debian 12 container (`scripts/docker/Dockerfile`), so the binary needs
  glibc 2.36 or newer. GPU (Vulkan) and X11/Wayland libraries are loaded at runtime.
- **Windows:** cross-compiled in the same container with MinGW-w64 (POSIX threads) for
  `x86_64-pc-windows-gnu`, statically linked. The container runs at the Mac's native
  arch, so no emulation is involved.

### Time and caching
OpenCascade is compiled from source once per platform: roughly 10–15 min per macOS arch on
Apple Silicon, and similar per Docker platform. Later runs reuse it (`target/` for macOS, Docker volumes
`easycad-target-*` for Linux/Windows); a rebuild after code changes takes a few minutes.
Speed-ups: `MACOS_ARCHS=arm64` (skip Intel), `LINUX_ARCHS=aarch64` (native on Apple
Silicon; `x86_64` runs under emulation and is slower).

### Knobs
`MACOS_ARCHS`, `LINUX_ARCHS`, `RUST_IMAGE`, `JOBS`: see the header of `scripts/build-release.sh`.
Reset the Docker caches with `docker volume rm $(docker volume ls -q | grep easycad-)`.

### Unsigned builds
- macOS: built and signed locally, so it opens normally on the Mac that built it. Copied to
  another Mac, it may need right-click → Open the first time.
- Windows: SmartScreen may warn ("More info → Run anyway").

## Development build (any platform)
```bash
cargo run -p easycad --release -- --sample
```
Needs cmake (3.x or 4.x: `crates/occt/build.rs` sets `CMAKE_POLICY_VERSION_MINIMUM=3.5`, which CMake 4 needs for OCCT 7.8) and a C++17 compiler; the first build compiles OpenCascade (~20 min). Another checkout (a git worktree) can reuse that build: `EASYCAD_OCCT_DIR=<first checkout>/target/OCCT` (the directory holding `lib/` and `include/`) skips the OCCT build.
