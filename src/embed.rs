//! Embedding API (fork addition, not upstream): drive Scalable in-process from
//! another application, such as a desktop UI. Commands return the same JSON
//! `data` that `sc <command> --json` prints, and keep the CLI's semantics,
//! including the two-step trade and savings-plan confirmation flows.
//!
//! All calls block. In async UIs, run them on a blocking thread
//! (e.g. `tokio::task::spawn_blocking`) and guard the `Client` with a mutex.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::de::DeserializeOwned;
use serde_json::Value;

mod types;

pub use types::*;

pub use crate::auth::{DeviceCodePrompt, LoginProgressSink};
pub use crate::broker_context::BrokerContext;
pub use crate::config::{
    AppConfig, DpopKeyBackend, Pkcs11Config, RuntimeAuthConfig, SessionBackendPreference,
    TradeControlsConfig,
};
use crate::session::SessionManager;

static CONFIG_DIR: OnceLock<PathBuf> = OnceLock::new();
static KEYRING_SERVICE: OnceLock<String> = OnceLock::new();

pub(crate) fn config_dir_override() -> Option<&'static Path> {
    CONFIG_DIR.get().map(PathBuf::as_path)
}

pub(crate) fn keyring_service_or(default: &'static str) -> &'static str {
    KEYRING_SERVICE.get().map_or(default, String::as_str)
}

pub struct Options {
    /// Holds the session (file backend), signing key, broker context and
    /// confirmation stores. Use an app-specific directory so the app and the
    /// `sc` CLI never overwrite each other's state.
    pub config_dir: PathBuf,
    /// Keychain service name for the session secret (keyring backend only).
    pub keyring_service: String,
    pub config: AppConfig,
}

impl Options {
    /// Defaults: session in the OS keyring, signing key as a private file in
    /// `config_dir`. This is less secure than the official CLI on macOS, which
    /// keeps the signing key in the Secure Enclave. The Secure Enclave backend
    /// fails in unsigned builds with OSStatus -34018 (missing entitlement).
    pub fn new(config_dir: impl Into<PathBuf>, keyring_service: impl Into<String>) -> Self {
        Self {
            config_dir: config_dir.into(),
            keyring_service: keyring_service.into(),
            config: AppConfig {
                auth: RuntimeAuthConfig {
                    signing_key_backend: DpopKeyBackend::File,
                    ..RuntimeAuthConfig::default()
                },
                trade_controls: None,
            },
        }
    }
}

pub struct Client {
    config: AppConfig,
    session_manager: SessionManager,
}

impl Client {
    /// Storage locations are process-wide: a second `Client` in the same
    /// process must use the same `config_dir` and `keyring_service`.
    pub fn new(options: Options) -> Result<Self> {
        set_once(&CONFIG_DIR, options.config_dir, "config_dir")?;
        set_once(&KEYRING_SERVICE, options.keyring_service, "keyring_service")?;
        let session_manager = SessionManager::new(&options.config)?;
        Ok(Self {
            config: options.config,
            session_manager,
        })
    }

    /// Device-code login. `on_device_code` receives the code and URL to show
    /// (and typically opens the browser); `progress` receives polling and 2FA
    /// events and must sleep for the durations passed to its `wait_*` methods.
    /// Like `sc login`, it then saves the default account and portfolio.
    pub fn login(
        &mut self,
        on_device_code: impl FnMut(&DeviceCodePrompt) -> Result<()>,
        progress: &mut impl LoginProgressSink,
    ) -> Result<BrokerContext> {
        let env = crate::channel::current_env();
        let env_cfg = crate::channel::current_env_config();
        let dpop_options = crate::channel::current_dpop_runtime_options(&self.config);
        crate::transport_security::validate_env_transport_security(&env_cfg)?;
        crate::auth::login_with_device_code_events(
            &mut self.session_manager,
            env,
            &env_cfg,
            None,
            &dpop_options,
            on_device_code,
            progress,
        )?;
        crate::cleanup_broker_context_best_effort();
        crate::broker_commands::bootstrap_broker_context_after_login(
            &mut self.session_manager,
            env,
            &env_cfg,
            &dpop_options,
        )
    }

    /// Revokes tokens and deletes local session state, like `sc logout`.
    pub fn logout(&mut self) -> Result<Value> {
        self.run(["logout"])
    }

    pub fn is_logged_in(&self) -> Result<bool> {
        Ok(self.session_manager.load_active()?.is_some())
    }

    /// Runs any `sc` command except `login`, given its arguments without the
    /// leading `sc`, e.g. `["broker", "quote", "--isin", "IE00B4L5Y983"]`.
    pub fn run<I, T>(&mut self, args: I) -> Result<Value>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let argv = std::iter::once(OsString::from("sc")).chain(args.into_iter().map(Into::into));
        let crate::cli::Cli { command } = crate::cli::Cli::try_parse_from(argv)?;
        self.run_command(command)
    }

    /// Like `run`, but deserializes the result into your own type, for
    /// commands that have no typed method here yet.
    pub fn run_as<R, I, T>(&mut self, args: I) -> Result<R>
    where
        R: DeserializeOwned,
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        parse(self.run(args)?)
    }

    /// `sc broker overview`. With `portfolio_id: None`, uses the portfolio
    /// saved at login.
    pub fn broker_overview(
        &mut self,
        portfolio_id: Option<&str>,
        include_year_to_date: bool,
    ) -> Result<BrokerResult<Overview>> {
        use crate::cli::{BrokerArgs, BrokerCommand, BrokerOverviewArgs, Commands};
        let command = Commands::Broker(BrokerArgs {
            command: BrokerCommand::Overview(BrokerOverviewArgs {
                portfolio_id: portfolio_id.map(str::to_string),
                include_year_to_date,
                json: true,
            }),
        });
        parse(self.run_command(command)?)
    }

    fn run_command(&mut self, command: crate::cli::Commands) -> Result<Value> {
        let command = crate::normalize_command(command)?;
        crate::run_machine_command(command, &self.config, &mut self.session_manager)
    }
}

fn parse<R: DeserializeOwned>(data: Value) -> Result<R> {
    serde_json::from_value(data).with_context(|| {
        format!(
            "unexpected result shape for {}; did the upstream JSON output change?",
            std::any::type_name::<R>()
        )
    })
}

fn set_once<T: PartialEq>(cell: &OnceLock<T>, value: T, name: &str) -> Result<()> {
    if let Err(value) = cell.set(value)
        && cell.get() != Some(&value)
    {
        bail!("embed::Client {name} is process-wide and already set to a different value");
    }
    Ok(())
}
