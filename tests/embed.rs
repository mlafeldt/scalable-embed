use scalable_cli::embed::{Client, Options, SessionBackendPreference};

// One test: storage locations are process-wide, so ordering matters.
#[test]
fn embed_client_uses_its_config_dir_and_pins_storage_locations() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("app-config");

    let mut options = Options::new(&dir, "test.scalable-embed");
    options.config.auth.session_backend = SessionBackendPreference::File;
    let mut client = Client::new(options).expect("client");

    assert!(!client.is_logged_in().expect("session lookup"));
    let err = client.run(["whoami"]).expect_err("no session yet");
    assert!(err.to_string().contains("No active session"), "{err:#}");
    assert!(dir.is_dir(), "state dir should be created under config_dir");

    let err = client.run(["broker", "bogus"]).expect_err("bad subcommand");
    assert!(err.to_string().contains("unrecognized subcommand"), "{err:#}");

    let same = Options::new(&dir, "test.scalable-embed");
    assert!(Client::new(same).is_ok(), "same locations are fine");

    let other = Options::new(tmp.path().join("elsewhere"), "test.scalable-embed");
    assert!(Client::new(other).is_err(), "different config_dir must be rejected");
}
