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

pub use error::{Error, Result};

use crate::auth::{DeviceCodeResponse, LoginProgressSink};
pub use crate::broker_context::BrokerContext;
pub use crate::config::{
    AppConfig, DpopKeyBackend, Pkcs11Config, RuntimeAuthConfig, SessionBackendPreference,
    TradeControlsConfig,
};
use crate::config::{EnvConfig, TargetEnv};
use crate::dpop::DpopRuntimeOptions;
use crate::session::{Session, SessionManager};

/// Config dir and keyring service, set by the first `Client::new`.
static STORAGE: OnceLock<Storage> = OnceLock::new();

#[derive(PartialEq, Eq)]
struct Storage {
    config_dir: PathBuf,
    keyring_service: String,
    secure_enclave_label: String,
}

impl Storage {
    fn new(config_dir: PathBuf, keyring_service: String) -> Self {
        let secure_enclave_label = format!("{keyring_service}:auth-signing-key:v2");
        Self {
            config_dir,
            keyring_service,
            secure_enclave_label,
        }
    }
}

pub(crate) fn config_dir_override() -> Option<&'static Path> {
    STORAGE.get().map(|storage| storage.config_dir.as_path())
}

/// The embedder's keyring service, or upstream's `default` without one.
pub(crate) fn keyring_service(default: &'static str) -> &'static str {
    STORAGE
        .get()
        .map_or(default, |storage| storage.keyring_service.as_str())
}

/// The embedder's Secure Enclave key label, or upstream's `default` without one.
pub(crate) fn secure_enclave_label(default: &'static str) -> &'static str {
    STORAGE
        .get()
        .map_or(default, |storage| storage.secure_enclave_label.as_str())
}

pub struct Options {
    /// Holds the session (file backend), signing key, broker context and
    /// confirmation stores. Use an app-specific directory so the app and the
    /// `sc` CLI never overwrite each other's state.
    pub config_dir: PathBuf,
    /// Keychain service name for the session secret and namespace for the
    /// Secure Enclave signing key, when those backends are selected.
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
        let storage = Storage::new(options.config_dir, options.keyring_service);
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

    /// Device-code login. `on_event` first receives [`LoginEvent::DeviceCode`]
    /// with the code to show and the browser URL. If linked-device approval is
    /// required, it then receives [`LoginEvent::WaitingForSecondFactor`] once.
    /// During waits, `cancel` is checked about every 100 ms. Requests in flight
    /// finish first. If cancellation is observed, the call fails with code
    /// `login_cancelled`; if login completes first, it succeeds.
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
            second_factor_sent: false,
            cancelled: false,
        };
        let result = crate::auth::login_with_device_code_with_hooks(
            &mut self.session_manager,
            env,
            &env_cfg,
            None,
            &dpop_options,
            LoginEvents::show_device_code,
            &mut events,
        );
        if events.cancelled {
            return Err(Error::new("login_cancelled", "login cancelled"));
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
    pub fn logout(&mut self) -> Result<()> {
        self.run(["logout"]).map(drop)
    }

    pub fn is_logged_in(&self) -> Result<bool> {
        Ok(self.session_manager.load_active()?.is_some())
    }

    /// Runs any `sc` command except `login`, given its arguments without the
    /// leading `sc`, e.g.
    /// `["broker", "trade", "buy", "--isin", "IE00B4L5Y983", "--amount", "100"]`, and
    /// returns the `data` that `sc <command> --json` prints. The way to trade
    /// and make other changes; for reading, `query` fetches more per request.
    /// The trade example only previews the order; repeating it with `--confirm`
    /// and the ID from the preview submits it.
    pub fn run<I, T>(&mut self, args: I) -> Result<Value>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString>,
    {
        let argv = std::iter::once(OsString::from("sc")).chain(args.into_iter().map(Into::into));
        let crate::cli::Cli { command } = crate::cli::Cli::try_parse_from(argv)?;
        // Upstream's `login` prints its prompt to the terminal.
        if matches!(command, crate::cli::Commands::Login(_)) {
            return Err(Error::invalid_input("run: use Client::login to log in"));
        }
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
        T: Into<OsString>,
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
    /// decimals the API sends as strings, see `scalable_embed::decimal`.
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

/// The device-code prompt to display in your login UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCodePrompt {
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: i64,
}

impl DeviceCodePrompt {
    /// The page to confirm the code at, with the code filled in if possible.
    pub fn url(&self) -> &str {
        self.verification_uri_complete
            .as_deref()
            .unwrap_or(&self.verification_uri)
    }
}

/// Login progress, for `Client::login`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginEvent {
    /// The code to show and confirm in the browser. Comes first.
    DeviceCode(DeviceCodePrompt),
    /// Waiting for the user to approve the login on their linked device.
    WaitingForSecondFactor,
}

/// Turns upstream's login progress hooks into `LoginEvent`s, each sent once,
/// and does their waiting in short steps so `cancel` takes effect quickly.
struct LoginEvents<'a, F> {
    on_event: F,
    cancel: &'a AtomicBool,
    second_factor_sent: bool,
    /// Set when the login stopped for `cancel`, as upstream rewords 2FA errors.
    cancelled: bool,
}

impl<F: FnMut(LoginEvent)> LoginEvents<'_, F> {
    const STEP: Duration = Duration::from_millis(100);

    fn show_device_code(&mut self, device: &DeviceCodeResponse) -> anyhow::Result<()> {
        (self.on_event)(LoginEvent::DeviceCode(DeviceCodePrompt {
            user_code: device.user_code.clone(),
            verification_uri: device.verification_uri.clone(),
            verification_uri_complete: device.verification_uri_complete.clone(),
            expires_in: device.expires_in,
        }));
        Ok(())
    }

    fn wait(&mut self, duration: Duration) -> anyhow::Result<()> {
        let end = Instant::now() + duration;
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                self.cancelled = true;
                anyhow::bail!("login cancelled");
            }
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
        self.wait(duration)
    }
    fn note_second_factor_wait(&mut self) -> anyhow::Result<()> {
        if !self.second_factor_sent {
            self.second_factor_sent = true;
            (self.on_event)(LoginEvent::WaitingForSecondFactor);
        }
        Ok(())
    }
    fn wait_for_second_factor(&mut self, duration: Duration) -> anyhow::Result<()> {
        self.wait(duration)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secure_enclave_label_is_scoped_to_the_app_service() {
        let app = Storage::new(PathBuf::from("app"), "test.scalable-embed".to_string());
        let other = Storage::new(PathBuf::from("other"), "test.other-app".to_string());
        assert_eq!(
            app.secure_enclave_label,
            "test.scalable-embed:auth-signing-key:v2"
        );
        assert_ne!(app.secure_enclave_label, other.secure_enclave_label);
        assert_ne!(
            app.secure_enclave_label,
            "scalable.capital:scalable-cli:auth-signing-key:v2"
        );
    }

    #[test]
    fn login_events_deliver_the_prompt_and_second_factor_once() {
        let cancel = AtomicBool::new(false);
        let mut received = Vec::new();
        let mut events = LoginEvents {
            on_event: |event| received.push(event),
            cancel: &cancel,
            second_factor_sent: false,
            cancelled: false,
        };
        let prompt = DeviceCodePrompt {
            user_code: "ABCD".to_string(),
            verification_uri: "https://verify".to_string(),
            verification_uri_complete: Some("https://verify?code=ABCD".to_string()),
            expires_in: 600,
        };
        let device: DeviceCodeResponse = serde_json::from_value(serde_json::json!({
            "device_code": "private-polling-code",
            "user_code": prompt.user_code,
            "verification_uri": prompt.verification_uri,
            "verification_uri_complete": prompt.verification_uri_complete,
            "expires_in": prompt.expires_in,
        }))
        .expect("device-code response");
        events.show_device_code(&device).expect("prompt");
        events
            .wait_for_browser_confirmation(Duration::ZERO)
            .expect("wait");
        events
            .note_browser_confirmation_pending(1)
            .expect("pending");
        events
            .note_browser_confirmation_slow_down(7)
            .expect("slow down");
        events
            .wait_for_browser_confirmation(Duration::ZERO)
            .expect("wait");
        events.note_second_factor_wait().expect("second factor");
        events.wait_for_second_factor(Duration::ZERO).expect("wait");
        events
            .note_second_factor_wait()
            .expect("second factor again");
        events.finish().expect("finish");

        assert_eq!(
            received,
            [
                LoginEvent::DeviceCode(prompt),
                LoginEvent::WaitingForSecondFactor
            ]
        );
    }

    #[test]
    fn login_can_be_cancelled_during_either_wait() {
        for second_factor in [false, true] {
            let cancel = AtomicBool::new(true);
            let mut events = LoginEvents {
                on_event: |_| {},
                cancel: &cancel,
                second_factor_sent: false,
                cancelled: false,
            };
            let duration = Duration::from_secs(60);
            let result = if second_factor {
                events.wait_for_second_factor(duration)
            } else {
                events.wait_for_browser_confirmation(duration)
            };
            assert!(result.is_err());
            assert!(events.cancelled);
        }
    }
}
