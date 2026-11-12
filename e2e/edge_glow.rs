//! End to end against a real haunt core. Copy into haunt's `cli/tests/` and run
//! `cargo test -p haunt-cli --features plugins --test edge_glow` (see README).
#![cfg(feature = "plugins")]

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use haunt_core::plugin_settings::PluginConfig;
use haunt_core::plugin_store::{PluginSet, Store, attach};
use haunt_core::plugins::{GlowLabel, HostRequest, PluginHost};
use haunt_core::{Core, SurfaceEnv};

const ID: &str = "dev.haunt.edge-glow";

fn repo() -> PathBuf {
    std::env::var_os("HAUNT_PLUGINS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../haunt-plugins"))
        .join("haunt-edge-glow")
}

fn send(sock: &mut UnixStream, env: &SurfaceEnv, nonce: bool, rest: &str) {
    let nonce = if nonce { format!(r#","nonce":"{}""#, env.nonce) } else { String::new() };
    let line = format!(r#"{{"v":1,"agent":"claude","surface_id":"{}"{nonce},"session_id":"s1","cwd":"/tmp/api",{rest}}}"#, env.surface_id);
    sock.write_all(format!("{line}\n").as_bytes()).unwrap();
}

/// The latest glow request: `Some(label)` for a show, `None` for a hide.
fn next_glow(set: &Mutex<PluginSet>) -> Option<GlowLabel> {
    let t = Instant::now();
    loop {
        let last = set.lock().unwrap().take_requests().into_iter().filter_map(|r| match r {
            HostRequest::GlowShow { plugin_id, label, .. } if plugin_id == ID => Some(Some(label)),
            HostRequest::GlowHide { plugin_id, .. } if plugin_id == ID => Some(None),
            _ => None,
        }).last();
        if let Some(g) = last {
            return g;
        }
        assert!(t.elapsed() < Duration::from_secs(5), "timed out waiting for the glow");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_screen_edge_glows_while_an_agent_waits() {
    let dir = repo();
    if !dir.join("haunt-plugin.toml").is_file() {
        eprintln!("skipped: haunt-edge-glow not found; set HAUNT_PLUGINS_DIR");
        return;
    }
    let home = PathBuf::from(format!("/tmp/haunt-fp-glow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "--release", "--target", "wasm32-wasip2"])
        .current_dir(&dir)
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .unwrap();
    assert!(status.success());
    let o = Command::new(env!("CARGO_BIN_EXE_haunt")).args(["plugin", "install", dir.to_str().unwrap(), "--yes"]).env("HAUNT_HOME", &home).output().unwrap();
    assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));

    let core = Core::new(&home);
    core.start().unwrap();
    let (env, other) = (core.register_surface("/tmp").unwrap(), core.register_surface("/tmp").unwrap());
    let set = Store::open(&home).unwrap().load_all(&PluginHost::new().unwrap(), Some(&core.handle())).unwrap();
    assert!(set.errors().is_empty(), "{:?}", set.errors());
    let set = Arc::new(Mutex::new(set));
    attach(&core, &set);

    let mut sock = UnixStream::connect(&env.socket_path).unwrap();
    send(&mut sock, &env, true, r#""event":"prompt_submit","query":"go""#);
    send(&mut sock, &other, false, r#""event":"permission_seen","tool_name":"Bash","tool_input":{"command":"rm -rf /"}"#);
    send(&mut sock, &env, true, r#""event":"permission_seen","tool_name":"Bash","tool_input":{"command":"cargo publish"}"#);
    let Some(GlowLabel::Row { surface, head, .. }) = next_glow(&set) else { panic!("no row label") };
    assert_eq!(surface, env.surface_id, "the forged ask counts for nothing");
    assert!(head.contains("cargo publish?"), "Haunt's words, with the command: {head:?}");

    let label = |l: &str| set.lock().unwrap().set_config(&PluginConfig::parse(&format!("[plugins.\"{ID}\"]\nlabel = \"{l}\"\n")));
    label("count");
    assert_eq!(next_glow(&set), Some(GlowLabel::Needs), "\"1 needs you\"");
    label("none");
    assert_eq!(next_glow(&set), Some(GlowLabel::NoLabel), "the glow alone");

    send(&mut sock, &env, true, r#""event":"prompt_submit","query":"yes""#);
    assert_eq!(next_glow(&set), None, "answered: the glow goes");

    drop(core);
    let _ = std::fs::remove_dir_all(&home);
}
