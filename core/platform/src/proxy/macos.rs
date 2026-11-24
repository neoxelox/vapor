//! macOS proxy settings, from the System Configuration framework: the
//! proxies of the network the device is on now, as System Settings
//! shows them under Network, Proxies.
#![allow(unsafe_code)]

use std::ffi::c_void;

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;

use super::{ProxySettings, SystemProxy};

#[link(name = "SystemConfiguration", kind = "framework")]
unsafe extern "C" {
    fn SCDynamicStoreCopyProxies(store: *const c_void) -> CFDictionaryRef;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NativeProxySettings;

impl NativeProxySettings {
    pub fn for_current_host() -> Self {
        Self
    }
}

impl ProxySettings for NativeProxySettings {
    fn https_proxy(&self) -> Option<SystemProxy> {
        // SAFETY: a null store asks for a temporary session, which the
        // function documents. The dictionary that comes back is owned
        // by the caller (the Copy rule), and null means "no answer".
        let raw = unsafe { SCDynamicStoreCopyProxies(std::ptr::null()) };
        if raw.is_null() {
            return None;
        }
        // SAFETY: `raw` is a non-null dictionary this call owns.
        let proxies: CFDictionary<CFString, CFType> =
            unsafe { CFDictionary::wrap_under_create_rule(raw) };
        let value = |key: &'static str| proxies.find(CFString::from_static_string(key));
        let number = |key: &'static str| {
            value(key)
                .and_then(|value| value.downcast::<CFNumber>())
                .and_then(|number| number.to_i64())
        };
        if number("HTTPSEnable") != Some(1) {
            return None;
        }
        let host = value("HTTPSProxy")
            .and_then(|value| value.downcast::<CFString>())
            .map(|host| host.to_string())
            .filter(|host| !host.trim().is_empty())?;
        let port = number("HTTPSPort")
            .and_then(|port| u16::try_from(port).ok())
            .filter(|port| *port != 0)?;
        let bypass = value("ExceptionsList")
            .and_then(|value| value.downcast::<CFArray>())
            .map(|rules| {
                rules
                    .iter()
                    .filter_map(|rule| {
                        // SAFETY: the array holds Core Foundation
                        // objects; each one is retained for the life of
                        // the wrapper, and a value that is no string is
                        // dropped by the downcast.
                        unsafe { CFType::wrap_under_get_rule(*rule) }.downcast::<CFString>()
                    })
                    .map(|rule| rule.to_string())
                    .collect()
            })
            .unwrap_or_default();
        Some(SystemProxy { host, port, bypass })
    }
}
