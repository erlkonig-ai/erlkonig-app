//! Acceptance-only bootstrap. Creates synthetic storage, never app services.
mod old_framing;
mod legacy32_input;

use erlkonig_core::{
    config::{Bootstrap, NodeLocator},
    init,
    node_store::{NodeSettings, NodeStore},
    paths::{Layout, ensure_private_dir},
};
use serde_json::json;
use std::{env, path::PathBuf};

fn main() {
    let args: Vec<_> = env::args_os().skip(1).collect();
    let old_framing = args.len() == 2 && args[1] == "--old-framing";
    let legacy32 = args.len() == 2 && args[1] == "--legacy32-input";
    assert!(args.len() == 1 || old_framing || legacy32,
        "usage: accept-fixture ABSOLUTE_NEW_FIXTURE_ROOT [--old-framing | --legacy32-input]");
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
    let selected_pile = if legacy32 { custody.join("historical.pile") } else { bootstrap.pile.clone() };
    if legacy32 {
        legacy32_input::export(&root, &bootstrap.pile, &bootstrap.key, &public, &selected_pile);
    }
    if old_framing {
        old_framing::append_released_blob(&bootstrap.pile);
    }
    if old_framing || legacy32 {
        ensure_private_dir(&app).unwrap();
        // The public app's real locator format, pointing at the same existing
        // key. No alternate test-only runtime configuration is introduced.
        std::fs::write(app.join("node.json"), serde_json::to_vec(&NodeLocator {
            pile: selected_pile.clone(), key: bootstrap.key.clone(), expected_signer: public.clone(),
        }).unwrap()).unwrap();
    }
    // No key bytes, app token, credentials, native owner or network endpoint.
    let layout = Layout::application(&app).unwrap();
    assert_eq!(layout.bootstrap().pile, bootstrap.pile);
    println!(
        "{}",
        json!({
            "app_data":app, "pile":selected_pile, "key":bootstrap.key,
            "node_id":public, "workspace":format!("{workspace:X}"),
            "old_framing":old_framing,
            "legacy32_input":legacy32,
        })
    );
}
