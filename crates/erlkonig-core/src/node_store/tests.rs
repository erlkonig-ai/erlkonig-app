use super::*;
use crate::node_metrics::{Sample, now_tai_ns};
use std::path::PathBuf;
use std::time::Duration;
use triblespace::core::collection::{CollectionRecord, CollectionStore};
use triblespace::core::repo::BlobStorePut;
use triblespace_net::{dashboard::CountMetric, telemetry::Metric};

fn scratch() -> (tempfile::TempDir, PathBuf, SigningKey, NodeStore) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("node.pile");
    std::fs::File::create(&path).unwrap();
    let signer = SigningKey::from_bytes(&[39; 32]);
    let node = NodeStore::open(&path, signer.clone()).unwrap();
    (dir, path, signer, node)
}

fn legacy_bundle(node: &mut NodeStore, roots: &[(Id, CollectionHandle)]) -> Workspace {
    let mut workspace = Workspace::new("legacy bundle");
    workspace.routes.extend(roots.iter().copied());
    let mut routes = Fragment::empty();
    for (role, root) in roots {
        routes += entity! { config::anchor: *role, config::selects: *root };
    }
    let fragment = entity! {
        metadata::tag: attrs::WORKSPACE, config::anchor: workspace.anchor,
        attrs::enrolled: true, metadata::name: workspace.label.clone(),
        attrs::workspace_route*: routes,
    };
    workspace.id = fragment.root().unwrap();
    node.pile.commit(node.config, &node.signer, fragment).unwrap();
    node.pile.flush().unwrap();
    workspace
}

#[test]
fn old_peer_annotations_are_inert_and_new_settings_do_not_emit_them() {
    use triblespace_net::health_record;
    let (_dir, path, signer, mut node) = scratch();
    for peer in [SigningKey::from_bytes(&[40; 32]), SigningKey::from_bytes(&[41; 32])] {
        node.pile.commit(node.config, &signer, entity! {
            config::anchor: attrs::NODE_SETTINGS, attrs::mcp_port: 41773_u16,
            attrs::sync_enabled: true, attrs::maintenance_enabled: true,
            health_record::attrs::peer: peer.verifying_key(),
        }).unwrap();
    }
    node.pile.flush().unwrap();
    let before = std::fs::read(&path).unwrap();
    let view = node.config().unwrap();
    assert_eq!(view.settings().unwrap(), NodeSettings::default());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let revision = node.put_settings(&view, &NodeSettings::default()).unwrap();
    let facts = node.config().unwrap().facts().unwrap();
    assert!(!exists!(pattern!(&facts, [{ revision @ health_record::attrs::peer: _?peer }])));
    assert!(exists!(pattern!(&facts, [{ _?old @ health_record::attrs::peer: _?peer }])));
    node.close().unwrap();
}

#[test]
fn legacy_sync_bootstrap_selects_only_current_bundles_and_restarts_without_growth() {
    use triblespace::core::collection::{grant_collection_read, selection};
    let (_dir, path, signer, mut node) = scratch();
    assert_eq!(node.config.handle(), selection::config_handle(signer.verifying_key()));
    let empty = std::fs::read(&path).unwrap();
    assert_eq!(node.initialize_workspace_sync().unwrap(), 0);
    assert_eq!(std::fs::read(&path).unwrap(), empty);
    assert!(crate::workspace::selected_sync_roots(&node.pile.snapshot().unwrap(), signer.verifying_key()).unwrap().is_empty());
    let saved = node.pile.collection("saved", faculties::collection_names::private_policy(signer.verifying_key())).unwrap();
    let ungrouped = node.pile.collection("ungrouped", faculties::collection_names::private_policy(signer.verifying_key())).unwrap();
    node.pile.commit(ungrouped, &signer, entity! { metadata::name: "resident but unconfigured" }).unwrap();
    let owner = SigningKey::from_bytes(&[42; 32]);
    let invited = node.pile.collection("grant only", faculties::collection_names::private_policy(owner.verifying_key())).unwrap();
    grant_collection_read(&mut node.pile, invited.handle(), &owner, signer.verifying_key()).unwrap();
    legacy_bundle(&mut node, &[(faculties::schemas::wiki::DEFAULT_SCOPE_ID, saved.handle())]);
    assert_eq!(node.initialize_workspace_sync().unwrap(), 1);
    let selected = crate::workspace::selected_sync_roots(&node.pile.snapshot().unwrap(), signer.verifying_key()).unwrap();
    assert_eq!(selected, [saved.handle()]);
    let facts = node.config().unwrap().facts().unwrap();
    assert_eq!(selection::sync_selection(&facts, invited.handle()), selection::Selection::Unset);
    assert_eq!(selection::sync_selection(&facts, ungrouped.handle()), selection::Selection::Unset);
    let before = std::fs::read(&path).unwrap();
    assert_eq!(node.initialize_workspace_sync().unwrap(), 0);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    node.close().unwrap();
    let mut node = NodeStore::open(&path, signer).unwrap();
    assert_eq!(node.initialize_workspace_sync().unwrap(), 0);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    node.close().unwrap();
}

#[test]
fn legacy_sync_bootstrap_preserves_unselected_and_conflicted_choices() {
    use triblespace::core::collection::selection::{self, Selection};
    let (_dir, path, signer, mut node) = scratch();
    let off = node.pile.collection("off", faculties::collection_names::private_policy(signer.verifying_key())).unwrap();
    let fork = node.pile.collection("fork", faculties::collection_names::private_policy(signer.verifying_key())).unwrap();
    legacy_bundle(&mut node, &[
        (faculties::schemas::wiki::DEFAULT_SCOPE_ID, off.handle()),
        (faculties::schemas::compass::DEFAULT_SCOPE_ID, fork.handle()),
    ]);
    selection::write_sync_selection(&mut node.pile, node.config, &signer, off.handle(), false).unwrap();
    for value in [true, false] {
        node.pile.commit(node.config, &signer, entity! { &genid() @
            selection::sync_collection: fork.handle(), selection::sync_selected: value,
        }).unwrap();
    }
    node.pile.flush().unwrap();
    let before = std::fs::read(&path).unwrap();
    assert_eq!(node.initialize_workspace_sync().unwrap(), 0);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let facts = node.config().unwrap().facts().unwrap();
    assert_eq!(selection::sync_selection(&facts, off.handle()), Selection::Unselected);
    assert!(matches!(selection::sync_selection(&facts, fork.handle()), Selection::Conflicted(_)));
    assert!(crate::workspace::selected_sync_roots(&node.pile.snapshot().unwrap(), signer.verifying_key()).unwrap().is_empty());
    node.close().unwrap();
}

#[test]
fn explicit_bundle_changes_select_only_new_roots_and_removal_does_not_unselect() {
    use triblespace::core::collection::selection::{self, Selection};
    let (_dir, _path, signer, mut node) = scratch();
    let first = node.pile.collection("first", faculties::collection_names::private_policy(signer.verifying_key())).unwrap();
    let second = node.pile.collection("second", faculties::collection_names::private_policy(signer.verifying_key())).unwrap();
    let manual = node.pile.collection("manual", faculties::collection_names::private_policy(signer.verifying_key())).unwrap();
    selection::write_sync_selection(&mut node.pile, node.config, &signer, manual.handle(), true).unwrap();
    let mut workspace = Workspace::new("explicit");
    workspace.routes.insert(faculties::schemas::wiki::DEFAULT_SCOPE_ID, first.handle());
    let view = node.config().unwrap();
    workspace.id = node.put_workspace(&view, &workspace).unwrap();
    selection::write_sync_selection(&mut node.pile, node.config, &signer, first.handle(), false).unwrap();
    workspace.label = "label-only edit".into();
    let view = node.config().unwrap();
    workspace.id = node.put_workspace(&view, &workspace).unwrap();
    let facts = node.config().unwrap().facts().unwrap();
    assert_eq!(selection::sync_selection(&facts, first.handle()), Selection::Unselected);
    workspace.routes.insert(faculties::schemas::compass::DEFAULT_SCOPE_ID, second.handle());
    let view = node.config().unwrap();
    workspace.id = node.put_workspace(&view, &workspace).unwrap();
    let expected = [second.handle(), manual.handle()].into_iter().collect::<std::collections::BTreeSet<_>>();
    assert_eq!(crate::workspace::selected_sync_roots(&node.pile.snapshot().unwrap(), signer.verifying_key()).unwrap().into_iter().collect::<std::collections::BTreeSet<_>>(), expected);
    let view = node.config().unwrap();
    node.remove_workspace_revision(&view, workspace.id).unwrap();
    assert_eq!(crate::workspace::selected_sync_roots(&node.pile.snapshot().unwrap(), signer.verifying_key()).unwrap().into_iter().collect::<std::collections::BTreeSet<_>>(), expected);
    node.close().unwrap();
}

#[test]
fn daemon_address_hints_follow_actual_bindings_without_duplicate_appends() {
    use triblespace::core::collection::selection;
    let (_dir, path, signer, mut node) = scratch();
    assert!(node.record_sync_addresses(&["0.0.0.0:7111".parse().unwrap(), "[::]:7112".parse().unwrap()]).unwrap());
    let facts = node.config().unwrap().facts().unwrap();
    assert_eq!(selection::sync_addresses(&facts, signer.verifying_key()), ["127.0.0.1:7111".parse().unwrap(), "[::1]:7112".parse().unwrap()].into_iter().collect());
    let before = std::fs::read(&path).unwrap();
    assert!(!node.record_sync_addresses(&["0.0.0.0:7111".parse().unwrap(), "[::]:7112".parse().unwrap()]).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(node.record_sync_addresses(&["127.0.0.1:7222".parse().unwrap()]).unwrap());
    let facts = node.config().unwrap().facts().unwrap();
    assert_eq!(selection::sync_addresses(&facts, signer.verifying_key()), ["127.0.0.1:7222".parse().unwrap()].into_iter().collect());
    node.close().unwrap();
}
#[test]
fn incomplete_workspace_without_anchor_can_be_withdrawn_without_projection() {
    let (_dir, _path, _signer, mut node) = scratch();
    let revision = genid();
    node.pile
        .commit(
            node.config,
            &node.signer,
            entity! { &revision @
                metadata::tag: attrs::WORKSPACE, attrs::enrolled: true,
            },
        )
        .unwrap();
    let view = node.config().unwrap();
    assert_eq!(view.workspace_ids().unwrap(), [revision.id]);
    assert!(view.workspace_revision(revision.id).is_err());
    node.remove_workspace_revision(&view, revision.id).unwrap();
    let current = node.config().unwrap();
    assert!(current.workspace_ids().unwrap().is_empty());
    assert!(
        node.remove_workspace_revision(&current, revision.id)
            .is_err()
    );
    node.close().unwrap();
}

#[test]
fn withdrawing_one_unavailable_revision_does_not_choose_a_concurrent_winner() {
    let (_dir, _path, _signer, mut node) = scratch();
    let anchor = genid().id;
    let first = genid();
    let second = genid();
    for revision in [first.id, second.id] {
        node.pile
            .commit(
                node.config,
                &node.signer,
                entity! { ExclusiveId::force_ref(&revision) @
                    metadata::tag: attrs::WORKSPACE, attrs::enrolled: true,
                    config::anchor: anchor,
                },
            )
            .unwrap();
    }
    let view = node.config().unwrap();
    node.remove_workspace_revision(&view, first.id).unwrap();
    let view = node.config().unwrap();
    assert_eq!(view.workspace_ids().unwrap(), [second.id]);
    assert!(view.workspace_revision(second.id).is_err());
    node.remove_workspace_revision(&view, second.id).unwrap();
    assert!(node.config().unwrap().workspace_ids().unwrap().is_empty());
    node.close().unwrap();
}

#[test]
fn unknown_known_commit_bytes_are_not_a_known_empty_configuration() {
    let (_dir, _path, _signer, mut node) = scratch();
    assert!(node.poll_config().unwrap().is_some());
    let mut source = MemoryRepo::default();
    let remote = source
        .commit(
            node.config,
            &node.signer,
            entity! {
                config::anchor: attrs::NODE_SETTINGS, attrs::mcp_port: 0_u16,
                attrs::sync_enabled: false, attrs::maintenance_enabled: false,
            },
        )
        .unwrap();
    // The admitted record arrived, its payload did not. A resident-only
    // collection read can be partial; configuration intentionally cannot.
    node.pile.insert(CollectionRecord::Commit(remote)).unwrap();
    assert!(node.config().is_err());
    assert!(node.poll_config().is_err());
    assert!(
        node.last_config.is_none(),
        "a failed poll must re-emit configuration after recovery"
    );
    let payload: Blob<blobencodings::SimpleArchive> = source
        .snapshot()
        .unwrap()
        .get(Inline::new(remote.data().raw))
        .unwrap();
    node.pile
        .put::<blobencodings::SimpleArchive, _>(payload)
        .unwrap();
    assert_eq!(
        node.poll_config()
            .unwrap()
            .unwrap()
            .settings()
            .unwrap()
            .mcp_port,
        0
    );
    node.close().unwrap();
}

#[test]
fn managed_measurements_cannot_impersonate_the_collector_identity() {
    let (_dir, _path, _signer, mut node) = scratch();
    let sample = Sample::now(
        Some(genid().id),
        "native",
        "sync",
        genid().id,
        Duration::ZERO,
    )
    .unwrap();
    assert!(
        node.append_metrics(&[sample])
            .unwrap_err()
            .to_string()
            .contains("worker signing identity")
    );
    assert!(
        node.metrics(now_tai_ns().unwrap(), Duration::from_secs(60))
            .unwrap()
            .is_empty()
    );
    node.close().unwrap();
}

#[test]
fn defaults_are_one_node_on_with_no_implicit_workspace() {
    fn send<T: Send>() {}
    send::<NodeStore>();
    let (_dir, _path, _signer, mut node) = scratch();
    let view = node.config().unwrap();
    assert_eq!(view.settings().unwrap(), NodeSettings::default());
    assert!(view.settings().unwrap().sync_enabled);
    assert!(view.workspace_ids().unwrap().is_empty());
    node.close().unwrap();
}

#[test]
fn workspace_revisions_are_path_free_and_old_requests_never_retarget() {
    let (_dir, path, signer, mut node) = scratch();
    let view = node.config().unwrap();
    let id = node.create_workspace(&view, "Research").unwrap();
    let view = node.config().unwrap();
    let mut workspace = view.workspace(id).unwrap().unwrap();
    assert_eq!(workspace.routes.len(), faculties::collection_names::table().len());
    let facts = view.facts().unwrap();
    assert!(!exists!(
        pattern!(&facts, [{id @ attrs::pile_path: _?path}])
    ));
    assert!(!exists!(pattern!(&facts, [{id @ attrs::key_path: _?key}])));
    assert!(!exists!(
        pattern!(&facts, [{id @ attrs::managed_pile: _?managed}])
    ));
    let roots = workspace.routes.clone();
    workspace.label = "Renamed".into();
    let next = node.put_workspace(&view, &workspace).unwrap();
    node.close().unwrap();
    let mut node = NodeStore::open(&path, signer).unwrap();
    let view = node.config().unwrap();
    assert!(view.workspace(id).unwrap().is_none());
    assert_eq!(view.workspace(next).unwrap().unwrap().routes, roots);
    node.remove_workspace(&view, workspace.anchor).unwrap();
    assert!(node.config().unwrap().workspace_ids().unwrap().is_empty());
    node.close().unwrap();
}

#[test]
fn equal_names_get_distinct_private_bundles_with_friendly_names() {
    let (_dir, _path, _signer, mut node) = scratch();
    let view = node.config().unwrap();
    let first = node.create_workspace(&view, "Research").unwrap();
    let view = node.config().unwrap();
    let second = node.create_workspace(&view, "Research").unwrap();
    let view = node.config().unwrap();
    let a = view.workspace(first).unwrap().unwrap();
    let b = view.workspace(second).unwrap().unwrap();
    for (role, _) in faculties::collection_names::table() {
        assert_ne!(a.routes[&role], b.routes[&role]);
        let descriptor: TribleSet = node.pile.snapshot().unwrap().get(a.routes[&role]).unwrap();
        let name = triblespace::core::collection::descriptor::name(&descriptor)
            .unwrap()
            .unwrap();
        let name: View<str> = node.pile.snapshot().unwrap().get(name).unwrap();
        assert!(name.starts_with("Research · "));
        assert!(!name.contains(&format!("{:X}", a.anchor)));
    }
    node.close().unwrap();
}

#[test]
fn cancel_stale_and_invalid_creation_leave_source_unchanged() {
    let (_dir, path, _signer, mut node) = scratch();
    let cancelled = node.config().unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(node.create_workspace(&cancelled, "  ").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    node.put_settings(
        &cancelled,
        &NodeSettings {
            mcp_port: 0,
            ..NodeSettings::default()
        },
    )
    .unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(node.create_workspace(&cancelled, "stale").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    node.close().unwrap();
}

#[test]
fn opaque_concurrent_settings_preserve_equal_values_and_conflicting_heads() {
    let (_dir, _path, _signer, mut node) = scratch();
    for _ in 0..2 {
        let revision = genid();
        node.pile
            .commit(
                node.config,
                &node.signer,
                entity! { &revision @
                    config::anchor: attrs::NODE_SETTINGS, attrs::mcp_port: 123_u16,
                    attrs::sync_enabled: true, attrs::maintenance_enabled: false,
                    metadata::description: "unmodeled annotation",
                },
            )
            .unwrap();
    }
    assert_eq!(node.config().unwrap().settings().unwrap().mcp_port, 123);
    node.pile
        .commit(
            node.config,
            &node.signer,
            entity! {
                config::anchor: attrs::NODE_SETTINGS, attrs::mcp_port: 124_u16,
                attrs::sync_enabled: true, attrs::maintenance_enabled: false,
            },
        )
        .unwrap();
    assert!(node.config().unwrap().settings().is_err());
    let view = node.config().unwrap();
    node.put_settings(&view, &NodeSettings::default()).unwrap();
    assert_eq!(
        node.config().unwrap().settings().unwrap(),
        NodeSettings::default()
    );
    node.close().unwrap();
}

#[test]
fn missing_text_remains_pending_and_arrival_invalidates_observation() {
    let (_dir, _path, _signer, mut node) = scratch();
    let item = genid();
    let mut source = MemoryRepo::default();
    let name = source
        .put::<blobencodings::UTF8String, _>("arrived later")
        .unwrap();
    node.pile
        .commit(
            node.config,
            &node.signer,
            entity! { &item @
                metadata::tag: attrs::WORKSPACE, config::anchor: item.id,
                attrs::enrolled:true, metadata::name:name,
            },
        )
        .unwrap();
    let before = node.poll_config().unwrap().unwrap();
    assert!(before.workspace(item.id).is_err());
    assert!(node.poll_config().unwrap().is_none());
    node.pile
        .put::<blobencodings::UTF8String, _>("arrived later")
        .unwrap();
    assert!(node.config_changed(&before).unwrap());
    assert_eq!(
        node.config()
            .unwrap()
            .workspace(item.id)
            .unwrap()
            .unwrap()
            .label,
        "arrived later"
    );
    node.close().unwrap();
}

#[test]
fn recovery_endpoint_uses_only_an_agreed_port_not_a_settings_fork() {
    let (_dir, _path, _signer, mut node) = scratch();
    assert_eq!(node.config().unwrap().mcp_port().unwrap(),41773);
    for sync in [true,false] {
        node.pile.commit(node.config,&node.signer,entity! {
            config::anchor:attrs::NODE_SETTINGS,attrs::mcp_port:0_u16,
            attrs::sync_enabled:sync,attrs::maintenance_enabled:false,
        }).unwrap();
    }
    let view = node.config().unwrap();
    assert!(view.settings().is_err());
    assert_eq!(view.mcp_port().unwrap(),0);
    node.pile.commit(node.config,&node.signer,entity! {
        config::anchor:attrs::NODE_SETTINGS,attrs::mcp_port:12123_u16,
    }).unwrap();
    let view = node.config().unwrap();
    assert!(view.settings().is_err());
    assert_eq!(view.mcp_port().unwrap(),41773);
    node.put_settings(&view,&NodeSettings {mcp_port:12123,..NodeSettings::default()}).unwrap();
    assert_eq!(node.config().unwrap().mcp_port().unwrap(),12123);
    node.close().unwrap();
}

#[test]
fn metrics_use_node_identity_without_invalidating_config() {
    let (_dir, path, signer, mut node) = scratch();
    let before = node.config().unwrap();
    let mut sample =
        Sample::now(None, "native", "sync", genid().id, Duration::from_secs(10)).unwrap();
    sample.received_bytes = Some(123);
    node.append_metrics(&[sample]).unwrap();
    assert!(!node.config_changed(&before).unwrap());
    node.close().unwrap();
    let mut node = NodeStore::open(&path, signer.clone()).unwrap();
    let reports = node
        .metrics(now_tai_ns().unwrap(), Duration::from_secs(60))
        .unwrap();
    assert_eq!(reports.len(), 1);
    assert!(reports[0].managed.is_empty());
    assert_eq!(reports[0].report.node, signer.verifying_key().to_bytes());
    assert!(
        reports[0]
            .report
            .metrics
            .iter()
            .any(|m| m.metric == Metric::ReceivedBytes && m.value == CountMetric::Value(123))
    );
    node.close().unwrap();
}
