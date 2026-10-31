use super::*;
use crate::node_store::NodeStore;
use faculties::out::Part;
use std::fs;
use std::sync::{Arc, Mutex};
use triblespace::core::collection::{AdmissionPolicy, CollectionPolicy, CollectionStoreExt};
use triblespace::core::{id::genid, signing_key_file};
use triblespace::prelude::*;

struct Fixture {
    dispatch: WorkspaceDispatch,
    node: Arc<Mutex<Option<NodeStore>>>,
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let node_path = dir.path().join("node.pile");
        fs::File::create(&node_path).unwrap();
        let key = dir.path().join("node.key");
        let signer = signing_key_file::init(&key).unwrap();
        let binding = NodeBinding {
            pile: node_path.clone(),
            key,
            signer: signer.verifying_key().to_bytes(),
        };
        let node = Arc::new(Mutex::new(Some(
            NodeStore::open(&node_path, signer).unwrap(),
        )));
        let reader = node.clone();
        let dispatch = WorkspaceDispatch::new(binding, move || {
            reader.lock().unwrap().as_mut().unwrap().config()
        })
        .unwrap();
        Self {
            dispatch,
            node,
            dir,
        }
    }

    fn configure(&self, workspace: &mut Workspace) {
        let mut guard = self.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let view = node.config().unwrap();
        workspace.id = node.put_workspace(&view, workspace).unwrap();
    }

    fn workspace(&self, label: &str) -> Workspace {
        let mut guard = self.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let view = node.config().unwrap();
        let revision = node.create_workspace(&view, label).unwrap();
        node.config().unwrap().workspace(revision).unwrap().unwrap()
    }

    fn call(&self, name: &str, arguments: Value) -> Result<String> {
        self.raw(name, &serde_json::to_string(&arguments).unwrap())
    }

    fn annotate(&self, fragment: Fragment) {
        let mut guard = self.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let signer = node.signer.clone();
        let config = faculties::collection_names::open(
            &mut node.pile,
            faculties::schemas::config::DEFAULT_SCOPE_ID,
            signer.verifying_key(),
        )
        .unwrap();
        node.pile.commit(config, &signer, fragment).unwrap();
    }

    fn raw(&self, name: &str, arguments: &str) -> Result<String> {
        let mut text = String::new();
        self.dispatch.call(
            name,
            Bytes::from(arguments.as_bytes().to_vec()),
            &mut Out::new(&mut |part| {
                if let Part::Text { text: value } = part {
                    text.push_str(&value);
                }
                Ok(())
            }),
        )?;
        Ok(text)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.dispatch.close().unwrap();
        if let Some(node) = self.node.lock().unwrap().take() {
            node.close().unwrap();
        }
    }
}

fn selection(workspace: &Workspace) -> Value {
    json!({"workspace": format!("{:X}", workspace.id)})
}

fn local_entry_args(workspace: &Workspace) -> Value {
    json!({"workspace": format!("{:X}", workspace.id), "service": "Synthetic service", "name": "fixture login"})
}

fn take_local_prompt(bridge: &SecretEntry) -> crate::secret_entry::SecretPrompt {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(prompt) = bridge.take_prompt() {
            return prompt;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "local prompt was not delivered"
        );
        std::thread::yield_now();
    }
}

fn take_protected_prompt(bridge: &ProtectedAccessBridge) -> crate::protected_access::ProtectedPrompt {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(prompt) = bridge.take_prompt() {
            return prompt;
        }
        assert!(std::time::Instant::now() < deadline, "protected prompt was not delivered");
        std::thread::yield_now();
    }
}

fn take_import_prompt(bridge: &LocalImport) -> crate::local_import::ImportPrompt {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(prompt) = bridge.take_prompt() { return prompt; }
        assert!(std::time::Instant::now() < deadline, "import prompt was not delivered");
        std::thread::yield_now();
    }
}

fn synthetic_import_file(path: &std::path::Path) {
    use std::io::Write;
    let mut archive = zip::ZipWriter::new(fs::File::create(path).unwrap());
    for (name, bytes) in [
        ("export.attributes", br#"{"version":3}"#.as_slice()),
        ("export.data", br#"{"accounts":[{"vaults":[{"items":[{"uuid":"synthetic-id","overview":{"title":"SYNTHETIC-PRIVATE-title"},"details":{"notesPlain":"SYNTHETIC-PRIVATE-value"}}]}]}]}"#.as_slice()),
    ] {
        archive.start_file(name, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
        archive.write_all(bytes).unwrap();
    }
    archive.finish().unwrap();
}

#[test]
fn local_import_arguments_cancel_and_unavailable_never_save() {
    let mut f = Fixture::new();
    let workspace = f.workspace("Synthetic import");
    let bridge = LocalImport::default();
    f.dispatch.local_import = Some(bridge.clone());
    let schema: Value = serde_json::from_str(f.dispatch.input_schema(&IMPORT_LOCAL)).unwrap();
    assert_eq!(schema["required"], json!(["workspace"]));
    assert_eq!(schema["properties"].as_object().unwrap().len(), 2);
    let before = fs::read(&f.dispatch.node.pile).unwrap();
    for key in ["password", "passphrase", "data", "bytes", "path", "protected", "kind", "SYNTHETIC-PRIVATE-key"] {
        let mut args = selection(&workspace);
        args[key] = json!("SYNTHETIC-PRIVATE-value");
        let error = f.call(IMPORT_LOCAL.name, args).unwrap_err().to_string();
        assert!(!error.contains("SYNTHETIC-PRIVATE"));
        assert!(bridge.take_prompt().is_none());
    }
    for args in [json!({}), json!({"workspace":"wrong-kind"}), json!({"workspace":format!("{:X}", workspace.id),"previous_import":42}), json!({"workspace":format!("{:X}", workspace.id),"previous_import":null})] {
        assert!(f.call(IMPORT_LOCAL.name, args).is_err());
        assert!(bridge.take_prompt().is_none());
    }
    let duplicate = format!(r#"{{"workspace":"{:X}","SYNTHETIC-PRIVATE-key":1,"SYNTHETIC-PRIVATE-key":2}}"#, workspace.id);
    assert!(!f.raw(IMPORT_LOCAL.name, &duplicate).unwrap_err().to_string().contains("SYNTHETIC-PRIVATE"));
    let worker_bridge = bridge.clone();
    let worker = std::thread::spawn(move || {
        let prompt = take_import_prompt(&worker_bridge);
        assert!(prompt.request.previous_import.is_none());
        prompt.answer(ImportAnswer::Cancelled);
    });
    assert_eq!(f.call(IMPORT_LOCAL.name, selection(&workspace)).unwrap(), r#"{"status":"cancelled"}"#);
    worker.join().unwrap();
    bridge.close();
    assert_eq!(f.call(IMPORT_LOCAL.name, selection(&workspace)).unwrap(), r#"{"status":"unavailable"}"#);
    assert_eq!(fs::read(&f.dispatch.node.pile).unwrap(), before);
}

#[test]
fn local_import_frozen_binding_and_metadata_only_success() {
    use crate::local_import::ImportSource;
    use zeroize::Zeroizing;
    let mut f = Fixture::new();
    let a = f.workspace("Same label");
    let b = f.workspace("Same label");
    let path = f.dir.path().join("SYNTHETIC-PRIVATE-filename.1pux");
    synthetic_import_file(&path);
    let source = ImportSource::open(&path).unwrap();
    let bridge = LocalImport::default();
    f.dispatch.local_import = Some(bridge.clone());
    let node = f.node.clone();
    let original = a.clone();
    let other = b.clone();
    let worker = std::thread::spawn(move || {
        let prompt = take_import_prompt(&bridge);
        assert_eq!(prompt.request.workspace, format!("{:X}", original.id));
        assert!(prompt.request.previous_import.is_none());
        // This also proves no store lease is held while UI is active.
        let mut guard = node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let view = node.config().unwrap();
        let mut changed = original;
        changed.routes = other.routes;
        node.put_workspace(&view, &changed).unwrap();
        prompt.answer(ImportAnswer::Submit { source, passphrase: Zeroizing::new("SYNTHETIC-PRIVATE-passphrase".into()) });
    });
    let result = f.call(IMPORT_LOCAL.name, selection(&a)).unwrap();
    worker.join().unwrap();
    assert!(!result.contains("SYNTHETIC-PRIVATE"));
    assert!(!result.contains("source"));
    assert!(!result.contains("archive\""));
    let result: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["status"], "saved");
    assert_eq!(result["protected"], true);
    assert_eq!(result["reused"], false);
    assert_eq!(result["counts"]["items"], 1);
    let receipt = Id::from_hex(result["receipt"].as_str().unwrap()).unwrap();
    let storage = f.dispatch.owners.borrow()[&a.id].storage.clone();
    let secrets = faculties::secrets::Secrets::with_storage(storage);
    assert!(secrets.get(receipt).is_err());
    let items = secrets.list_items().unwrap();
    assert_eq!(items.len(), 1);
    assert!(!format!("{items:?}").contains("SYNTHETIC-PRIVATE"));
    assert!(secrets.open_item_body(items[0].id, items[0].body).is_err());
    assert!(f.call("secrets_items", selection(&b)).unwrap().contains("(no structured items)"));
    let pile = fs::read(&f.dispatch.node.pile).unwrap();
    assert!(!pile.windows(b"SYNTHETIC-PRIVATE".len()).any(|part| part == b"SYNTHETIC-PRIVATE"));
}

#[test]
fn local_import_invalid_source_is_not_saved_and_does_not_echo_content() {
    use crate::local_import::ImportSource;
    use zeroize::Zeroizing;
    let mut f = Fixture::new();
    let workspace = f.workspace("Synthetic invalid import");
    let path = f.dir.path().join("synthetic.1pux");
    fs::write(&path, b"SYNTHETIC-PRIVATE-invalid-archive").unwrap();
    let source = ImportSource::open(&path).unwrap();
    fs::write(&path, b"SYNTHETIC-PRIVATE-changed-source").unwrap();
    let bridge = LocalImport::default();
    f.dispatch.local_import = Some(bridge.clone());
    let before = fs::read(&f.dispatch.node.pile).unwrap();
    let worker = std::thread::spawn(move || {
        take_import_prompt(&bridge).answer(ImportAnswer::Submit { source, passphrase: Zeroizing::new("synthetic".into()) });
    });
    let result = f.call(IMPORT_LOCAL.name, selection(&workspace)).unwrap();
    worker.join().unwrap();
    assert!(!result.contains("SYNTHETIC-PRIVATE"));
    assert_eq!(serde_json::from_str::<Value>(&result).unwrap()["status"], "not_saved");
    assert_eq!(fs::read(&f.dispatch.node.pile).unwrap(), before);
}

#[test]
fn local_import_explicit_receipt_verifies_without_replay_or_downgrade() {
    use crate::local_import::ImportSource;
    use zeroize::Zeroizing;
    let mut f = Fixture::new();
    let workspace = f.workspace("Synthetic receipt verification");
    let path = f.dir.path().join("synthetic.1pux");
    synthetic_import_file(&path);
    let mut receipt: Option<String> = None;
    for (password, expected) in [("synthetic passphrase", "saved"), ("wrong synthetic passphrase", "not_saved"), ("synthetic passphrase", "saved")] {
        let source = ImportSource::open(&path).unwrap();
        let bridge = LocalImport::default();
        f.dispatch.local_import = Some(bridge.clone());
        let previous_import = receipt.clone();
        let worker = std::thread::spawn(move || {
            let prompt = take_import_prompt(&bridge);
            assert_eq!(prompt.request.previous_import, previous_import);
            prompt.answer(ImportAnswer::Submit { source, passphrase: Zeroizing::new(password.into()) });
        });
        let mut arguments = selection(&workspace);
        if let Some(receipt) = &receipt { arguments["previous_import"] = json!(receipt); }
        let before = fs::read(&f.dispatch.node.pile).unwrap();
        let result = f.call(IMPORT_LOCAL.name, arguments).unwrap();
        worker.join().unwrap();
        assert!(!result.contains(password));
        let result: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(result["status"], expected);
        if receipt.is_some() {
            assert_eq!(fs::read(&f.dispatch.node.pile).unwrap(), before);
            if expected == "saved" {
                assert_eq!(result["reused"], true);
                assert_eq!(result["receipt"], receipt.as_ref().unwrap().as_str());
            }
        } else {
            receipt = Some(result["receipt"].as_str().unwrap().to_owned());
        }
    }
}

#[test]
fn local_import_missing_route_never_opens_a_prompt_or_node_key() {
    let f = Fixture::new();
    let mut workspace = f.workspace("No import route");
    workspace.routes.remove(&faculties::secrets::DEFAULT_SCOPE_ID);
    f.configure(&mut workspace);
    let view = f.node.lock().unwrap().as_mut().unwrap().config().unwrap();
    let absent = f.dir.path().join("absent");
    let bridge = LocalImport::default();
    let dispatch = WorkspaceDispatch::new(NodeBinding {
        pile: absent.join("node.pile"), key: absent.join("node.key"), signer: f.dispatch.node.signer,
    }, move || Ok(view.clone())).unwrap().with_local_import(bridge.clone());
    let response = rpc_call(&mut session(&dispatch), IMPORT_LOCAL.name, selection(&workspace));
    assert!(response.to_string().contains("secrets is unconfigured"));
    assert!(bridge.take_prompt().is_none());
    assert!(!absent.exists());
}

#[test]
fn local_import_rejections_report_only_static_classification_and_positive_errno() {
    use faculties::secrets::onepassword::{ProtectedImportCode as Code, ProtectedImportRejection, ProtectedImportStage as Stage};
    for (stage, code, stage_name, code_name) in [
        (Stage::Parse, Code::ArchiveByteLimit, "parse", "archive_byte_limit"),
        (Stage::Parse, Code::ArchiveEntryLimit, "parse", "archive_entry_limit"),
        (Stage::Parse, Code::MemberByteLimit, "parse", "member_byte_limit"),
        (Stage::Parse, Code::ExpandedByteLimit, "parse", "expanded_byte_limit"),
        (Stage::Parse, Code::ItemCountLimit, "parse", "item_count_limit"),
        (Stage::Parse, Code::ProjectedByteLimit, "parse", "projected_byte_limit"),
        (Stage::Parse, Code::InvalidFormat, "parse", "invalid_format"),
        (Stage::Parse, Code::ZipDirectory, "parse", "zip_directory"),
        (Stage::Parse, Code::ZipArchive, "parse", "zip_archive"),
        (Stage::Parse, Code::ZipLayout, "parse", "zip_layout"),
        (Stage::Parse, Code::ZipMember, "parse", "zip_member"),
        (Stage::Parse, Code::InvalidMemberPath, "parse", "invalid_member_path"),
        (Stage::Parse, Code::UnsafeMember, "parse", "unsafe_member"),
        (Stage::Parse, Code::ZipIntegrity, "parse", "zip_integrity"),
        (Stage::Parse, Code::MemberSizeMismatch, "parse", "member_size_mismatch"),
        (Stage::Parse, Code::MissingAttributes, "parse", "missing_attributes"),
        (Stage::Parse, Code::UnsupportedVersion, "parse", "unsupported_version"),
        (Stage::Parse, Code::MissingData, "parse", "missing_data"),
        (Stage::Parse, Code::MalformedJson, "parse", "malformed_json"),
        (Stage::Parse, Code::InvalidStructure, "parse", "invalid_structure"),
        (Stage::DestinationObservation, Code::DestinationUnavailable, "destination_observation", "destination_unavailable"),
        (Stage::Encryption, Code::EncryptionFailed, "encryption", "encryption_failed"),
        (Stage::ReceiptVerification, Code::ReceiptRejected, "receipt_verification", "receipt_rejected"),
        (Stage::Preparation, Code::PreparationFailed, "preparation", "preparation_failed"),
    ] {
        for errno in [None, Some(0), Some(-1), Some(libc::EACCES)] {
            let error = anyhow::Error::new(ProtectedImportRejection { stage, code, os_error_code: errno })
                .context("SYNTHETIC-PRIVATE path/name/value/passphrase must never cross the result boundary");
            let result = local_import_failure(&error);
            let mut expected = json!({"status":"not_saved", "stage":stage_name, "code":code_name});
            if let Some(errno) = errno.filter(|code| *code > 0) { expected["os_error_code"] = json!(errno); }
            assert_eq!(result, expected);
            assert!(!result.to_string().contains("SYNTHETIC-PRIVATE"));
            assert!(result.get("receipt").is_none());
        }
    }
}

#[test]
fn local_import_publication_unknown_keeps_receipt_and_never_becomes_rejection() {
    use faculties::secrets::onepassword::ProtectedImportPublicationError;
    let receipt = genid().id;
    let error = anyhow::Error::new(ProtectedImportPublicationError { receipt })
        .context("SYNTHETIC-PRIVATE publication error details");
    let result = local_import_failure(&error);
    assert_eq!(result["status"], "outcome_unknown");
    assert_eq!(result["code"], "publication_uncertain");
    assert_eq!(result["receipt"], format!("{receipt:X}"));
    assert!(result.get("stage").is_none());
    assert!(result.get("os_error_code").is_none());
    assert!(!result.to_string().contains("SYNTHETIC-PRIVATE"));
    let unknown = local_import_failure(&anyhow::anyhow!("SYNTHETIC-PRIVATE unclassified error"));
    assert_eq!(unknown["status"], "not_saved");
    assert_eq!(unknown["code"], "import_rejected");
    assert!(!unknown.to_string().contains("SYNTHETIC-PRIVATE"));
}

#[test]
fn local_import_malformed_synthetic_archive_reports_parse_without_publication_or_echo() {
    use crate::local_import::ImportSource;
    use zeroize::Zeroizing;
    let mut f = Fixture::new();
    let workspace = f.workspace("Synthetic malformed import");
    let path = f.dir.path().join("SYNTHETIC-PRIVATE-file.1pux");
    fs::write(&path, b"SYNTHETIC-PRIVATE-malformed-export").unwrap();
    let source = ImportSource::open(&path).unwrap();
    let bridge = LocalImport::default();
    f.dispatch.local_import = Some(bridge.clone());
    let before = fs::read(&f.dispatch.node.pile).unwrap();
    let worker = std::thread::spawn(move || {
        take_import_prompt(&bridge).answer(ImportAnswer::Submit {
            source, passphrase: Zeroizing::new("SYNTHETIC-PRIVATE-passphrase".into()),
        });
    });
    let result = f.call(IMPORT_LOCAL.name, selection(&workspace)).unwrap();
    worker.join().unwrap();
    assert_eq!(serde_json::from_str::<Value>(&result).unwrap(), json!({"status":"not_saved", "stage":"parse", "code":"zip_directory"}));
    assert!(!result.contains("SYNTHETIC-PRIVATE"));
    assert_eq!(fs::read(&f.dispatch.node.pile).unwrap(), before);
}

#[test]
fn protected_local_secret_prompts_for_exact_workspace_each_time_and_cancel_emits_nothing() {
    use crate::secret_entry::EntryValue;
    use faculties::secrets::protected::Purpose;
    use zeroize::Zeroizing;
    let mut f = Fixture::new();
    let workspace = f.workspace("Protected fixture");
    let other = f.workspace("Other fixture");
    let unlock = ProtectedAccessBridge::default();
    f.dispatch.protected_access = Some(unlock.clone());
    let entry = SecretEntry::default();
    f.dispatch.secret_entry = Some(entry.clone());
    let worker = std::thread::spawn(move || {
        let prompt = take_local_prompt(&entry);
        assert!(prompt.request.protected);
        prompt.answer(EntryAnswer::Submit(
            EntryValue::secret(Zeroizing::new("synthetic protected bytes".into()))
                .with_protection(Zeroizing::new("synthetic protection passphrase".into())),
        ));
    });
    let mut args = local_entry_args(&workspace);
    args["kind"] = json!("secret");
    args["protected"] = json!(true);
    let saved: Value = serde_json::from_str(&f.call(ENTER_LOCAL.name, args).unwrap()).unwrap();
    worker.join().unwrap();
    assert_eq!(saved["status"], "saved");
    assert_eq!(saved.as_object().unwrap().len(), 2);
    let secret = Id::from_hex(saved["secret"].as_str().unwrap()).unwrap();
    assert!(f.call("secrets_list", selection(&workspace)).unwrap().contains("fixture login"));
    assert!(unlock.take_prompt().is_none(), "metadata must not unlock");
    let storage = f.dispatch.owners.borrow()[&workspace.id].storage.clone();
    let secrets = faculties::secrets::Secrets::with_storage(storage);
    let ordinary = secrets.add("ordinary", b"ordinary fixture").unwrap();
    assert_eq!(secrets.get(ordinary).unwrap().as_slice(), b"ordinary fixture");
    assert!(unlock.take_prompt().is_none(), "ordinary values must not prompt");
    let mut get = selection(&other);
    get["secret"] = json!(format!("{secret:X}"));
    assert!(f.call("secrets_get", get).is_err());
    assert!(unlock.take_prompt().is_none(), "another collection cannot request this secret");
    let mut get = selection(&workspace);
    get["secret"] = json!(format!("{secret:X}"));
    for approve in [true, false, true] {
        let bridge = unlock.clone();
        let expected = workspace.clone();
        let worker = std::thread::spawn(move || {
            let prompt = take_protected_prompt(&bridge);
            assert_eq!(prompt.workspace, format!("{:X}", expected.id));
            assert_eq!(prompt.workspace_label, expected.label);
            assert_eq!(prompt.request.secret(), secret);
            assert_eq!(prompt.request.collection(), expected.routes[&faculties::secrets::DEFAULT_SCOPE_ID]);
            assert!(matches!(prompt.request.purpose(), Purpose::Get));
            if approve {
                let unlocked = prompt.request.unlock_password(b"synthetic protection passphrase");
                prompt.answer(unlocked);
            } else {
                drop(prompt);
            }
        });
        let mut parts = Vec::new();
        let result = f.dispatch.call(
            "secrets_get",
            Bytes::from(serde_json::to_vec(&get).unwrap()),
            &mut Out::new(&mut |part| { parts.push(part); Ok(()) }),
        );
        worker.join().unwrap();
        if approve {
            result.unwrap();
            assert_eq!(parts.len(), 1);
            let Part::Blob { bytes, .. } = &parts[0] else { panic!("expected exact binary export") };
            assert_eq!(bytes.as_ref(), b"synthetic protected bytes");
        } else {
            assert!(result.is_err());
            assert!(parts.is_empty());
        }
        assert!(unlock.take_prompt().is_none());
    }
    let pile = fs::read(&f.dispatch.node.pile).unwrap();
    for plaintext in ["synthetic protected bytes", "synthetic protection passphrase"] {
        assert!(!pile.windows(plaintext.len()).any(|window| window == plaintext.as_bytes()));
    }
}

#[test]
fn protected_local_login_stays_structured_and_requires_unlock() {
    use crate::secret_entry::EntryValue;
    use faculties::secrets::protected::Purpose;
    use zeroize::Zeroizing;
    let mut f = Fixture::new();
    let workspace = f.workspace("Protected login fixture");
    let unlock = ProtectedAccessBridge::default();
    f.dispatch.protected_access = Some(unlock.clone());
    let entry = SecretEntry::default();
    f.dispatch.secret_entry = Some(entry.clone());
    let worker = std::thread::spawn(move || {
        let prompt = take_local_prompt(&entry);
        assert!(prompt.request.protected);
        assert_eq!(prompt.request.kind, EntryKind::Login);
        prompt.answer(EntryAnswer::Submit(
            EntryValue::login(
                Zeroizing::new("synthetic@example.invalid".into()),
                Zeroizing::new("synthetic login password".into()),
            ).with_protection(Zeroizing::new("synthetic protection passphrase".into())),
        ));
    });
    let mut args = local_entry_args(&workspace);
    args["protected"] = json!(true);
    let saved: Value = serde_json::from_str(&f.call(ENTER_LOCAL.name, args).unwrap()).unwrap();
    worker.join().unwrap();
    assert_eq!(saved["status"], "saved");
    let secret = Id::from_hex(saved["secret"].as_str().unwrap()).unwrap();
    let storage = f.dispatch.owners.borrow()[&workspace.id].storage.clone();
    let secrets = faculties::secrets::Secrets::with_storage(storage);
    let items = secrets.list_items().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, secret);
    assert!(secrets.list().unwrap().is_empty());
    assert!(unlock.take_prompt().is_none());
    let worker = std::thread::spawn(move || {
        let prompt = take_protected_prompt(&unlock);
        assert_eq!(prompt.request.secret(), secret);
        assert!(matches!(prompt.request.purpose(), Purpose::OpenFacts));
        let unlocked = prompt.request.unlock_password(b"synthetic protection passphrase");
        prompt.answer(unlocked);
    });
    secrets.open_item_body(secret, items[0].body).unwrap();
    worker.join().unwrap();
}

#[test]
fn local_entry_missing_route_rejects_before_key_or_pile_io() {
    let f = Fixture::new();
    let mut workspace = f.workspace("No secrets");
    workspace
        .routes
        .remove(&faculties::secrets::DEFAULT_SCOPE_ID);
    f.configure(&mut workspace);
    let view = f.node.lock().unwrap().as_mut().unwrap().config().unwrap();
    let absent = f.dir.path().join("absent");
    let bridge = SecretEntry::default();
    let dispatch = WorkspaceDispatch::new(
        NodeBinding {
            pile: absent.join("node.pile"),
            key: absent.join("node.key"),
            signer: f.dispatch.node.signer,
        },
        move || Ok(view.clone()),
    )
    .unwrap()
    .with_secret_entry(bridge.clone());
    let error = rpc_call(
        &mut session(&dispatch),
        ENTER_LOCAL.name,
        local_entry_args(&workspace),
    );
    assert!(error.to_string().contains("secrets is unconfigured"));
    assert!(dispatch.owners.borrow().is_empty());
    assert!(bridge.take_prompt().is_none());
    assert!(!absent.exists());
}

#[test]
fn local_entry_cancel_publishes_nothing_and_never_accepts_secret_arguments() {
    let mut f = Fixture::new();
    let workspace = f.workspace("Cancel fixture");
    let bridge = SecretEntry::default();
    f.dispatch.secret_entry = Some(bridge.clone());
    let before = fs::read(&f.dispatch.node.pile).unwrap();
    let worker = std::thread::spawn(move || {
        let prompt = take_local_prompt(&bridge);
        assert_eq!(prompt.request.kind, EntryKind::Login);
        assert!(!prompt.request.protected);
        prompt.answer(EntryAnswer::Cancelled);
    });
    let response = f
        .call(ENTER_LOCAL.name, local_entry_args(&workspace))
        .unwrap();
    worker.join().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&response).unwrap(),
        json!({"status":"cancelled"})
    );
    assert_eq!(fs::read(&f.dispatch.node.pile).unwrap(), before);
    let mut arguments = local_entry_args(&workspace);
    arguments["value"] = json!("invented input that must be rejected");
    assert!(f.call(ENTER_LOCAL.name, arguments).is_err());
    let mut arguments = local_entry_args(&workspace);
    arguments["passphrase"] = json!("never a protocol argument");
    assert!(f.call(ENTER_LOCAL.name, arguments).is_err());
    assert!(
        f.call("secrets_list", selection(&workspace))
            .unwrap()
            .contains("(no secrets)")
    );
    assert!(
        f.call("secrets_items", selection(&workspace))
            .unwrap()
            .contains("(no structured items)")
    );
}

#[test]
fn local_entry_protection_schema_is_optional_and_mismatch_never_downgrades() {
    use crate::secret_entry::EntryValue;
    use zeroize::Zeroizing;
    let mut f = Fixture::new();
    let workspace = f.workspace("Protection mismatch fixture");
    let schema: Value = serde_json::from_str(f.dispatch.input_schema(&ENTER_LOCAL)).unwrap();
    assert_eq!(schema["properties"]["protected"]["default"], false);
    assert!(!schema["required"].as_array().unwrap().contains(&json!("protected")));
    for protected in [true, false] {
        let bridge = SecretEntry::default();
        f.dispatch.secret_entry = Some(bridge.clone());
        let before = fs::read(&f.dispatch.node.pile).unwrap();
        let worker = std::thread::spawn(move || {
            let prompt = take_local_prompt(&bridge);
            assert_eq!(prompt.request.protected, protected);
            let value = EntryValue::secret(Zeroizing::new("synthetic value".into()));
            let value = if protected {
                value
            } else {
                value.with_protection(Zeroizing::new("synthetic passphrase".into()))
            };
            prompt.answer(EntryAnswer::Submit(value));
        });
        let mut args = local_entry_args(&workspace);
        args["kind"] = json!("secret");
        args["protected"] = json!(protected);
        let error = f.call(ENTER_LOCAL.name, args).unwrap_err();
        worker.join().unwrap();
        assert_eq!(error.to_string(), "local entry protection mismatch; no secret was saved");
        assert_eq!(fs::read(&f.dispatch.node.pile).unwrap(), before);
    }
}

#[test]
fn local_entry_empty_save_returns_fixed_error_and_publishes_nothing() {
    use crate::secret_entry::EntryValue;
    use zeroize::Zeroizing;
    let mut f = Fixture::new();
    let workspace = f.workspace("Empty Save fixture");
    let login_error = "Local login entry requires both username and password. No secret was saved. Request a new local entry and fill both fields.";
    let secret_error = "Local secret entry requires a nonempty value. No secret was saved. Request a new local entry.";
    for (kind, username, value, expected) in [
        (EntryKind::Login, "", "", login_error),
        (
            EntryKind::Login,
            "invented@example.invalid",
            "",
            login_error,
        ),
        (EntryKind::Login, "", "invented-password", login_error),
        (EntryKind::Secret, "", "", secret_error),
    ] {
        let bridge = SecretEntry::default();
        f.dispatch.secret_entry = Some(bridge.clone());
        let before = fs::read(&f.dispatch.node.pile).unwrap();
        let worker = std::thread::spawn(move || {
            let prompt = take_local_prompt(&bridge);
            assert_eq!(prompt.request.kind, kind);
            prompt.answer(EntryAnswer::Submit(match kind {
                EntryKind::Login => EntryValue::login(
                    Zeroizing::new(username.into()),
                    Zeroizing::new(value.into()),
                ),
                EntryKind::Secret => EntryValue::secret(Zeroizing::new(value.into())),
            }));
        });
        let mut args = local_entry_args(&workspace);
        if kind == EntryKind::Secret {
            args["kind"] = json!("secret");
        }
        let error = f.call(ENTER_LOCAL.name, args).unwrap_err();
        worker.join().unwrap();
        assert_eq!(error.to_string(), expected);
        assert_eq!(fs::read(&f.dispatch.node.pile).unwrap(), before);
    }
    assert!(
        f.call("secrets_list", selection(&workspace))
            .unwrap()
            .contains("(no secrets)")
    );
    assert!(
        f.call("secrets_items", selection(&workspace))
            .unwrap()
            .contains("(no structured items)")
    );
}

#[test]
fn local_entry_write_denied_route_never_requests_plaintext() {
    let mut f = Fixture::new();
    let mut workspace = f.workspace("Not writable");
    let other = signing_key_file::init(&f.dir.path().join("synthetic-other.key")).unwrap();
    {
        // Reuse the existing node owner, not another opener alongside it.
        let mut guard = f.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let root = node
            .pile
            .collection(
                "Synthetic denied Secrets",
                CollectionPolicy::new(
                    AdmissionPolicy::Open,
                    AdmissionPolicy::direct(other.verifying_key()),
                ),
            )
            .unwrap()
            .handle();
        workspace
            .routes
            .insert(faculties::secrets::DEFAULT_SCOPE_ID, root);
    }
    f.configure(&mut workspace);
    let bridge = SecretEntry::default();
    f.dispatch.secret_entry = Some(bridge.clone());
    let before = fs::read(&f.dispatch.node.pile).unwrap();
    let error = f
        .call(ENTER_LOCAL.name, local_entry_args(&workspace))
        .unwrap_err();
    assert!(error.to_string().contains("not admitted to WRITE"));
    assert!(bridge.take_prompt().is_none());
    assert_eq!(fs::read(&f.dispatch.node.pile).unwrap(), before);
}

#[test]
fn local_entry_keeps_the_original_workspace_if_configuration_changes_during_input() {
    use crate::secret_entry::EntryValue;
    use faculties::secrets::item::PrivateText;
    use faculties::secrets::schema::{
        KIND_ITEM, ROLE_PASSWORD, ROLE_USERNAME, field_role, field_text, item_field,
    };
    use triblespace::core::metadata;
    use zeroize::Zeroizing;
    let mut f = Fixture::new();
    let a = f.workspace("Same label");
    let b = f.workspace("Same label");
    let bridge = SecretEntry::default();
    f.dispatch.secret_entry = Some(bridge.clone());
    let node = f.node.clone();
    let original = a.clone();
    let other = b.clone();
    let worker = std::thread::spawn(move || {
        let prompt = take_local_prompt(&bridge);
        assert_eq!(prompt.request.workspace, format!("{:X}", original.id));
        assert_eq!(prompt.request.workspace_label, original.label);
        assert_eq!(prompt.request.service, "Synthetic service");
        assert_eq!(prompt.request.name, "fixture login");
        let mut node = node.lock().unwrap();
        let node = node.as_mut().unwrap();
        let view = node.config().unwrap();
        let mut changed = original;
        changed.routes = other.routes;
        node.put_workspace(&view, &changed).unwrap();
        drop(view);
        prompt.answer(EntryAnswer::Submit(EntryValue::login(
            Zeroizing::new("invented@example.invalid".into()),
            Zeroizing::new("invented-password".into()),
        )));
    });
    let response = f.call(ENTER_LOCAL.name, local_entry_args(&a)).unwrap();
    worker.join().unwrap();
    assert!(!response.contains("invented@example.invalid"));
    assert!(!response.contains("invented-password"));
    let response: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response.as_object().unwrap().len(), 2);
    assert_eq!(response["status"], "saved");
    let secret = Id::from_hex(response["secret"].as_str().unwrap()).unwrap();
    let storage = f.dispatch.owners.borrow()[&a.id].storage.clone();
    let secrets = faculties::secrets::Secrets::with_storage(storage);
    let items = secrets.list_items().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, secret);
    assert_eq!(items[0].name.as_deref(), Some("fixture login"));
    let body = items[0].body;
    let facts = secrets.open_item_body(secret, body).unwrap();
    for (role, expected) in [
        (ROLE_USERNAME, "invented@example.invalid"),
        (ROLE_PASSWORD, "invented-password"),
    ] {
        let children: Vec<_> = find!(child: PrivateText, pattern!(&facts, [
            {metadata::tag: KIND_ITEM, item_field: _?field},
            {_?field @ field_role: role, field_text: ?child},
        ]))
        .collect();
        assert_eq!(children.len(), 1);
        let opened = secrets.open_item_child(secret, body, children[0]).unwrap();
        assert_eq!(&*opened.bytes, expected.as_bytes());
        let pile = fs::read(&f.dispatch.node.pile).unwrap();
        assert!(
            !pile
                .windows(expected.len())
                .any(|window| window == expected.as_bytes())
        );
    }
    // Structured logins do not silently become raw bodies or legacy JSON.
    assert!(secrets.list().unwrap().is_empty());
    assert!(secrets.get(secret).is_err());
    // The old workspace request ID was superseded during entry, so inspect
    // the captured owner's metadata directly rather than reselecting it.
    let metadata = format!("{items:?}");
    assert!(!metadata.contains("invented@example.invalid"));
    assert!(!metadata.contains("invented-password"));
    assert!(
        !f.call("secrets_items", selection(&b))
            .unwrap()
            .contains(&format!("{secret:x}"))
    );
}

#[test]
fn local_entry_closed_bridge_is_unavailable_and_generic_success_returns_id_only() {
    use crate::secret_entry::EntryValue;
    use zeroize::Zeroizing;
    let mut f = Fixture::new();
    let workspace = f.workspace("Generic fixture");
    let bridge = SecretEntry::default();
    f.dispatch.secret_entry = Some(bridge.clone());
    let worker_bridge = bridge.clone();
    let worker = std::thread::spawn(move || {
        let prompt = take_local_prompt(&worker_bridge);
        assert_eq!(prompt.request.kind, EntryKind::Secret);
        prompt.answer(EntryAnswer::Submit(EntryValue::secret(Zeroizing::new(
            "  invented generic value\n{\"password\":\"not a login\"} 🗝\r\n".into(),
        ))));
    });
    let mut args = local_entry_args(&workspace);
    args["kind"] = json!("secret");
    let response = f.call(ENTER_LOCAL.name, args).unwrap();
    assert!(!response.contains("invented generic value"));
    assert!(!response.contains("not a login"));
    let response: Value = serde_json::from_str(&response).unwrap();
    worker.join().unwrap();
    assert_eq!(response.as_object().unwrap().len(), 2);
    assert_eq!(response["status"], "saved");
    let id = Id::from_hex(response["secret"].as_str().unwrap()).unwrap();
    let storage = f.dispatch.owners.borrow()[&workspace.id].storage.clone();
    let secrets = faculties::secrets::Secrets::with_storage(storage);
    assert_eq!(
        secrets.get(id).unwrap().as_slice(),
        "  invented generic value\n{\"password\":\"not a login\"} 🗝\r\n".as_bytes()
    );
    assert!(secrets.list_items().unwrap().is_empty());
    assert!(secrets.open_item(id).is_err());
    bridge.close();
    let response = f
        .call(ENTER_LOCAL.name, local_entry_args(&workspace))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&response).unwrap(),
        json!({"status":"unavailable"})
    );
    assert!(bridge.take_prompt().is_none());
}

#[test]
fn secure_entry_output_never_contains_plaintext() {
    // Capture actual stdout/stderr, including dependency logs, from isolated
    // synthetic-only success cases. This never invokes an app or live endpoint.
    for name in [
        "local_entry_keeps_the_original_workspace_if_configuration_changes_during_input",
        "local_entry_closed_bridge_is_unavailable_and_generic_success_returns_id_only",
    ] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child.args([
            "--exact",
            &format!("workspace_dispatch::tests::{name}"),
            "--nocapture",
        ]);
        for (key, _) in std::env::vars_os() {
            if key == "PILE"
                || key == "TRIBLESPACE_KEY"
                || key.to_string_lossy().starts_with("TRIBLESPACE_COLLECTION_")
            {
                child.env_remove(key);
            }
        }
        let output = child.output().unwrap();
        assert!(output.status.success(), "synthetic entry subprocess failed");
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        for stream in [&output.stdout, &output.stderr] {
            for private in [
                "invented@example.invalid",
                "invented-password",
                "invented generic value",
                "not a login",
            ] {
                assert!(
                    !stream
                        .windows(private.len())
                        .any(|window| window == private.as_bytes()),
                    "local entry logged plaintext"
                );
            }
        }
    }
}

#[test]
fn schema_registration_does_not_open_a_pile_key_or_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let pile = dir.path().join("absent.pile");
    let key = dir.path().join("absent.key");
    let dispatch = WorkspaceDispatch::new(
        NodeBinding {
            pile: pile.clone(),
            key: key.clone(),
            signer: [0; 32],
        },
        || panic!("schema registration must not request configuration"),
    )
    .unwrap();
    assert!(dispatch.tools().len() > 100);
    assert!(dispatch.owners.borrow().is_empty());
    dispatch.close().unwrap();
    assert!(!pile.exists());
    assert!(!key.exists());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn discord_auth_uses_only_the_explicit_workspace_secret_route() {
    let f = Fixture::new();
    let mut workspace = f.workspace("Bot fixture");
    let storage = Storage::shared_local(
        f.dispatch.node.pile.clone(),
        Some(f.dispatch.node.key.clone()),
    )
    .with_collections(workspace.routes.clone());
    let secret = faculties::secrets::Secrets::with_storage(storage.clone())
        .add("fixture bot", b"synthetic-workspace-bot-token")
        .unwrap();
    storage.close().unwrap();

    let mut args = selection(&workspace);
    args["secret_version"] = json!(format!("{secret:X}"));
    let bound = f.call("discord_auth_set", args.clone()).unwrap();
    assert!(bound.contains(&format!("{secret:X}")));
    assert!(!bound.contains("synthetic-workspace-bot-token"));
    let status = f
        .call("discord_auth_status", selection(&workspace))
        .unwrap();
    assert!(status.contains("settled"));
    assert!(status.contains(&format!("{secret:X}")));
    assert!(!status.contains("synthetic-workspace-bot-token"));

    // Omission is unavailable, not a fallback to a signer-private Secrets
    // collection containing the same version elsewhere in the physical pile.
    workspace
        .routes
        .remove(&faculties::secrets::DEFAULT_SCOPE_ID);
    f.configure(&mut workspace);
    args["workspace"] = json!(format!("{:X}", workspace.id));
    let error = f.call("discord_auth_set", args).unwrap_err();
    assert!(!format!("{error:#}").contains("synthetic-workspace-bot-token"));
    assert!(
        format!("{error:#}").contains("is unconfigured"),
        "{error:#}"
    );
    // Metadata-only status remains available without opening Secrets.
    assert!(
        f.call("discord_auth_status", selection(&workspace))
            .unwrap()
            .contains("settled")
    );
}

fn session(dispatch: &WorkspaceDispatch) -> mcp::Server<'_> {
    let mut server = mcp::Server::new(&[dispatch]).unwrap();
    let initialize = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":mcp::PROTOCOL_VERSION,"capabilities":{},
        "clientInfo":{"name":"workspace test","version":"1"},
    }});
    server
        .dispatch(Bytes::from(serde_json::to_vec(&initialize).unwrap()))
        .unwrap();
    server
        .dispatch(Bytes::from(
            br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.to_vec(),
        ))
        .unwrap();
    server
}

fn rpc_call(server: &mut mcp::Server<'_>, name: &str, args: Value) -> Value {
    let request = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
        "name":name,"arguments":args,
    }});
    serde_json::from_str(
        &server
            .dispatch(Bytes::from(serde_json::to_vec(&request).unwrap()))
            .unwrap()
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn every_schema_requires_workspace_except_passive_list_and_protocol_has_no_ambient_reads() {
    let f = Fixture::new();
    assert!(f.dispatch.tools().len() > 100);
    for tool in f.dispatch.tools() {
        let schema: Value = serde_json::from_str(f.dispatch.input_schema(tool)).unwrap();
        if tool.name == "list_workspaces" {
            assert!(schema["properties"].as_object().unwrap().is_empty());
        } else {
            assert!(
                schema["required"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("workspace")),
                "{}",
                tool.name
            );
            assert_eq!(schema["properties"]["workspace"]["type"], "string");
            assert!(
                !tool.input_schema.contains("\"workspace\""),
                "static adapter schema changed"
            );
        }
    }
    let mut server = mcp::Server::new(&[&f.dispatch]).unwrap();
    for method in [
        "resources/read",
        "resources/list",
        "prompts/get",
        "prompts/list",
    ] {
        let request = json!({"jsonrpc":"2.0","id":1,"method":method,"params":{}});
        let response: Value = serde_json::from_str(
            &server
                .dispatch(Bytes::from(serde_json::to_vec(&request).unwrap()))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["error"]["code"], -32601);
    }
}

#[test]
fn list_is_config_only_and_mutating_faculty_never_runs_without_an_accepted_workspace() {
    let f = Fixture::new();
    let mut configured = Workspace::new("Readable label");
    f.configure(&mut configured);
    let listed: Value =
        serde_json::from_str(&f.call("list_workspaces", json!({})).unwrap()).unwrap();
    assert_eq!(
        listed["workspaces"][0]["workspace"],
        format!("{:X}", configured.id)
    );
    assert_eq!(listed["workspaces"][0]["label"], "Readable label");
    assert!(listed["workspaces"][0].get("key").is_none());
    assert!(f.dispatch.owners.borrow().is_empty());
    let before = fs::read(f.dir.path().join("node.pile")).unwrap();
    for args in [
        json!({"title":"must not exist"}),
        json!({"workspace": format!("{:X}", genid().id), "title":"must not exist"}),
        json!({"workspace":"Readable label", "title":"must not exist"}),
        json!({"workspace":123, "title":"must not exist"}),
        json!({"workspace":null, "title":"must not exist"}),
        json!({"workspace":format!("{:X}", configured.id), "title":"must not exist"}),
    ] {
        assert!(f.call("compass_add", args).is_err());
    }
    assert_eq!(fs::read(f.dir.path().join("node.pile")).unwrap(), before);
    // A failed role lookup may retain its local-only adapter, never an endpoint.
}

#[test]
fn interleaved_calls_in_two_protocol_sessions_do_not_set_a_current_workspace() {
    let f = Fixture::new();
    let a = f.workspace("A");
    let b = f.workspace("B");
    let mut first = session(&f.dispatch);
    let mut second = session(&f.dispatch);
    for (server, workspace, title) in [
        (&mut first, &a, "Only A first"),
        (&mut second, &b, "Only B"),
    ] {
        let response = rpc_call(
            server,
            "compass_add",
            json!({"workspace":format!("{:X}",workspace.id),"title":title}),
        );
        assert!(response.get("error").is_none(), "{response}");
        assert_ne!(response["result"]["isError"], true, "{response}");
    }
    // Deliberately use B's previous session for A, and A's for B.
    let response = rpc_call(
        &mut second,
        "compass_add",
        json!({"workspace":format!("{:X}",a.id),"title":"Only A second"}),
    );
    assert_ne!(response["result"]["isError"], true, "{response}");
    let response = rpc_call(&mut first, "compass_list", selection(&b));
    assert!(response.to_string().contains("Only B"));
    assert!(!response.to_string().contains("Only A"));
    assert_eq!(
        rpc_call(&mut first, "compass_list", json!({}))["error"]["code"],
        -32602
    );
    let a_result = f.call("compass_list", selection(&a)).unwrap();
    let b_result = f.call("compass_list", selection(&b)).unwrap();
    assert!(a_result.contains("Only A first") && a_result.contains("Only A second"));
    assert!(!a_result.contains("Only B"));
    assert!(b_result.contains("Only B") && !b_result.contains("Only A"));
    assert!(f.call("compass_list", json!({})).is_err());
    assert_eq!(f.dispatch.owners.borrow().len(), 2);
    f.dispatch.close().unwrap();
    assert!(f.dispatch.owners.borrow().is_empty());
    // Closing flushes successful writes. Independent request owners can reopen.
    assert!(
        f.call("compass_list", selection(&a))
            .unwrap()
            .contains("Only A first")
    );
}

#[test]
fn same_name_roots_are_exact_and_old_binding_never_retargets() {
    let f = Fixture::new();
    let a = f.workspace("one pile");
    let signer = signing_key_file::load_existing(&f.dispatch.node.key).unwrap();
    let mut pile =
        faculties::storage::open_pile_strict_as(&f.dispatch.node.pile, signer.verifying_key())
            .unwrap();
    let alternate = pile
        .collection(
            "compass",
            CollectionPolicy::new(
                AdmissionPolicy::Open,
                AdmissionPolicy::direct(signer.verifying_key()),
            ),
        )
        .unwrap()
        .handle();
    pile.close().unwrap();
    let mut b = a.clone();
    b.anchor = genid().id;
    b.label = "one pile".into(); // Identical labels never identify a workspace.
    b.routes
        .insert(faculties::schemas::compass::DEFAULT_SCOPE_ID, alternate);
    let mut guard = f.node.lock().unwrap();
    let node = guard.as_mut().unwrap();
    let view = node.config().unwrap();
    b.id = node.put_workspace(&view, &b).unwrap();
    drop(guard);
    f.call(
        "compass_add",
        json!({"workspace":format!("{:X}", a.id), "title":"First exact root"}),
    )
    .unwrap();
    f.call(
        "compass_add",
        json!({"workspace":format!("{:X}", b.id), "title":"Second exact root"}),
    )
    .unwrap();
    assert!(
        !f.call("compass_list", selection(&a))
            .unwrap()
            .contains("Second exact root")
    );
    assert!(
        !f.call("compass_list", selection(&b))
            .unwrap()
            .contains("First exact root")
    );

    let mut replacement = a.clone();
    replacement.routes = b.routes.clone();
    let mut guard = f.node.lock().unwrap();
    let node = guard.as_mut().unwrap();
    let view = node.config().unwrap();
    replacement.id = node.put_workspace(&view, &replacement).unwrap();
    drop(guard);
    assert_ne!(replacement.id, a.id);
    assert!(
        f.call(
            "compass_add",
            json!({"workspace":format!("{:X}", a.id),"title":"Must not retarget"})
        )
        .is_err()
    );
    assert!(!f.dispatch.owners.borrow().contains_key(&a.id));
    let after = f.call("compass_list", selection(&replacement)).unwrap();
    assert!(after.contains("Second exact root") && !after.contains("Must not retarget"));
}

#[test]
fn request_cannot_inject_routing_and_duplicate_arguments_are_not_normalized() {
    let f = Fixture::new();
    let a = f.workspace("selected");
    let before = fs::read(&f.dispatch.node.pile).unwrap();
    for field in ["pile", "key", "routes", "root", "collections"] {
        let mut args = selection(&a);
        args["title"] = json!("Never written");
        args[field] = json!("/untrusted/target");
        assert!(f.call("compass_add", args).is_err(), "{field}");
    }
    assert_eq!(fs::read(&f.dispatch.node.pile).unwrap(), before);
    let id = format!("{:X}", a.id);
    for raw in [
        format!(r#"{{"workspace":"{id}","workspace":"{id}","title":"no"}}"#),
        format!(r#"{{"workspace":"{id}","title":"first","title":"second"}}"#),
    ] {
        assert!(f.raw("compass_add", &raw).is_err());
    }
    let nested =
        format!(r#"{{"workspace":"{id}","label":"no","profile":{{"note":"one","note":"two"}}}}"#);
    assert!(f.raw("relations_add", &nested).is_err());
    assert_eq!(fs::read(&f.dispatch.node.pile).unwrap(), before);
}

#[test]
fn missing_role_and_replaced_signer_fail_without_node_or_private_root_fallback() {
    let f = Fixture::new();
    let mut a = f.workspace("no role");
    a.routes
        .remove(&faculties::schemas::compass::DEFAULT_SCOPE_ID);
    let mut guard = f.node.lock().unwrap();
    let node = guard.as_mut().unwrap();
    let view = node.config().unwrap();
    a.id = node.put_workspace(&view, &a).unwrap();
    drop(guard);
    let error = f.call("compass_list", selection(&a)).unwrap_err();
    assert!(format!("{error:#}").contains("role"), "{error:#}");
    let before = fs::read(&f.dispatch.node.pile).unwrap();
    // Replace only the scratch key at the same selected path.
    fs::rename(
        &f.dispatch.node.key,
        f.dispatch.node.key.with_extension("old-key"),
    )
    .unwrap();
    signing_key_file::init(&f.dispatch.node.key).unwrap();
    let error = f
        .call(
            "compass_add",
            json!({"workspace":format!("{:X}",a.id),"title":"no"}),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("signing identity changed"));
    assert_eq!(fs::read(&f.dispatch.node.pile).unwrap(), before);
}

#[test]
fn schema_reserves_workspace_instead_of_silently_overwriting_faculty_arguments() {
    for schema in [
        r#"{"type":"array"}"#,
        r#"{"type":"object","properties":{"workspace":{"type":"integer"}}}"#,
        r#"{"type":"object","properties":{},"required":["workspace"]}"#,
        r#"{"type":"object","properties":{},"required":"name"}"#,
    ] {
        assert!(wrapped_schema(schema).is_err());
    }
    let schema: Value = serde_json::from_str(&wrapped_schema(
        r#"{"type":"object","properties":{"title":{"type":"string"}},"required":["title"],"additionalProperties":false}"#,
    ).unwrap()).unwrap();
    assert_eq!(schema["required"], json!(["title", "workspace"]));
    assert_eq!(schema["additionalProperties"], false);
    assert!(
        register_tool(
            &mut Vec::new(),
            &mut BTreeMap::new(),
            &LIST,
            LIST.input_schema
        )
        .is_err()
    );
    let tool = Tool {
        name: "native_tool",
        description: "test",
        input_schema: LIST.input_schema,
    };
    let mut tools = Vec::new();
    let mut schemas = BTreeMap::new();
    register_tool(&mut tools, &mut schemas, &tool, tool.input_schema).unwrap();
    assert!(register_tool(&mut tools, &mut schemas, &tool, tool.input_schema).is_err());
}

#[test]
fn same_id_annotation_replaces_warm_owner_and_matches_a_fresh_request() {
    use crate::node_schema as attrs;
    use faculties::schemas::{compass, config::config};
    let f = Fixture::new();
    let mut workspace = f.workspace("annotated");
    let root = workspace.routes.remove(&compass::DEFAULT_SCOPE_ID).unwrap();
    {
        let mut guard = f.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let view = node.config().unwrap();
        workspace.id = node.put_workspace(&view, &workspace).unwrap();
    }
    f.call(
        "relations_add",
        json!({
            "workspace":format!("{:X}", workspace.id),"label":"Retained before annotation",
        }),
    )
    .unwrap();
    assert!(f.call("compass_list", selection(&workspace)).is_err());
    let before = f.node.lock().unwrap().as_mut().unwrap().config().unwrap();
    let revision = workspace.id;
    f.annotate(
        entity! { ExclusiveId::force_ref(&revision) @ attrs::workspace_route*: entity! {
            config::anchor: compass::DEFAULT_SCOPE_ID, config::selects: root,
        } },
    );
    // An in-flight observation keeps the original binding; the later request
    // resolves the additive native facts without treating the ID as a hash.
    assert!(
        !before
            .workspace(revision)
            .unwrap()
            .unwrap()
            .routes
            .contains_key(&compass::DEFAULT_SCOPE_ID)
    );
    f.call(
        "compass_add",
        json!({
            "workspace":format!("{revision:X}"),"title":"Newly configured role",
        }),
    )
    .unwrap();
    assert_eq!(
        f.dispatch.owners.borrow()[&revision]
            .binding
            .routes
            .get(&compass::DEFAULT_SCOPE_ID),
        Some(&root)
    );
    assert!(
        f.call("relations_list", selection(&workspace))
            .unwrap()
            .contains("Retained before annotation")
    );
    let warm = f.call("compass_list", selection(&workspace)).unwrap();
    let node = f.node.clone();
    let fresh = WorkspaceDispatch::new(f.dispatch.node.clone(), move || {
        node.lock().unwrap().as_mut().unwrap().config()
    })
    .unwrap();
    let mut text = String::new();
    fresh
        .call(
            "compass_list",
            Bytes::from(serde_json::to_vec(&selection(&workspace)).unwrap()),
            &mut Out::new(&mut |part| {
                if let Part::Text { text: value } = part {
                    text.push_str(&value);
                }
                Ok(())
            }),
        )
        .unwrap();
    assert_eq!(warm, text);
    fresh.close().unwrap();
}

#[test]
fn unavailable_revision_does_not_hide_healthy_workspaces_and_can_be_withdrawn() {
    use crate::node_schema as attrs;
    use faculties::schemas::{compass, config::config};
    let f = Fixture::new();
    let healthy = f.workspace("healthy");
    let bad = f.workspace("conflicted");
    f.call("compass_list", selection(&bad)).unwrap();
    let revision = bad.id;
    f.annotate(
        entity! { ExclusiveId::force_ref(&revision) @ attrs::workspace_route*: entity! {
            config::anchor: compass::DEFAULT_SCOPE_ID,
            config::selects: triblespace::core::collection::CollectionHandle::new([31; 32]),
        } },
    );
    let listed: Value =
        serde_json::from_str(&f.call("list_workspaces", json!({})).unwrap()).unwrap();
    assert_eq!(listed["workspaces"].as_array().unwrap().len(), 1);
    assert_eq!(
        listed["workspaces"][0]["workspace"],
        format!("{:X}", healthy.id)
    );
    assert_eq!(listed["node_signer"], hex::encode(f.dispatch.node.signer));
    assert_eq!(
        listed["unavailable"][0]["workspace"],
        format!("{revision:X}")
    );
    assert!(
        listed["unavailable"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("conflicting exact roots")
    );
    assert!(!f.dispatch.owners.borrow().contains_key(&revision));
    {
        let mut guard = f.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let view = node.config().unwrap();
        assert!(view.workspace_revision(revision).is_err());
        node.remove_workspace_revision(&view, revision).unwrap();
    }
    assert!(f.call("compass_list", selection(&bad)).is_err());
    let listed: Value =
        serde_json::from_str(&f.call("list_workspaces", json!({})).unwrap()).unwrap();
    assert_eq!(listed["unavailable"], json!([]));
    assert_eq!(listed["workspaces"].as_array().unwrap().len(), 1);
}
