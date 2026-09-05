#![forbid(unsafe_code)]
mod backend;
mod http;
mod mcp;
mod models;
mod service;

use avw_core::{artifacts::{Artifacts, atomic_create, read_bounded}, audio::MAX_AUDIO_BYTES, journal::JobState, transcript::{Transcript,parse_moss}, types::{AssetId,Device,ModelId,Submission}};
use clap::{Parser,Subcommand,ValueEnum};
use serde_json::{Value,json};
use std::{io::{self,Write},net::SocketAddr,path::PathBuf};
use service::App;

#[derive(Parser)]
#[command(name="avw",version,about="Local, Rust-native voice workbench for agents")]
struct Cli {
    #[arg(long,default_value=".avw",global=true)] workspace:PathBuf,
    #[arg(long,value_enum,default_value="cpu",global=true)] device:DeviceArg,
    /// Connect instead of owning a workspace; valid for call/import/export only.
    #[arg(long,global=true)] server:Option<String>,
    #[command(subcommand)] command:Command,
}
#[derive(Clone,Copy,ValueEnum)] enum DeviceArg { Cpu,Wgpu,Metal,Auto }
impl From<DeviceArg> for Device { fn from(d:DeviceArg)->Self { match d { DeviceArg::Cpu=>Self::Cpu,DeviceArg::Wgpu=>Self::Wgpu,DeviceArg::Metal=>Self::Metal,DeviceArg::Auto=>Self::Auto } } }
#[derive(Subcommand)]
enum Command {
    Doctor,
    Capabilities,
    Schema,
    Import { input:PathBuf },
    ImportTranscript { input:PathBuf },
    ParseMoss { input:PathBuf },
    Export { asset_id:String, #[arg(long)] output:PathBuf },
    Models { #[command(subcommand)] command:ModelCommand },
    /// Execute one JSON submission and wait. Run outside an already-owned workspace.
    Run { #[arg(long)] request:PathBuf },
    /// Invoke the same compact tools exposed to MCP. Prefer --server with a daemon.
    Call { tool:String, #[arg(long,default_value="{}")] arguments:String },
    Serve { #[arg(long,default_value="127.0.0.1:8765")] listen:SocketAddr },
    Mcp,
}
#[derive(Subcommand)] enum ModelCommand {
    Add { #[arg(long)] id:ModelId, #[arg(long)] directory:PathBuf, #[arg(long)] revision:String },
    Verify { #[arg(long)] id:ModelId },
    List,
}
fn emit(v:&impl serde::Serialize)->anyhow::Result<()> { let mut out=io::stdout().lock(); serde_json::to_writer(&mut out,v)?; out.write_all(b"\n")?; Ok(()) }
fn main() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into())).with_writer(io::stderr).init();
    if let Err(error)=run(Cli::parse()) { eprintln!("{}",json!({"error":{"code":"command_failed","message":format!("{error:#}")}})); std::process::exit(1); }
}
fn run(cli:Cli)->anyhow::Result<()> {
    let device:Device=cli.device.into();
    if let Some(server)=cli.server { return remote(&server,cli.command); }
    match cli.command {
        Command::Doctor => emit(&json!({"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"version":env!("CARGO_PKG_VERSION"),
            "features":{"native_voxcpm":cfg!(feature="native-voxcpm"),"native_qwen":cfg!(feature="native-qwen"),"wgpu":cfg!(feature="wgpu"),"metal":cfg!(feature="metal")},
            "hardware_probe":"not performed","inference_verified":false,"python_required":false,"notes":"Compile flags do not prove that hardware drivers, checkpoints or inference are operational."})),
        Command::Schema => emit(&schemars::schema_for!(Submission)),
        Command::Capabilities => emit(&models::Registry::new(&cli.workspace).capabilities(device)),
        Command::Import { input } => { let assets=Artifacts::open(&cli.workspace)?; emit(&assets.import_wav(&read_bounded(&input,MAX_AUDIO_BYTES)?)?) }
        Command::ImportTranscript { input } => { let assets=Artifacts::open(&cli.workspace)?; let transcript:Transcript=serde_json::from_slice(&read_bounded(&input,8*1024*1024)?)?; emit(&assets.put_transcript(&transcript)?) }
        Command::ParseMoss { input } => {
            let assets=Artifacts::open(&cli.workspace)?; let bytes=read_bounded(&input,4*1024*1024)?;
            let transcript=parse_moss(std::str::from_utf8(&bytes)?,None)?; emit(&assets.put_transcript(&transcript)?)
        }
        Command::Export { asset_id,output } => {
            let assets=Artifacts::open(&cli.workspace)?; let id=AssetId::try_from(asset_id).map_err(anyhow::Error::msg)?;
            let (_,bytes)=assets.read(&id)?; write_export(&output,&bytes)?; emit(&json!({"output":output,"bytes":bytes.len(),"asset_id":id}))
        }
        Command::Models { command } => {
            let assets=Artifacts::open(&cli.workspace)?; let registry=models::Registry::new(assets.root());
            match command { ModelCommand::Add { id,directory,revision }=>emit(&registry.add(id,&directory,revision)?),ModelCommand::Verify{id}=>emit(&registry.verify(id)?),ModelCommand::List=>emit(&registry.capabilities(device)) }
        }
        Command::Run { request } => {
            let submission:Submission=serde_json::from_slice(&read_bounded(&request,1024*1024)?)?;
            let app=App::open(&cli.workspace,device)?; let submitted=app.submit(submission)?;
            let result=app.runtime.wait(&submitted.job.id)?; emit(&result)?; app.runtime.shutdown()?;
            if result.state!=JobState::Succeeded { anyhow::bail!("job ended in {:?}",result.state); } Ok(())
        }
        Command::Call { .. } => {
            anyhow::bail!("call requires --server; use run for a one-shot execution, or serve/mcp to own the durable worker");
        }
        Command::Mcp => {
            let app=App::open(&cli.workspace,device)?; let result=mcp::serve(&app,&mut io::stdin().lock(),&mut io::stdout().lock());
            app.runtime.shutdown()?; result?; Ok(())
        }
        Command::Serve { listen } => {
            let security=http::Security::new(std::env::var("AVW_TOKEN").ok())?;
            let app=App::open(&cli.workspace,device)?;
            tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(http::serve(app,listen,security))
        }
    }
}
fn write_export(output:&std::path::Path,bytes:&[u8])->anyhow::Result<()> {
    let output=if output.is_absolute() { output.to_path_buf() } else { std::env::current_dir()?.join(output) };
    if !atomic_create(&output,bytes)? { anyhow::bail!("output exists; refusing to overwrite {}",output.display()); } Ok(())
}
fn remote(server:&str,command:Command)->anyhow::Result<()> {
    let base=reqwest::Url::parse(server)?;
    if !matches!(base.scheme(),"http"|"https") || base.host_str().is_none() || !base.username().is_empty() || base.password().is_some() || base.query().is_some() || base.fragment().is_some() { anyhow::bail!("server must be an HTTP(S) base URL without credentials/query/fragment"); }
    if base.scheme()=="http" && !matches!(base.host_str(),Some("localhost"|"127.0.0.1"|"[::1]"|"::1")) { anyhow::bail!("plain HTTP is permitted only on loopback"); }
    let client=reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(120)).redirect(reqwest::redirect::Policy::none()).build()?;
    let token=std::env::var("AVW_TOKEN").ok();
    let mut request=match &command {
        Command::Call { tool,arguments } => {
            if !tool.bytes().all(|b| b.is_ascii_lowercase() || b==b'_') || tool.is_empty() { anyhow::bail!("invalid tool name"); }
            client.post(base.join(&format!("/v1/tools/{tool}"))?).json(&serde_json::from_str::<Value>(arguments)?)
        }
        Command::Import { input } => client.post(base.join("/v1/assets")?).header("content-type","audio/wav").body(read_bounded(input,MAX_AUDIO_BYTES)?),
        Command::Export { asset_id,.. } => { let id=AssetId::try_from(asset_id.clone()).map_err(anyhow::Error::msg)?; client.get(base.join(&format!("/v1/assets/{id}/content"))?) }
        _ => anyhow::bail!("--server supports call, import and export"),
    };
    if let Some(token)=token { request=request.bearer_auth(token); }
    let response=request.send()?; let status=response.status();
    let mut bytes=vec![]; use std::io::Read;
    response.take((MAX_AUDIO_BYTES+1) as u64).read_to_end(&mut bytes)?;
    if bytes.len()>MAX_AUDIO_BYTES { anyhow::bail!("response exceeds size limit"); }
    if !status.is_success() { anyhow::bail!("server returned {}: {}",status,String::from_utf8_lossy(&bytes[..bytes.len().min(4096)])); }
    match command {
        Command::Export { output,asset_id } => { if avw_core::artifacts::digest(&bytes)!=asset_id { anyhow::bail!("downloaded asset checksum mismatch"); } write_export(&output,&bytes)?; emit(&json!({"output":output,"asset_id":asset_id,"bytes":bytes.len()})) }
        _=>emit(&serde_json::from_slice::<Value>(&bytes)?),
    }
}
