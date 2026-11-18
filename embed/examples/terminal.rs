//! Minimal embedder: what a desktop app's backend would do.
//!
//!   cargo run --example terminal -- login
//!   cargo run --example terminal -- broker overview   # raw JSON, any sc command
//!   cargo run --example terminal -- overview          # typed
//!   cargo run --example terminal -- logout
//!
//! The session goes to the OS keychain; the signing key and other files go to
//! ~/.config/scalable-embed-example, next to the CLI's ~/.config/scalable-cli.
//! Pass `--file-session` first to store the session in that folder as well.

use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result};
use scalable_embed::{Client, LoginEvent, Options, SessionBackendPreference};

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let file_session = args.first().is_some_and(|a| a == "--file-session");
    if file_session {
        args.remove(0);
    }
    let command = &args[..];
    anyhow::ensure!(
        !command.is_empty(),
        "usage: terminal [--file-session] <command...>"
    );
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let config_dir = std::path::Path::new(&home).join(".config/scalable-embed-example");

    let mut options = Options::new(config_dir, "dev.example.scalable-embed");
    if file_session {
        options.config.auth.session_backend = SessionBackendPreference::File;
    }
    let mut client = Client::new(options)?;

    if command.first().is_some_and(|c| c == "login") {
        let never = AtomicBool::new(false); // Ctrl+C ends it.
        let context = client.login(&never, |event| match event {
            LoginEvent::DeviceCode(prompt) => {
                eprintln!("code {} — confirm at {}", prompt.user_code, prompt.url());
                let _ = std::process::Command::new("open")
                    .arg(prompt.url())
                    .status();
            }
            LoginEvent::WaitingForBrowser => eprintln!("waiting for confirmation in the browser"),
            LoginEvent::WaitingForSecondFactor => {
                eprintln!("approve the login on your linked device")
            }
        })?;
        match context {
            Some(ctx) => eprintln!(
                "account {}, portfolio {:?}",
                ctx.account_id, ctx.portfolio_id
            ),
            None => eprintln!("logged in, but saving the default account and portfolio failed"),
        }
        return Ok(());
    }

    if command.first().is_some_and(|c| c == "overview") {
        let overview = client.broker_overview(None, false)?;
        println!("{overview:#?}");
        return Ok(());
    }

    let data = client.run(command)?;
    println!("{}", serde_json::to_string_pretty(&data)?);
    Ok(())
}
