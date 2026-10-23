//! Request-scoped MCP selection of an explicitly configured workspace binding.
//!
//! Protocol connections have no current workspace. Only live native owners are
//! retained here; target data and configuration remain queried at their source.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use ed25519_dalek::VerifyingKey;
use faculties::mcp::{self, Bytes, Faculty, Tool, catalog};
use faculties::out::Out;
use faculties::storage::Storage;
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json, value::RawValue};
use triblespace::core::id::Id;

use crate::config::NodeBinding;
use crate::node_store::ConfigView;
use crate::protected_access::ProtectedAccessBridge;
use crate::secret_entry::{EntryAnswer, EntryInput, EntryKind, EntryRequest, SecretEntry};
use crate::local_import::{ImportAnswer, ImportRequest, LocalImport};
use crate::workspace::Workspace;

const LIST: Tool = Tool {
    name: "list_workspaces",
    description: "List explicitly configured Erlkonig workspaces, exact revision IDs, collection roles, alongside unavailable revisions. This reads app configuration only; it does not open the store or key. Pass one returned workspace ID on every faculty tool call. Names are labels, never selectors; each call resolves and freezes the current binding value.",
    input_schema: r#"{"type":"object","properties":{},"additionalProperties":false}"#,
};

const ENTER_LOCAL: Tool = Tool {
    name: "secrets_enter_local",
    description: "Ask the running app to show one local secure-entry dialog for the exact workspace, service and secret name. Never put credentials or protection passphrases in these arguments. The person enters a login (native structured encrypted username/password fields) or a generic secret (unchanged raw UTF-8 bytes) locally. Set protected=true to create a new locally passphrase-protected version; the person enters and confirms the protection passphrase locally. This does not convert existing secrets. Protected access later prompts for this exact operation; approval may disclose plaintext to the calling integration when the requested tool exports values. Omitted protected=false preserves ordinary entry. Both login fields, or the generic value, must be nonempty; invalid Save publishes nothing and asks for a new local entry. Save returns only an immutable version ID; Cancel saves nothing. Only one prompt may be active. This synchronous call waits for the person; keep the connection open. Disconnect/cancellation does not undo an admitted Save; an unknown outcome must not be retried automatically. Inspect secrets_items metadata for login or secrets_list for generic entry before any new entry. Creates no grants; collection sharing and secret delivery authority are unchanged.",
    input_schema: r#"{"type":"object","properties":{"service":{"type":"string","minLength":1,"maxLength":160},"name":{"type":"string","minLength":1,"maxLength":160},"kind":{"type":"string","enum":["login","secret"],"default":"login"},"protected":{"type":"boolean","default":false}},"required":["service","name"],"additionalProperties":false}"#,
};

const IMPORT_LOCAL: Tool = Tool {
    name: "secrets_import_1password_local",
    description: "Ask the running app to import a 1Password .1pux export into the exact workspace with user-only passphrase protection. The person chooses the file and enters and confirms an independent app passphrase locally. Never provide a file path, export bytes, credentials or passphrase to this tool. The original archive, imported items, attachments and receipt are protected; ordinary node keys cannot unlock them. Older readable copies are not revoked. No Touch ID or sharing grants are configured. Returns an opaque receipt ID and counts, never export data. Omit previous_import for a NEW import (no automatic deduplication). Supply a returned receipt ID to verify that exact previous import using its passphrase; failure never creates a replacement import. Cancel or expiry before admission saves nothing; an admitted operation runs once. Keep the connection open; disconnect does not undo admitted work. If outcome is unknown, do not retry automatically: inspect metadata first.",
    input_schema: r#"{"type":"object","properties":{"previous_import":{"type":"string","pattern":"^[0-9A-F]{32}$","description":"Optional receipt ID of the exact previous protected import to verify. Omit for a new import; no global content deduplication."}},"additionalProperties":false}"#,
};

pub struct WorkspaceDispatch {
    node: NodeBinding,
    config: Box<dyn Fn() -> Result<ConfigView>>,
    tools: Vec<Tool>,
    schemas: BTreeMap<&'static str, String>,
    // Handles and their adapters, never a materialized dataset or config cache.
    owners: RefCell<BTreeMap<Id, Owner>>,
    secret_entry: Option<SecretEntry>,
    protected_access: Option<ProtectedAccessBridge>,
    local_import: Option<LocalImport>,
}

struct Owner {
    binding: Workspace,
    catalog: NativeFaculties,
    storage: Storage,
}

/// Fixed private composition, not a runtime plugin registry. Both adapters
/// borrow clones of the one workspace storage owner; only that owner closes.
struct NativeFaculties {
    public: catalog::Catalog,
    erp: faculties_erp::bookkeeping::mcp::Erp,
}

impl NativeFaculties {
    fn with_storage(config: catalog::Config, storage: Storage) -> Self {
        Self {
            public: catalog::Catalog::with_storage(config, storage.clone()),
            erp: faculties_erp::bookkeeping::mcp::Erp::with_storage(storage),
        }
    }

    fn registrations(&self) -> Vec<&dyn Faculty> {
        let mut registrations = self.public.registrations();
        registrations.push(&self.erp);
        registrations
    }

    fn close(&self) -> Result<()> {
        self.public.close()
    }
}

impl WorkspaceDispatch {
    pub fn new(
        node: NodeBinding,
        config: impl Fn() -> Result<ConfigView> + 'static,
    ) -> Result<Self> {
        // Catalog construction is I/O-free. The schema-only catalog has no
        // usable path and is never dispatched; there is no node-pile fallback.
        let path = PathBuf::new();
        let catalog = NativeFaculties::with_storage(
            catalog::Config::new(&path),
            Storage::shared_with_daemon(path, None, None).with_collections(BTreeMap::new()),
        );
        let mut tools = vec![LIST];
        let mut schemas = BTreeMap::new();
        for faculty in catalog.registrations() {
            for tool in faculty.tools() {
                register_tool(&mut tools, &mut schemas, tool, faculty.input_schema(tool))?;
            }
        }
        register_tool(
            &mut tools,
            &mut schemas,
            &ENTER_LOCAL,
            ENTER_LOCAL.input_schema,
        )?;
        register_tool(&mut tools, &mut schemas, &IMPORT_LOCAL, IMPORT_LOCAL.input_schema)?;
        Ok(Self {
            node,
            config: Box::new(config),
            tools,
            schemas,
            owners: RefCell::new(BTreeMap::new()),
            secret_entry: None,
            protected_access: None,
            local_import: None,
        })
    }

    pub fn with_secret_entry(mut self, secret_entry: SecretEntry) -> Self {
        self.secret_entry = Some(secret_entry);
        self
    }

    pub fn with_protected_access(mut self, protected_access: ProtectedAccessBridge) -> Self {
        self.protected_access = Some(protected_access);
        self
    }

    pub fn with_local_import(mut self, local_import: LocalImport) -> Self {
        self.local_import = Some(local_import);
        self
    }

    /// Stop owners whose binding was removed or superseded. HTTP dispatch is
    /// sequential: an already admitted native call drains before this runs.
    fn retire(&self, view: &ConfigView, selected: Option<Id>) -> Result<()> {
        let ids: Vec<_> = self.owners.borrow().keys().copied().collect();
        let mut failures = Vec::new();
        for id in ids {
            // The selected value is resolved exactly once by its caller.
            if selected == Some(id) {
                continue;
            }
            let current = view.workspace(id);
            let unchanged = matches!(&current, Ok(Some(binding))
                if self.owners.borrow().get(&id).is_some_and(|owner| owner.binding == *binding));
            if !unchanged {
                if let Some(owner) = self.owners.borrow_mut().remove(&id) {
                    if let Err(error) = owner.catalog.close() {
                        failures.push(format!("close workspace {id:X}: {error:#}"));
                    }
                }
            }
        }
        if !failures.is_empty() {
            bail!(failures.join("; "));
        }
        Ok(())
    }

    fn open(&self, workspace: Workspace) -> Result<Owner> {
        let signer = VerifyingKey::from_bytes(&self.node.signer)
            .context("workspace has an invalid configured signing identity")?;
        // Workspace selection alone does not authorize an acquisition network
        // owner. Missing payloads are reported; managed sync is independent.
        let mut storage = Storage::shared_local(self.node.pile.clone(), Some(self.node.key.clone()))
            .with_collections(workspace.routes.clone())
            .with_expected_signer(signer);
        if let Some(bridge) = &self.protected_access {
            let bridge = bridge.clone();
            let workspace_id = format!("{:X}", workspace.id);
            let workspace_label = workspace
                .label
                .chars()
                .filter(|c| !c.is_control())
                .take(160)
                .collect::<String>();
            // Secrets alone invokes this resolver, after releasing its store
            // lease. It reopens and validates the exact detached operation
            // before using the returned key; dispatch never retries a call.
            storage = storage.with_protected_access(Arc::new(move |request| {
                bridge.request(workspace_id.clone(), workspace_label.clone(), request.clone())
            }));
        }
        let mut config = catalog::Config::new(&self.node.pile);
        config.key = Some(self.node.key.clone());
        Ok(Owner {
            binding: workspace,
            storage: storage.clone(),
            catalog: NativeFaculties::with_storage(config, storage),
        })
    }

    /// Transport shutdown first drains requests; then close every native owner,
    /// reporting all close failures rather than relying on best-effort Drop.
    pub fn close(&self) -> Result<()> {
        let owners = std::mem::take(&mut *self.owners.borrow_mut());
        let mut failures = Vec::new();
        for (id, owner) in owners {
            if let Err(error) = owner.catalog.close() {
                failures.push(format!("close workspace {id:X}: {error:#}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            bail!(failures.join("; "))
        }
    }

    pub fn finish<T>(&self, result: Result<T>) -> Result<T> {
        match (result, self.close()) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
            (Err(error), Err(close)) => {
                Err(error.context(format!("workspace shutdown: {close:#}")))
            }
        }
    }
}

impl Drop for WorkspaceDispatch {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            eprintln!("Erlkonig workspace shutdown: {error:#}");
        }
    }
}

impl Faculty for WorkspaceDispatch {
    fn tools(&self) -> &[Tool] {
        &self.tools
    }

    fn input_schema<'a>(&'a self, tool: &'a Tool) -> &'a str {
        self.schemas
            .get(tool.name)
            .map(String::as_str)
            .unwrap_or(tool.input_schema)
    }

    fn call(&self, name: &str, arguments: Bytes, out: &mut Out<'_>) -> Result<()> {
        if name == LIST.name {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Empty {}
            let _: Empty = mcp::decode_arguments(arguments)?;
            let view = (self.config)().map_err(|_| {
                anyhow::anyhow!(
                    "node configuration is unavailable; native data recovery is required"
                )
            })?;
            self.retire(&view, None)?;
            let mut workspaces = Vec::new();
            let mut unavailable = Vec::new();
            for id in view.workspace_ids()? {
                match view.workspace(id) {
                    Ok(Some(workspace)) => {
                        let roles: Vec<_> = workspace
                            .routes
                            .iter()
                            .map(|(role, root)| {
                                json!({
                                    "role": crate::workspace::role_name(*role),
                                    "role_id": format!("{role:X}"),
                                    "root": format!("blake3:{}", hex::encode(root.raw)),
                                })
                            })
                            .collect();
                        workspaces.push(json!({
                            "workspace": format!("{:X}", workspace.id),
                            "label": workspace.label,

                            "roles": roles,
                        }));
                    }
                    other => unavailable.push(json!({
                        "workspace": format!("{id:X}"),
                        "reason": match other {
                            Err(error) => error.to_string(),
                            _ => "workspace was removed or superseded".to_owned(),
                        },
                    })),
                }
            }
            return out.text(serde_json::to_string(&json!({
                "node_signer": hex::encode(self.node.signer), "workspaces": workspaces, "unavailable": unavailable,
            }))?);
        }
        if !self.schemas.contains_key(name) {
            bail!("unknown workspace MCP tool {name:?}");
        }
        let selected: SelectedArguments = mcp::decode_arguments(arguments).map_err(|error| {
            if name == IMPORT_LOCAL.name {
                mcp::invalid_arguments("local import requires one exact workspace ID and optional previous_import receipt ID; invalid arguments")
            } else { error }
        })?;
        let view = (self.config)().map_err(|_| {
            anyhow::anyhow!("node configuration is unavailable; native data recovery is required")
        })?;
        // Config text handles are bearer capabilities, not diagnostic detail.
        let resolved = view
            .workspace(selected.workspace)
            .map_err(|error| anyhow::anyhow!(error.to_string()));
        // IDs are opaque: native annotation can change the value under an
        // existing ID. Reuse only an owner for this exact observed value.
        let unchanged = matches!(&resolved, Ok(Some(binding))
            if self.owners.borrow().get(&selected.workspace).is_some_and(|owner| owner.binding == *binding));
        if !unchanged {
            if let Some(owner) = self.owners.borrow_mut().remove(&selected.workspace) {
                owner
                    .catalog
                    .close()
                    .context("close replaced workspace owner")?;
            }
        }
        self.retire(&view, Some(selected.workspace))?;
        let workspace = resolved?.ok_or_else(|| {
            mcp::invalid_arguments("unknown, removed, or superseded workspace binding; call list_workspaces and explicitly choose a current ID")
        })?;
        if (name == ENTER_LOCAL.name || name == IMPORT_LOCAL.name)
            && !workspace
                .routes
                .contains_key(&faculties::secrets::DEFAULT_SCOPE_ID)
        {
            return Err(mcp::invalid_arguments(
                "workspace collection role secrets is unconfigured; no local entry was requested",
            ));
        }
        // Even a tool without a storage entrypoint must not run under a
        // silently replaced binding. Storage repeats this at its own opening
        // boundary, so a replacement between these checks cannot retarget it.
        let signer = triblespace::core::signing_key_file::load_existing(&self.node.key)
            .context("load explicitly selected workspace signing identity")?;
        if signer.verifying_key().to_bytes() != self.node.signer {
            bail!("workspace signing identity changed; configure and select a new binding");
        }
        let mut owners = self.owners.borrow_mut();
        if let std::collections::btree_map::Entry::Vacant(entry) = owners.entry(workspace.id) {
            entry.insert(self.open(workspace.clone())?);
        }
        let owner = &owners[&workspace.id];
        if name == IMPORT_LOCAL.name {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Import {
                #[serde(default, deserialize_with = "present_receipt")]
                previous_import: Option<String>,
            }
            fn present_receipt<'de, D: Deserializer<'de>>(value: D) -> std::result::Result<Option<String>, D::Error> {
                String::deserialize(value).map(Some)
            }
            // Fixed error: serde's unknown-field diagnostics can echo an
            // arbitrary caller-provided key. Nothing supplied is repeated.
            let args: Import = mcp::decode_arguments(selected.arguments).map_err(|_| {
                mcp::invalid_arguments("local import accepts only workspace and optional previous_import receipt ID; select the export and enter its protection passphrase in the app")
            })?;
            let previous_receipt = args.previous_import.as_ref().map(|text| {
                Id::from_hex(text).filter(|id| format!("{id:X}") == *text)
                    .ok_or_else(|| mcp::invalid_arguments("previous_import must be an exact uppercase receipt ID"))
            }).transpose()?;
            let Some(bridge) = &self.local_import else {
                return out.text(r#"{"status":"unavailable"}"#);
            };
            owner.storage.with_store(|store, signer, runtime| {
                owner.storage.open_collection_write(store, faculties::secrets::DEFAULT_SCOPE_ID,
                    signer.verifying_key(), runtime).map(|_| ())
            })?;
            // Storage lease is released for the entire file picker/passphrase
            // interaction. The owner and its workspace route remain frozen.
            let answer = bridge.request(ImportRequest {
                workspace: format!("{:X}", owner.binding.id),
                workspace_label: owner.binding.label.chars().filter(|c| !c.is_control()).take(160).collect(),
                previous_import: args.previous_import,
            })?;
            let response = match answer {
                ImportAnswer::Cancelled => json!({"status": "cancelled"}),
                ImportAnswer::Unavailable => json!({"status": "unavailable"}),
                ImportAnswer::Invalid => json!({"status": "not_saved", "message": "The local selection or passphrase was invalid; nothing was saved."}),
                ImportAnswer::Submit { source, passphrase } => {
                    if passphrase.is_empty() || passphrase.len() > 1024 {
                        return out.text(r#"{"status":"not_saved","message":"Invalid local protection passphrase; nothing was saved."}"#);
                    }
                    let limits = faculties::secrets::onepassword::ImportLimits::default();
                    let bytes = match source.read(limits.archive_bytes) {
                        Ok(bytes) => bytes,
                        Err(_) => return out.text(r#"{"status":"not_saved","message":"Selected export changed, is unreadable, or exceeds the import limit; nothing was saved."}"#),
                    };
                    let secrets = faculties::secrets::Secrets::with_storage(owner.storage.clone());
                    let protection = faculties::secrets::protected::Protection { password: passphrase.as_bytes(), device: None };
                    match secrets.import_1password_protected(&bytes, limits, protection, previous_receipt) {
                        Ok(report) => json!({
                            "status": "saved",
                            "receipt": format!("{:X}", report.receipt), "protected": true,
                            "reused": report.reused, "view_maintenance_pending": report.view_maintenance_pending,
                            "counts": {"accounts": report.counts.accounts, "vaults": report.counts.vaults,
                                "items": report.counts.items, "fields": report.counts.fields,
                                "attachments": report.counts.attachments, "files": report.counts.files,
                                "archive_only_files": report.counts.archive_only_files,
                                "archive_only_values": report.counts.archive_only_values},
                        }),
                        Err(error) => local_import_failure(&error),
                    }
                }
            };
            return out.text(serde_json::to_string(&response)?);
        }
        if name == ENTER_LOCAL.name {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Entry {
                service: String,
                name: String,
                #[serde(default)]
                kind: EntryKind,
                #[serde(default)]
                protected: bool,
            }
            let args: Entry = mcp::decode_arguments(selected.arguments)?;
            for text in [&args.service, &args.name] {
                if text.trim().is_empty()
                    || text.chars().count() > 160
                    || text.chars().any(char::is_control)
                {
                    return Err(mcp::invalid_arguments(
                        "service and name must be 1–160 visible characters",
                    ));
                }
            }
            let Some(bridge) = &self.secret_entry else {
                return out.text(r#"{"status":"unavailable"}"#);
            };
            // A read-only admission preflight on the same owner, before asking
            // for plaintext. Rechecked by native Secrets authoring at its write
            // boundary for either raw bytes or a structured item.
            owner.storage.with_store(|store, signer, runtime| {
                owner
                    .storage
                    .open_collection_write(
                        store,
                        faculties::secrets::DEFAULT_SCOPE_ID,
                        signer.verifying_key(),
                        runtime,
                    )
                    .map(|_| ())
            })?;
            let answer = bridge.request(EntryRequest {
                workspace: format!("{:X}", owner.binding.id),
                workspace_label: owner
                    .binding
                    .label
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(160)
                    .collect(),
                service: args.service,
                name: args.name.clone(),
                kind: args.kind,
                protected: args.protected,
            })?;
            let response = match answer {
                EntryAnswer::Cancelled => json!({"status": "cancelled"}),
                EntryAnswer::Unavailable => json!({"status": "unavailable"}),
                EntryAnswer::Submit(value) => {
                    use faculties::secrets::item::{FieldInput, FieldValue, ItemInput};
                    use faculties::secrets::schema::{ROLE_PASSWORD, ROLE_USERNAME};
                    let input = value.input(args.kind)?;
                    let protection = value.protection();
                    if args.protected != protection.is_some() {
                        bail!("local entry protection mismatch; no secret was saved");
                    }
                    let secrets = faculties::secrets::Secrets::with_storage(owner.storage.clone());
                    // Only this worker touches the request-bound storage. An
                    // error may follow commit, so never claim that it rolled back.
                    let saved = match input {
                        EntryInput::Login { username, password } => {
                            let fields = [
                                FieldInput {
                                    role: ROLE_USERNAME,
                                    label: None,
                                    value: FieldValue::Text(username),
                                },
                                FieldInput {
                                    role: ROLE_PASSWORD,
                                    label: None,
                                    value: FieldValue::Text(password),
                                },
                            ];
                            let item = ItemInput {
                                fields: &fields,
                                attachments: &[],
                            };
                            match protection {
                                Some(protection) => {
                                    secrets.add_item_protected(&args.name, item, protection)
                                }
                                None => secrets.add_item(&args.name, item),
                            }
                        }
                        EntryInput::Secret(plaintext) => match protection {
                            Some(protection) => {
                                secrets.add_protected(&args.name, plaintext, protection)
                            }
                            None => secrets.add(&args.name, plaintext),
                        },
                    };
                    match saved {
                        Ok(id) => json!({"status": "saved", "secret": format!("{id:X}")}),
                        Err(_) => {
                            json!({"status": "outcome_unknown", "message": "Saving did not report success; it may have committed. Inspect secret metadata before any new entry; nothing was retried."})
                        }
                    }
                }
            };
            return out.text(serde_json::to_string(&response)?);
        }
        let faculty = owner
            .catalog
            .registrations()
            .into_iter()
            .find(|faculty| faculty.tools().iter().any(|tool| tool.name == name))
            .context("registered workspace tool has no native adapter")?;
        faculty.call(name, selected.arguments, out)
    }
}

/// Only core-owned static classifications cross this boundary. Never format an
/// underlying error: archive paths, field labels and values may be in its cause.
fn local_import_failure(error: &anyhow::Error) -> Value {
    use faculties::secrets::onepassword::{ProtectedImportPublicationError, ProtectedImportRejection};
    if let Some(unknown) = error.downcast_ref::<ProtectedImportPublicationError>() {
        return json!({"status": "outcome_unknown", "receipt": format!("{:X}", unknown.receipt),
            "code": "publication_uncertain", "message": "Import may have committed. Keep this receipt; inspect metadata before explicitly verifying it. Nothing was retried."});
    }
    if let Some(rejection) = error.downcast_ref::<ProtectedImportRejection>() {
        let mut response = json!({"status": "not_saved", "stage": rejection.stage.as_str(), "code": rejection.code.as_str()});
        if let Some(code) = rejection.os_error_code.filter(|code| *code > 0) {
            response["os_error_code"] = json!(code);
        }
        return response;
    }
    json!({"status": "not_saved", "code": "import_rejected",
        "message": "The export or previous receipt could not be accepted; no import was published. Nothing was retried."})
}

fn register_tool(
    tools: &mut Vec<Tool>,
    schemas: &mut BTreeMap<&'static str, String>,
    tool: &Tool,
    schema: &str,
) -> Result<()> {
    if tool.name == LIST.name || schemas.contains_key(tool.name) {
        bail!("workspace MCP tool name collision: {}", tool.name);
    }
    schemas.insert(
        tool.name,
        wrapped_schema(schema).with_context(|| format!("wrap MCP tool {}", tool.name))?,
    );
    tools.push(Tool {
        name: tool.name,
        description: tool.description,
        input_schema: tool.input_schema,
    });
    Ok(())
}

fn wrapped_schema(source: &str) -> Result<String> {
    let mut schema: Value = serde_json::from_str(source)?;
    if schema["type"] != "object" {
        bail!("workspace wrapper requires an object input schema");
    }
    let properties = schema
        .get_mut("properties")
        .and_then(Value::as_object_mut)
        .context("workspace wrapper requires explicit object properties")?;
    if properties.contains_key("workspace") {
        bail!("faculty input schema already declares the reserved workspace argument");
    }
    properties.insert("workspace".into(), json!({
        "type": "string",
        "pattern": "^[0-9A-F]{32}$",
        "description": "Exact workspace revision ID returned by list_workspaces. The current configured binding value is frozen for this call. Required on every call; never a label, prefix, path, key, or connection-global selection.",
    }));
    let required = schema
        .as_object_mut()
        .context("object schema")?
        .entry("required")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .context("schema required must be an array")?;
    if required.iter().any(|field| field == "workspace") {
        bail!("faculty input schema already requires reserved workspace argument");
    }
    required.push(json!("workspace"));
    Ok(serde_json::to_string(&schema)?)
}

struct SelectedArguments {
    workspace: Id,
    arguments: Bytes,
}

impl<'de> Deserialize<'de> for SelectedArguments {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Selection;
        impl<'de> Visitor<'de> for Selection {
            type Value = SelectedArguments;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("tool argument object with one exact workspace ID")
            }
            fn visit_map<M: MapAccess<'de>>(
                self,
                mut map: M,
            ) -> std::result::Result<Self::Value, M::Error> {
                use serde::de::Error;
                let mut seen = BTreeSet::new();
                let mut workspace = None;
                let mut arguments = String::from("{");
                while let Some(key) = map.next_key::<String>()? {
                    if !seen.insert(key.clone()) {
                        return Err(M::Error::custom(format!("duplicate argument {key:?}")));
                    }
                    if key == "workspace" {
                        let text = map.next_value::<String>()?;
                        let id = Id::from_hex(&text)
                            .filter(|id| format!("{id:X}") == text)
                            .ok_or_else(|| {
                                M::Error::custom(
                                    "workspace must be an exact ID returned by list_workspaces",
                                )
                            })?;
                        workspace = Some(id);
                    } else {
                        // Keep nested JSON verbatim, especially duplicate keys
                        // that the adapter must reject rather than normalize.
                        let value = map.next_value::<Box<RawValue>>()?;
                        if arguments.len() > 1 {
                            arguments.push(',');
                        }
                        arguments.push_str(&serde_json::to_string(&key).map_err(M::Error::custom)?);
                        arguments.push(':');
                        arguments.push_str(value.get());
                    }
                }
                arguments.push('}');
                Ok(SelectedArguments {
                    workspace: workspace.ok_or_else(|| M::Error::missing_field("workspace"))?,
                    arguments: Bytes::from(arguments.into_bytes()),
                })
            }
        }
        deserializer.deserialize_map(Selection)
    }
}

#[cfg(test)]
mod tests;
