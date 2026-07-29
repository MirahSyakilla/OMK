use super::*;
use std::ops::Deref;
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
        let _ = fs::remove_file(format!("{}.bak", self.0.display()));
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
    assert_eq!(config.main.log_level_filter(), LevelFilter::Debug);
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
fn missing_and_invalid_config_recover_safely() {
    let path = temp_config_path("missing");

    let loaded = load_or_seed(&path, LoadContext::Startup);
    assert!(path.exists(), "missing config should be written to disk");

    let on_disk = fs::read_to_string(&*path).expect("written config should be readable");
    let reparsed = parse_config(&on_disk).expect("written config should parse");
    assert_eq!(reparsed.scoop, loaded.scoop);

    let path = temp_config_path("invalid");
    let backup = PathBuf::from(format!("{}.bak", path.display()));
    fs::write(&*path, "[main\nbroken").expect("invalid config should be written");

    let loaded = load_or_seed(&path, LoadContext::Startup);
    assert!(backup.exists(), "invalid config should be backed up");

    let rewritten = fs::read_to_string(&*path).expect("rewritten config should be readable");
    let reparsed = parse_config(&rewritten).expect("rewritten config should parse");
    assert_eq!(reparsed.scoop, loaded.scoop);

    let backup_contents = fs::read_to_string(&backup).expect("backup config should be readable");
    assert!(
        backup_contents.contains("# injector config recovery reason:"),
        "backup should contain the appended error reason"
    );
}

#[test]
fn template_scope_matches_default_scope() {
    let template = include_str!("../../../template/injector.toml");
    let parsed = parse_config(template).expect("template injector config should parse");
    assert_eq!(parsed.scoop, default_scoop());
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
