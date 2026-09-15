use std::{
    cell::Cell,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, OnceLock, RwLock,
    },
};

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use der::Encode;
use kmr_common::{
    crypto::{ec, rsa, KeyMaterial, Sha256},
    runtime::fs::atomic_replace_with_metadata,
    Error,
};
use kmr_crypto_boring::{ec::BoringEc, mldsa::BoringMlDsa, rsa::BoringRsa, sha256::BoringSha256};
use kmr_ta::device::{
    RetrieveCertSigningInfo, SigningAlgorithm, SigningInfoSnapshot, SigningKeyType,
};
use kmr_wire::keymint;
use log::{debug, error, info, warn};
use regex::Regex;
use x509_cert::der as x509_der;
use x509_cert::Certificate;

pub const KEYBOX_PATH: &str = "/data/misc/keystore/omk/keybox.xml";
pub const KEYBOX_DIRECTORY: &str = "/data/misc/keystore/omk";
pub const MAX_KEYBOX_SLOT: u32 = 1024;

const BUNDLED_KEYBOX_XML: &str = include_str!("../template/keybox.xml");

lazy_static::lazy_static! {
    pub static ref KEYBOX: RwLock<KeyBox> = RwLock::new(KeyBox::new());
    static ref KEYBOX_IO_LOCK: Mutex<()> = Mutex::new(());
    static ref KEY_BLOCK_RE: Regex =
        Regex::new(r#"(?s)<Key\s+algorithm="([^"]+)">\s*(.*?)\s*</Key>"#).unwrap();
    static ref PRIVATE_KEY_RE: Regex =
        Regex::new(r#"(?s)<PrivateKey[^>]*>\s*(.*?)\s*</PrivateKey>"#).unwrap();
    static ref CERT_COUNT_RE: Regex =
        Regex::new(r#"(?s)<NumberOfCertificates>\s*(\d+)\s*</NumberOfCertificates>"#).unwrap();
    static ref CERT_RE: Regex =
        Regex::new(r#"(?s)<Certificate(?:\s+[^>]*)?>\s*(.*?)\s*</Certificate>"#).unwrap();
}

static KEYBOX_WATCHER: OnceLock<()> = OnceLock::new();
static KEYBOX_DB_RETIRE_ALLOWED: AtomicBool = AtomicBool::new(false);
static KEYBOX_RUNTIME_LOADED: AtomicBool = AtomicBool::new(false);

thread_local! {
    // The initializer is already const; current nightly Clippy reports a
    // false positive for this macro expansion.
    #[allow(clippy::missing_const_for_thread_local)]
    static ACTIVE_KEYBOX_SLOT: Cell<u32> = const { Cell::new(0) };
}

#[derive(Clone)]
pub struct CertSignAlgoInfo {
    key: KeyMaterial,
    key_der: Vec<u8>,
    chain: Vec<keymint::Certificate>,
}

#[derive(Clone)]
pub struct KeyBox {
    rsa_info: CertSignAlgoInfo,
    ec_info: CertSignAlgoInfo,
    identity_digest: [u8; 32],
}

#[derive(Clone, Copy)]
enum KeyAlgorithm {
    Ec,
    Rsa,
}

struct ParsedKeyEntry {
    key_der: Vec<u8>,
    chain: Vec<Vec<u8>>,
}

impl KeyBox {
    pub fn new() -> Self {
        Self::from_xml_str(BUNDLED_KEYBOX_XML).expect("bundled keybox.xml must be valid")
    }

    pub fn from_xml_str(xml: &str) -> Result<Self> {
        let mut rsa_entry = None;
        let mut ec_entry = None;

        for captures in KEY_BLOCK_RE.captures_iter(xml) {
            let algorithm = match captures.get(1).map(|m| m.as_str().trim()) {
                Some("ecdsa") | Some("ec") => KeyAlgorithm::Ec,
                Some("rsa") => KeyAlgorithm::Rsa,
                Some(other) => bail!("unsupported key algorithm `{other}` in keybox.xml"),
                None => bail!("missing key algorithm in keybox.xml"),
            };
            let body = captures
                .get(2)
                .map(|m| m.as_str())
                .ok_or_else(|| anyhow!("missing key block body"))?;
            let entry = ParsedKeyEntry::from_xml_block(body).with_context(|| {
                format!("failed to parse {:?} key entry", algorithm_name(algorithm))
            })?;
            match algorithm {
                KeyAlgorithm::Ec => ec_entry = Some(entry),
                KeyAlgorithm::Rsa => rsa_entry = Some(entry),
            }
        }

        let rsa_entry = rsa_entry.context("missing RSA key entry in keybox.xml")?;
        let ec_entry = ec_entry.context("missing EC key entry in keybox.xml")?;

        let rsa_info = Self::build_rsa_info(rsa_entry)?;
        let ec_info = Self::build_ec_info(ec_entry)?;
        let identity_digest = Self::compute_identity_digest(&rsa_info, &ec_info)?;

        Ok(Self {
            rsa_info,
            ec_info,
            identity_digest,
        })
    }

    fn build_rsa_info(entry: ParsedKeyEntry) -> Result<CertSignAlgoInfo> {
        if entry.chain.is_empty() {
            bail!("RSA certificate chain is empty");
        }
        let key = rsa::import_pkcs1_key(&entry.key_der)
            .map(|(key, _, _)| key)
            .map_err(|e| anyhow!("failed to import RSA private key: {e:?}"))?;
        let chain: Vec<keymint::Certificate> = entry
            .chain
            .into_iter()
            .map(|encoded_certificate| keymint::Certificate {
                encoded_certificate,
            })
            .collect();
        validate_chain_matches_key(&key, &chain, KeyAlgorithm::Rsa)?;
        Ok(CertSignAlgoInfo {
            key,
            key_der: entry.key_der,
            chain,
        })
    }

    fn build_ec_info(entry: ParsedKeyEntry) -> Result<CertSignAlgoInfo> {
        if entry.chain.is_empty() {
            bail!("EC certificate chain is empty");
        }
        let key = ec::import_sec1_private_key(&entry.key_der)
            .map_err(|e| anyhow!("failed to import EC private key: {e:?}"))?;
        let chain: Vec<keymint::Certificate> = entry
            .chain
            .into_iter()
            .map(|encoded_certificate| keymint::Certificate {
                encoded_certificate,
            })
            .collect();
        validate_chain_matches_key(&key, &chain, KeyAlgorithm::Ec)?;
        Ok(CertSignAlgoInfo {
            key,
            key_der: entry.key_der,
            chain,
        })
    }

    fn compute_identity_digest(
        rsa_info: &CertSignAlgoInfo,
        ec_info: &CertSignAlgoInfo,
    ) -> Result<[u8; 32]> {
        let mut material = Vec::new();
        append_labeled_bytes(&mut material, b"rsa-key", &rsa_info.key_der);
        append_labeled_chain(&mut material, b"rsa-chain", &rsa_info.chain);
        append_labeled_bytes(&mut material, b"ec-key", &ec_info.key_der);
        append_labeled_chain(&mut material, b"ec-chain", &ec_info.chain);

        BoringSha256 {}
            .hash(&material)
            .map_err(|e| anyhow!("failed to hash keybox identity: {e:?}"))
    }

    fn refresh_identity_digest(&mut self) -> Result<()> {
        self.identity_digest = Self::compute_identity_digest(&self.rsa_info, &self.ec_info)?;
        Ok(())
    }

    pub fn identity_digest(&self) -> [u8; 32] {
        self.identity_digest
    }

    fn signing_info(&self, key_type: SigningKeyType) -> Result<SigningInfoSnapshot, Error> {
        let (signing_key, cert_chain) = match key_type.algo_hint {
            SigningAlgorithm::Rsa => (&self.rsa_info.key, &self.rsa_info.chain),
            SigningAlgorithm::Ec => (&self.ec_info.key, &self.ec_info.chain),
        };

        Ok(SigningInfoSnapshot {
            signing_key: signing_key.clone(),
            cert_chain: cert_chain.clone(),
            identity_digest: self.identity_digest,
        })
    }

    pub fn update_rsa_keybox(
        &mut self,
        key_der: Vec<u8>,
        chain: Vec<keymint::Certificate>,
    ) -> Result<()> {
        self.update_keybox(KeyAlgorithm::Rsa, key_der, chain)
    }

    pub fn update_ec_keybox(
        &mut self,
        key_der: Vec<u8>,
        chain: Vec<keymint::Certificate>,
    ) -> Result<()> {
        self.update_keybox(KeyAlgorithm::Ec, key_der, chain)
    }

    fn update_keybox(
        &mut self,
        algorithm: KeyAlgorithm,
        key_der: Vec<u8>,
        chain: Vec<keymint::Certificate>,
    ) -> Result<()> {
        let entry = ParsedKeyEntry {
            key_der,
            chain: chain
                .into_iter()
                .map(|certificate| certificate.encoded_certificate)
                .collect(),
        };
        match algorithm {
            KeyAlgorithm::Ec => self.ec_info = Self::build_ec_info(entry)?,
            KeyAlgorithm::Rsa => self.rsa_info = Self::build_rsa_info(entry)?,
        }
        self.refresh_identity_digest()
    }

    pub fn to_xml_string(&self) -> String {
        format!(
            concat!(
                "<?xml version=\"1.0\"?>\n",
                "<AndroidAttestation>\n",
                "<NumberOfKeyboxes>2</NumberOfKeyboxes>\n",
                "<Keybox DeviceID=\"sw\">\n",
                "{}\n",
                "{}\n",
                "</Keybox>\n",
                "</AndroidAttestation>\n"
            ),
            self.to_xml_block(KeyAlgorithm::Ec),
            self.to_xml_block(KeyAlgorithm::Rsa),
        )
    }

    fn to_xml_block(&self, algorithm: KeyAlgorithm) -> String {
        let (name, private_label, info) = match algorithm {
            KeyAlgorithm::Ec => ("ecdsa", "EC PRIVATE KEY", &self.ec_info),
            KeyAlgorithm::Rsa => ("rsa", "RSA PRIVATE KEY", &self.rsa_info),
        };
        let certificates = info
            .chain
            .iter()
            .map(|certificate| {
                format!(
                    "<Certificate format=\"pem\">\n{}\n</Certificate>",
                    encode_pem_block("CERTIFICATE", &certificate.encoded_certificate)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            concat!(
                "<Key algorithm=\"{name}\">\n",
                "<PrivateKey format=\"pem\">\n",
                "{private_key}\n",
                "</PrivateKey>\n",
                "<CertificateChain>\n",
                "<NumberOfCertificates>{cert_count}</NumberOfCertificates>\n",
                "{certificates}\n",
                "</CertificateChain>\n",
                "</Key>"
            ),
            name = name,
            private_key = encode_pem_block(private_label, &info.key_der),
            cert_count = info.chain.len(),
            certificates = certificates,
        )
    }
}

pub fn keybox_slot_path(slot: u32) -> PathBuf {
    Path::new(KEYBOX_DIRECTORY).join(format!("keybox-slot-{slot}.xml"))
}

pub fn with_active_slot<T>(slot: u32, f: impl FnOnce() -> T) -> T {
    let slot = if (1..=MAX_KEYBOX_SLOT).contains(&slot) {
        slot
    } else {
        0
    };
    ACTIVE_KEYBOX_SLOT.with(|active| {
        let previous = active.replace(slot);
        let result = f();
        active.set(previous);
        result
    })
}

fn active_slot() -> u32 {
    ACTIVE_KEYBOX_SLOT.with(Cell::get)
}

impl Default for KeyBox {
    fn default() -> Self {
        Self::new()
    }
}

impl ParsedKeyEntry {
    fn from_xml_block(block: &str) -> Result<Self> {
        let private_key_pem = PRIVATE_KEY_RE
            .captures(block)
            .and_then(|captures| captures.get(1))
            .map(|m| m.as_str())
            .context("missing <PrivateKey> block")?;
        let key_der = decode_pem(private_key_pem)?;

        let expected_cert_count = CERT_COUNT_RE
            .captures(block)
            .and_then(|captures| captures.get(1))
            .map(|m| m.as_str())
            .context("missing <NumberOfCertificates> in certificate chain")?
            .parse::<usize>()
            .context("invalid certificate count in keybox.xml")?;

        let chain = CERT_RE
            .captures_iter(block)
            .filter_map(|captures| captures.get(1).map(|m| m.as_str()))
            .map(decode_pem)
            .collect::<Result<Vec<_>>>()?;

        if chain.len() != expected_cert_count {
            bail!(
                "certificate count mismatch: declared {}, parsed {}",
                expected_cert_count,
                chain.len()
            );
        }

        Ok(Self { key_der, chain })
    }
}

fn append_labeled_bytes(buffer: &mut Vec<u8>, label: &[u8], data: &[u8]) {
    buffer.extend_from_slice(&(label.len() as u32).to_be_bytes());
    buffer.extend_from_slice(label);
    buffer.extend_from_slice(&(data.len() as u32).to_be_bytes());
    buffer.extend_from_slice(data);
}

fn append_labeled_chain(buffer: &mut Vec<u8>, label: &[u8], chain: &[keymint::Certificate]) {
    buffer.extend_from_slice(&(label.len() as u32).to_be_bytes());
    buffer.extend_from_slice(label);
    buffer.extend_from_slice(&(chain.len() as u32).to_be_bytes());
    for certificate in chain {
        buffer.extend_from_slice(&(certificate.encoded_certificate.len() as u32).to_be_bytes());
        buffer.extend_from_slice(&certificate.encoded_certificate);
    }
}

fn algorithm_name(algorithm: KeyAlgorithm) -> &'static str {
    match algorithm {
        KeyAlgorithm::Ec => "EC",
        KeyAlgorithm::Rsa => "RSA",
    }
}

fn validate_chain_matches_key(
    key: &KeyMaterial,
    chain: &[keymint::Certificate],
    algorithm: KeyAlgorithm,
) -> Result<()> {
    let first_cert = chain
        .first()
        .context("certificate chain must contain a leaf certificate")?;
    let certificate = <Certificate as x509_der::Decode>::from_der(&first_cert.encoded_certificate)
        .with_context(|| {
            format!(
                "failed to parse {} leaf certificate from keybox chain",
                algorithm_name(algorithm)
            )
        })?;
    let mut spki_buf = Vec::new();
    let derived_spki = key
        .subject_public_key_info(
            &mut spki_buf,
            &BoringEc::default(),
            &BoringRsa::default(),
            &BoringMlDsa,
        )
        .map_err(|e| {
            anyhow!(
                "failed to derive {} public key info from private key: {e:?}",
                algorithm_name(algorithm)
            )
        })?
        .context("symmetric key cannot back an attestation certificate")?
        .to_der()
        .with_context(|| {
            format!(
                "failed to encode {} public key info from private key",
                algorithm_name(algorithm)
            )
        })?;
    let certificate_spki =
        x509_der::Encode::to_der(certificate.tbs_certificate().subject_public_key_info())
            .with_context(|| {
                format!(
                    "failed to encode {} public key info from certificate chain",
                    algorithm_name(algorithm)
                )
            })?;
    if derived_spki != certificate_spki {
        bail!(
            "{} certificate chain does not match the supplied private key",
            algorithm_name(algorithm)
        );
    }
    for pair in chain.windows(2) {
        let current = <Certificate as x509_der::Decode>::from_der(&pair[0].encoded_certificate)
            .context("failed to parse certificate while validating chain order")?;
        let issuer = <Certificate as x509_der::Decode>::from_der(&pair[1].encoded_certificate)
            .context("failed to parse issuer while validating chain order")?;
        let current_issuer = current.tbs_certificate().issuer().to_der()?;
        let issuer_subject = issuer.tbs_certificate().subject().to_der()?;
        if current_issuer != issuer_subject {
            bail!(
                "{} certificate chain has a broken issuer/subject link",
                algorithm_name(algorithm)
            );
        }
    }
    Ok(())
}

fn decode_pem(pem: &str) -> Result<Vec<u8>> {
    let base64_body = pem
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with("-----BEGIN ") && !line.starts_with("-----END "))
        .collect::<String>();

    if base64_body.is_empty() {
        bail!("empty PEM payload");
    }

    STANDARD
        .decode(base64_body.as_bytes())
        .context("failed to decode PEM payload")
}

fn encode_pem_block(label: &str, der: &[u8]) -> String {
    let mut pem = String::new();
    pem.push_str(&format!("-----BEGIN {label}-----\n"));
    let base64 = STANDARD.encode(der);
    for chunk in base64.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(chunk).expect("base64 is valid UTF-8"));
        pem.push('\n');
    }
    pem.push_str(&format!("-----END {label}-----"));
    pem
}

fn write_keybox_xml(path: &str, xml: &str) -> Result<()> {
    atomic_replace_with_metadata(Path::new(path), xml.as_bytes(), 0o600, 1017, 1017)
        .with_context(|| format!("failed to atomically replace keybox.xml at {path}"))
}

fn write_bundled_keybox(path: &str) -> Result<()> {
    write_keybox_xml(path, BUNDLED_KEYBOX_XML)
}

pub fn ensure_keybox_file(path: &str) -> Result<()> {
    if Path::new(path).exists() {
        return Ok(());
    }
    info!("keybox.xml missing at {}; seeding bundled template", path);
    write_bundled_keybox(path)
}

fn is_bundled_keybox_xml(xml: &str) -> bool {
    xml.trim() == BUNDLED_KEYBOX_XML.trim()
}

fn retire_stale_keybox_bound_entries(current_identity: [u8; 32]) {
    if !db_retirement_allowed() {
        warn!("skipping stale keybox-bound DB retirement while active keybox came from fallback");
        return;
    }

    match crate::global::DB.with(|db| {
        db.borrow_mut()
            .retire_stale_keybox_bound_entries(current_identity)
    }) {
        Ok(0) => debug!("no stale keybox-bound key entries needed retirement"),
        Ok(retired) => info!("retired {retired} stale keybox-bound key entries"),
        Err(error) => error!("failed to retire stale keybox-bound key entries: {error:#}"),
    }
}

fn install_keybox(
    new_keybox: KeyBox,
    retire_db_entries: bool,
    db_retirement_allowed: bool,
) -> bool {
    let new_identity = new_keybox.identity_digest();
    let changed = {
        let mut keybox = KEYBOX.write().unwrap();
        let changed = keybox.identity_digest() != new_identity;
        *keybox = new_keybox;
        changed
    };
    KEYBOX_DB_RETIRE_ALLOWED.store(db_retirement_allowed, Ordering::Release);
    KEYBOX_RUNTIME_LOADED.store(true, Ordering::Release);

    if changed {
        crate::keymaster::keymint_device::clear_initialized_attestation_caches();
    }

    if retire_db_entries {
        retire_stale_keybox_bound_entries(new_identity);
    }

    changed
}

pub fn db_retirement_allowed() -> bool {
    KEYBOX_DB_RETIRE_ALLOWED.load(Ordering::Acquire)
}

fn is_fallback_continuation(keybox: &KeyBox, contents: &str) -> bool {
    let current_identity = KEYBOX
        .read()
        .map(|current| current.identity_digest())
        .unwrap_or([0u8; 32]);
    is_fallback_continuation_with_state(
        keybox,
        contents,
        KEYBOX_RUNTIME_LOADED.load(Ordering::Acquire),
        db_retirement_allowed(),
        current_identity,
    )
}

fn is_fallback_continuation_with_state(
    keybox: &KeyBox,
    contents: &str,
    runtime_loaded: bool,
    retirement_allowed: bool,
    current_identity: [u8; 32],
) -> bool {
    runtime_loaded
        && !retirement_allowed
        && is_bundled_keybox_xml(contents)
        && current_identity == keybox.identity_digest()
}

fn load_keybox_with_fallback(path: &str) -> Result<(KeyBox, bool)> {
    match fs::read_to_string(path) {
        Ok(contents) => match KeyBox::from_xml_str(&contents) {
            Ok(keybox) => {
                let fallback_origin = is_fallback_continuation(&keybox, &contents);
                Ok((keybox, fallback_origin))
            }
            Err(error) => Err(error).with_context(|| format!("invalid keybox.xml at {path}")),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            info!("keybox.xml missing at {}; writing bundled template", path);
            write_bundled_keybox(path)?;
            Ok((KeyBox::new(), true))
        }
        Err(error) => Err(error).with_context(|| format!("failed to read keybox.xml from {path}")),
    }
}

pub fn reload_from_disk() -> Result<bool> {
    reload_from_disk_inner(true)
}

fn reload_from_disk_inner(retire_db_entries: bool) -> Result<bool> {
    let _io_guard = KEYBOX_IO_LOCK.lock().unwrap();
    let (keybox, used_fallback) = load_keybox_with_fallback(KEYBOX_PATH)?;
    let changed = install_keybox(keybox, retire_db_entries, !used_fallback);
    if changed {
        info!(
            "active keybox identity updated from {} (fallback={})",
            KEYBOX_PATH, used_fallback
        );
    } else {
        debug!(
            "keybox reload completed without identity change (fallback={})",
            used_fallback
        );
    }
    Ok(changed)
}

pub fn initialize() -> Result<()> {
    ensure_keybox_file(KEYBOX_PATH)?;
    reload_from_disk_inner(false)?;
    KEYBOX_WATCHER.get_or_init(|| {
        if let Err(error) = kmr_common::runtime::file_watch::spawn_path_watcher(
            "omk-keybox-watch",
            PathBuf::from(KEYBOX_PATH),
            |_trigger| {
                if let Err(reload_error) = reload_from_disk() {
                    error!("failed to reload keybox.xml after change: {reload_error:#}");
                }
            },
        ) {
            error!("failed to watch keybox.xml: {error:#}");
        }
    });
    Ok(())
}

pub fn update_rsa_keybox(key_der: Vec<u8>, chain: Vec<keymint::Certificate>) -> Result<bool> {
    update_keybox_file(KeyAlgorithm::Rsa, key_der, chain)
}

pub fn update_ec_keybox(key_der: Vec<u8>, chain: Vec<keymint::Certificate>) -> Result<bool> {
    update_keybox_file(KeyAlgorithm::Ec, key_der, chain)
}

fn update_keybox_file(
    algorithm: KeyAlgorithm,
    key_der: Vec<u8>,
    chain: Vec<keymint::Certificate>,
) -> Result<bool> {
    let _io_guard = KEYBOX_IO_LOCK.lock().unwrap();
    let mut keybox = KEYBOX.read().unwrap().clone();
    keybox.update_keybox(algorithm, key_der, chain)?;
    write_keybox_xml(KEYBOX_PATH, &keybox.to_xml_string())?;
    Ok(install_keybox(keybox, true, true))
}

pub fn current_identity_digest() -> [u8; 32] {
    KEYBOX.read().unwrap().identity_digest()
}

/// Return the identity digest for a keybox slot without changing the process-wide
/// default keybox. Slot zero is the legacy keybox loaded in `KEYBOX`.
pub fn identity_digest_for_slot(slot: u32) -> Result<[u8; 32]> {
    let slot = if (1..=MAX_KEYBOX_SLOT).contains(&slot) {
        slot
    } else {
        0
    };
    if slot == 0 {
        return Ok(current_identity_digest());
    }

    let path = keybox_slot_path(slot);
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("failed to read keybox slot {slot} from {}", path.display()))?;
    let keybox = KeyBox::from_xml_str(&contents)
        .with_context(|| format!("failed to parse keybox slot {slot} from {}", path.display()))?;
    Ok(keybox.identity_digest())
}

pub(crate) fn signing_certificate_ders_from_disk() -> Result<[Vec<u8>; 2]> {
    let _io_guard = KEYBOX_IO_LOCK.lock().unwrap();
    let (keybox, _) = load_keybox_with_fallback(KEYBOX_PATH)?;
    Ok([
        keybox.rsa_info.chain[0].encoded_certificate.clone(),
        keybox.ec_info.chain[0].encoded_certificate.clone(),
    ])
}

pub struct KeyboxManager;

impl RetrieveCertSigningInfo for KeyboxManager {
    fn signing_info(&self, key_type: SigningKeyType) -> Result<SigningInfoSnapshot, Error> {
        let slot = active_slot();
        if slot == 0 {
            let keybox = KEYBOX
                .read()
                .map_err(|_| kmr_common::km_err!(UnknownError, "failed to lock KEYBOX"))?;
            return keybox.signing_info(key_type);
        }

        let path = keybox_slot_path(slot);
        let contents = fs::read_to_string(&path).map_err(|error| {
            kmr_common::km_err!(
                UnknownError,
                "failed to read keybox slot {slot} from {}: {error}",
                path.display()
            )
        })?;
        let keybox = KeyBox::from_xml_str(&contents).map_err(|error| {
            kmr_common::km_err!(
                UnknownError,
                "failed to parse keybox slot {slot} from {}: {error:#}",
                path.display()
            )
        })?;
        keybox.signing_info(key_type)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kmr_ta::device::SigningKey;

    fn write_temp_keybox(name: &str, contents: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("omk-keybox-{name}-{}.xml", std::process::id()));
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn parses_bundled_template() {
        let keybox = KeyBox::from_xml_str(BUNDLED_KEYBOX_XML).unwrap();
        assert_eq!(keybox.ec_info.chain.len(), 2);
        assert_eq!(keybox.rsa_info.chain.len(), 2);
        assert_ne!(keybox.identity_digest(), [0u8; 32]);
    }

    #[test]
    fn rejects_invalid_xml() {
        assert!(KeyBox::from_xml_str("<AndroidAttestation/>").is_err());
    }

    #[test]
    fn identity_changes_when_chain_changes() {
        let original = KeyBox::from_xml_str(BUNDLED_KEYBOX_XML).unwrap();
        let mut changed = original.clone();
        changed.ec_info.chain.push(changed.ec_info.chain[0].clone());
        changed.refresh_identity_digest().unwrap();

        let modified = KeyBox::from_xml_str(&changed.to_xml_string()).unwrap();
        assert_ne!(original.identity_digest(), modified.identity_digest());
    }

    #[test]
    fn rejects_mismatched_private_key_and_certificate_chain() {
        let keybox = KeyBox::from_xml_str(BUNDLED_KEYBOX_XML).unwrap();
        let rsa_cert =
            encode_pem_block("CERTIFICATE", &keybox.rsa_info.chain[0].encoded_certificate);
        let ec_cert = encode_pem_block("CERTIFICATE", &keybox.ec_info.chain[0].encoded_certificate);
        let modified_xml = BUNDLED_KEYBOX_XML.replacen(&rsa_cert, &ec_cert, 1);
        assert!(KeyBox::from_xml_str(&modified_xml).is_err());
    }

    #[test]
    fn signing_snapshot_keeps_key_chain_and_digest_in_sync() {
        let keybox = KeyBox::from_xml_str(BUNDLED_KEYBOX_XML).unwrap();

        let rsa_snapshot = keybox
            .signing_info(SigningKeyType {
                which: SigningKey::Batch,
                algo_hint: SigningAlgorithm::Rsa,
            })
            .unwrap();
        assert_eq!(rsa_snapshot.identity_digest, keybox.identity_digest());
        validate_chain_matches_key(
            &rsa_snapshot.signing_key,
            &rsa_snapshot.cert_chain,
            KeyAlgorithm::Rsa,
        )
        .unwrap();

        let ec_snapshot = keybox
            .signing_info(SigningKeyType {
                which: SigningKey::Batch,
                algo_hint: SigningAlgorithm::Ec,
            })
            .unwrap();
        assert_eq!(ec_snapshot.identity_digest, keybox.identity_digest());
        validate_chain_matches_key(
            &ec_snapshot.signing_key,
            &ec_snapshot.cert_chain,
            KeyAlgorithm::Ec,
        )
        .unwrap();
    }

    #[test]
    fn invalid_file_is_rejected_without_replacement() {
        let path = write_temp_keybox("invalid", "<not-xml>");
        let error = match load_keybox_with_fallback(path.to_str().unwrap()) {
            Ok(_) => panic!("invalid keybox was accepted"),
            Err(error) => error,
        };
        assert!(format!("{error:#}").contains("invalid keybox.xml"));
        let written = fs::read_to_string(path).unwrap();
        assert_eq!(written, "<not-xml>");
    }

    #[test]
    fn explicit_bundled_template_is_retirement_eligible_before_runtime_fallback() {
        let keybox = KeyBox::from_xml_str(BUNDLED_KEYBOX_XML).unwrap();

        assert!(!is_fallback_continuation_with_state(
            &keybox,
            BUNDLED_KEYBOX_XML,
            false,
            false,
            keybox.identity_digest(),
        ));
    }

    #[test]
    fn non_bundled_keybox_is_retirement_eligible() {
        let modified_xml = format!("{BUNDLED_KEYBOX_XML}\n<!-- explicit local keybox -->\n");
        let path = write_temp_keybox("modified", &modified_xml);

        let (_, used_fallback) = load_keybox_with_fallback(path.to_str().unwrap()).unwrap();

        assert!(!used_fallback);
    }

    #[test]
    fn rewritten_bundled_template_can_continue_runtime_fallback() {
        let keybox = KeyBox::from_xml_str(BUNDLED_KEYBOX_XML).unwrap();

        assert!(is_fallback_continuation_with_state(
            &keybox,
            BUNDLED_KEYBOX_XML,
            true,
            false,
            keybox.identity_digest(),
        ));
        assert!(!is_fallback_continuation_with_state(
            &keybox,
            BUNDLED_KEYBOX_XML,
            true,
            true,
            keybox.identity_digest(),
        ));
    }

    #[test]
    fn slot_paths_are_numeric_and_separate_from_legacy_keybox() {
        assert_eq!(
            keybox_slot_path(2),
            Path::new(KEYBOX_DIRECTORY).join("keybox-slot-2.xml")
        );
        assert_ne!(keybox_slot_path(2), PathBuf::from(KEYBOX_PATH));
    }

    #[test]
    fn active_slot_scope_restores_previous_value() {
        assert_eq!(active_slot(), 0);
        with_active_slot(2, || {
            assert_eq!(active_slot(), 2);
            with_active_slot(3, || assert_eq!(active_slot(), 3));
            assert_eq!(active_slot(), 2);
        });
        assert_eq!(active_slot(), 0);
    }
}
