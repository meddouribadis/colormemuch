//! Thin, safe-ish wrapper over the `AcerGamingFunction` WMI interface.
//!
//! This module owns *transport only* — connecting to `root\WMI`, marshalling
//! parameters, invoking methods, unpacking returns. It deliberately knows
//! nothing about what any particular byte means; that lives in `rgb.rs`, built
//! from observed data (see `scripts/probe_rgb.ps1`).
//!
//! Two shapes of method exist on this interface:
//!
//! * **Packed scalar** — `SetGamingRgbKb(UInt64 gmInput, UInt32 gmOutput)` and
//!   most others. Arguments are bit-packed into a single u64.
//! * **Byte array** — `SetGamingLED(UInt8Array gmInput, ...)` and
//!   `SetGamingKBBacklight`. These take a `VT_ARRAY | VT_UI1` SAFEARRAY, which
//!   is why this module uses the raw `windows` bindings rather than a
//!   query-oriented WMI wrapper.
//!
//! Every entry point requires the process to be elevated. Instantiating the
//! class unelevated fails with `Access denied` even for read-only methods, so
//! [`Wmi::connect`] surfaces that as [`WmiError::AccessDenied`] rather than
//! letting it read as a bad payload.

#![cfg(windows)]
// The RGB layer is exercised by tests and the (coming) CLI, not yet from the
// egui `main` path, so most of it reads as dead code to the default build.
// Silence that until `main`/`cli.rs` wires it up — it's just noise now, and it
// was burying the actual test output. Remove when the layer has real callers.
#![allow(dead_code)]

use std::fmt;

use windows::core::{BSTR, HRESULT, VARIANT};
use windows::Win32::Foundation::E_ACCESSDENIED;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoInitializeSecurity, CoSetProxyBlanket, CLSCTX_INPROC_SERVER,
    COINIT_MULTITHREADED, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL, RPC_C_AUTHN_LEVEL_DEFAULT,
    RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE};
use windows::Win32::System::Wmi::{
    IWbemClassObject, IWbemLocator, IWbemServices, WbemLocator, WBEM_FLAG_RETURN_WBEM_COMPLETE,
};

/// WMI namespace holding the Acer vendor classes.
const NAMESPACE: &str = r"root\WMI";
/// The vendor class itself. Its `guid` qualifier is
/// `{7A4DDFE7-5B5D-40B4-8595-4408E0CC7F56}` — verify this matches before
/// trusting any payload built for it, since the byte layout is firmware-version
/// specific (observed: "Gaming Function, Version 2.94").
const CLASS: &str = "AcerGamingFunction";

// WMI reports failures in its own HRESULT facility (0x8004_1xxx) rather than
// the generic Win32 ones — an unelevated call comes back as
// WBEM_E_ACCESS_DENIED, *not* E_ACCESSDENIED, so matching only the latter
// misreports "not elevated" as an unknown COM error.
const WBEM_E_ACCESS_DENIED: HRESULT = HRESULT(0x8004_1003_u32 as i32);
const WBEM_E_INVALID_CLASS: HRESULT = HRESULT(0x8004_1010_u32 as i32);
const WBEM_E_NOT_FOUND: HRESULT = HRESULT(0x8004_1002_u32 as i32);

#[derive(Debug)]
pub enum WmiError {
    /// The process is not elevated, or the ACL on the class rejected us.
    /// Class *metadata* is readable unelevated; instances are not — so this is
    /// the expected failure when running as a normal user.
    AccessDenied,
    /// The class isn't present. Means this isn't Acer gaming hardware, or the
    /// vendor WMI provider isn't installed.
    ClassNotFound,
    /// A method returned a non-success `gmReturn`, or an output parameter was
    /// missing/of unexpected type.
    Protocol(String),
    /// Anything else from COM.
    Com(windows::core::Error),
}

impl fmt::Display for WmiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AccessDenied => write!(
                f,
                "access denied talking to {CLASS} — colormemuch must run elevated \
                 (the manifest requests it; check the process is actually admin)"
            ),
            Self::ClassNotFound => write!(
                f,
                "{CLASS} not found in {NAMESPACE} — unsupported hardware, or the \
                 Acer WMI provider is missing"
            ),
            Self::Protocol(m) => write!(f, "WMI protocol error: {m}"),
            Self::Com(e) => write!(f, "COM error: {e}"),
        }
    }
}

impl std::error::Error for WmiError {}

impl From<windows::core::Error> for WmiError {
    fn from(e: windows::core::Error) -> Self {
        match e.code() {
            E_ACCESSDENIED | WBEM_E_ACCESS_DENIED => Self::AccessDenied,
            WBEM_E_INVALID_CLASS | WBEM_E_NOT_FOUND => Self::ClassNotFound,
            _ => Self::Com(e),
        }
    }
}

pub type Result<T> = std::result::Result<T, WmiError>;

/// A live connection to the vendor WMI provider.
///
/// Construction is the expensive part (COM init + DCOM connect + proxy
/// blanket); method calls afterwards are cheap. Hold one for the process
/// lifetime rather than reconnecting per call.
pub struct Wmi {
    services: IWbemServices,
    /// Full `__PATH` of the singleton instance, e.g.
    /// `AcerGamingFunction.InstanceName="ACPI\\..."`. Method invocation needs
    /// the instance path, not the bare class name.
    instance_path: BSTR,
}

impl Wmi {
    /// Connect to `root\WMI` and locate the singleton `AcerGamingFunction`
    /// instance.
    ///
    /// Returns [`WmiError::AccessDenied`] when not elevated — check that first
    /// before suspecting anything else.
    pub fn connect() -> Result<Self> {
        unsafe {
            // COINIT_MULTITHREADED: egui drives the UI thread, and we want to
            // be callable from a worker without an STA message pump.
            // Ignore RPC_E_CHANGED_MODE — another component may have already
            // initialised COM in this process, which is fine.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

            // Process-wide, and legal to call only once. A second call returns
            // RPC_E_TOO_LATE, which is harmless here — some host (eframe, rfd)
            // may have got there first.
            let _ = CoInitializeSecurity(
                None,
                -1,
                None,
                None,
                RPC_C_AUTHN_LEVEL_DEFAULT,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
                None,
            );

            let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)?;

            let services: IWbemServices = locator.ConnectServer(
                &BSTR::from(NAMESPACE),
                &BSTR::new(), // user: current token
                &BSTR::new(), // password: current token
                &BSTR::new(), // locale: default
                0,
                &BSTR::new(), // authority
                None,
            )?;

            // Without impersonation the provider sees the caller as anonymous
            // and refuses even read methods.
            CoSetProxyBlanket(
                &services,
                RPC_C_AUTHN_WINNT,
                RPC_C_AUTHZ_NONE,
                None,
                RPC_C_AUTHN_LEVEL_CALL,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
            )?;

            let instance_path = Self::find_instance_path(&services)?;

            Ok(Self {
                services,
                instance_path,
            })
        }
    }

    /// Enumerate the class and take the first instance's `__PATH`.
    ///
    /// The class is a singleton in practice, but it is declared `dynamic`, so
    /// the path is provider-generated and must be read rather than assumed.
    unsafe fn find_instance_path(services: &IWbemServices) -> Result<BSTR> {
        let enumerator = services
            .CreateInstanceEnum(&BSTR::from(CLASS), Default::default(), None)
            .map_err(WmiError::from)?;

        let mut objects: [Option<IWbemClassObject>; 1] = [None];
        let mut returned = 0u32;
        // WBEM_INFINITE — the provider talks to firmware and can be slow, and a
        // spurious timeout here would look like missing hardware.
        enumerator.Next(-1, &mut objects, &mut returned).ok()?;

        if returned == 0 {
            return Err(WmiError::ClassNotFound);
        }

        let obj = objects[0]
            .as_ref()
            .ok_or_else(|| WmiError::Protocol("enumerator returned a null instance".into()))?;

        let mut path = VARIANT::default();
        obj.Get(&BSTR::from("__PATH"), 0, &mut path, None, None)?;

        BSTR::try_from(&path)
            .map_err(|_| WmiError::Protocol("instance __PATH was not a string".into()))
    }

    /// Invoke a method taking a packed `UInt64 gmInput`, returning `gmOutput`.
    ///
    /// Covers most of the interface: `SetGamingRgbKb`, `SetGamingLEDColor`,
    /// `SetGamingLEDBehavior`, the fan family, and their `Get*` counterparts.
    /// The meaning of the packed bits is method-specific and lives in `rgb.rs`.
    pub fn call_packed(&self, method: &str, gm_input: u64) -> Result<u64> {
        let out = self.invoke(method, |in_params| unsafe {
            // CIM uint64 parameters are NOT passed as integer VARIANTs — WMI's
            // convention marshals 64-bit values as decimal *strings* (VT_BSTR).
            // Passing VT_I8 here fails with 0x80041005 WBEM_E_TYPE_MISMATCH
            // (verified on hardware 2026-07-20).
            let v = VARIANT::from(BSTR::from(gm_input.to_string()));
            in_params.Put(&BSTR::from("gmInput"), 0, &v, 0)?;
            Ok(())
        })?;

        self.read_scalar(&out, "gmOutput")
    }

    /// Invoke a method taking a `UInt8Array gmInput`.
    ///
    /// This is the shape used by `SetGamingLED` (the lid shield) and
    /// `SetGamingKBBacklight` (the 4-zone keyboard).
    ///
    /// # Safety of the *caller*, not the code
    ///
    /// This writes firmware state on real hardware. The lid-shield byte layout
    /// is not documented by the Linux prior art — capture a known-good packet
    /// with the probe script and replay it rather than sweeping values blind.
    pub fn call_bytes(&self, method: &str, gm_input: &[u8]) -> Result<u64> {
        let out = self.invoke(method, |in_params| unsafe {
            let v = byte_array_variant(gm_input)?;
            in_params.Put(&BSTR::from("gmInput"), 0, &v, 0)?;
            Ok(())
        })?;

        self.read_scalar(&out, "gmOutput")
    }

    /// Invoke a *reader* taking a `UInt32 gmInput` and returning a
    /// `UInt8Array gmOutput` plus a `UInt8 gmReturn` status.
    ///
    /// This is the shape of `GetGamingLED` and `GetGamingKBBacklight`. The
    /// observed packet is 15 bytes wide. `gmReturn` is a validity code: `0`
    /// means the payload is real, non-zero means this `gmInput` selector has no
    /// answer (see the probe findings) — callers should treat non-zero as
    /// "absent", not as an error.
    pub fn call_read_bytes(&self, method: &str, gm_input: u32) -> Result<(Vec<u8>, u8)> {
        let out = self.invoke(method, |in_params| unsafe {
            let v = VARIANT::from(gm_input as i32);
            in_params.Put(&BSTR::from("gmInput"), 0, &v, 0)?;
            Ok(())
        })?;

        let bytes = self.read_byte_array(&out, "gmOutput")?;
        let status = u8::try_from(self.read_scalar(&out, "gmReturn").unwrap_or(0) & 0xFF)
            .unwrap_or(0);
        Ok((bytes, status))
    }

    /// Shared invoke path: clone the method's in-params, let `fill` populate
    /// them, execute, hand back the out-params object.
    fn invoke<F>(&self, method: &str, fill: F) -> Result<IWbemClassObject>
    where
        F: FnOnce(&IWbemClassObject) -> Result<()>,
    {
        unsafe {
            // Method signatures come from the class definition, so fetch the
            // class (not the instance) to build the in-params template.
            let mut class_def: Option<IWbemClassObject> = None;
            self.services.GetObject(
                &BSTR::from(CLASS),
                Default::default(),
                None,
                Some(&mut class_def),
                None,
            )?;
            let class_def = class_def
                .ok_or_else(|| WmiError::Protocol(format!("could not load class {CLASS}")))?;

            let mut in_sig: Option<IWbemClassObject> = None;
            class_def.GetMethod(&BSTR::from(method), 0, &mut in_sig, std::ptr::null_mut())?;
            let in_sig = in_sig.ok_or_else(|| {
                WmiError::Protocol(format!("{method} has no input signature — wrong method name?"))
            })?;

            let in_params = in_sig.SpawnInstance(0)?;
            fill(&in_params)?;

            let mut out_params: Option<IWbemClassObject> = None;
            self.services.ExecMethod(
                &self.instance_path,
                &BSTR::from(method),
                WBEM_FLAG_RETURN_WBEM_COMPLETE,
                None,
                &in_params,
                Some(&mut out_params),
                None,
            )?;

            out_params.ok_or_else(|| {
                WmiError::Protocol(format!("{method} returned no output parameters"))
            })
        }
    }

    /// Pull a named integer out of an out-params object.
    fn read_scalar(&self, out: &IWbemClassObject, name: &str) -> Result<u64> {
        unsafe {
            let mut v = VARIANT::default();
            out.Get(&BSTR::from(name), 0, &mut v, None, None)?;

            i64::try_from(&v)
                .map(|n| n as u64)
                .map_err(|_| WmiError::Protocol(format!("{name} was not an integer")))
        }
    }

    /// Pull a named `VT_ARRAY | VT_UI1` out-parameter into an owned `Vec<u8>`.
    fn read_byte_array(&self, out: &IWbemClassObject, name: &str) -> Result<Vec<u8>> {
        use windows::Win32::System::Com::SAFEARRAY;
        use windows::Win32::System::Ole::{
            SafeArrayAccessData, SafeArrayGetLBound, SafeArrayGetUBound, SafeArrayUnaccessData,
        };
        use windows::Win32::System::Variant::{VT_ARRAY, VT_UI1};

        unsafe {
            let mut v = VARIANT::default();
            out.Get(&BSTR::from(name), 0, &mut v, None, None)?;

            // Reuse the layout mirror to reach `parray` — windows_core exposes
            // no typed accessor for the array arm. `as_raw` hands us the same
            // repr(C) bytes RawVariant models.
            let raw: &RawVariant = &*(v.as_raw() as *const _ as *const RawVariant);
            if raw.vt != (VT_ARRAY.0 | VT_UI1.0) {
                return Err(WmiError::Protocol(format!(
                    "{name} was not a byte array (vt=0x{:04X})",
                    raw.vt
                )));
            }

            let sa = raw.parray as *const SAFEARRAY;
            if sa.is_null() {
                return Ok(Vec::new());
            }

            let lo = SafeArrayGetLBound(sa, 1)?;
            let hi = SafeArrayGetUBound(sa, 1)?;
            let len = (hi - lo + 1).max(0) as usize;

            let mut data: *mut core::ffi::c_void = std::ptr::null_mut();
            SafeArrayAccessData(sa, &mut data)?;
            let slice = std::slice::from_raw_parts(data as *const u8, len);
            let owned = slice.to_vec();
            let _ = SafeArrayUnaccessData(sa);

            Ok(owned)
        }
    }
}

/// Mirror of the OLE `VARIANT` layout, narrowed to the one case we construct.
///
/// `windows_core::VARIANT` is `#[repr(transparent)]` over a `#[repr(C)]`
/// `VARIANT`, but its `from_raw` constructor takes a crate-private type, so
/// there is no public way to build a `VT_ARRAY` variant. Mirroring the layout
/// and transmuting is the supported workaround; the size assertion below is
/// what keeps it honest if the crate ever changes shape.
#[repr(C)]
struct RawVariant {
    vt: u16,
    reserved1: u16,
    reserved2: u16,
    reserved3: u16,
    /// `parray` arm of the payload union — valid only while `vt` has VT_ARRAY set.
    parray: *mut core::ffi::c_void,
    /// The payload union is 16 bytes wide (its widest arm is `DECIMAL`), so the
    /// pointer arm leaves 8 bytes of tail padding that must still be present.
    _tail: u64,
}

const _: () = assert!(
    core::mem::size_of::<RawVariant>() == core::mem::size_of::<VARIANT>(),
    "RawVariant no longer matches windows_core::VARIANT layout — \
     byte_array_variant would corrupt memory"
);

/// Build a `VT_ARRAY | VT_UI1` VARIANT from a byte slice.
///
/// Ownership of the SAFEARRAY transfers to the returned VARIANT, which frees it
/// on drop — do not destroy it here. On the error path we do own it, and must.
unsafe fn byte_array_variant(bytes: &[u8]) -> Result<VARIANT> {
    use windows::Win32::System::Ole::{
        SafeArrayCreateVector, SafeArrayDestroy, SafeArrayPutElement,
    };
    use windows::Win32::System::Variant::{VT_ARRAY, VT_UI1};

    let sa = SafeArrayCreateVector(VT_UI1, 0, bytes.len() as u32);
    if sa.is_null() {
        return Err(WmiError::Protocol(
            "failed to allocate SAFEARRAY for byte payload".into(),
        ));
    }

    for (i, b) in bytes.iter().enumerate() {
        let idx = i as i32;
        if let Err(e) = SafeArrayPutElement(sa, &idx, (b as *const u8).cast()) {
            // We still own the array — don't leak it on the way out.
            let _ = SafeArrayDestroy(sa);
            return Err(WmiError::from(e));
        }
    }

    let raw = RawVariant {
        vt: (VT_ARRAY.0 | VT_UI1.0),
        reserved1: 0,
        reserved2: 0,
        reserved3: 0,
        parray: sa.cast(),
        _tail: 0,
    };

    Ok(core::mem::transmute::<RawVariant, VARIANT>(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Connecting is expected to fail unelevated — assert the error is the
    /// *specific* one, so a genuine regression doesn't hide behind it.
    #[test]
    fn connect_reports_access_denied_or_succeeds() {
        match Wmi::connect() {
            Ok(_) => { /* elevated on supported hardware */ }
            Err(WmiError::AccessDenied) => { /* expected unelevated */ }
            Err(WmiError::ClassNotFound) => { /* expected off-target hardware */ }
            Err(e) => panic!("unexpected connect failure: {e}"),
        }
    }
}
