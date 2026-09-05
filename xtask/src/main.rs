//! Development automation only. No shell strings, secret files or model subprocesses.
use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::{path::Path, process::Command};
#[derive(Parser)] struct Args { #[command(subcommand)] command:Action }
#[derive(Subcommand)] enum Action {
    /// Format check, clippy and Rust tests. Native check is opt-in because it is heavy.
    Check { #[arg(long)] native:bool },
    /// Create a NEW PRIVATE GitHub repository and push the clean local Git history.
    Publish { #[arg(long)] repo:String },
}
fn checked(program:&str,args:&[&str])->Result<()> {
    let status=Command::new(program).args(args).env("GH_HOST","github.com").status().with_context(|| format!("could not run {program}; install/authenticate it first"))?;
    if !status.success() { bail!("{program} failed with {status}"); } Ok(())
}
fn capture(program:&str,args:&[&str])->Result<String> {
    let output=Command::new(program).args(args).output()?;
    if !output.status.success() { bail!("{program} {} failed",args.join(" ")); }
    Ok(String::from_utf8(output.stdout)?.trim().into())
}
fn main()->Result<()> {
    std::env::set_current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).parent().context("workspace directory missing")?)?;
    match Args::parse().command {
        Action::Check { native } => {
            checked("cargo", &["fmt","--all","--","--check"])?;
            checked("cargo", &["clippy","--workspace","--all-targets"])?;
            checked("cargo", &["test","--workspace"])?;
            if native { checked("cargo", &["check","-p","avw","--features","native"])?; }
        }
        Action::Publish { repo } => {
            let parts:Vec<_>=repo.split('/').collect();
            if parts.len()!=2 || parts.iter().any(|s| s.is_empty() || s.starts_with('-') || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))) { bail!("--repo must be OWNER/NAME"); }
            checked("gh", &["auth","status","--hostname","github.com"])?;
            capture("git", &["rev-parse","--verify","HEAD"])?;
            if !capture("git", &["status","--porcelain"])?.is_empty() { bail!("commit all changes before publishing; no automatic staging of private files"); }
            if capture("git", &["remote"])?.lines().any(|s| s=="origin") { bail!("origin already exists; refusing to change or replace it"); }
            // gh refuses to create an existing repository. Never push to a pre-existing target.
            checked("gh", &["repo","create",&repo,"--private","--source=.","--remote=origin","--push"])?;
            checked("gh", &["repo","view",&repo,"--json","nameWithOwner,url,isPrivate"])?;
        }
    }
    Ok(())
}
