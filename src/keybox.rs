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
use quick_xml::events::Event;
use quick_xml::Reader;
use x509_cert::der as x509_der;
use x509_cert::Certificate;

pub const KEYBOX_PATH: &str = "/data/misc/keystore/omk/keybox.xml";
pub const KEYBOX_DIRECTORY: &str = "/data/misc/keystore/omk";
pub const MAX_KEYBOX_SLOT: u32 = 1024;
const LAST_GOOD_PATH: &str = "/data/misc/keystore/omk/data/keybox.last-good.xml";

const BUNDLED_KEYBOX_XML: &str = include_str!("../template/keybox.xml");

lazy_static::lazy_static! {
    pub static ref KEYBOX: RwLock<KeyBox> = RwLock::new(KeyBox::new());
    static ref KEYBOX_IO_LOCK: Mutex<()> = Mutex::new(());
}

static KEYBOX_WATCHER: OnceLock<()> = OnceLock::new();
static KEYBOX_DB_RETIRE_ALLOWED: AtomicBool = AtomicBool::new(false);
static KEYBOX_RUNTIME_LOADED: AtomicBool = AtomicBool::new(false);

thread_local! {
    // The initializer is already const; current nightly Clippy reports a
    // false positive for this macro expansion.
    #[allow(clippy::missing_const_for_thread_local)]
    static ACTIVE_KEYBOX_SLOT: Cell<u32> = const { Cell::new(0) };
    #[allow(clippy::missing_const_for_thread_local)]
    static ACTIVE_RKP_CREDENTIAL: Cell<u32> = const { Cell::new(0) };
}

#[derive(Clone)]
pub struct CertSignAlgoInfo {
    key: KeyMaterial,
    key_der: Vec<u8>,
    chain: Vec<keymint::Certificate>,
}

#[derive(Clone)]
pub struct KeyBox {
    rsa_info: Option<CertSignAlgoInfo>,
    ec_infos: Vec<CertSignAlgoInfo>,
    identity_digest: [u8; 32],
}

#[derive(Clone, Copy, PartialEq, Eq)]
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
        let (rsa_entry, ec_entries) = parse_xml_key_entries(xml)?;
        let rsa_info = rsa_entry
            .map(Self::build_rsa_info)
            .transpose()
            .context("failed to parse RSA key entry")?;
        let mut ec_infos = Vec::new();
        for (index, entry) in ec_entries.into_iter().enumerate() {
            match Self::build_ec_info(entry) {
                Ok(info) => ec_infos.push(info),
                Err(error) => warn!("skipping EC keybox entry {index}: {error:#}"),
            }
        }
        if rsa_info.is_none() && ec_infos.is_empty() {
            bail!("keybox.xml has no RSA or EC key entry");
        }
        let identity_digest = Self::compute_identity_digest(&rsa_info, &ec_infos)?;

        Ok(Self {
            rsa_info,
            ec_infos,
            identity_digest,
        })
    }

    fn build_rsa_info(entry: ParsedKeyEntry) -> Result<CertSignAlgoInfo> {
        if entry.chain.is_empty() {
            bail!("RSA certificate chain is empty");
        }
        let key = import_rsa_key_der(&entry.key_der)?;
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
        let key = import_ec_key_der(&entry.key_der)?;
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
        rsa_info: &Option<CertSignAlgoInfo>,
        ec_infos: &[CertSignAlgoInfo],
    ) -> Result<[u8; 32]> {
        let mut material = Vec::new();
        if let Some(rsa_info) = rsa_info {
            append_labeled_bytes(&mut material, b"rsa-key", &rsa_info.key_der);
            append_labeled_chain(&mut material, b"rsa-chain", &rsa_info.chain);
        }
        for (index, ec_info) in ec_infos.iter().enumerate() {
            let key_label = format!("ec-key-{index}");
            let chain_label = format!("ec-chain-{index}");
            append_labeled_bytes(&mut material, key_label.as_bytes(), &ec_info.key_der);
            append_labeled_chain(&mut material, chain_label.as_bytes(), &ec_info.chain);
        }

        BoringSha256 {}
            .hash(&material)
            .map_err(|e| anyhow!("failed to hash keybox identity: {e:?}"))
    }

    fn refresh_identity_digest(&mut self) -> Result<()> {
        self.identity_digest = Self::compute_identity_digest(&self.rsa_info, &self.ec_infos)?;
        Ok(())
    }

    pub fn identity_digest(&self) -> [u8; 32] {
        self.identity_digest
    }

    fn pick_ec(&self, index: u32) -> Option<&CertSignAlgoInfo> {
        if self.ec_infos.is_empty() {
            return None;
        }
        self.ec_infos.get((index as usize) % self.ec_infos.len())
    }

    fn pick(&self, prefer_ec: bool) -> Result<&CertSignAlgoInfo, Error> {
        let ec = self.pick_ec(active_credential());
        let rsa = self.rsa_info.as_ref();
        let (primary, other) = if prefer_ec { (ec, rsa) } else { (rsa, ec) };
        primary.or(other).ok_or_else(|| {
            kmr_common::km_err!(
                AttestationKeysNotProvisioned,
                "keybox has no RSA or EC signing key"
            )
        })
    }

    fn signing_info(&self, key_type: SigningKeyType) -> Result<SigningInfoSnapshot, Error> {
        let prefer_ec = matches!(key_type.algo_hint, SigningAlgorithm::Ec);
        let info = self.pick(prefer_ec)?;
        Ok(SigningInfoSnapshot {
            signing_key: info.key.clone(),
            cert_chain: info.chain.clone(),
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
            KeyAlgorithm::Ec => self.ec_infos = vec![Self::build_ec_info(entry)?],
            KeyAlgorithm::Rsa => self.rsa_info = Some(Self::build_rsa_info(entry)?),
        }
        self.refresh_identity_digest()
    }

    pub fn to_xml_string(&self) -> String {
        let mut blocks = Vec::new();
        for info in &self.ec_infos {
            blocks.push(Self::to_xml_block(KeyAlgorithm::Ec, info));
        }
        if let Some(info) = &self.rsa_info {
            blocks.push(Self::to_xml_block(KeyAlgorithm::Rsa, info));
        }
        format!(
            concat!(
                "<?xml version=\"1.0\"?>\n",
                "<AndroidAttestation>\n",
                "<NumberOfKeyboxes>1</NumberOfKeyboxes>\n",
                "<Keybox DeviceID=\"sw\">\n",
                "{}\n",
                "</Keybox>\n",
                "</AndroidAttestation>\n"
            ),
            blocks.join("\n"),
        )
    }

    fn to_xml_block(algorithm: KeyAlgorithm, info: &CertSignAlgoInfo) -> String {
        let (name, private_label) = match algorithm {
            KeyAlgorithm::Ec => ("ecdsa", "EC PRIVATE KEY"),
            KeyAlgorithm::Rsa => ("rsa", "RSA PRIVATE KEY"),
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

pub fn with_active_credential<T>(index: u32, f: impl FnOnce() -> T) -> T {
    ACTIVE_RKP_CREDENTIAL.with(|active| {
        let previous = active.replace(index);
        let result = f();
        active.set(previous);
        result
    })
}

fn active_credential() -> u32 {
    ACTIVE_RKP_CREDENTIAL.with(Cell::get)
}

impl Default for KeyBox {
    fn default() -> Self {
        Self::new()
    }
}

impl ParsedKeyEntry {
    fn from_pem_parts(private_key_pem: &str, certs: &[String]) -> Result<Self> {
        if private_key_pem.trim().is_empty() {
            bail!("missing <PrivateKey> block");
        }
        let key_der = decode_pem(private_key_pem)?;
        let chain = certs
            .iter()
            .map(|pem| decode_pem(pem))
            .collect::<Result<Vec<_>>>()?;
        if chain.is_empty() {
            bail!("certificate chain is empty");
        }
        Ok(Self { key_der, chain })
    }
}

fn parse_key_algorithm(raw: &str) -> Result<KeyAlgorithm> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "rsa" => Ok(KeyAlgorithm::Rsa),
        "ecdsa" | "ec" => Ok(KeyAlgorithm::Ec),
        other => bail!("unsupported key algorithm `{other}` in keybox.xml"),
    }
}

fn parse_xml_key_entries(xml: &str) -> Result<(Option<ParsedKeyEntry>, Vec<ParsedKeyEntry>)> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut rsa = None;
    let mut ec = Vec::new();
    let mut current_algo: Option<KeyAlgorithm> = None;
    let mut private_key = String::new();
    let mut certs: Vec<String> = Vec::new();
    let mut in_private = false;
    let mut in_cert = false;
    let mut in_key = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                "Key" => {
                    in_key = true;
                    current_algo = None;
                    private_key.clear();
                    certs.clear();
                    in_private = false;
                    in_cert = false;
                    if let Ok(Some(attr)) = e.try_get_attribute("algorithm") {
                        let raw = attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)?;
                        current_algo = Some(parse_key_algorithm(raw.as_ref())?);
                    }
                }
                "PrivateKey" if in_key => in_private = true,
                "Certificate" if in_key => {
                    in_cert = true;
                    certs.push(String::new());
                }
                _ => {}
            },
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                "PrivateKey" => in_private = false,
                "Certificate" => in_cert = false,
                "Key" => {
                    in_key = false;
                    let algo = current_algo
                        .take()
                        .context("missing key algorithm in keybox.xml")?;
                    match ParsedKeyEntry::from_pem_parts(&private_key, &certs) {
                        Ok(entry) => match algo {
                            KeyAlgorithm::Rsa => rsa = Some(entry),
                            KeyAlgorithm::Ec => ec.push(entry),
                        },
                        Err(error) if algo == KeyAlgorithm::Rsa => {
                            warn!("skipping RSA keybox entry: {error:#}");
                        }
                        Err(error) => {
                            return Err(error).context(format!(
                                "failed to parse {} key entry",
                                algorithm_name(algo)
                            ));
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::Text(t)) if in_key => {
                let text = t.xml10_content();
                if in_private {
                    private_key.push_str(&text);
                } else if in_cert {
                    if let Some(last) = certs.last_mut() {
                        last.push_str(&text);
                    }
                }
            }
            Ok(Event::CData(t)) if in_key => {
                let text = t.xml10_content();
                if in_private {
                    private_key.push_str(&text);
                } else if in_cert {
                    if let Some(last) = certs.last_mut() {
                        last.push_str(&text);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => bail!("invalid keybox.xml: {error}"),
            _ => {}
        }
        buf.clear();
    }

    Ok((rsa, ec))
}

fn import_rsa_key_der(der: &[u8]) -> Result<KeyMaterial> {
    match rsa::import_pkcs8_key(der) {
        Ok((key, _, _)) => Ok(key),
        Err(_) => rsa::import_pkcs1_key(der)
            .map(|(key, _, _)| key)
            .map_err(|e| anyhow!("failed to import RSA private key: {e:?}")),
    }
}

fn import_ec_key_der(der: &[u8]) -> Result<KeyMaterial> {
    match ec::import_pkcs8_key(der) {
        Ok(key) => Ok(key),
        Err(_) => ec::import_sec1_private_key(der)
            .map_err(|e| anyhow!("failed to import EC private key: {e:?}")),
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

fn persist_last_good(contents: &str) {
    if let Err(error) = fs::create_dir_all("/data/misc/keystore/omk/data") {
        warn!("failed to create last-good keybox directory: {error:#}");
        return;
    }
    if let Err(error) = write_keybox_xml(LAST_GOOD_PATH, contents) {
        warn!("failed to persist last-good keybox: {error:#}");
    }
}

fn load_last_good() -> Option<KeyBox> {
    let contents = fs::read_to_string(LAST_GOOD_PATH).ok()?;
    match KeyBox::from_xml_str(&contents) {
        Ok(keybox) => Some(keybox),
        Err(error) => {
            warn!("last-good keybox is invalid: {error:#}");
            None
        }
    }
}

fn load_keybox_with_fallback(path: &str) -> Result<(KeyBox, bool)> {
    match fs::read_to_string(path) {
        Ok(contents) => match KeyBox::from_xml_str(&contents) {
            Ok(keybox) => {
                let fallback_origin = is_fallback_continuation(&keybox, &contents);
                if path == KEYBOX_PATH {
                    persist_last_good(&contents);
                }
                Ok((keybox, fallback_origin))
            }
            Err(error) => {
                warn!("invalid keybox.xml at {path}: {error:#}");
                if let Some(keybox) = load_last_good() {
                    warn!("invalid keybox.xml at {path}; using last-good");
                    return Ok((keybox, true));
                }
                warn!("invalid keybox.xml at {path}; using bundled template");
                Ok((KeyBox::new(), true))
            }
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
        keybox
            .rsa_info
            .as_ref()
            .and_then(|info| info.chain.first())
            .map(|certificate| certificate.encoded_certificate.clone())
            .unwrap_or_default(),
        keybox
            .ec_infos
            .first()
            .and_then(|info| info.chain.first())
            .map(|certificate| certificate.encoded_certificate.clone())
            .unwrap_or_default(),
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
        assert_eq!(keybox.ec_infos.first().unwrap().chain.len(), 1);
        assert_eq!(keybox.rsa_info.as_ref().unwrap().chain.len(), 2);
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
        changed.rsa_info = None;
        changed.refresh_identity_digest().unwrap();
        assert_ne!(original.identity_digest(), changed.identity_digest());

        let xml = changed.to_xml_string();
        let modified = KeyBox::from_xml_str(&xml).unwrap();
        assert_ne!(original.identity_digest(), modified.identity_digest());
        assert_eq!(changed.identity_digest(), modified.identity_digest());
    }

    #[test]
    fn rejects_mismatched_private_key_and_certificate_chain() {
        let keybox = KeyBox::from_xml_str(BUNDLED_KEYBOX_XML).unwrap();
        let rsa_cert = encode_pem_block(
            "CERTIFICATE",
            &keybox.rsa_info.as_ref().unwrap().chain[0].encoded_certificate,
        );
        let ec_cert = encode_pem_block(
            "CERTIFICATE",
            &keybox.ec_infos.first().unwrap().chain[0].encoded_certificate,
        );
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
    fn invalid_file_falls_back() {
        let path = write_temp_keybox("invalid", "<not-xml>");
        let (keybox, used_fallback) = load_keybox_with_fallback(path.to_str().unwrap()).unwrap();
        assert!(used_fallback);
        assert!(keybox.rsa_info.is_some() || !keybox.ec_infos.is_empty());
        let written = fs::read_to_string(&path).unwrap();
        assert_eq!(written, "<not-xml>");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn ec_only_covers_rsa_hint() {
        let bundled = KeyBox::from_xml_str(BUNDLED_KEYBOX_XML).unwrap();
        let xml = bundled.to_xml_string();
        let rsa_block_start = xml.find("<Key algorithm=\"rsa\">").unwrap();
        let rsa_block_end = xml[rsa_block_start..]
            .find("</Key>")
            .map(|offset| rsa_block_start + offset + "</Key>".len())
            .unwrap();
        let ec_only = format!("{}{}", &xml[..rsa_block_start], &xml[rsa_block_end..]);
        let keybox = KeyBox::from_xml_str(&ec_only).unwrap();
        assert!(keybox.rsa_info.is_none());
        assert!(!keybox.ec_infos.is_empty());

        let rsa_hint = keybox
            .signing_info(SigningKeyType {
                which: SigningKey::Batch,
                algo_hint: SigningAlgorithm::Rsa,
            })
            .unwrap();
        validate_chain_matches_key(
            &rsa_hint.signing_key,
            &rsa_hint.cert_chain,
            KeyAlgorithm::Ec,
        )
        .unwrap();
    }

    #[test]
    fn keeps_all_ec_credentials() {
        let bundled = KeyBox::from_xml_str(BUNDLED_KEYBOX_XML).unwrap();
        let xml = bundled.to_xml_string();
        let start = xml.find("<Key algorithm=\"ecdsa\">").unwrap();
        let end = xml[start..]
            .find("</Key>")
            .map(|offset| start + offset + "</Key>".len())
            .unwrap();
        let ec_block = &xml[start..end];
        let dual = xml.replacen(ec_block, &format!("{ec_block}\n{ec_block}"), 1);
        let keybox = KeyBox::from_xml_str(&dual).unwrap();
        assert_eq!(keybox.ec_infos.len(), 2);
        assert_ne!(keybox.identity_digest(), bundled.identity_digest());

        let first = keybox
            .signing_info(SigningKeyType {
                which: SigningKey::Batch,
                algo_hint: SigningAlgorithm::Ec,
            })
            .unwrap();
        let second = with_active_credential(1, || {
            keybox
                .signing_info(SigningKeyType {
                    which: SigningKey::Batch,
                    algo_hint: SigningAlgorithm::Ec,
                })
                .unwrap()
        });
        validate_chain_matches_key(&first.signing_key, &first.cert_chain, KeyAlgorithm::Ec)
            .unwrap();
        validate_chain_matches_key(&second.signing_key, &second.cert_chain, KeyAlgorithm::Ec)
            .unwrap();
    }

    #[test]
    fn skips_rsa_entry_with_empty_certificate_chain() {
        let bundled = KeyBox::from_xml_str(BUNDLED_KEYBOX_XML).unwrap();
        let xml = bundled.to_xml_string();
        let rsa_start = xml.find("<Key algorithm=\"rsa\">").unwrap();
        let chain_start = xml[rsa_start..].find("<CertificateChain>").unwrap() + rsa_start;
        let chain_end = xml[chain_start..]
            .find("</CertificateChain>")
            .map(|offset| chain_start + offset + "</CertificateChain>".len())
            .unwrap();
        let stripped = format!("{}{}", &xml[..chain_start], &xml[chain_end..]);
        let keybox = KeyBox::from_xml_str(&stripped).unwrap();
        assert!(keybox.rsa_info.is_none());
        assert!(!keybox.ec_infos.is_empty());
    }

    #[test]
    fn parses_keybox_with_comments() {
        let commented = BUNDLED_KEYBOX_XML.replace(
            "<Key algorithm=\"rsa\">",
            "<!-- watermark -->\n<Key algorithm=\"rsa\">",
        );
        let keybox = KeyBox::from_xml_str(&commented).unwrap();
        assert!(keybox.rsa_info.is_some());
        assert!(!keybox.ec_infos.is_empty());
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
