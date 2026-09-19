use std::collections::BTreeMap;

pub const TOML_PATH: &str = "/data/adb/omk/integrity.toml";
pub const PROP_PATH: &str = "/data/adb/omk/integrity.prop";
pub const TOML_PATH_FALLBACK: &str = "/data/misc/keystore/omk/data/integrity.toml";
pub const PROP_PATH_FALLBACK: &str = "/data/misc/keystore/omk/data/integrity.prop";

#[repr(C)]
#[derive(Clone, Copy)]
pub struct IntegrityPayload {
    pub enabled: u8,
    pub spoof_build: u8,
    pub spoof_props: u8,
    pub spoof_vending: u8,
    pub fingerprint: [u8; 384],
    pub brand: [u8; 64],
    pub product: [u8; 64],
    pub device: [u8; 64],
    pub model: [u8; 64],
    pub manufacturer: [u8; 64],
    pub id: [u8; 64],
    pub incremental: [u8; 64],
    pub type_: [u8; 32],
    pub tags: [u8; 32],
    pub release: [u8; 32],
    pub security_patch: [u8; 16],
    pub initial_sdk: [u8; 8],
}

impl Default for IntegrityPayload {
    fn default() -> Self {
        Self {
            enabled: 0,
            spoof_build: 1,
            spoof_props: 1,
            spoof_vending: 1,
            fingerprint: [0; 384],
            brand: [0; 64],
            product: [0; 64],
            device: [0; 64],
            model: [0; 64],
            manufacturer: [0; 64],
            id: [0; 64],
            incremental: [0; 64],
            type_: [0; 32],
            tags: [0; 32],
            release: [0; 32],
            security_patch: [0; 16],
            initial_sdk: [0; 8],
        }
    }
}

fn write_cstr(dest: &mut [u8], value: &str) {
    dest.fill(0);
    let bytes = value.as_bytes();
    let len = bytes.len().min(dest.len().saturating_sub(1));
    dest[..len].copy_from_slice(&bytes[..len]);
}

fn parse_bool(value: Option<&str>, default: bool) -> bool {
    match value.map(str::trim).unwrap_or("") {
        "" => default,
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        other => match other.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        },
    }
}

fn parse_kv(content: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for raw in content.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        map.insert(key.trim().to_string(), value.trim().to_string());
    }
    map
}

fn expand_fingerprint(payload: &mut IntegrityPayload, fingerprint: &str) {
    write_cstr(&mut payload.fingerprint, fingerprint);
    let parts: Vec<&str> = fingerprint.split(['/', ':']).collect();
    let get = |idx: usize| parts.get(idx).copied().unwrap_or("");
    write_cstr(&mut payload.brand, get(0));
    write_cstr(&mut payload.product, get(1));
    write_cstr(&mut payload.device, get(2));
    write_cstr(&mut payload.release, get(3));
    write_cstr(&mut payload.id, get(4));
    write_cstr(&mut payload.incremental, get(5));
    write_cstr(&mut payload.type_, get(6));
    write_cstr(&mut payload.tags, get(7));
}

pub fn parse_files(toml: &str, prop: &str) -> IntegrityPayload {
    let mut payload = IntegrityPayload::default();
    let toml_map = parse_kv(toml);
    payload.enabled = parse_bool(toml_map.get("enabled").map(String::as_str), false) as u8;
    payload.spoof_build = parse_bool(toml_map.get("spoof_build").map(String::as_str), true) as u8;
    payload.spoof_props = parse_bool(toml_map.get("spoof_props").map(String::as_str), true) as u8;
    payload.spoof_vending = parse_bool(
        toml_map.get("spoof_vending_finger").map(String::as_str),
        true,
    ) as u8;

    let prop_map = parse_kv(prop);
    if let Some(fingerprint) = prop_map.get("FINGERPRINT") {
        expand_fingerprint(&mut payload, fingerprint);
    }
    if let Some(value) = prop_map.get("BRAND") {
        write_cstr(&mut payload.brand, value);
    }
    if let Some(value) = prop_map.get("PRODUCT") {
        write_cstr(&mut payload.product, value);
    }
    if let Some(value) = prop_map.get("DEVICE") {
        write_cstr(&mut payload.device, value);
    }
    if let Some(value) = prop_map.get("MODEL") {
        write_cstr(&mut payload.model, value);
    }
    if let Some(value) = prop_map.get("MANUFACTURER") {
        write_cstr(&mut payload.manufacturer, value);
    }
    if let Some(value) = prop_map.get("ID") {
        write_cstr(&mut payload.id, value);
    }
    if let Some(value) = prop_map.get("INCREMENTAL") {
        write_cstr(&mut payload.incremental, value);
    }
    if let Some(value) = prop_map.get("TYPE") {
        write_cstr(&mut payload.type_, value);
    }
    if let Some(value) = prop_map.get("TAGS") {
        write_cstr(&mut payload.tags, value);
    }
    if let Some(value) = prop_map.get("RELEASE") {
        write_cstr(&mut payload.release, value);
    }
    if let Some(value) = prop_map.get("SECURITY_PATCH") {
        write_cstr(&mut payload.security_patch, value);
    }
    if let Some(value) = prop_map.get("DEVICE_INITIAL_SDK_INT") {
        write_cstr(&mut payload.initial_sdk, value);
    } else {
        write_cstr(&mut payload.initial_sdk, "32");
    }

    if payload.enabled == 1 && payload.fingerprint[0] == 0 {
        payload.enabled = 0;
    }
    payload
}

fn read_first(paths: &[&str]) -> String {
    for path in paths {
        if let Ok(content) = std::fs::read_to_string(path) {
            if !content.is_empty() {
                return content;
            }
        }
    }
    String::new()
}

pub fn load_from_disk() -> IntegrityPayload {
    let toml = read_first(&[TOML_PATH, TOML_PATH_FALLBACK]);
    let prop = read_first(&[PROP_PATH, PROP_PATH_FALLBACK]);
    parse_files(&toml, &prop)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cstr(buf: &[u8]) -> &str {
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        std::str::from_utf8(&buf[..end]).unwrap()
    }

    #[test]
    fn expands_pixel_fingerprint() {
        let payload = parse_files(
            "enabled = true\nspoof_build = true\n",
            "FINGERPRINT=google/oriole/oriole:16/BP2A.250605.031.A2/123456:user/release-keys\nMODEL=Pixel 6\nMANUFACTURER=Google\nSECURITY_PATCH=2025-06-05\n",
        );
        assert_eq!(payload.enabled, 1);
        assert_eq!(cstr(&payload.brand), "google");
        assert_eq!(cstr(&payload.product), "oriole");
        assert_eq!(cstr(&payload.device), "oriole");
        assert_eq!(cstr(&payload.release), "16");
        assert_eq!(cstr(&payload.id), "BP2A.250605.031.A2");
        assert_eq!(cstr(&payload.incremental), "123456");
        assert_eq!(cstr(&payload.type_), "user");
        assert_eq!(cstr(&payload.tags), "release-keys");
        assert_eq!(cstr(&payload.model), "Pixel 6");
        assert_eq!(cstr(&payload.manufacturer), "Google");
        assert_eq!(cstr(&payload.security_patch), "2025-06-05");
        assert_eq!(cstr(&payload.initial_sdk), "32");
    }

    #[test]
    fn disable_without_fingerprint() {
        let payload = parse_files("enabled = true\n", "");
        assert_eq!(payload.enabled, 0);
    }
}
