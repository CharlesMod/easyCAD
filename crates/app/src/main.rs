//! `easycad [path.ecad | --sample | --smoke <dir> | --sample-board <dir>] [--mcp [--mcp-port N]]`
//!
//! `--mcp` turns on the Claude bridge (MCP server on 127.0.0.1, see docs/MCP.md) for
//! this run; Settings → Claude bridge turns it on at every start.

use easycad_app::{native_options, EasyCadApp};
use easycad_model::document::Document;

/// Inside a distrobox/toolbox container that has no GPU Vulkan driver while the
/// host has one, wgpu falls back to llvmpipe (software rendering on the CPU).
/// Run the same binary on the host instead, with the same arguments and the
/// easyCAD / XDG settings. `EASYCAD_NO_HOST=1` keeps it in the container.
#[cfg(target_os = "linux")]
fn relaunch_on_host() {
    use std::path::Path;
    let in_container = Path::new("/run/.containerenv").exists() || Path::new("/.dockerenv").exists();
    if !in_container || std::env::var_os("EASYCAD_NO_HOST").is_some() || std::env::var_os("EASYCAD_ON_HOST").is_some() {
        return;
    }
    let gpu_icd = |dir: &str| {
        std::fs::read_dir(dir).into_iter().flatten().flatten().any(|e| {
            let n = e.file_name().to_string_lossy().to_lowercase();
            n.starts_with("nvidia") || n.starts_with("radeon") || n.starts_with("intel") && !n.contains("hasvk")
        })
    };
    let host_has = ["/run/host/usr/share/vulkan/icd.d", "/run/host/etc/vulkan/icd.d"].iter().any(|d| gpu_icd(d));
    let here_has = Path::new("/proc/driver/nvidia").exists()
        && ["/usr/share/vulkan/icd.d", "/etc/vulkan/icd.d"].iter().any(|d| {
            std::fs::read_dir(d).into_iter().flatten().flatten().any(|e| e.file_name().to_string_lossy().starts_with("nvidia"))
        });
    let exec = Path::new("/usr/bin/distrobox-host-exec");
    if !host_has || here_has || !exec.exists() {
        return;
    }
    let Ok(me) = std::env::current_exe() else { return };
    let mut cmd = std::process::Command::new(exec);
    cmd.arg("env").arg("EASYCAD_ON_HOST=1");
    for (k, v) in std::env::vars() {
        if k.starts_with("EASYCAD_") || k.starts_with("XDG_") || k.starts_with("RUST_") || k == "LP_NUM_THREADS" {
            cmd.arg(format!("{k}={v}"));
        }
    }
    cmd.arg(me).args(std::env::args().skip(1));
    eprintln!("easycad: no GPU driver in this container; starting on the host (EASYCAD_NO_HOST=1 to stay)");
    match cmd.status() {
        Ok(s) => std::process::exit(s.code().unwrap_or(1)),
        Err(e) => eprintln!("easycad: could not start on the host ({e}); continuing here"),
    }
}

/// Take `--mcp` / `--mcp-port N` out of the arguments: `Some(port)` when the bridge
/// was asked for.
fn mcp_flags(args: &mut Vec<String>) -> Option<u16> {
    let mut on = false;
    let mut port = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--mcp" => {
                on = true;
                args.remove(i);
            }
            "--mcp-port" if i + 1 < args.len() => {
                on = true;
                port = args[i + 1].parse().ok();
                args.drain(i..i + 2);
            }
            _ => i += 1,
        }
    }
    on.then(|| port.unwrap_or(easycad_mcp::DEFAULT_PORT))
}

fn main() -> eframe::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mcp = mcp_flags(&mut args);
    let arg = args.first().cloned();
    #[cfg(target_os = "linux")]
    if !matches!(arg.as_deref(), Some("--smoke") | Some("--sample-board")) {
        relaunch_on_host();
    }
    if arg.as_deref() == Some("--smoke") {
        let dir = args.get(1).cloned().unwrap_or_else(|| "easycad-smoke".into());
        match easycad_app::smoke::run(std::path::Path::new(&dir)) {
            Ok(log) => {
                for l in log {
                    println!("{l}");
                }
                println!("smoke test passed");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("smoke test FAILED: {e}");
                std::process::exit(1);
            }
        }
    }
    if arg.as_deref() == Some("--sample-board") {
        // the sample's PCB as EasyEDA-style files, for trying the PCB wizard
        let dir = std::path::PathBuf::from(args.get(1).cloned().unwrap_or_else(|| ".".into()));
        let r = std::fs::create_dir_all(&dir)
            .and_then(|_| std::fs::write(dir.join("sensor-node.step"), easycad_app::sample::pcb_step()))
            .and_then(|_| std::fs::write(dir.join("sensor-node.dxf"), easycad_app::sample::board_dxf()));
        match r {
            Ok(()) => println!("wrote {0}/sensor-node.step and {0}/sensor-node.dxf", dir.display()),
            Err(e) => {
                eprintln!("easycad: {e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    // `--resume <file>`: this process replaced an older build (live reload)
    let mut resume = None;
    if arg.as_deref() == Some("--resume") {
        if let Some(json) = easycad_app::reload::resume_arg() {
            match easycad_app::reload::read_session(&json) {
                Ok((snap, doc)) => resume = Some((json, snap, doc)),
                Err(e) => {
                    // the session files stay on disk; unsaved work is in the recovery offer
                    eprintln!("easycad: reload restore failed ({e}); falling back to recovery");
                    easycad_app::reload::orphan_own_recovery_copies();
                }
            }
        }
    }
    let mut snap = None;
    let mut opts = native_options();
    let (doc, path) = match resume {
        Some((json, s, d)) => {
            if let Some(w) = &s.window {
                easycad_app::reload::apply_window(&mut opts, w);
            }
            snap = Some((json, s));
            (Some(d), None)
        }
        None => match arg.as_deref() {
            Some("--resume") => (None, None),
            Some("--sample") => (Some(easycad_app::sample::sensor_node()), None),
            Some(p) => match Document::load_from_dir(std::path::Path::new(p)) {
                Ok(d) => (Some(d), Some(std::path::PathBuf::from(p))),
                Err(e) => {
                    eprintln!("easycad: cannot open {p}: {e}");
                    (None, None)
                }
            },
            None => (None, None),
        },
    };
    eframe::run_native(
        "easyCAD",
        opts,
        Box::new(move |cc| {
            let mut app = EasyCadApp::new(cc, doc);
            app.path = path;
            if let Some((json, s)) = &snap {
                app.apply_snapshot(s);
                easycad_app::reload::clear_session(json);
            }
            app.start_reload_watch(&cc.egui_ctx);
            app.cloud = easycad_app::cloud::Cloud::start(&cc.egui_ctx, easycad_app::settings::dir());
            let port = app.settings.mcp_port.unwrap_or(easycad_mcp::DEFAULT_PORT);
            match mcp {
                Some(p) => app.set_mcp(&cc.egui_ctx, true, p),
                None if app.settings.mcp => app.set_mcp(&cc.egui_ctx, true, port),
                None => {}
            }
            Ok(Box::new(app))
        }),
    )
}
