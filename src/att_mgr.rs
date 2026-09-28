use std::sync::Mutex;

use kmr_common::km_err;
use kmr_ta::device::RetrieveAttestationIds;
use kmr_wire::AttestationIdInfo;

use crate::config::config;

pub struct AttestationIdMgr;

static ATTESTATION_IDS: Mutex<Option<AttestationIdInfo>> = Mutex::new(None);

impl RetrieveAttestationIds for AttestationIdMgr {
    fn get(&self) -> Result<AttestationIdInfo, kmr_common::Error> {
        self.get_ids()?
            .ok_or_else(|| km_err!(CannotAttestIds, "attestation ID info not available"))
    }

    fn get_ids(&self) -> Result<Option<AttestationIdInfo>, kmr_common::Error> {
        let mut cached = ATTESTATION_IDS
            .lock()
            .map_err(|_| km_err!(UnknownError, "attestation ID cache lock poisoned"))?;
        if let Some(ids) = cached.as_ref() {
            return Ok(Some(ids.clone()));
        }

        let guard = config()
            .read()
            .map_err(|_| km_err!(UnknownError, "config lock poisoned"))?;
        let mut device = guard.device.clone();
        let overridden = device.override_telephony_properties;

        // Telephony identifiers are resolved per field. A MEID is a CDMA
        // concept that GSM-only SKUs never report, so an absent MEID must not
        // suppress the IMEIs that are present, and an identifier that this
        // hardware does not have is not an attestation failure.
        let has_missing_id = |device: &crate::config::DeviceProperty| {
            [
                device.imei.as_str(),
                device.imei2.as_str(),
                device.meid.as_str(),
            ]
            .iter()
            .any(|value| value.trim().is_empty())
        };
        let mut deferred = false;
        if !overridden && has_missing_id(&device) {
            drop(guard);
            match crate::plat::device_ids::resolve_runtime_device_ids() {
                Ok(Some(runtime)) => merge_missing_ids(&mut device, &runtime),
                // Telephony is not ready yet. Serve what we have and retry on
                // a later call rather than reporting a false attestation error.
                Ok(None) => deferred = true,
                Err(error) => {
                    log::warn!("failed to resolve runtime attestation IDs: {error:#}");
                    deferred = true;
                }
            }
        }

        let ids = AttestationIdInfo {
            brand: device.brand.into_bytes(),
            device: device.device.into_bytes(),
            product: device.product.into_bytes(),
            serial: device.serial.into_bytes(),
            imei: device.imei.into_bytes(),
            imei2: device.imei2.into_bytes(),
            meid: device.meid.into_bytes(),
            manufacturer: device.manufacturer.into_bytes(),
            model: device.model.into_bytes(),
        };
        // A deferred resolution is not cached, so identifiers that arrive once
        // telephony is up are still picked up. A genuinely absent identifier
        // caches, because the attempt that proved it is no longer deferred.
        if !deferred {
            *cached = Some(ids.clone());
        }
        Ok(Some(ids))
    }

    fn get_alternate_ids(&self) -> Result<Option<AttestationIdInfo>, kmr_common::Error> {
        let Some(hardware) = hardware_product_ids() else {
            return Ok(None);
        };
        let Some(configured) = self.get_ids()? else {
            return Ok(Some(hardware));
        };
        if identities_match(&configured, &hardware) {
            return Ok(None);
        }
        log::info!(
            "alternate attestation IDs from ROM: brand={} device={}",
            String::from_utf8_lossy(&hardware.brand),
            String::from_utf8_lossy(&hardware.device)
        );
        Ok(Some(hardware))
    }

    fn destroy_all(&mut self) -> Result<(), kmr_common::Error> {
        // ignore this
        Ok(())
    }
}

fn hardware_product_ids() -> Option<AttestationIdInfo> {
    let brand = product_prop("ro.product.brand")?;
    let device = product_prop("ro.product.device")?;
    let product = product_prop("ro.product.name").unwrap_or_else(|| device.clone());
    let manufacturer = product_prop("ro.product.manufacturer").unwrap_or_else(|| brand.clone());
    let model = product_prop("ro.product.model").unwrap_or_default();
    Some(AttestationIdInfo {
        brand: brand.into_bytes(),
        device: device.into_bytes(),
        product: product.into_bytes(),
        serial: Vec::new(),
        imei: Vec::new(),
        imei2: Vec::new(),
        meid: Vec::new(),
        manufacturer: manufacturer.into_bytes(),
        model: model.into_bytes(),
    })
}

fn product_prop(key: &str) -> Option<String> {
    crate::plat::vbmeta::read_build_prop_value(key)
        .or_else(|| crate::plat::resetprop::read_string_property(key))
}

fn identities_match(left: &AttestationIdInfo, right: &AttestationIdInfo) -> bool {
    left.brand == right.brand
        && left.device == right.device
        && left.product == right.product
        && left.manufacturer == right.manufacturer
        && left.model == right.model
}

/// Fills only the identifiers the config left blank. Values already present
/// are left untouched so a configured value is never silently replaced.
fn merge_missing_ids(
    device: &mut crate::config::DeviceProperty,
    runtime: &crate::config::DeviceProperty,
) {
    for (target, source) in [
        (&mut device.imei, &runtime.imei),
        (&mut device.imei2, &runtime.imei2),
        (&mut device.meid, &runtime.meid),
    ] {
        if target.trim().is_empty() && !source.trim().is_empty() {
            *target = source.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::merge_missing_ids;
    use crate::config::DeviceProperty;

    fn device(imei: &str, imei2: &str, meid: &str) -> DeviceProperty {
        DeviceProperty {
            imei: imei.to_string(),
            imei2: imei2.to_string(),
            meid: meid.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn absent_meid_keeps_resolved_imeis() {
        // GSM-only hardware: runtime reports IMEIs but no MEID at all.
        let mut target = device("", "", "");
        merge_missing_ids(
            &mut target,
            &device("865950051121317", "865950051121325", ""),
        );
        assert_eq!(target.imei, "865950051121317");
        assert_eq!(target.imei2, "865950051121325");
        // Absent MEID stays absent rather than becoming an attestation error.
        assert!(target.meid.trim().is_empty());
    }

    #[test]
    fn blank_only_is_treated_as_missing() {
        let mut target = device("  ", "", "\t");
        merge_missing_ids(&mut target, &device("imei-a", "imei-b", "meid-c"));
        assert_eq!(target.imei, "imei-a");
        assert_eq!(target.imei2, "imei-b");
        assert_eq!(target.meid, "meid-c");
    }

    #[test]
    fn configured_values_are_never_replaced() {
        let mut target = device("configured-imei", "configured-imei2", "");
        merge_missing_ids(&mut target, &device("hw-imei", "hw-imei2", "hw-meid"));
        assert_eq!(target.imei, "configured-imei");
        assert_eq!(target.imei2, "configured-imei2");
        // Only the blank field is backfilled.
        assert_eq!(target.meid, "hw-meid");
    }

    #[test]
    fn empty_runtime_adds_nothing() {
        let mut target = device("configured-imei", "", "");
        merge_missing_ids(&mut target, &device("", "", ""));
        assert_eq!(target.imei, "configured-imei");
        assert!(target.imei2.trim().is_empty());
        assert!(target.meid.trim().is_empty());
    }
}
