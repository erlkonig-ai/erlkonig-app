//! Local entry and one-use Secure Enclave operations. No keychain operation is
//! performed at startup, by a test, or merely by drawing a prompt.

use anyhow::{Context, Result, bail};
use block2::RcBlock;
use erlkonig_core::protected_access::{AccessResult, ProtectedPrompt, Purpose};
use objc2::{MainThreadMarker, MainThreadOnly, rc::Retained};
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication, NSModalPanelRunLoopMode, NSSecureTextField, NSTextField, NSView};
use objc2_foundation::{NSPoint, NSRect, NSRunLoop, NSSize, NSString, NSTimer};
use std::sync::{Arc, atomic::{AtomicBool, AtomicU8, Ordering}};
use zeroize::Zeroizing;

pub fn passphrase(
    mtm: MainThreadMarker,
    title: &str,
    detail: &str,
    confirm: bool,
    cancelled: impl Fn() -> bool + 'static,
    terminate: &Arc<AtomicBool>,
) -> Result<Zeroizing<String>> {
    if cancelled() || terminate.load(Ordering::Relaxed) { bail!("protected access cancelled"); }
    let cancelled = Arc::new(cancelled);
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(detail));
    alert.addButtonWithTitle(&NSString::from_str(if confirm { "Protect and save" } else { "Allow this request" }));
    let cancel = alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    cancel.setKeyEquivalent(&NSString::from_str("\u{1b}"));
    let frame = |y, h| NSRect::new(NSPoint::new(0.0, y), NSSize::new(410.0, h));
    let view = NSView::initWithFrame(NSView::alloc(mtm), frame(0.0, if confirm { 120.0 } else { 60.0 }));
    let first = NSSecureTextField::initWithFrame(NSSecureTextField::alloc(mtm), frame(if confirm { 60.0 } else { 0.0 }, 24.0));
    let label = NSTextField::labelWithString(&NSString::from_str("App passphrase (not your Mac login password)"), mtm);
    label.setFrame(frame(if confirm { 88.0 } else { 28.0 }, 20.0));
    view.addSubview(&label);
    view.addSubview(&first);
    let second = confirm.then(|| {
        let field = NSSecureTextField::initWithFrame(NSSecureTextField::alloc(mtm), frame(0.0, 24.0));
        let label = NSTextField::labelWithString(&NSString::from_str("Confirm app passphrase"), mtm);
        label.setFrame(frame(28.0, 20.0));
        view.addSubview(&label);
        view.addSubview(&field);
        unsafe { first.setNextKeyView(Some(&field)); }
        field
    });
    alert.setAccessoryView(Some(&view));
    let window = alert.window();
    window.setInitialFirstResponder(Some(&first));
    #[allow(deprecated)]
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    let check = cancelled.clone();
    let stopping = terminate.clone();
    let timer_block = RcBlock::new(move |_: std::ptr::NonNull<NSTimer>| {
        if check() || stopping.load(Ordering::Relaxed) {
            if let Some(mtm) = MainThreadMarker::new() { NSApplication::sharedApplication(mtm).abortModal(); }
        }
    });
    let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(0.1, true, &timer_block) };
    unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSModalPanelRunLoopMode); }
    let response = alert.runModal();
    timer.invalidate();
    let password = Zeroizing::new(first.stringValue().to_string());
    let confirmation = second.as_ref().map(|f| Zeroizing::new(f.stringValue().to_string()));
    first.setStringValue(&NSString::from_str(""));
    if let Some(second) = second { second.setStringValue(&NSString::from_str("")); }
    window.orderOut(None);
    if response != NSAlertFirstButtonReturn || cancelled() || terminate.load(Ordering::Relaxed) {
        bail!("protected access cancelled or expired");
    }
    validate_passphrase(&password, confirmation.as_deref().map(|s| s.as_str()))?;
    Ok(password)
}

fn validate_passphrase(password: &str, confirmation: Option<&str>) -> Result<()> {
    if password.is_empty() || password.len() > 1024 { bail!("app passphrase must contain 1–1024 UTF-8 bytes; nothing was saved"); }
    if confirmation.is_some_and(|value| value != password) { bail!("app passphrases did not match; nothing was saved"); }
    Ok(())
}

pub(crate) fn bounded_label(text: &str) -> String {
    let mut chars = text.chars().filter(|c| !c.is_control() && !matches!(*c, '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'));
    let mut label: String = chars.by_ref().take(56).collect();
    if chars.next().is_some() { label.push('…'); }
    label
}

fn action(purpose: &Purpose) -> String {
    match purpose {
        Purpose::Get => "Reveal raw plaintext".into(),
        Purpose::OpenFacts => "Reveal private item facts (including inline values)".into(),
        Purpose::Inspect => "Inspect private structural metadata".into(),
        Purpose::ExportText { child } => format!("Export text field {}…", &hex::encode(child)[..12]),
        Purpose::ExportBytes { child } => format!("Export binary field {}…", &hex::encode(child)[..12]),
        Purpose::Totp { child } => format!("Generate one TOTP code from field {}…", &hex::encode(child)[..12]),
    }
}

pub fn unlock(mtm: MainThreadMarker, prompt: &ProtectedPrompt, terminate: &Arc<AtomicBool>) -> AccessResult {
    if prompt.cancellation.is_cancelled() || terminate.load(Ordering::Relaxed) { bail!("protected access cancelled"); }
    let request = &prompt.request;
    let detail = format!(
        "Workspace: {} ({}…)\nItem: {}\nAction: {}\nSecret: {}… · version: {}…\n\nOutput goes to this pending local MCP caller, not an authenticated named agent. Allow this request only; the vault stays locked.\n\nTouch ID is not configured. Enter this item's app passphrase. Cancel reveals nothing.",
        bounded_label(&prompt.workspace_label), prompt.workspace.chars().take(12).collect::<String>(), bounded_label(request.label()), action(request.purpose()),
        format!("{}", request.secret()).chars().take(12).collect::<String>(), &hex::encode(request.body().raw)[..12],
    );
    let cancellation = prompt.cancellation.clone();
    let password = passphrase(mtm, "Allow one protected secret request", &detail, false, move || cancellation.is_cancelled(), terminate)?;
    let answer = request.unlock_password(password.as_bytes());
    if prompt.cancellation.is_cancelled() || terminate.load(Ordering::Relaxed) { bail!("protected access cancelled or expired"); }
    answer
}

/// Concrete backend retained as source until a properly signed/provisioned
/// bundle has passed synthetic hardware acceptance. UI enrollment is NOT wired
/// to this backend in the current ad-hoc build. No silent software-key fallback.
#[allow(dead_code)]
mod enclave {
    use super::*;
    use objc2_core_foundation::{CFBoolean, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType};
    use objc2_local_authentication::{LAAccessControlOperation, LAContext, LAPolicy};
    use objc2_security::*;
    use std::ptr::{self, NonNull};

    fn signed_identity() -> Result<()> {
        // This is only a necessary preflight. securityd remains the authority
        // for provisioned entitlements and keychain access; failure is terminal.
        let task = unsafe { SecTask::from_self(None) }.context("Touch ID signing identity unavailable")?;
        let identifier = unsafe { task.value_for_entitlement(&CFString::from_str("com.apple.application-identifier"), ptr::null_mut()) };
        if identifier.is_none() { bail!("Touch ID is not configured: a provisioned app signing identity is required"); }
        Ok(())
    }

    struct ContextGuard(Retained<LAContext>);
    impl Drop for ContextGuard { fn drop(&mut self) { unsafe { self.0.invalidate(); } } }

    fn fresh_context(reason: &str) -> Result<ContextGuard> {
        signed_identity()?;
        let context = unsafe { LAContext::new() };
        unsafe {
            context.setTouchIDAuthenticationAllowableReuseDuration(0.0);
            context.setLocalizedFallbackTitle(Some(&NSString::from_str("")));
            context.setLocalizedReason(&NSString::from_str(reason));
            context.canEvaluatePolicy_error(LAPolicy::DeviceOwnerAuthenticationWithBiometrics)
                .map_err(|_| anyhow::anyhow!("Touch ID unavailable, unenrolled, locked out, or noninteractive"))?;
        }
        Ok(ContextGuard(context))
    }

    /// Explicit enrollment only. Caller must mint an unpredictable tag and
    /// already have a recovery/password wrapper before publishing an envelope.
    pub fn create(tag: &[u8]) -> Result<CFRetained<SecKey>> {
        signed_identity()?;
        if tag.len() != 32 { bail!("invalid device key identifier"); }
        let context = fresh_context("Create protected secret device key")?;
        let _ = context; // preflight only; creating a public-key wrapper authorizes no read.
        unsafe {
            let access = SecAccessControl::with_flags(None, kSecAttrAccessibleWhenUnlockedThisDeviceOnly.as_ref(),
                SecAccessControlCreateFlags::PrivateKeyUsage | SecAccessControlCreateFlags::BiometryCurrentSet, ptr::null_mut())
                .context("Touch ID access control unavailable")?;
            let attrs = CFDictionary::<CFType, CFType>::from_slices(
                &[kSecAttrIsPermanent.as_ref(), kSecAttrApplicationTag.as_ref(), kSecAttrAccessControl.as_ref()],
                &[CFBoolean::new(true).as_ref(), CFData::from_bytes(tag).as_ref(), access.as_ref()],
            );
            let params = CFDictionary::<CFType, CFType>::from_slices(
                &[kSecAttrKeyType.as_ref(), kSecAttrKeySizeInBits.as_ref(), kSecAttrTokenID.as_ref(), kSecPrivateKeyAttrs.as_ref(), kSecUseDataProtectionKeychain.as_ref()],
                &[kSecAttrKeyTypeECSECPrimeRandom.as_ref(), CFNumber::new_i32(256).as_ref(), kSecAttrTokenIDSecureEnclave.as_ref(), attrs.as_ref(), CFBoolean::new(true).as_ref()],
            );
            SecKey::new_random_key(params.as_opaque(), ptr::null_mut()).context("Secure Enclave key creation failed; no fallback key was created")
        }
    }

    pub fn seal(key: &SecKey, payload: &[u8]) -> Result<Vec<u8>> {
        unsafe {
            let public = key.public_key().context("Secure Enclave public key unavailable")?;
            let algorithm = kSecKeyAlgorithmECIESEncryptionCofactorX963SHA256AESGCM;
            if !public.is_algorithm_supported(SecKeyOperationType::Encrypt, algorithm) { bail!("Secure Enclave wrapping algorithm unavailable"); }
            let ciphertext = public.encrypted_data(algorithm, &CFData::from_bytes(payload), ptr::null_mut()).context("Secure Enclave wrapping failed")?;
            Ok(ciphertext.to_vec())
        }
    }

    /// One protected private-key operation. LA policy success alone never
    /// returns a key or an authorization: the actual decryption must succeed.
    pub fn unseal(mtm: MainThreadMarker, prompt: &ProtectedPrompt, terminate: &Arc<AtomicBool>) -> AccessResult {
        if prompt.cancellation.is_cancelled() { bail!("protected access cancelled"); }
        let request = &prompt.request;
        let tag = request.device_key_id().context("no Touch ID wrapper exists for this item")?;
        let ciphertext = request.device_ciphertext().context("no Touch ID wrapper exists for this item")?;
        let reason = format!("{}: {} in {} for this pending local MCP request", action(request.purpose()), bounded_label(request.label()), bounded_label(&prompt.workspace_label));
        let context = fresh_context(&reason)?;
        unsafe {
            // Looking up the handle must not start an unbounded system prompt.
            // Authentication is initiated below, while our cancellation modal
            // is active, and private-key usage remains enforced by securityd.
            context.0.setInteractionNotAllowed(true);
            // LAContext is an NSObject supported as a dictionary value by the
            // Security API. CF retains it while the query exists.
            let context_value = &*(Retained::as_ptr(&context.0) as *const CFType);
            let query = CFDictionary::<CFType, CFType>::from_slices(
                &[kSecClass.as_ref(), kSecAttrApplicationTag.as_ref(), kSecAttrKeyType.as_ref(), kSecAttrTokenID.as_ref(), kSecReturnRef.as_ref(), kSecReturnAttributes.as_ref(), kSecUseDataProtectionKeychain.as_ref(), kSecUseAuthenticationContext.as_ref()],
                &[kSecClassKey.as_ref(), CFData::from_bytes(tag).as_ref(), kSecAttrKeyTypeECSECPrimeRandom.as_ref(), kSecAttrTokenIDSecureEnclave.as_ref(), CFBoolean::new(true).as_ref(), CFBoolean::new(true).as_ref(), CFBoolean::new(true).as_ref(), context_value],
            );
            let mut value: *const CFType = ptr::null();
            if SecItemCopyMatching(query.as_opaque(), &mut value) != errSecSuccess { bail!("Touch ID key unavailable; recovery passphrase is required"); }
            let value = CFRetained::from_raw(NonNull::new(value.cast_mut()).context("Touch ID key unavailable")?);
            let attributes = value.downcast::<CFDictionary>().map_err(|_| anyhow::anyhow!("invalid Touch ID key attributes"))?;
            // SecItem attributes are CF string keys and retained CF/ObjC values.
            let attributes = attributes.cast_unchecked::<CFType, CFType>();
            let key = attributes.get(kSecValueRef.as_ref()).and_then(|value| value.downcast::<SecKey>().ok())
                .context("Touch ID key unavailable")?;
            let access = attributes.get(kSecAttrAccessControl.as_ref()).and_then(|value| value.downcast::<SecAccessControl>().ok())
                .context("Touch ID key access control unavailable")?;
            context.0.setInteractionNotAllowed(false);
            let finished = Arc::new(AtomicU8::new(0));
            let done = finished.clone();
            let reply = RcBlock::new(move |success: objc2::runtime::Bool, _: *mut objc2_foundation::NSError| {
                done.store(if success.as_bool() { 1 } else { 2 }, Ordering::Release);
            });
            context.0.evaluateAccessControl_operation_localizedReason_reply(&access, LAAccessControlOperation::UseKeyDecrypt, &NSString::from_str(&reason), &reply);
            let alert = NSAlert::new(mtm);
            alert.setMessageText(&NSString::from_str("Waiting for Touch ID"));
            alert.setInformativeText(&NSString::from_str(&reason));
            alert.addButtonWithTitle(&NSString::from_str("Cancel"));
            let cancellation = prompt.cancellation.clone();
            let stopping = terminate.clone();
            let done = finished.clone();
            let timer_block = RcBlock::new(move |_: NonNull<NSTimer>| {
                if done.load(Ordering::Acquire) != 0 || cancellation.is_cancelled() || stopping.load(Ordering::Relaxed) {
                    if let Some(mtm) = MainThreadMarker::new() { NSApplication::sharedApplication(mtm).abortModal(); }
                }
            });
            let timer = NSTimer::timerWithTimeInterval_repeats_block(0.1, true, &timer_block);
            NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSModalPanelRunLoopMode);
            let response = alert.runModal();
            timer.invalidate();
            alert.window().orderOut(None);
            if response == NSAlertFirstButtonReturn || prompt.cancellation.is_cancelled() || terminate.load(Ordering::Relaxed) || finished.load(Ordering::Acquire) != 1 {
                bail!("Touch ID cancelled, expired, or denied");
            }
            // Reuse within THIS operation only, without another interactive
            // evaluation. The guard invalidates the context on every return.
            context.0.setInteractionNotAllowed(true);
            let algorithm = kSecKeyAlgorithmECIESEncryptionCofactorX963SHA256AESGCM;
            if !key.is_algorithm_supported(SecKeyOperationType::Decrypt, algorithm) { bail!("Secure Enclave unwrapping algorithm unavailable"); }
            let data = key.decrypted_data(algorithm, &CFData::from_bytes(ciphertext), ptr::null_mut()).context("Touch ID decryption denied")?;
            let payload = Zeroizing::new(data.to_vec());
            drop(data); // Apple-owned immutable buffer cannot promise zeroization.
            if prompt.cancellation.is_cancelled() || terminate.load(Ordering::Relaxed) { bail!("protected access cancelled or expired"); }
            request.unlock_device_payload(&payload)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{bounded_label, validate_passphrase};
    #[test]
    fn prompt_labels_are_bounded_and_cannot_inject_lines_or_direction() {
        assert_eq!(bounded_label("label\n\u{202e}\titem"), "labelitem");
        assert_eq!(bounded_label(&"a".repeat(1000)).chars().count(), 57);
    }
    #[test]
    fn passphrase_confirmation_is_exact_and_not_a_login_password_check() {
        assert!(validate_passphrase("", Some("")).is_err());
        assert!(validate_passphrase("a", Some("b")).is_err());
        assert!(validate_passphrase(" a ", Some(" a ")).is_ok());
        assert!(validate_passphrase(" a ", Some("a")).is_err());
        assert!(validate_passphrase(&"é".repeat(512), None).is_ok());
        assert!(validate_passphrase(&"é".repeat(513), None).is_err());
    }
}
