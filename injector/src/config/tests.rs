use super::*;
use std::ops::Deref;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::time::{SystemTime, UNIX_EPOCH};

struct TempConfigPath(PathBuf);

impl Deref for TempConfigPath {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for TempConfigPath {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
        if let (Some(parent), Some(file_name)) = (self.0.parent(), self.0.file_name()) {
            let temp = parent.join(format!(
                ".{}.tmp-{}",
                file_name.to_string_lossy(),
                std::process::id()
            ));
            let _ = fs::remove_file(temp);
        }
    }
}

fn temp_config_path(name: &str) -> TempConfigPath {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should move forward")
        .as_nanos();
    TempConfigPath(std::env::temp_dir().join(format!("omk-injector-{name}-{unique}.toml")))
}

#[test]
fn config_defaults_and_log_levels_match_contract() {
    let config = InjectorConfig::default();
    assert!(config.main.enabled);
    assert_eq!(config.scoop, default_scoop());
    assert!(config.scoop_details.is_empty());
    assert_eq!(config.main.log_level_filter(), LevelFilter::Off);
    assert!(!config.main.debug_logging);
    assert_eq!(config.main.effective_log_level(), LevelFilter::Off);
    assert!(config.filter.block_android_package);
    assert!(!config.filter.allow_unknown_package);
    assert!(config.intercept.get_security_level);
    assert!(config.intercept.get_key_entry);
    assert!(config.intercept.update_subcomponent);
    assert!(config.intercept.list_entries);
    assert!(config.intercept.delete_key);
    assert!(config.intercept.grant);
    assert!(config.intercept.ungrant);
    assert!(config.intercept.get_number_of_entries);
    assert!(config.intercept.list_entries_batched);
    assert!(config.intercept.get_supplementary_attestation_info);

    assert_eq!(parse_level_filter("warn"), Some(LevelFilter::Warn));
    assert_eq!(parse_level_filter("WARNING"), Some(LevelFilter::Warn));
    assert_eq!(parse_level_filter("trace"), Some(LevelFilter::Trace));
    assert_eq!(parse_level_filter("unknown"), None);

    let unknown = MainConfig {
        log_level: "not-a-level".to_string(),
        ..Default::default()
    };
    assert_eq!(unknown.log_level_filter(), LevelFilter::Off);
    assert_eq!(unknown.effective_log_level(), LevelFilter::Off);
}

#[test]
fn debug_logging_caps_effective_level_at_warn() {
    let mut main = MainConfig::default();
    assert!(!main.debug_logging);
    assert_eq!(main.log_level_filter(), LevelFilter::Off);
    assert_eq!(main.effective_log_level(), LevelFilter::Off);

    main.log_level = "trace".to_string();
    assert_eq!(main.effective_log_level(), LevelFilter::Warn);

    main.log_level = "info".to_string();
    assert_eq!(main.effective_log_level(), LevelFilter::Warn);

    main.log_level = "error".to_string();
    assert_eq!(main.effective_log_level(), LevelFilter::Error);

    main.log_level = "off".to_string();
    assert_eq!(main.effective_log_level(), LevelFilter::Off);

    main.debug_logging = true;
    main.log_level = "debug".to_string();
    assert_eq!(main.effective_log_level(), LevelFilter::Debug);

    main.log_level = "trace".to_string();
    assert_eq!(main.effective_log_level(), LevelFilter::Trace);

    main.log_level = "info".to_string();
    assert_eq!(main.effective_log_level(), LevelFilter::Info);

    main.log_level = "warn".to_string();
    assert_eq!(main.effective_log_level(), LevelFilter::Warn);
}

#[test]
fn parses_new_scoop_format_and_preserves_package_details() {
    let parsed = parse_config(
        r#"
scoop = ["com.example.app", "com.other.app", "com.example.app"]

[scoop."com.example.app"]
mode = "strict"
keybox_slot = 2

[main]
enabled = false
log_level = "trace"
debug_logging = true

[filter]
enabled = true
deny_packages = ["com.blocked"]
block_android_package = false
allow_unknown_package = true

[intercept]
get_security_level = false
get_key_entry = true
update_subcomponent = false
list_entries = false
delete_key = false
grant = false
ungrant = false
get_number_of_entries = false
list_entries_batched = false
get_supplementary_attestation_info = true
"#,
    )
    .expect("config should parse");

    assert_eq!(
        parsed.scoop,
        vec!["com.example.app".to_string(), "com.other.app".to_string()]
    );
    assert_eq!(parsed.main.log_level_filter(), LevelFilter::Trace);
    assert!(parsed.main.debug_logging);
    assert_eq!(parsed.main.effective_log_level(), LevelFilter::Trace);
    assert!(!parsed.main.enabled);
    assert_eq!(
        parsed
            .scoop_details
            .get("com.example.app")
            .and_then(|table| table.get("mode"))
            .and_then(toml::Value::as_str),
        Some("strict")
    );
    assert_eq!(
        parsed.keybox_slot_for_packages(&["com.example.app".to_string()]),
        2
    );
    assert_eq!(
        parsed.keybox_slot_for_packages(&["com.other.app".to_string()]),
        0
    );
    assert!(!parsed.intercept.get_security_level);
    assert!(parsed.intercept.get_key_entry);
    assert!(!parsed.intercept.update_subcomponent);
    assert!(!parsed.intercept.list_entries);
    assert!(!parsed.intercept.delete_key);
    assert!(!parsed.intercept.grant);
    assert!(!parsed.intercept.ungrant);
    assert!(!parsed.intercept.get_number_of_entries);
    assert!(!parsed.intercept.list_entries_batched);
    assert!(parsed.intercept.get_supplementary_attestation_info);
}

#[test]
fn legacy_config_syntax_is_rejected() {
    let error = parse_config(
        r#"
[[scope]]
package = "com.legacy.app"
"#,
    )
    .expect_err("legacy scope syntax should be rejected");
    assert!(error.contains("unknown field"));

    let error = parse_config(
        r#"
scoop = ["com.example.app"]

[filter]
allow_packages = ["com.legacy.app"]
"#,
    )
    .expect_err("legacy allow_packages should be rejected");
    assert!(error.contains("unknown field"));
}

#[test]
fn rendered_config_uses_new_scoop_format() {
    let mut config = InjectorConfig {
        scoop: vec!["com.example.app".to_string()],
        ..Default::default()
    };
    let mut table = toml::Table::new();
    table.insert("enabled".to_string(), toml::Value::Boolean(true));
    config
        .scoop_details
        .insert("com.example.app".to_string(), table);

    let rendered = render_config(&config).expect("config should render");
    assert!(rendered.contains("scoop = ["));
    assert!(rendered.contains("[scoop.com.example.app]"));
    assert!(!rendered.contains("[[scope]]"));
    let reparsed = parse_config(&rendered).expect("rendered config should parse");
    assert_eq!(reparsed.scoop_details, config.scoop_details);
}

#[test]
fn missing_config_is_seeded_but_invalid_startup_config_is_untouched() {
    let path = temp_config_path("missing");

    let loaded = load_or_seed(&path, LoadContext::Startup).unwrap();
    assert!(path.exists(), "missing config should be written to disk");
    assert_eq!(loaded.version, CURRENT_CONFIG_VERSION);

    let on_disk = fs::read_to_string(&*path).expect("written config should be readable");
    let reparsed = parse_config(&on_disk).expect("written config should parse");
    assert_eq!(reparsed.scoop, loaded.scoop);

    let path = temp_config_path("invalid");
    let invalid = "[main\nbroken";
    fs::write(&*path, invalid).expect("invalid config should be written");

    let loaded = load_or_seed(&path, LoadContext::Startup).unwrap();
    assert!(
        !loaded.main.enabled,
        "invalid startup config must disable injection"
    );
    assert_eq!(fs::read_to_string(&*path).unwrap(), invalid);
    assert!(!PathBuf::from(format!("{}.bak", path.display())).exists());
}

#[test]
fn v0_config_migrates_through_public_scoop_syntax_and_preserves_mode() {
    let path = temp_config_path("v0-migration");
    let v0 = "\u{feff}scoop = [\"com.example.app\"]\r\n\r\n\
              [scoop.com.example.app]\r\nmode = \"strict\"\r\n";
    fs::write(&*path, v0).unwrap();
    fs::set_permissions(&*path, fs::Permissions::from_mode(0o640)).unwrap();

    let loaded = load_from_path(&path, true).expect("v0 config should migrate");
    assert_eq!(loaded.version, CURRENT_CONFIG_VERSION);
    assert_eq!(
        loaded
            .scoop_details
            .get("com.example.app")
            .and_then(|table| table.get("mode"))
            .and_then(toml::Value::as_str),
        Some("strict")
    );

    let migrated = fs::read_to_string(&*path).unwrap();
    assert_eq!(
        migrated,
        v0.replacen('\u{feff}', "\u{feff}version = 1\r\n", 1)
    );
    assert_eq!(fs::metadata(&*path).unwrap().mode() & 0o777, 0o640);
}

#[test]
fn explicit_v0_migration_only_replaces_the_version_value() {
    let v0 = "# keep this comment\nversion = 0 # and this one\nscoop = [\"com.example.app\"]\n";
    let (_, migrated) = parse_versioned_config(v0, true).expect("v0 config should migrate");
    assert_eq!(
        migrated.as_deref(),
        Some("# keep this comment\nversion = 1 # and this one\nscoop = [\"com.example.app\"]\n")
    );
}

#[test]
fn reload_rejects_v0_without_rewriting() {
    let path = temp_config_path("reload-v0");
    let v0 = "scoop = [\"com.example.app\"]\n";
    fs::write(&*path, v0).unwrap();

    assert!(load_or_seed(&path, LoadContext::Reload(WatchTrigger::CloseWrite)).is_none());
    assert_eq!(fs::read_to_string(&*path).unwrap(), v0);
}

#[test]
fn unsupported_versions_are_rejected_without_rewriting() {
    for contents in ["version = -1\n", "version = 2\n", "version = \"1\"\n"] {
        assert!(parse_config(contents).is_err());
    }

    let path = temp_config_path("future-version");
    let future = "version = 2\n";
    fs::write(&*path, future).unwrap();
    let loaded = load_or_seed(&path, LoadContext::Startup).unwrap();
    assert!(!loaded.main.enabled);
    assert_eq!(fs::read_to_string(&*path).unwrap(), future);
}

#[test]
fn template_scope_matches_default_scope() {
    let template = include_str!("../../../template/injector.toml");
    let parsed = parse_config(template).expect("template injector config should parse");
    assert_eq!(parsed.scoop, default_scoop());
    assert_eq!(parsed.main.log_level, "off");
    assert!(!parsed.main.debug_logging);
    assert_eq!(parsed.main.effective_log_level(), LevelFilter::Off);
}

#[test]
fn replace_save_retry_only_retries_read_failures() {
    let path = temp_config_path("replace-save-retry");
    let mut attempts = 0usize;
    let mut sleeps = Vec::new();

    let loaded = load_with_read_race_retry(
        &path,
        LoadContext::Reload(WatchTrigger::ReplaceSave),
        |_path| {
            attempts += 1;
            match attempts {
                1 => Err(LoadError::Io(io::Error::from(io::ErrorKind::NotFound))),
                2 => Err(LoadError::Io(io::Error::from(
                    io::ErrorKind::PermissionDenied,
                ))),
                _ => Ok(InjectorConfig::default()),
            }
        },
        |duration| sleeps.push(duration),
    )
    .expect("replace-save retry should eventually succeed");

    assert_eq!(loaded.retries, 2);
    assert_eq!(attempts, 3);
    assert_eq!(sleeps.len(), 2);
    assert!(sleeps
        .iter()
        .all(|duration| *duration == REPLACE_SAVE_RETRY_INTERVAL));

    let path = temp_config_path("replace-save-parse");
    let mut sleeps = Vec::new();

    let error = load_with_read_race_retry(
        &path,
        LoadContext::Reload(WatchTrigger::ReplaceSave),
        |_path| Err(LoadError::Parse("broken".to_string())),
        |duration| sleeps.push(duration),
    )
    .expect_err("parse failures should bypass replace-save retries");

    assert!(matches!(error, LoadError::Parse(_)));
    assert!(sleeps.is_empty());
}

#[test]
fn attest_key_fallback_flag_is_parsed_per_package() {
    let config = parse_config(
        r#"
scoop = ["com.probe.app", "com.plain.app"]

[scoop."com.probe.app"]
attest_key_fallback = true

[scoop."com.plain.app"]
keybox_slot = 1
"#,
    )
    .unwrap();

    assert!(!config.main.debug_logging);
    assert!(config.attest_key_fallback_for_packages(&["com.probe.app".to_string()]));
    assert!(!config.attest_key_fallback_for_packages(&["com.plain.app".to_string()]));
    assert!(!config.attest_key_fallback_for_packages(&["com.unknown.app".to_string()]));
}
