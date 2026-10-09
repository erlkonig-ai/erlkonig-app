use super::*;
use crate::{config::Bootstrap, init};
use triblespace::core::repo::{SnapshotSource, StorageFlush};

#[test]
fn key_handoff_preserves_conflicts_and_out_of_store_data_without_changing_source() {
    let dir = tempfile::tempdir().unwrap();
    let source_boot = Bootstrap::new(&dir.path().join("old"));
    let target_boot = Bootstrap::new(&dir.path().join("new"));
    std::fs::create_dir(source_boot.pile.parent().unwrap()).unwrap();
    std::fs::create_dir(target_boot.pile.parent().unwrap()).unwrap();
    let old = init::prepare(&source_boot).unwrap();
    let new = init::prepare(&target_boot).unwrap();
    let mut source = NodeStore::open(&source_boot.pile, old.clone()).unwrap();
    let mut target = NodeStore::open(&target_boot.pile, new.clone()).unwrap();
    let binding = NodeBinding {
        pile: target_boot.pile.clone(),
        key: target_boot.key.clone(),
        signer: new.verifying_key().to_bytes(),
    };
    let root = target
        .pile
        .collection(
            "existing wiki",
            faculties::collection_names::private_policy(new.verifying_key()),
        )
        .unwrap();
    let anchor = genid().id;
    let good = genid();
    let bad = genid();
    for port in [111_u16, 222_u16] {
        source
            .pile
            .commit(
                source.config,
                &old,
                entity! {
                    config::anchor:attrs::NODE_SETTINGS,attrs::mcp_port:port,
                },
            )
            .unwrap();
    }
    for (revision, pile) in [
        (good.id, binding.pile.clone()),
        (bad.id, dir.path().join("unapproved.pile")),
    ] {
        source.pile.commit(source.config,&old,entity!{ExclusiveId::force_ref(&revision) @
            metadata::tag:attrs::WORKSPACE,config::anchor:if revision==good.id {anchor}else{genid().id},
            attrs::enrolled:true,metadata::name:"preserved label",
            attrs::managed_pile:genid().id,attrs::pile_path:pile.to_str().unwrap().to_owned(),
            attrs::key_path:binding.key.to_str().unwrap().to_owned(),attrs::expected_signer:new.verifying_key(),
            attrs::workspace_route*:entity!{config::anchor:faculties::schemas::wiki::DEFAULT_SCOPE_ID,config::selects:root.handle()},
        }).unwrap();
    }
    source.pile.flush().unwrap();
    let before = std::fs::read(&source_boot.pile).unwrap();
    let old_snapshot = source.pile.snapshot().unwrap();
    let target_snapshot = target.pile.snapshot().unwrap();
    let defaults = NodeSettings {
        peers: vec![old.verifying_key().to_bytes()],
        ..NodeSettings::default()
    };
    let plan = MigrationPlan::prepare(
        &old_snapshot,
        source.config.handle(),
        None,
        &target_snapshot,
        &binding,
        &[binding.clone()],
        &defaults,
    )
    .unwrap();
    assert_eq!(plan.workspace_successors, 1);
    assert_eq!(plan.settings_successors, 2);
    assert!(
        plan.retained
            .iter()
            .any(|text| text.contains(&format!("{:X}", bad.id)))
    );
    assert_eq!(
        std::fs::read(&source_boot.pile).unwrap(),
        before,
        "prepare is read only"
    );
    let expected = target.config().unwrap();
    plan.apply(&mut target, &expected).unwrap();
    let view = target.config().unwrap();
    assert!(
        view.settings().is_err(),
        "concurrent ports remain conflicts"
    );
    assert!(view.workspace(good.id).unwrap().is_none());
    assert!(
        view.workspace(bad.id).is_err(),
        "out-of-store data never silently redirects"
    );
    let current = view
        .workspace_ids()
        .unwrap()
        .into_iter()
        .filter_map(|id| view.workspace(id).ok().flatten())
        .collect::<Vec<_>>();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].anchor, anchor);
    assert_eq!(
        current[0].routes[&faculties::schemas::wiki::DEFAULT_SCOPE_ID],
        root.handle()
    );
    assert_eq!(
        std::fs::read(&source_boot.pile).unwrap(),
        before,
        "apply changes target only"
    );
    source.close().unwrap();
    target.close().unwrap();
}

#[test]
fn cancelled_or_failed_plan_leaves_both_piles_unchanged_and_metrics_keep_collector() {
    let dir = tempfile::tempdir().unwrap();
    let a = Bootstrap {
        pile: dir.path().join("a.pile"),
        key: dir.path().join("a.key"),
    };
    let b = Bootstrap {
        pile: dir.path().join("b.pile"),
        key: dir.path().join("b.key"),
    };
    let old = init::prepare(&a).unwrap();
    let new = init::prepare(&b).unwrap();
    let mut source = NodeStore::open(&a.pile, old.clone()).unwrap();
    let mut target = NodeStore::open(&b.pile, new.clone()).unwrap();
    source
        .append_metrics(&[crate::node_metrics::Sample::now(
            None,
            "native",
            "session",
            genid().id,
            std::time::Duration::ZERO,
        )
        .unwrap()])
        .unwrap();
    source.pile.flush().unwrap();
    target.pile.flush().unwrap();
    let before_a = std::fs::read(&a.pile).unwrap();
    let before_b = std::fs::read(&b.pile).unwrap();
    let binding = NodeBinding {
        pile: b.pile.clone(),
        key: b.key.clone(),
        signer: new.verifying_key().to_bytes(),
    };
    let source_snapshot = source.pile.snapshot().unwrap();
    let target_snapshot = target.pile.snapshot().unwrap();
    assert!(
        MigrationPlan::prepare(
            &source_snapshot,
            CollectionHandle::new([99; 32]),
            None,
            &target_snapshot,
            &binding,
            &[],
            &NodeSettings::default()
        )
        .is_err()
    );
    let plan = MigrationPlan::prepare(
        &source_snapshot,
        source.config.handle(),
        Some(source.metrics.handle()),
        &target_snapshot,
        &binding,
        &[],
        &NodeSettings::default(),
    )
    .unwrap();
    drop(plan);
    assert_eq!(std::fs::read(&a.pile).unwrap(), before_a);
    assert_eq!(std::fs::read(&b.pile).unwrap(), before_b);
    let stale = MigrationPlan::prepare(
        &source_snapshot,
        source.config.handle(),
        None,
        &target_snapshot,
        &binding,
        &[],
        &NodeSettings::default(),
    )
    .unwrap();
    let expected = target.config().unwrap();
    target
        .put_settings(&expected, &NodeSettings::default())
        .unwrap();
    let before_failed_apply = std::fs::read(&b.pile).unwrap();
    let newly_observed = target.config().unwrap();
    assert!(
        stale.apply(&mut target, &newly_observed).is_err(),
        "a new apply-time observation must not bypass the prepare-time guard"
    );
    assert_eq!(std::fs::read(&b.pile).unwrap(), before_failed_apply);
    assert_eq!(std::fs::read(&a.pile).unwrap(), before_a);
    let target_snapshot = target.pile.snapshot().unwrap();
    let plan = MigrationPlan::prepare(
        &source_snapshot,
        source.config.handle(),
        Some(source.metrics.handle()),
        &target_snapshot,
        &binding,
        &[],
        &NodeSettings::default(),
    )
    .unwrap();
    let expected = target.config().unwrap();
    plan.apply(&mut target, &expected).unwrap();
    let reports = target
        .metrics(
            crate::node_metrics::now_tai_ns().unwrap(),
            std::time::Duration::from_secs(60),
        )
        .unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].collectors, vec![old.verifying_key().to_bytes()]);
    source.close().unwrap();
    target.close().unwrap();
}
