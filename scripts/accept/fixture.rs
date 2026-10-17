//! Acceptance-only bootstrap. Creates synthetic storage, never app services.
use erlkonig_core::{
    config::Bootstrap,
    init,
    node_store::{NodeSettings, NodeStore},
    paths::{Layout, ensure_private_dir},
};
use serde_json::json;
use std::{env, path::PathBuf};

fn main() {
    let args: Vec<_> = env::args_os().skip(1).collect();
    assert_eq!(
        args.len(),
        1,
        "usage: accept-fixture ABSOLUTE_NEW_FIXTURE_ROOT"
    );
    let root = PathBuf::from(&args[0]);
    assert!(root.is_absolute() && root.is_dir());
    for (name, relative) in [
        ("HOME", "home"),
        ("CODEX_HOME", "codex"),
        ("CLAUDE_CONFIG_DIR", "claude"),
        ("TMPDIR", "tmp"),
    ] {
        assert_eq!(
            PathBuf::from(env::var_os(name).expect("isolated environment")),
            root.join(relative)
        );
    }
    let home = root.join("home");
    let custody = home.join(".local/share/triblespace/custody");
    let app = home.join("Library/Application Support/Erlkonig");
    assert!(
        !custody.exists() && !app.exists(),
        "refuse an existing fixture identity"
    );
    ensure_private_dir(&custody).unwrap();
    let bootstrap = Bootstrap::new(&custody);
    let signer = init::prepare(&bootstrap).unwrap();
    let public = hex::encode(signer.verifying_key().to_bytes());
    let mut node = NodeStore::open(&bootstrap.pile, signer).unwrap();
    let view = node.config().unwrap();
    node.put_settings(
        &view,
        &NodeSettings {
            mcp_port: 0,
            sync_enabled: false,
            maintenance_enabled: false,
        },
    )
    .unwrap();
    let view = node.config().unwrap();
    let workspace = node
        .create_workspace(&view, "Synthetic acceptance fixture")
        .unwrap();
    node.close().unwrap();
    // No key bytes, app token, credentials, native owner or network endpoint.
    let layout = Layout::application(&app).unwrap();
    assert_eq!(layout.bootstrap().pile, bootstrap.pile);
    println!(
        "{}",
        json!({
            "app_data":app, "pile":bootstrap.pile, "key":bootstrap.key,
            "node_id":public, "workspace":format!("{workspace:X}"),
        })
    );
}
