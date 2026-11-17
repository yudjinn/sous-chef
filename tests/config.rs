//! T1.5: `sous-chef.toml` loading, flag merging and validation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::PathBuf;

use common::*;
use sous_chef::config::{CliOverrides, Config, HooksConfig};
use sous_chef::error::Error;
use sous_chef::lint::Severity;
use sous_chef::workspace::Workspace;

fn cli(work_dir: PathBuf) -> CliOverrides {
    CliOverrides {
        work_dir,
        ..CliOverrides::default()
    }
}

fn config_err(r: sous_chef::Result<impl std::fmt::Debug>) -> String {
    match r {
        Err(e @ Error::Config { .. }) => {
            let s = e.to_string();
            assert!(!s.contains('\n'), "{s}");
            s
        }
        other => panic!("expected a configuration error, got {other:?}"),
    }
}

#[test]
fn missing_file_gives_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = Config::load(&cli(tmp.path().to_path_buf())).unwrap();
    assert_eq!(
        cfg,
        Config {
            work_dir: tmp.path().to_path_buf(),
            ..Config::default()
        }
    );
    assert!(cfg.bitbake.enabled);
    assert!(!cfg.bitbake.allow_destructive);
    assert_eq!(cfg.output.budget_tokens, 600);
    assert_eq!(cfg.build_dir, None);
}

#[test]
fn missing_explicit_config_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let mut o = cli(tmp.path().to_path_buf());
    o.config = Some("nope.toml".into());
    let msg = config_err(Config::load(&o));
    assert!(msg.contains("nope.toml"), "{msg}");
}

#[test]
fn fixture_file_matches_fixture_config() {
    let root = fixture_ws_root();
    let cfg = Config::load(&cli(root.clone())).unwrap();
    assert_eq!(cfg, fixture_config());
    let ws = Workspace::discover(&cfg).unwrap();
    cfg.validate(&ws).unwrap();
}

#[test]
fn full_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("etc");
    fs::create_dir_all(&dir).unwrap();
    let text = r#"
build_dir = "build"
layers = ["../meta-extra", "/abs/meta-x"]
writable_layers = ["meta-mycompany"]
init_script = "poky/oe-init-build-env"
release = "scarthgap"

[bitbake]
enabled = false
allow_destructive = true

[lint]
disable = ["style/variable-order", "hygiene"]

[lint.severity]
"style/variable-order" = "error"
"hygiene/license" = "info"

[output]
budget_tokens = 900

[handles]
max_total_mb = 100
max_age_days = 7
"#;
    fs::write(dir.join("my.toml"), text).unwrap();
    let mut o = cli(tmp.path().to_path_buf());
    o.config = Some("etc/my.toml".into());
    let cfg = Config::load(&o).unwrap();
    assert_eq!(
        cfg,
        Config {
            build_dir: Some(dir.join("build")),
            layers: vec![dir.join("../meta-extra"), PathBuf::from("/abs/meta-x")],
            writable_layers: vec!["meta-mycompany".into()],
            init_script: Some(dir.join("poky/oe-init-build-env")),
            release: Some("scarthgap".into()),
            kas: Vec::new(),
            bitbake: sous_chef::config::BitbakeConfig {
                enabled: false,
                allow_destructive: true,
            },
            lint: sous_chef::config::LintConfig {
                disable: vec!["style/variable-order".into(), "hygiene".into()],
                severity: [
                    ("style/variable-order".to_string(), Severity::Error),
                    ("hygiene/license".to_string(), Severity::Info),
                ]
                .into_iter()
                .collect(),
            },
            output: sous_chef::config::OutputConfig { budget_tokens: 900 },
            hooks: HooksConfig::default(),
            handles: sous_chef::config::HandlesConfig {
                max_total_mb: 100,
                max_age_days: 7,
            },
            work_dir: tmp.path().to_path_buf(),
        }
    );
}

#[test]
fn hooks_switches_default_on() {
    let origin = PathBuf::from("/x/sous-chef.toml");
    let d = Config::from_toml("", &origin).unwrap().hooks;
    assert_eq!(
        d,
        HooksConfig {
            grep_read: true,
            bash: true,
            edit: true
        }
    );
    assert_eq!(Config::from_toml("[hooks]\n", &origin).unwrap().hooks, d);
    for key in ["grep_read", "bash", "edit", "bash_grep", "anything"] {
        assert!(d.enabled(key), "{key}");
    }
}

#[test]
fn each_hooks_switch_turns_one_hook_off() {
    let origin = PathBuf::from("/x/sous-chef.toml");
    for key in ["grep_read", "bash", "edit"] {
        let h = Config::from_toml(&format!("[hooks]\n{key} = false\n"), &origin)
            .unwrap()
            .hooks;
        for other in ["grep_read", "bash", "edit"] {
            assert_eq!(h.enabled(other), other != key, "{key} off: {other}");
        }
    }
    let h = Config::from_toml(
        "[hooks]\ngrep_read = false\nbash = true\nedit = false\n",
        &origin,
    )
    .unwrap()
    .hooks;
    assert_eq!(
        h,
        HooksConfig {
            grep_read: false,
            bash: true,
            edit: false
        }
    );
}

#[test]
fn unknown_or_mistyped_hooks_keys_are_errors() {
    let origin = PathBuf::from("/x/sous-chef.toml");
    let msg = config_err(Config::from_toml("[hooks]\ngrep = false\n", &origin));
    assert!(msg.contains("grep") && msg.contains(":2:"), "{msg}");
    let msg = config_err(Config::from_toml("[hooks]\nbash = \"no\"\n", &origin));
    assert!(msg.contains("/x/sous-chef.toml:2:"), "{msg}");
}

#[test]
fn bad_toml_and_unknown_keys_are_errors() {
    let origin = PathBuf::from("/x/sous-chef.toml");
    let msg = config_err(Config::from_toml("build_dir = ", &origin));
    assert!(msg.contains("/x/sous-chef.toml"), "{msg}");
    let msg = config_err(Config::from_toml("bild_dir = \"b\"\n", &origin));
    assert!(msg.contains("bild_dir"), "{msg}");
    let msg = config_err(Config::from_toml(
        "[output]\nbudget_tokens = \"x\"\n",
        &origin,
    ));
    // Points at the offending line.
    assert!(
        msg.contains("/x/sous-chef.toml:2:") && msg.contains("expected usize"),
        "{msg}"
    );
}

#[test]
fn lint_severity_values_and_errors() {
    let origin = PathBuf::from("/x/sous-chef.toml");
    let cfg = Config::from_toml("[lint.severity]\n\"a/b\" = \"warning\"\n", &origin).unwrap();
    assert_eq!(cfg.lint.severity.get("a/b"), Some(&Severity::Warning));
    assert!(cfg.lint.disable.is_empty());
    assert_eq!(cfg.release, None);

    // Only the three lowercase severities are accepted.
    for bad in ["fatal", "Error"] {
        let text = format!("[lint]\n[lint.severity]\n\"a/b\" = \"{bad}\"\n");
        let msg = config_err(Config::from_toml(&text, &origin));
        assert!(msg.contains("/x/sous-chef.toml:3:"), "{msg}");
        assert!(msg.contains(bad) || msg.contains("variant"), "{msg}");
    }
    // A value that is not a string, and an unknown key in [lint].
    let msg = config_err(Config::from_toml("[lint.severity]\n\"a/b\" = 2\n", &origin));
    assert!(msg.contains("/x/sous-chef.toml:2:"), "{msg}");
    let msg = config_err(Config::from_toml("[lint]\nseverities = {}\n", &origin));
    assert!(msg.contains("severities"), "{msg}");
}

#[test]
fn release_key_is_read_and_checked_at_discovery() {
    let origin = PathBuf::from("/x/sous-chef.toml");
    let cfg = Config::from_toml("release = \"kirkstone\"\n", &origin).unwrap();
    assert_eq!(cfg.release.as_deref(), Some("kirkstone"));
    let msg = config_err(Config::from_toml("release = 4\n", &origin));
    assert!(msg.contains("/x/sous-chef.toml:1:"), "{msg}");

    // A misspelt codename is reported when the workspace is discovered.
    let mut cfg = fixture_config();
    cfg.release = Some("scarthgab".into());
    let msg = config_err(Workspace::discover(&cfg));
    assert!(
        msg.contains("scarthgab") && msg.contains("wrynose"),
        "{msg}"
    );
}

#[test]
fn flags_override_file() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("sous-chef.toml"),
        "build_dir = \"from-file\"\nlayers = [\"file-layer\"]\n",
    )
    .unwrap();
    let mut o = cli(tmp.path().to_path_buf());
    o.build_dir = Some("from-flag".into());
    o.layers = vec!["flag-a".into(), "/abs/flag-b".into()];
    let cfg = Config::load(&o).unwrap();
    assert_eq!(cfg.build_dir, Some(tmp.path().join("from-flag")));
    assert_eq!(
        cfg.layers,
        vec![tmp.path().join("flag-a"), PathBuf::from("/abs/flag-b")]
    );

    // Without flags the file's values stand.
    let cfg = Config::load(&cli(tmp.path().to_path_buf())).unwrap();
    assert_eq!(cfg.build_dir, Some(tmp.path().join("from-file")));
    assert_eq!(cfg.layers, vec![tmp.path().join("file-layer")]);
}

#[test]
fn validate_unknown_writable_layer() {
    let ws = fixture_ws();
    let mut cfg = fixture_config();
    cfg.writable_layers = vec!["meta-mine".into(), "meta-legacy".into()];
    let msg = config_err(cfg.validate(&ws));
    assert!(msg.contains("\"meta-legacy\""), "{msg}");
    assert!(msg.contains("meta-core, meta-vendor, meta-mine"), "{msg}");
    assert!(!msg.contains("\"meta-mine\""), "{msg}");
}

#[test]
fn validate_build_dir_without_bblayers() {
    let ws = fixture_ws();
    let mut cfg = fixture_config();
    cfg.build_dir = Some(fixture_path("ws/meta-core"));
    let msg = config_err(cfg.validate(&ws));
    assert!(
        msg.contains("build_dir") && msg.contains("bblayers.conf"),
        "{msg}"
    );
}

#[test]
fn validate_layer_without_layer_conf() {
    let ws = fixture_ws();
    let mut cfg = fixture_config();
    cfg.layers = vec![fixture_path("ws/build")];
    let msg = config_err(cfg.validate(&ws));
    assert!(msg.contains("layer.conf"), "{msg}");
}

#[test]
fn validate_missing_init_script() {
    let ws = fixture_ws();
    let mut cfg = fixture_config();
    cfg.init_script = Some(fixture_path("ws/poky/oe-init-build-env"));
    let msg = config_err(cfg.validate(&ws));
    assert!(msg.contains("init_script"), "{msg}");
}

#[test]
fn validate_zero_budget() {
    let ws = fixture_ws();
    let mut cfg = fixture_config();
    cfg.output.budget_tokens = 0;
    let msg = config_err(cfg.validate(&ws));
    assert!(msg.contains("budget_tokens"), "{msg}");
}

#[test]
fn validate_reports_every_problem_on_one_line() {
    let ws = fixture_ws();
    let mut cfg = fixture_config();
    cfg.writable_layers = vec!["nope".into()];
    cfg.init_script = Some("/nonexistent/init".into());
    let msg = config_err(cfg.validate(&ws));
    assert!(
        msg.contains("nope") && msg.contains("init_script") && msg.contains("; "),
        "{msg}"
    );
}

#[test]
fn handles_table_defaults_match_the_store_and_values_are_read() {
    use sous_chef::config::HandlesConfig;
    use sous_chef::handles::Retention;
    let origin = PathBuf::from("/x/sous-chef.toml");
    let cfg = Config::from_toml("", &origin).unwrap();
    assert_eq!(cfg.handles, HandlesConfig::default());
    assert_eq!(cfg.handles.max_total_mb, 500);
    assert_eq!(cfg.handles.max_age_days, 14);
    assert_eq!(cfg.handles.retention(), Retention::default());

    let cfg = Config::from_toml(
        "[handles]\nmax_total_mb = 2000\nmax_age_days = 3\n",
        &origin,
    )
    .unwrap();
    assert_eq!(
        cfg.handles.retention(),
        Retention {
            max_age_secs: 3 * 24 * 60 * 60,
            max_total_bytes: 2_000_000_000,
        }
    );
    // One key alone keeps the other's default.
    let cfg = Config::from_toml("[handles]\nmax_age_days = 30\n", &origin).unwrap();
    assert_eq!(cfg.handles.max_total_mb, 500);
    assert_eq!(cfg.handles.max_age_days, 30);
}

#[test]
fn handles_values_out_of_range_and_unknown_keys_are_errors() {
    let origin = PathBuf::from("/x/sous-chef.toml");
    let msg = config_err(Config::from_toml("[handles]\nmax_total_mb = 0\n", &origin));
    assert!(
        msg.contains("/x/sous-chef.toml")
            && msg.contains("handles.max_total_mb = 0 is out of range")
            && msg.contains("1 to 1000000"),
        "{msg}"
    );
    let msg = config_err(Config::from_toml("[handles]\nmax_age_days = 0\n", &origin));
    assert!(
        msg.contains("handles.max_age_days = 0 is out of range"),
        "{msg}"
    );
    // Absurd values, both reported on one line.
    let msg = config_err(Config::from_toml(
        "[handles]\nmax_total_mb = 99999999\nmax_age_days = 100000\n",
        &origin,
    ));
    assert!(
        msg.contains("max_total_mb = 99999999") && msg.contains("max_age_days = 100000"),
        "{msg}"
    );
    // A negative value is a type error, with the line.
    let msg = config_err(Config::from_toml("[handles]\nmax_age_days = -1\n", &origin));
    assert!(msg.contains(":2:"), "{msg}");
    // An unknown key, with the line.
    let msg = config_err(Config::from_toml("[handles]\nmax_bytes = 5\n", &origin));
    assert!(msg.contains("max_bytes") && msg.contains(":2:"), "{msg}");
    // Through `Config::load` too.
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("sous-chef.toml"),
        "[handles]\nmax_total_mb = 0\n",
    )
    .unwrap();
    let msg = config_err(Config::load(&cli(tmp.path().to_path_buf())));
    assert!(msg.contains("max_total_mb"), "{msg}");
}
