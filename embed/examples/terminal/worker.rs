//! Worker thread: owns the client and answers one request at a time.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use scalable_embed::{Client, LoginEvent};

use super::model::Snapshot;
use super::portfolio;

pub enum Request {
    Refresh,
    /// `cancel` cancels this login only.
    Login(Arc<AtomicBool>),
    Logout,
}

pub enum Reply {
    Data(Snapshot),
    NeedsLogin,
    /// The backend refused the request as too many came before it.
    RateLimited(String),
    Error(String),
    Login(LoginEvent),
    LoginDone(Result<(), String>),
    LoggedOut(Result<(), String>),
}

pub fn spawn(client: Client) -> (Sender<Request>, Receiver<Reply>) {
    let (requests, worker_requests) = mpsc::channel();
    let (worker_replies, replies) = mpsc::channel();
    thread::spawn(move || run(client, worker_requests, worker_replies));
    (requests, replies)
}

fn run(mut client: Client, requests: Receiver<Request>, replies: Sender<Reply>) {
    for request in requests {
        let reply = match request {
            Request::Refresh => match portfolio::fetch(&mut client) {
                Ok(snapshot) => Reply::Data(snapshot),
                Err(err) if err.needs_login() => Reply::NeedsLogin,
                Err(err) if err.code() == "rate_limited" => Reply::RateLimited(err.to_string()),
                Err(err) => Reply::Error(err.to_string()),
            },
            Request::Login(cancel) => Reply::LoginDone(
                client
                    .login(&cancel, |event| {
                        let _ = replies.send(Reply::Login(event));
                    })
                    .map(drop)
                    .map_err(|err| err.to_string()),
            ),
            Request::Logout => Reply::LoggedOut(client.logout().map_err(|e| e.to_string())),
        };
        if replies.send(reply).is_err() {
            return;
        }
    }
}
