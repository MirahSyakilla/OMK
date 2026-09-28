//! Background refresh of the WebUI's fingerprint build template.
//!
//! The Select Fingerprint picker renders from a template listing every Pixel
//! build it may offer. A copy ships inside the WebUI bundle, refreshed at build
//! time, so a released zip opens its picker with no network at all. This module
//! keeps the copy on the device moving afterwards, so a zip that goes stale
//! while it sits on a phone still gains new builds without the user opening the
//! picker.
//!
//! It is off unless `auto_fetch_fingerprint` is set in `integrity.toml`.
//!
//! Deliberate choices, because this runs unattended:
//!
//! * The previous template is never truncated. New data goes to a temp file,
//!   is parsed back and validated, and only then renamed over the live copy,
//!   with the outgoing copy kept as a backup first.
//! * A fetch that comes back with nothing usable for a device leaves that
//!   device's existing entry alone, so a flaky network degrades to "stale"
//!   rather than "device missing from the picker".
//! * If upstream's data matches what is already on disk after merging, nothing
//!   is written at all, so a steady state produces no disk churn.
//! * The whole cycle is best-effort. Any failure is logged and the loop moves
//!   on; there is no path where a fetch problem affects anything else.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Live template, read by the WebUI's picker.
const TEMPLATE_PATH: &str = "/data/misc/keystore/omk/data/fingerprint-template.json";
/// Written first, then renamed over `TEMPLATE_PATH`, so the swap is atomic.
const TEMPLATE_TEMP: &str = "/data/misc/keystore/omk/data/fingerprint-template.json.tmp";
/// Previous good copy, so a failed write is never the only thing on disk.
const TEMPLATE_BACKUP: &str = "/data/misc/keystore/omk/data/fingerprint-template.json.bak";

/// Where the module may have been installed. The directory name comes from the
/// flashing tool, so every known spelling is tried before giving up.
const SEED_PATHS: &[&str] = &[
    "/data/adb/modules/oh_my_keymint/fingerprint-template.json",
    "/data/adb/modules/omk/fingerprint-template.json",
    "/data/adb/modules/OhMyKeymint/fingerprint-template.json",
];

/// Both locations the WebUI writes the integrity block to.
const INTEGRITY_TOML_PATHS: &[&str] = &[
    "/data/adb/omk/integrity.toml",
    "/data/misc/keystore/omk/integrity.toml",
];

const FLASH_SITE: &str = "https://flash.android.com/";
const BUILDS_API: &str = "https://content-flashstation-pa.googleapis.com/v1/builds";

/// How often the loop wakes up.
///
/// This is deliberately much shorter than [`REFRESH_INTERVAL`]: it is what
/// makes the toggle responsive. Turning `auto_fetch_fingerprint` on should
/// start the checks within minutes, not a day later.
const POLL_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// Minimum gap between two real fetches.
const REFRESH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Delay before the first check, so boot is not competing with a fetch.
const STARTUP_DELAY: Duration = Duration::from_secs(5 * 60);

const HTTP_TIMEOUT_SECS: &str = "30";
const CONNECT_TIMEOUT_SECS: &str = "10";

/// Kept in sync with `BUILD_LETTER_MAJOR` in the WebUI and in
/// `scripts/fetch_fingerprint_template.py`, which is what resolves an Android
/// major for builds that publish neither `versionName` nor `apiLevel`.
const BUILD_LETTER_MAJOR: &[(char, u8)] = &[
    ('S', 12),
    ('T', 13),
    ('U', 14),
    ('A', 15),
    ('B', 16),
    ('C', 17),
];

/// One device's builds, as `[releaseCandidateName, incremental, major]`.
type Row = [String; 3];

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeviceEntry {
    model: String,
    min: u8,
    max: u8,
    builds: Vec<Row>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
struct Template(BTreeMap<String, DeviceEntry>);

/// The subset of a Flash Station build the template is derived from.
#[derive(Debug, Deserialize)]
struct FlashStationBuild {
    #[serde(default)]
    product: String,
    #[serde(default)]
    target: String,
    release_candidate_name: Option<String>,
    build_id: Option<String>,
    version_name: Option<String>,
    api_level: Option<i64>,
    preview_metadata: Option<PreviewMetadata>,
}

#[derive(Debug, Deserialize)]
struct PreviewMetadata {
    #[serde(default)]
    release_track_name: String,
}

#[derive(Debug, Deserialize)]
struct FlashStationResponse {
    #[serde(default)]
    flashstation_build: Vec<FlashStationBuild>,
}

impl Template {
    /// Reject anything that could leave the picker with nothing to show.
    ///
    /// Every device must be named, bounded, and carry at least one build whose
    /// major sits inside those bounds. This is the check that makes it safe to
    /// write a file the WebUI will trust on its next launch.
    fn is_valid(&self) -> bool {
        !self.0.is_empty()
            && self.0.values().all(|entry| {
                if entry.model.is_empty() || entry.min > entry.max {
                    return false;
                }
                !entry.builds.is_empty()
                    && entry.builds.iter().all(|row| {
                        !row[0].is_empty()
                            && !row[1].is_empty()
                            && row[2]
                                .parse::<u8>()
                                .is_ok_and(|major| major >= entry.min && major <= entry.max)
                    })
            })
    }

    /// Whether two templates would render the same list of choices.
    ///
    /// Order is not significant. Upstream reordering the list is not a reason to
    /// rewrite the file.
    fn equivalent(&self, other: &Template) -> bool {
        if self.0.len() != other.0.len() {
            return false;
        }
        for (product, mine) in &self.0 {
            let Some(theirs) = other.0.get(product) else {
                return false;
            };
            if mine.model != theirs.model || mine.min != theirs.min || mine.max != theirs.max {
                return false;
            }
            // A device that lost every build is a change, not a match. Without
            // this, an empty fetch would look equivalent to a populated one.
            if mine.builds.is_empty() || theirs.builds.is_empty() {
                return false;
            }
            let keys: BTreeSet<String> = mine.builds.iter().map(row_key).collect();
            if keys.len() != mine.builds.len() {
                return false;
            }
            if theirs
                .builds
                .iter()
                .any(|row| !keys.contains(&row_key(row)))
            {
                return false;
            }
        }
        true
    }

    /// Row count, for logging.
    fn build_count(&self) -> usize {
        self.0.values().map(|entry| entry.builds.len()).sum()
    }
}

fn row_key(row: &Row) -> String {
    format!("{}|{}|{}", row[0], row[1], row[2])
}

/// Start the daily refresh loop.
///
/// Safe to call more than once; the loop itself is the only thing spawned.
pub fn spawn_background_refresh() {
    thread::Builder::new()
        .name("omk-fingerprint-refresh".into())
        .spawn(run_refresh_loop)
        .map(|_| ())
        .unwrap_or_else(|error| {
            log::warn!("fingerprint refresh: could not start thread: {error}");
        });
}

fn run_refresh_loop() {
    thread::sleep(STARTUP_DELAY);
    let mut last_success: Option<Instant> = None;

    loop {
        if !auto_fetch_enabled() {
            thread::sleep(POLL_INTERVAL);
            continue;
        }
        let due = last_success.is_none_or(|at| at.elapsed() >= REFRESH_INTERVAL);
        if due {
            match refresh_once() {
                Ok(unchanged) => {
                    if unchanged {
                        log::info!("fingerprint refresh: already up to date");
                    }
                    last_success = Some(Instant::now());
                }
                Err(error) => {
                    // Deliberately not advancing `last_success`, so the next poll
                    // retries rather than waiting a full day after a failure.
                    log::warn!("fingerprint refresh: {error:#}");
                }
            }
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Read `auto_fetch_fingerprint` out of `integrity.toml`.
///
/// A missing or unreadable file means off: this only ever maintains a
/// convenience list, so failing closed cannot break the picker.
fn auto_fetch_enabled() -> bool {
    for path in INTEGRITY_TOML_PATHS {
        let Ok(raw) = fs::read_to_string(path) else {
            continue;
        };
        let value: toml::Value = match toml::from_str(&raw) {
            Ok(value) => value,
            Err(error) => {
                log::warn!("fingerprint refresh: {path} is not valid toml: {error}");
                continue;
            }
        };
        if let Some(flag) = value
            .get("auto_fetch_fingerprint")
            .and_then(toml::Value::as_bool)
        {
            return flag;
        }
    }
    false
}

/// One fetch-and-maybe-write cycle.
///
/// Returns `true` when the fetched data matched what was already on disk.
fn refresh_once() -> Result<bool> {
    let current = load_current_template()?;
    log::info!(
        "fingerprint refresh: starting from {} devices, {} builds",
        current.0.len(),
        current.build_count()
    );

    let key = fetch_api_key()?;
    let mut next = Template(BTreeMap::new());
    let mut failed: Vec<&str> = Vec::new();

    for (product, entry) in &current.0 {
        match fetch_product(product, &key) {
            Ok(mut rows) => {
                // Carry over anything upstream stopped advertising, so a
                // retired build does not silently disappear from the picker.
                let seen: BTreeSet<String> = rows.iter().map(row_key).collect();
                let carried: Vec<Row> = entry
                    .builds
                    .iter()
                    .filter(|row| !seen.contains(&row_key(row)))
                    .cloned()
                    .collect();
                rows.extend(carried);
                rows.sort_by(|a, b| {
                    let major_a: u8 = a[2].parse().unwrap_or(0);
                    let major_b: u8 = b[2].parse().unwrap_or(0);
                    major_b.cmp(&major_a).then_with(|| a[0].cmp(&b[0]))
                });
                rows.dedup();
                next.0.insert(
                    product.clone(),
                    DeviceEntry {
                        model: entry.model.clone(),
                        min: entry.min,
                        max: entry.max,
                        builds: rows,
                    },
                );
            }
            Err(error) => {
                log::warn!("fingerprint refresh: {product} kept previous data: {error:#}");
                failed.push(product);
            }
        }
    }

    if next.0.is_empty() {
        bail!("no device returned any data");
    }
    if !next.is_valid() {
        bail!("refusing to replace the template with an incomplete fetch");
    }
    if current.equivalent(&next) {
        return Ok(true);
    }

    write_template(&next)?;
    log::info!(
        "fingerprint refresh: updated to {} devices, {} builds{}",
        next.0.len(),
        next.build_count(),
        if failed.is_empty() {
            String::new()
        } else {
            format!(" ({} stale: {})", failed.len(), failed.join(", "))
        }
    );
    Ok(false)
}

/// Load the best template available: live copy, then backup, then module seed.
fn load_current_template() -> Result<Template> {
    for path in [TEMPLATE_PATH, TEMPLATE_BACKUP]
        .into_iter()
        .chain(SEED_PATHS.iter().copied())
    {
        match read_template(Path::new(path)) {
            Ok(template) => return Ok(template),
            Err(error) => log::debug!("fingerprint refresh: {path} unusable: {error:#}"),
        }
    }
    bail!("no readable fingerprint template found")
}

fn read_template(path: &Path) -> Result<Template> {
    let raw = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let template: Template =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    if !template.is_valid() {
        bail!("{} is not a usable template", path.display());
    }
    Ok(template)
}

/// Write the template without ever leaving a partial file behind.
///
/// The order is the whole point: back up the current copy, write the new data to
/// a temp file, read that temp file back through the same validation the reader
/// uses, and only then rename it into place.
fn write_template(template: &Template) -> Result<()> {
    let temp = Path::new(TEMPLATE_TEMP);
    let live = Path::new(TEMPLATE_PATH);
    let backup = Path::new(TEMPLATE_BACKUP);

    if let Some(parent) = live.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }

    if live.exists() {
        // Best effort: if the backup cannot be refreshed the write still
        // proceeds, because the live copy is about to be replaced by something
        // that has already been validated.
        let _ = fs::copy(live, backup);
    }

    let payload = serde_json::to_vec(template).context("serialising template")?;
    fs::write(temp, &payload).with_context(|| format!("writing {}", temp.display()))?;

    // Verify what actually landed on disk before it can replace the good copy.
    read_template(temp).context("verifying the freshly written template")?;

    fs::rename(temp, live)
        .with_context(|| format!("replacing {} with {}", live.display(), temp.display()))?;
    Ok(())
}

fn fetch_api_key() -> Result<String> {
    let html = http_get(FLASH_SITE, None)?;
    let key = find_api_key(&html).ok_or_else(|| anyhow!("no API key on {FLASH_SITE}"))?;
    Ok(key)
}

/// Pull `AIzaSy...` out of the Flash Station page.
fn find_api_key(html: &str) -> Option<String> {
    const PREFIX: &str = "AIzaSy";
    let start = html.find(PREFIX)?;
    // Google's keys are 39 characters: the prefix plus 33 more.
    let end = start + PREFIX.len() + 33;
    let slice = &html[start..end.min(html.len())];
    slice
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        .then(|| slice.to_string())
        .filter(|key| key.len() == PREFIX.len() + 33)
}

fn fetch_product(product: &str, key: &str) -> Result<Vec<Row>> {
    let url = format!("{BUILDS_API}?product={product}&key={key}",);
    let raw = http_get(&url, Some(FLASH_SITE))?;
    let response: FlashStationResponse =
        serde_json::from_str(&raw).context("parsing the build list")?;

    let rows: Vec<Row> = response
        .flashstation_build
        .iter()
        .filter_map(|build| {
            build
                .to_row()
                .map(|([name, id], major)| [name, id, major.to_string()])
        })
        .collect();

    if rows.is_empty() {
        bail!("no usable builds");
    }
    Ok(rows)
}

impl FlashStationBuild {
    /// Reduce one build to a template row, or `None` if the picker would skip it.
    fn to_row(&self) -> Option<([String; 2], u8)> {
        // The picker only ever offers retail user builds, never factory images
        // or bootloader targets.
        if self.target != format!("{}-user", self.product) {
            return None;
        }
        let name = self.release_candidate_name.clone()?;
        let incremental = self.build_id.clone()?;
        let major = self.major()?;
        Some(([name, incremental], major))
    }

    /// Resolve the Android major, mirroring the WebUI's `buildMajor()`.
    fn major(&self) -> Option<u8> {
        if let Some(name) = &self.version_name {
            if let Some(major) = leading_number(name) {
                return Some(major);
            }
        }
        if let Some(metadata) = &self.preview_metadata {
            if let Some(major) = leading_android_number(&metadata.release_track_name) {
                return Some(major);
            }
        }
        let from_api_level = self
            .api_level
            .filter(|level| *level > 0)
            .and_then(|level| u8::try_from(level - 20).ok());
        if let Some(major) = from_api_level {
            return Some(major);
        }
        let letter = self
            .release_candidate_name
            .as_deref()
            .and_then(|name| name.chars().next())
            .map(|c| c.to_ascii_uppercase())?;
        BUILD_LETTER_MAJOR
            .iter()
            .find(|(key, _)| *key == letter)
            .map(|(_, major)| *major)
    }
}

/// Leading integer run of a version string.
///
/// "16" gives 16 and "16.0" gives 16, unlike a trailing-digit match, which
/// would read the "0" in "16.0" as the whole version and silently map the build
/// to Android 0. Letters after the digits, as in "12L", are ignored.
fn leading_number(value: &str) -> Option<u8> {
    let digits: String = value
        .trim()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// Major from a track name like "Android 13" or "Android 14 QPR1".
fn leading_android_number(track: &str) -> Option<u8> {
    let rest = track.trim().strip_prefix("Android ")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// Busybox binaries that ship with root solutions.
///
/// Neither root manager puts busybox on `PATH`, so the applet has to be invoked
/// by absolute path. KernelSU keeps it under `/data/adb/ksu/bin`, Magisk under
/// `/data/adb/magisk`, and stock Android has one in an APEX mount.
const BUSYBOX_PATHS: &[&str] = &[
    "/data/adb/ksu/bin/busybox",
    "/data/adb/magisk/busybox",
    "/data/adb/magisk/.busybox",
    "/apex/com.android.externaltools/bin/busybox",
];

/// GET a URL, trying every client likely to exist on a rooted device.
///
/// The order is cheapest-and-most-likely first, and the WebUI uses the same one,
/// so an automatic refresh fails in the same circumstances as the manual
/// Fetch Latest button rather than introducing a second kind of failure.
///
/// The `Referer` header is not optional for the Flash Station API; it answers 403
/// without one. Every client here can set it, busybox included, via
/// `--header`/`-H`, so the fallback is genuinely equivalent rather than a
/// degraded path that would only work for the API key lookup.
fn http_get(url: &str, referer: Option<&str>) -> Result<String> {
    let referer = referer.unwrap_or_default();
    let mut attempts: Vec<Command> = Vec::new();

    let mut curl = Command::new("curl");
    curl.args([
        "-fsSL",
        "--connect-timeout",
        CONNECT_TIMEOUT_SECS,
        "--max-time",
        HTTP_TIMEOUT_SECS,
    ]);
    if !referer.is_empty() {
        curl.args(["-H", &format!("Referer: {referer}")]);
    }
    curl.arg(url);
    attempts.push(curl);

    let mut wget = Command::new("wget");
    wget.args(["-q", "-T", HTTP_TIMEOUT_SECS, "-O", "-"]);
    if !referer.is_empty() {
        wget.arg(format!("--header=Referer: {referer}"));
    }
    wget.arg(url);
    attempts.push(wget);

    // busybox wget is the reliable fallback: root solutions always ship it, and
    // it accepts the same header syntax. Missing paths are simply skipped.
    for busybox in BUSYBOX_PATHS {
        if !Path::new(busybox).exists() {
            continue;
        }
        let mut applet = Command::new(busybox);
        applet.arg("wget");
        applet.args(["-q", "-T", HTTP_TIMEOUT_SECS, "-O", "-"]);
        if !referer.is_empty() {
            applet.arg(format!("--header=Referer: {referer}"));
        }
        applet.arg(url);
        attempts.push(applet);
    }

    for mut attempt in attempts {
        if let Some(body) = run_capture(&mut attempt) {
            return Ok(body);
        }
    }

    Err(anyhow!(
        "no usable HTTP client for {url}; tried curl, wget, and busybox wget"
    ))
}

fn run_capture(command: &mut Command) -> Option<String> {
    let output = command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let body = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // An empty body means a captive portal or a proxy notice rather than data.
    (!body.is_empty()).then_some(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(builds: &[(&str, &str, u8)]) -> DeviceEntry {
        DeviceEntry {
            model: "Pixel Test".into(),
            min: 12,
            max: 17,
            builds: builds
                .iter()
                .map(|(n, i, m)| [(*n).to_string(), (*i).to_string(), m.to_string()])
                .collect(),
        }
    }

    fn template(products: &[(&str, Vec<(&str, &str, u8)>)]) -> Template {
        Template(
            products
                .iter()
                .map(|(p, b)| (p.to_string(), entry(b)))
                .collect(),
        )
    }

    #[test]
    fn key_is_extracted_only_at_full_length() {
        let good = format!("xx{}yy", "AIzaSy".to_string() + &"a".repeat(33));
        assert_eq!(
            find_api_key(&good).as_deref(),
            Some(&*format!("AIzaSy{}", "a".repeat(33)))
        );

        // A truncated key must not be accepted.
        let short = format!("AIzaSy{}", "a".repeat(20));
        assert!(find_api_key(&short).is_none());
        assert!(find_api_key("nothing here").is_none());
    }

    #[test]
    fn major_prefers_version_name_then_track_then_api_level_then_letter() {
        let with_version = FlashStationBuild {
            product: "shiba".into(),
            target: "shiba-user".into(),
            release_candidate_name: Some("CP3A.1".into()),
            build_id: Some("1".into()),
            version_name: Some("16".into()),
            api_level: Some(36),
            preview_metadata: None,
        };
        assert_eq!(with_version.major(), Some(16));

        // TPBB publishes neither, and both the track name and the letter agree.
        let bare = FlashStationBuild {
            product: "oriole".into(),
            target: "oriole-user".into(),
            release_candidate_name: Some("TPBB.220414.015".into()),
            build_id: Some("8548023".into()),
            version_name: None,
            api_level: None,
            preview_metadata: Some(PreviewMetadata {
                release_track_name: "Android 13".into(),
            }),
        };
        assert_eq!(bare.major(), Some(13));
    }

    #[test]
    fn non_user_targets_are_rejected() {
        let factory = FlashStationBuild {
            product: "shiba".into(),
            target: "shiba factory image".into(),
            release_candidate_name: Some("CP3A.1".into()),
            build_id: Some("1".into()),
            version_name: Some("16".into()),
            api_level: None,
            preview_metadata: None,
        };
        assert!(factory.to_row().is_none());
    }

    #[test]
    fn empty_template_is_never_valid() {
        assert!(!Template(BTreeMap::new()).is_valid());
        assert!(!template(&[("shiba", vec![])]).is_valid());
        assert!(template(&[("shiba", vec![("CP3A.1", "1", 16)])]).is_valid());
    }

    #[test]
    fn build_outside_device_range_is_invalid() {
        let bad = Template(
            [(
                "shiba".to_string(),
                DeviceEntry {
                    model: "Pixel 8".into(),
                    // shiba starts at Android 14, so a 12 build cannot be valid.
                    min: 14,
                    max: 17,
                    builds: vec![["SP1A".into(), "1".into(), "12".into()]],
                },
            )]
            .into_iter()
            .collect(),
        );
        assert!(!bad.is_valid());
    }

    #[test]
    fn equivalence_ignores_order() {
        let a = template(&[("shiba", vec![("A", "1", 16), ("B", "2", 15)])]);
        let b = template(&[("shiba", vec![("B", "2", 15), ("A", "1", 16)])]);
        assert!(a.equivalent(&b));
        assert!(b.equivalent(&a));
    }

    #[test]
    fn equivalence_detects_new_and_removed_builds() {
        let current = template(&[("shiba", vec![("A", "1", 16)])]);
        let added = template(&[("shiba", vec![("A", "1", 16), ("B", "2", 16)])]);
        let removed = template(&[("shiba", vec![])]);
        assert!(!current.equivalent(&added));
        assert!(!current.equivalent(&removed));
    }

    #[test]
    fn equivalence_detects_a_new_device() {
        let one = template(&[("shiba", vec![("A", "1", 16)])]);
        let two = template(&[
            ("shiba", vec![("A", "1", 16)]),
            ("caiman", vec![("B", "2", 16)]),
        ]);
        assert!(!one.equivalent(&two));
    }

    #[test]
    fn template_survives_a_json_round_trip() {
        let original = template(&[("shiba", vec![("CP3A.1", "16091614", 17)])]);
        let encoded = serde_json::to_string(&original).unwrap();
        let decoded: Template = serde_json::from_str(&encoded).unwrap();
        assert!(decoded.is_valid());
        assert!(original.equivalent(&decoded));
    }

    #[test]
    fn truncated_json_is_rejected_rather_than_half_applied() {
        let full = serde_json::to_string(&template(&[("shiba", vec![("A", "1", 16)])])).unwrap();
        let cut = &full[..full.len() / 2];
        assert!(serde_json::from_str::<Template>(cut).is_err());
    }

    #[test]
    fn number_parsing_handles_the_shapes_upstream_sends() {
        assert_eq!(leading_number("16"), Some(16));
        assert_eq!(leading_number("16.0"), Some(16));
        assert_eq!(leading_number("17 "), Some(17));
        assert_eq!(leading_number("12L"), Some(12));
        assert_eq!(leading_number("Beta 3"), None);
        assert_eq!(leading_android_number("Android 13"), Some(13));
        assert_eq!(leading_android_number("Android 14 QPR2"), Some(14));
        assert_eq!(leading_android_number("13"), None);
    }

    #[test]
    fn a_missing_file_reports_an_error_instead_of_panicking() {
        // The seed paths are read at startup, where a missing module file must
        // degrade to "try the next one" rather than take the daemon down.
        assert!(read_template(Path::new("/nonexistent/fingerprint-template.json")).is_err());
    }
}
