use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use crate::hook::binder::LocalBinderTarget;
use crate::identify;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecurityLevelTargetInfo {
    pub security_level: crate::android::hardware::security::keymint::SecurityLevel::SecurityLevel,
}

static SECURITY_LEVEL_TARGETS: LazyLock<
    Mutex<HashMap<LocalBinderTarget, SecurityLevelTargetInfo>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));
static BINDER_INTERFACES: LazyLock<Mutex<HashMap<LocalBinderTarget, &'static str>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
#[cfg(test)]
static STATE_TEST_LOCK: Mutex<()> = Mutex::new(());

fn intern_keystore_interface(interface: &str) -> Option<&'static str> {
    identify::KNOWN_KEYSTORE_INTERFACES
        .iter()
        .copied()
        .find(|&known| known == interface)
}

pub(crate) fn accept_binder_interface(target: LocalBinderTarget, interface: &str) -> bool {
    if target.ptr == 0 {
        return true;
    }
    let Some(interface) = intern_keystore_interface(interface) else {
        return false;
    };
    let mut map = BINDER_INTERFACES
        .lock()
        .expect("binder interface map poisoned");
    match map.entry(target) {
        std::collections::hash_map::Entry::Occupied(entry) => *entry.get() == interface,
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(interface);
            true
        }
    }
}

pub(crate) fn forget_binder_interface(target: LocalBinderTarget) {
    BINDER_INTERFACES
        .lock()
        .expect("binder interface map poisoned")
        .remove(&target);
}

#[cfg(test)]
pub(crate) fn lookup_binder_interface_for_tests(target: LocalBinderTarget) -> Option<&'static str> {
    BINDER_INTERFACES
        .lock()
        .expect("binder interface map poisoned")
        .get(&target)
        .copied()
}

pub(crate) fn remember_security_level_target(
    target: LocalBinderTarget,
    info: SecurityLevelTargetInfo,
) {
    SECURITY_LEVEL_TARGETS
        .lock()
        .expect("security level target map poisoned")
        .insert(target, info);
}

pub(crate) fn lookup_security_level_target(
    target: LocalBinderTarget,
) -> Option<SecurityLevelTargetInfo> {
    SECURITY_LEVEL_TARGETS
        .lock()
        .expect("security level target map poisoned")
        .get(&target)
        .copied()
}

#[cfg(test)]
pub fn clear_state_for_tests() {
    SECURITY_LEVEL_TARGETS
        .lock()
        .expect("security level target map poisoned")
        .clear();
    BINDER_INTERFACES
        .lock()
        .expect("binder interface map poisoned")
        .clear();
}

#[cfg(test)]
pub fn state_test_guard() -> std::sync::MutexGuard<'static, ()> {
    let guard = STATE_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    clear_state_for_tests();
    guard
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binder_interface_pin_rejects_mismatched_token() {
        let _guard = state_test_guard();
        let service = LocalBinderTarget {
            ptr: 0x1000,
            cookie: 0x2000,
        };
        let maintenance = LocalBinderTarget {
            ptr: 0x1001,
            cookie: 0x2001,
        };
        let reused = LocalBinderTarget {
            ptr: 0x1000,
            cookie: 0x2002,
        };
        assert!(accept_binder_interface(
            service,
            crate::identify::KEYSTORE_SERVICE_INTERFACE
        ));
        assert!(!accept_binder_interface(
            service,
            crate::identify::KEYSTORE_MAINTENANCE_INTERFACE
        ));
        assert!(accept_binder_interface(
            service,
            crate::identify::KEYSTORE_SERVICE_INTERFACE
        ));
        assert_eq!(
            lookup_binder_interface_for_tests(service),
            Some(crate::identify::KEYSTORE_SERVICE_INTERFACE)
        );
        assert!(accept_binder_interface(
            maintenance,
            crate::identify::KEYSTORE_MAINTENANCE_INTERFACE
        ));
        assert_eq!(
            lookup_binder_interface_for_tests(maintenance),
            Some(crate::identify::KEYSTORE_MAINTENANCE_INTERFACE)
        );
        assert!(accept_binder_interface(
            LocalBinderTarget { ptr: 0, cookie: 1 },
            crate::identify::KEYSTORE_MAINTENANCE_INTERFACE
        ));
        assert!(accept_binder_interface(
            reused,
            crate::identify::KEYSTORE_SECURITY_LEVEL_INTERFACE
        ));
        assert_eq!(
            lookup_binder_interface_for_tests(reused),
            Some(crate::identify::KEYSTORE_SECURITY_LEVEL_INTERFACE)
        );
        forget_binder_interface(service);
        assert!(accept_binder_interface(
            service,
            crate::identify::KEYSTORE_SECURITY_LEVEL_INTERFACE
        ));
        assert_eq!(
            lookup_binder_interface_for_tests(service),
            Some(crate::identify::KEYSTORE_SECURITY_LEVEL_INTERFACE)
        );
    }
}
