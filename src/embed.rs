//! Embedding API (fork addition, not upstream): drive Scalable in-process from
//! another application, such as a terminal or desktop UI. Read with GraphQL
//! queries of your own (`Client::query`); trade and make other changes with
//! `sc` commands (`Client::run`), which return the same JSON `data` that
//! `sc <command> --json` prints and keep the CLI's semantics, including the
//! two-step trade and savings-plan confirmation flows.
//!
//! The split follows what a mistake costs. A wrong query shows a wrong number,
//! so reads are free-form and fetch what a screen needs in one request, where
//! several commands would hit the rate limit. A wrong mutation places a wrong
//! order, so changes only go through upstream's commands, with their previews,
//! confirmations and trade controls, and `query` rejects mutations.
//!
//! All calls block. In async UIs, run them on a blocking thread
//! (e.g. `tokio::task::spawn_blocking`) and guard the `Client` with a mutex.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Parser;
use serde::de::DeserializeOwned;
use serde_json::Value;

mod document;
mod error;
mod types;

pub use error::{Error, Result};
pub use types::*;

pub use crate::auth::DeviceCodePrompt;
use crate::auth::LoginProgressSink;
pub use crate::broker_context::BrokerContext;
pub use crate::config::{
    AppConfig, DpopKeyBackend, Pkcs11Config, RuntimeAuthConfig, SessionBackendPreference,
    TradeControlsConfig,
};
use crate::config::{EnvConfig, TargetEnv};
use crate::dpop::DpopRuntimeOptions;
use crate::session::{Session, SessionManager};

/// Config dir and keyring service, set by the first `Client::new`.
static STORAGE: OnceLock<(PathBuf, String)> = OnceLock::new();

pub(crate) fn config_dir_override() -> Option<&'static Path> {
    STORAGE.get().map(|(dir, _)| dir.as_path())
}

pub(crate) fn keyring_service_or(default: &'static str) -> &'static str {
    STORAGE
        .get()
        .map_or(default, |(_, service)| service.as_str())
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
    /// Storage locations are process-wide: the first call sets them, even if
    /// it fails, and later calls must pass the same `config_dir` and
    /// `keyring_service`.
    pub fn new(options: Options) -> Result<Self> {
        let storage = (options.config_dir, options.keyring_service);
        if let Err(storage) = STORAGE.set(storage)
            && STORAGE.get() != Some(&storage)
        {
            return Err(Error::invalid_input(
                "embed::Client storage is process-wide and already set to a different \
                 config_dir or keyring_service",
            ));
        }
        let session_manager = SessionManager::new(&options.config)?;
        Ok(Self {
            config: options.config,
            session_manager,
        })
    }

    /// Device-code login. `on_event` gets the code to show (and typically
    /// open in the browser), then each new wait. Setting `cancel` cancels the
    /// login within about 100 ms of a wait, or after the request in flight;
    /// it then fails with code [`Error::LOGIN_CANCELLED`].
    ///
    /// Like `sc login`, it then saves and returns the default account and
    /// portfolio. If that fails, the login still succeeds with `None`, and
    /// commands look them up on each call.
    pub fn login(
        &mut self,
        cancel: &AtomicBool,
        on_event: impl FnMut(LoginEvent),
    ) -> Result<Option<BrokerContext>> {
        let env = crate::channel::current_env();
        let env_cfg = crate::channel::current_env_config();
        let dpop_options = crate::channel::current_dpop_runtime_options(&self.config);
        let mut events = LoginEvents {
            on_event,
            cancel,
            last: None,
            cancelled: false,
        };
        let result = crate::auth::login_with_device_code_events(
            &mut self.session_manager,
            env,
            &env_cfg,
            None,
            &dpop_options,
            |events, prompt| {
                events.send(LoginEvent::DeviceCode(prompt.clone()));
                Ok(())
            },
            &mut events,
        );
        if events.cancelled {
            return Err(Error::new(Error::LOGIN_CANCELLED, "login cancelled"));
        }
        result?;
        crate::cleanup_broker_context_best_effort();
        Ok(
            crate::broker_commands::bootstrap_broker_context_after_login(
                &mut self.session_manager,
                env,
                &env_cfg,
                &dpop_options,
            )
            .ok(),
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
    /// leading `sc`, e.g. `["broker", "quote", "--isin", "IE00B4L5Y983"]`, and
    /// returns the `data` that `sc <command> --json` prints. The way to trade
    /// and make other changes; for reading, `query` fetches more per request.
    pub fn run<I, T>(&mut self, args: I) -> Result<Value>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let argv = std::iter::once(OsString::from("sc")).chain(args.into_iter().map(Into::into));
        let crate::cli::Cli { command } = crate::cli::Cli::try_parse_from(argv)?;
        let command = crate::normalize_command(command)?;
        Ok(crate::run_machine_command(
            command,
            &self.config,
            &mut self.session_manager,
        )?)
    }

    /// Like `run`, but deserializes the result into your own type.
    pub fn run_as<R, I, T>(&mut self, args: I) -> Result<R>
    where
        R: DeserializeOwned,
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        parse(self.run(args)?)
    }

    /// The account and portfolio IDs broker commands use, for the variables
    /// of your own queries. With `portfolio_id: None`, the portfolio saved at
    /// login.
    pub fn broker_ids(&mut self, portfolio_id: Option<&str>) -> Result<BrokerIds> {
        let mut active = self.active_session()?;
        let ids = crate::broker_shared::resolve_broker_ids(
            &mut self.session_manager,
            active.env,
            &active.env_cfg,
            &mut active.session,
            &active.dpop_options,
            portfolio_id,
        )?;
        Ok(BrokerIds {
            account_id: ids.account_id,
            portfolio_id: ids.portfolio_id,
        })
    }

    /// Sends a GraphQL query of your own to the broker API and returns its
    /// `data`, with the session, request signing and token refresh handled
    /// as for commands. `variables` is a JSON object or null.
    ///
    /// Only for reading: a document with a mutation or subscription fails
    /// with `invalid_input`, so trades and other changes go through commands,
    /// with their confirmation flows and trade controls. Upstream doesn't
    /// maintain these queries, so an API change breaks them without a failing
    /// test here.
    pub fn query(&mut self, operation_name: &str, query: &str, variables: Value) -> Result<Value> {
        if !document::only_queries(query) {
            return Err(Error::invalid_input(
                "query: mutations and subscriptions aren't allowed; use commands instead",
            ));
        }
        let variables = match variables {
            Value::Null => Value::Object(Default::default()),
            Value::Object(_) => variables,
            _ => {
                return Err(Error::invalid_input(
                    "query: variables must be a JSON object",
                ));
            }
        };
        let mut active = self.active_session()?;
        Ok(crate::session_refresh::execute_with_refresh_retry(
            &mut self.session_manager,
            active.env,
            &active.env_cfg,
            &mut active.session,
            &active.dpop_options,
            |token| {
                crate::graphql::execute_graphql(
                    &active.env_cfg.graphql_url,
                    token,
                    query,
                    &variables,
                    Some(operation_name),
                    active.access_context,
                    &active.dpop_options,
                )
            },
        )?)
    }

    /// Like `query`, but deserializes the `data` into your own type. For
    /// decimals the API sends as strings, see [`decimal`].
    pub fn query_as<R: DeserializeOwned>(
        &mut self,
        operation_name: &str,
        query: &str,
        variables: Value,
    ) -> Result<R> {
        parse(self.query(operation_name, query, variables)?)
    }

    /// Loads the session, refreshing it if needed, as commands do.
    fn active_session(&mut self) -> Result<ActiveSession> {
        let env = crate::resolve_active_env(&self.session_manager)?;
        let env_cfg = crate::channel::current_env_config();
        let dpop_options = crate::channel::current_dpop_runtime_options(&self.config);
        let loaded = crate::active_session::load_active_session(
            &mut self.session_manager,
            env,
            &env_cfg,
            &dpop_options,
        )?;
        Ok(ActiveSession {
            env,
            env_cfg,
            dpop_options,
            session: loaded.session,
            access_context: loaded.access_context,
        })
    }
}

struct ActiveSession {
    env: TargetEnv,
    env_cfg: EnvConfig,
    dpop_options: DpopRuntimeOptions,
    session: Session,
    access_context: crate::graphql::GraphqlAccessContext,
}

/// The IDs most broker queries take as `$accountId` and `$portfolioId`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerIds {
    pub account_id: String,
    pub portfolio_id: String,
}

/// Login progress, for `Client::login`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginEvent {
    /// The code to show and confirm in the browser. Comes first.
    DeviceCode(DeviceCodePrompt),
    /// Waiting for the user to confirm the code in the browser.
    WaitingForBrowser,
    /// Waiting for the user to approve the login on their linked device.
    WaitingForSecondFactor,
}

impl DeviceCodePrompt {
    /// The page to confirm the code at, with the code filled in if possible.
    pub fn url(&self) -> &str {
        self.verification_uri_complete
            .as_deref()
            .unwrap_or(&self.verification_uri)
    }
}

/// Turns upstream's login progress hooks into `LoginEvent`s, each sent once,
/// and does their waiting in short steps so `cancel` takes effect quickly.
struct LoginEvents<'a, F> {
    on_event: F,
    cancel: &'a AtomicBool,
    last: Option<LoginEvent>,
    /// Set when the login stopped for `cancel`, as upstream rewords 2FA errors.
    cancelled: bool,
}

impl<F: FnMut(LoginEvent)> LoginEvents<'_, F> {
    const STEP: Duration = Duration::from_millis(100);

    fn send(&mut self, event: LoginEvent) {
        if self.last.as_ref() != Some(&event) {
            self.last = Some(event.clone());
            (self.on_event)(event);
        }
    }

    fn wait(&mut self, event: LoginEvent, duration: Duration) -> anyhow::Result<()> {
        let end = Instant::now() + duration;
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                self.cancelled = true;
                anyhow::bail!("login cancelled");
            }
            self.send(event.clone());
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            std::thread::sleep(left.min(Self::STEP));
        }
    }
}

impl<F: FnMut(LoginEvent)> LoginProgressSink for LoginEvents<'_, F> {
    fn note_browser_confirmation_pending(&mut self, _pending_polls: u64) -> anyhow::Result<()> {
        Ok(())
    }
    fn note_browser_confirmation_slow_down(&mut self, _interval_secs: u64) -> anyhow::Result<()> {
        Ok(())
    }
    fn wait_for_browser_confirmation(&mut self, duration: Duration) -> anyhow::Result<()> {
        self.wait(LoginEvent::WaitingForBrowser, duration)
    }
    fn note_second_factor_wait(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
    fn wait_for_second_factor(&mut self, duration: Duration) -> anyhow::Result<()> {
        self.wait(LoginEvent::WaitingForSecondFactor, duration)
    }
    fn finish(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

fn parse<R: DeserializeOwned>(data: Value) -> Result<R> {
    Ok(serde_json::from_value(data).with_context(|| {
        format!(
            "unexpected result shape for {}; did the API or upstream's JSON output change?",
            std::any::type_name::<R>()
        )
    })?)
}
