//! Worker thread: owns the client and answers one request at a time.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::Context;
use scalable_embed::{Client, LoginEvent, Options, SessionBackendPreference};

use super::model::Snapshot;
use super::portfolio;

pub enum Request {
    Refresh,
    /// `cancel` cancels this login only.
    Login {
        id: u64,
        cancel: Arc<AtomicBool>,
    },
    Logout,
}

pub enum Reply {
    Data(Snapshot),
    NeedsLogin,
    Error(String),
    /// Progress of the login with this `id`.
    Login {
        id: u64,
        event: LoginEvent,
    },
    LoginDone {
        id: u64,
        result: Result<(), String>,
    },
    LoggedOut(Result<(), String>),
}

impl Reply {
    /// Whether this reply ends a request, as opposed to reporting progress.
    pub fn is_final(&self) -> bool {
        !matches!(self, Reply::Login { .. })
    }
}

pub fn spawn(file_session: bool) -> (Sender<Request>, Receiver<Reply>) {
    let (requests, worker_requests) = mpsc::channel();
    let (worker_replies, replies) = mpsc::channel();
    thread::spawn(move || run(worker_requests, worker_replies, file_session));
    (requests, replies)
}

fn run(requests: Receiver<Request>, replies: Sender<Reply>, file_session: bool) {
    // Without a client, every request gets the reason, so each still ends.
    let mut client = client(file_session).map_err(|err| format!("{err:#}"));
    for request in requests {
        let reply = match (&mut client, request) {
            (Err(err), Request::Login { id, .. }) => Reply::LoginDone {
                id,
                result: Err(err.clone()),
            },
            (Err(err), Request::Refresh | Request::Logout) => Reply::Error(err.clone()),
            (Ok(client), Request::Refresh) => match portfolio::fetch(client) {
                Ok(snapshot) => Reply::Data(snapshot),
                Err(err) if err.needs_login() => Reply::NeedsLogin,
                Err(err) => Reply::Error(err.to_string()),
            },
            (Ok(client), Request::Login { id, cancel }) => Reply::LoginDone {
                id,
                result: client
                    .login(&cancel, |event| {
                        let _ = replies.send(Reply::Login { id, event });
                    })
                    .map(drop)
                    .map_err(|err| err.to_string()),
            },
            (Ok(client), Request::Logout) => {
                Reply::LoggedOut(client.logout().map(drop).map_err(|e| e.to_string()))
            }
        };
        if replies.send(reply).is_err() {
            return;
        }
    }
}

/// With files and a keychain entry of its own, apart from the `sc` CLI's.
fn client(file_session: bool) -> anyhow::Result<Client> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let config_dir = std::path::Path::new(&home).join(".config/scalable-embed-example");
    let mut options = Options::new(config_dir, "dev.example.scalable-embed");
    if file_session {
        options.config.auth.session_backend = SessionBackendPreference::File;
    }
    Ok(Client::new(options)?)
}

