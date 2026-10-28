// SPDX-License-Identifier: MPL-2.0

//! `koru-mcp` — a quiet stdio bridge suitable for ChatGPT, Codex and Claude.
//!
//! During `serve`, stdout belongs exclusively to MCP framing. Diagnostics go
//! to stderr; `doctor` is a separate process invocation and may print JSON.

use std::{path::PathBuf, sync::Arc};

use koru_mcp_server::{
    Backend, KoruMcp, LocalBackend, MODEL_RESOURCE_URI, PAGED_TOOLS_BY_DEFAULT, RequestContext,
    verify_model_documentation,
};
use rmcp::ServiceExt;

// `koru_workspace::sync` is the store link's own account of how a connection
// ended, which a gateway's reset warning is read against.
const LOG_FILTER: &str = "warn,koru_mcp=info,koru_mcp_server=info,koru_workspace::sync=info";

fn main() {
    if let Err(error) = run() {
        eprintln!("koru-mcp: {error}");
        std::process::exit(1);
    }
}

fn init_logging(root: &std::path::Path) -> koru_log::Logging {
    // stdout belongs exclusively to JSON-RPC framing while serving. stderr is
    // safely capturable by MCP hosts; the rotating file is what a person can
    // tail when that host-launched child has no visible console.
    koru_log::init_file_and_stderr(root, "koru-mcp.log", LOG_FILTER)
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let options = parse_args(std::env::args().skip(1)).map_err(std::io::Error::other)?;
    let command = options.command.clone();
    let root = options.root.clone().map(absolute_from_cwd).transpose()?;
    let stdio = options.stdio;
    // `koru.bat` changes to the repository before launching us. Resolve a
    // relative developer root there before deriving the Store endpoint: IPC
    // isolation hashes the root spelling, and hashing `.dev-data\x\store`
    // would not find the Store that bound `C:\repo\.dev-data\x\store`.
    let store_root = root.as_deref().map(koru_channel::store_root_under);
    match command.as_str() {
        "serve" if stdio => {
            let log_root = store_root
                .clone()
                .or_else(koru_channel::store_root)
                .ok_or_else(|| {
                    std::io::Error::other("could not work out where this machine keeps Koru logs")
                })?;
            let _logging = init_logging(&log_root);
            verify_model_documentation()?;
            // The client dials the gateway by the *channel* root's identity
            // (`koru_api_discovery::gateway_endpoint_at`), the same value the
            // Agent and the app were given; the store root is one level
            // down and names only the logs above.
            let backend = Arc::new(LocalBackend::connect(
                koru_channel::segment(),
                root.as_deref(),
            )?);
            tracing::info!(
                version = env!("CARGO_PKG_VERSION"),
                pid = std::process::id(),
                executable = %std::env::current_exe()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|_| "<unknown>".into()),
                model = MODEL_RESOURCE_URI,
                logs = %log_root.join(koru_log::LOG_DIR).display(),
                "serving stdio"
            );
            // Keep one backend owner outside Tokio. The Store client is
            // blocking and owns a private Reqwest runtime; its final drop must
            // happen back in this synchronous process context, not while the
            // MCP transport runtime is shutting down.
            let service_backend = backend.clone();
            // `--tool-pages on|off` overrides the build's default either way
            // (ADR-433-compacttools): an argument, because every agent renders
            // a server's argv and not every one passes its env on.
            let paged_tools = options.tool_pages.unwrap_or(PAGED_TOOLS_BY_DEFAULT);
            tracing::info!(paged_tools, "tools/list paging");
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            let served: Result<(), String> = runtime.block_on(async move {
                let service = KoruMcp::new(service_backend, RequestContext::local())
                    .with_paged_tools(paged_tools)
                    .serve(rmcp::transport::stdio())
                    .await
                    .map_err(|error| error.to_string())?;
                service
                    .waiting()
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            });
            served.map_err(std::io::Error::other)?;
            tracing::info!("client disconnected; stopping");
            drop(runtime);
            drop(backend);
            Ok(())
        }
        "serve" => Err("serve requires --stdio in this release".into()),
        "doctor" => {
            let backend = LocalBackend::connect(koru_channel::segment(), root.as_deref())?;
            println!("{}", serde_json::to_string_pretty(&backend.doctor()?)?);
            Ok(())
        }
        "help" | "--help" | "-h" => {
            println!(
                "koru-mcp serve --stdio [--root PATH|--root=PATH] [--tool-pages on|off]\nkoru-mcp doctor [--root PATH|--root=PATH]"
            );
            Ok(())
        }
        "version" | "--version" | "-V" => {
            println!("koru-mcp {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => Err(format!("unknown command: {command}").into()),
    }
}

struct Options {
    command: String,
    root: Option<PathBuf>,
    stdio: bool,
    /// `--tool-pages on|off`: serve `tools/list` in pages under 7 KB, or in
    /// one reply. Absent means the build's default.
    tool_pages: Option<bool>,
}

fn tool_pages(value: Option<&str>) -> Result<bool, String> {
    match value {
        Some("on") => Ok(true),
        Some("off") => Ok(false),
        _ => Err("--tool-pages needs on or off".into()),
    }
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let command = args.next().unwrap_or_else(|| "help".into());
    let mut root = None;
    let mut stdio = false;
    let mut pages = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = Some(PathBuf::from(args.next().ok_or("--root needs a path")?)),
            "--stdio" => stdio = true,
            "--tool-pages" => pages = Some(tool_pages(args.next().as_deref())?),
            _ => {
                if let Some(value) = arg.strip_prefix("--tool-pages=") {
                    pages = Some(tool_pages(Some(value))?);
                } else if let Some(value) = arg.strip_prefix("--root=") {
                    if value.is_empty() {
                        return Err("--root needs a path".into());
                    }
                    root = Some(PathBuf::from(value));
                } else {
                    return Err(format!("unknown argument: {arg}"));
                }
            }
        }
    }
    Ok(Options {
        command,
        root,
        stdio,
        tool_pages: pages,
    })
}

fn absolute_from_cwd(path: PathBuf) -> std::io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn serving_logging_creates_an_mcp_log_under_the_store_root() {
        let dir = tempfile::tempdir().expect("temporary store root");
        let logging = super::init_logging(dir.path());
        tracing::info!("mcp logging smoke test");
        drop(logging);

        let log_dir = dir.path().join(koru_log::LOG_DIR);
        let names = std::fs::read_dir(log_dir)
            .expect("MCP log directory")
            .map(|entry| {
                entry
                    .expect("MCP log entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        assert!(
            names.iter().any(|name| name.starts_with("koru-mcp.log.")),
            "expected a rotating MCP log, found {names:?}"
        );
    }

    #[test]
    fn a_cli_root_names_the_channel_and_discovery_uses_its_store_child() {
        let channel = std::path::Path::new(r"C:\a channel root");
        assert_eq!(
            koru_channel::store_root_under(channel),
            channel.join("store")
        );
    }

    #[test]
    fn tool_pages_is_on_or_off_and_nothing_else() {
        let parse = |args: &[&str]| super::parse_args(args.iter().map(|arg| arg.to_string()));
        assert_eq!(
            parse(&["serve", "--stdio"]).unwrap().tool_pages,
            None,
            "absent means the build's default"
        );
        assert_eq!(
            parse(&["serve", "--stdio", "--tool-pages", "on"])
                .unwrap()
                .tool_pages,
            Some(true)
        );
        assert_eq!(
            parse(&["serve", "--tool-pages=off", "--stdio"])
                .unwrap()
                .tool_pages,
            Some(false)
        );
        for bad in [
            &["serve", "--tool-pages"][..],
            &["serve", "--tool-pages", "yes"],
            &["serve", "--tool-pages="],
        ] {
            assert!(parse(bad).is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn equals_root_spelling_is_accepted_and_made_absolute() {
        let options = super::parse_args(
            [
                "serve".to_owned(),
                "--stdio".to_owned(),
                "--root=.dev-data\\history-smoke".to_owned(),
            ]
            .into_iter(),
        )
        .expect("parse --root=PATH");

        assert!(options.stdio);
        assert_eq!(options.command, "serve");
        let root = super::absolute_from_cwd(options.root.expect("root")).expect("absolute root");
        assert!(root.is_absolute());
        assert!(root.ends_with(std::path::Path::new(r".dev-data\history-smoke")));
    }
}
