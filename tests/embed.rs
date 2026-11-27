use std::process::{Command, Output};

use scalable_cli::embed::{Client, Options, SessionBackendPreference};
use serde_json::Value;

const RUN_CLI_TEST: &str = "run_cli_runs_sc_commands_with_the_apps_name_and_storage";
const RUN_CLI_ARGS: &str = "SCALABLE_EMBED_TEST_RUN_CLI_ARGS";
const RUN_CLI_DIR: &str = "SCALABLE_EMBED_TEST_RUN_CLI_DIR";

// `run_cli` exits the process, so this test runs it in a child: this test
// binary again, filtered to this test, with the arguments in the environment.
#[test]
fn run_cli_runs_sc_commands_with_the_apps_name_and_storage() {
    if let Ok(args) = std::env::var(RUN_CLI_ARGS) {
        let args: Vec<String> = serde_json::from_str(&args).expect("child args");
        let dir = std::env::var_os(RUN_CLI_DIR).expect("child config dir");
        let mut options = Options::new(dir, "test.scalable-embed");
        options.config.auth.session_backend = SessionBackendPreference::File;
        scalable_cli::embed::run_cli(options, "app sc", args);
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("app-config");
    std::fs::create_dir(&dir).expect("config dir");
    // `sc` would fail to load this; `run_cli` takes the app's config instead.
    std::fs::write(dir.join("config.toml"), "not toml").expect("config file");
    let run = |args: &[&str]| -> Output {
        Command::new(std::env::current_exe().expect("test binary"))
            .args([RUN_CLI_TEST, "--exact", "--nocapture"])
            .env(RUN_CLI_ARGS, serde_json::to_string(args).expect("args"))
            .env(RUN_CLI_DIR, &dir)
            .output()
            .expect("child")
    };

    let help = run(&["login", "--help"]);
    let stdout = String::from_utf8_lossy(&help.stdout);
    assert!(help.status.success(), "{help:?}");
    assert!(stdout.contains("Usage: app sc login"), "{stdout}");

    let json = run(&["whoami", "--json"]);
    let stdout = String::from_utf8_lossy(&json.stdout);
    let envelope: Value = stdout
        .lines()
        .find_map(|line| serde_json::from_str(line).ok())
        .unwrap_or_else(|| panic!("no JSON envelope in {stdout}"));
    assert_eq!(envelope["error"]["code"], "no_session", "{envelope}");
    assert_eq!(json.status.code(), Some(20), "{json:?}");

    let human = run(&["whoami"]);
    let stderr = String::from_utf8_lossy(&human.stderr);
    assert!(stderr.contains("Error: No active session"), "{stderr}");
    assert_eq!(human.status.code(), Some(1), "{human:?}");
}

// One test sets storage in this process: its locations are process-wide, so
// ordering matters.
#[test]
fn embed_client_uses_its_config_dir_and_pins_storage_locations() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("app-config");

    let mut options = Options::new(&dir, "test.scalable-embed");
    options.config.auth.session_backend = SessionBackendPreference::File;
    let mut client = Client::new(options).expect("client");

    assert!(!client.is_logged_in().expect("session lookup"));
    let err = client.run(["whoami"]).expect_err("no session yet");
    assert_eq!(err.code(), "no_session", "{err}");
    assert!(err.needs_login());
    assert!(dir.is_dir(), "state dir should be created under config_dir");

    let err = client.run(["broker", "bogus"]).expect_err("bad subcommand");
    assert_eq!(err.code(), "invalid_input", "{err}");

    let err = client.run(["login"]).expect_err("login has its own method");
    assert_eq!(err.code(), "invalid_input", "{err}");

    let err = client
        .query("Buy", "mutation Buy { buy }", Value::Null)
        .expect_err("mutation");
    assert_eq!(err.code(), "invalid_input", "{err}");
    let err = client
        .query("Q", "{ account { id } }", Value::Null)
        .expect_err("no session yet");
    assert!(err.needs_login(), "{err}");

    let same = Options::new(&dir, "test.scalable-embed");
    assert!(Client::new(same).is_ok(), "same locations are fine");

    let other = Options::new(tmp.path().join("elsewhere"), "test.scalable-embed");
    assert!(
        Client::new(other).is_err(),
        "different config_dir must be rejected"
    );
    let other = Options::new(&dir, "test.other");
    assert!(
        Client::new(other).is_err(),
        "different keyring_service must be rejected"
    );
}
