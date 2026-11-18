//! Minimal embedder: what a desktop app's backend would do.
//!
//!   cargo run --example embed -- login
//!   cargo run --example embed -- broker overview
//!   cargo run --example embed -- logout
//!
//! The session goes to the OS keychain; the signing key and other files go to
//! ~/.config/scalable-embed-example, next to the CLI's ~/.config/scalable-cli.
//! Pass `--file-session` first to store the session in that folder as well.

use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use scalable_cli::embed::{
    Client, DeviceCodePrompt, LoginProgressSink, Options, SessionBackendPreference,
};

struct Progress;

impl LoginProgressSink for Progress {
    fn note_browser_confirmation_pending(&mut self, polls: u64) -> Result<()> {
        eprintln!("waiting for browser confirmation (poll {polls})");
        Ok(())
    }
    fn note_browser_confirmation_slow_down(&mut self, secs: u64) -> Result<()> {
        eprintln!("slowing down, polling every {secs}s");
        Ok(())
    }
    fn wait_for_browser_confirmation(&mut self, d: Duration) -> Result<()> {
        thread::sleep(d);
        Ok(())
    }
    fn note_second_factor_wait(&mut self) -> Result<()> {
        eprintln!("approve the 2FA request in the Scalable app");
        Ok(())
    }
    fn wait_for_second_factor(&mut self, d: Duration) -> Result<()> {
        thread::sleep(d);
        Ok(())
    }
    fn finish(&mut self) -> Result<()> {
        eprintln!("login finished");
        Ok(())
    }
}

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let file_session = args.first().is_some_and(|a| a == "--file-session");
    if file_session {
        args.remove(0);
    }
    let command = &args[..];
    anyhow::ensure!(!command.is_empty(), "usage: embed [--file-session] <command...>");
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let config_dir = std::path::Path::new(&home).join(".config/scalable-embed-example");

    let mut options = Options::new(config_dir, "dev.example.scalable-embed");
    if file_session {
        options.config.auth.session_backend = SessionBackendPreference::File;
    }
    let mut client = Client::new(options)?;

    if command.first().is_some_and(|c| c == "login") {
        let ctx = client.login(
            |p: &DeviceCodePrompt| {
                let url = p.verification_uri_complete.as_ref().unwrap_or(&p.verification_uri);
                eprintln!("code {} — confirm at {url}", p.user_code);
                let _ = std::process::Command::new("open").arg(url).status();
                Ok(())
            },
            &mut Progress,
        )?;
        eprintln!("account {}, portfolio {:?}", ctx.account_id, ctx.portfolio_id);
        return Ok(());
    }

    let data = client.run(command)?;
    println!("{}", serde_json::to_string_pretty(&data)?);
    Ok(())
}
