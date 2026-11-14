// mb-mcp: mini-browser as an MCP server over stdio (docs/concept.md G2).
//
// Two threads:
//   main   — the browser (`AgentBrowser` is !Send: Rc DOM + JS runtime).
//   tokio  — the rmcp server reading stdin / writing stdout.
// They talk only through `worker::Job`s. When the client closes stdin the
// server ends, its job sender drops, and the browser loop returns.
//
// stdout is the JSON-RPC channel. Nothing else may write to it: every log
// line in the workspace goes to stderr (AGENTS.md "stdout 금지").

#![deny(clippy::print_stdout)]

mod cli;
mod prompts;
mod server;
mod worker;

use std::process::ExitCode;
use std::sync::mpsc;

use rmcp::ServiceExt;

fn main() -> ExitCode {
    let mut options = match cli::parse(std::env::args().skip(1)) {
        Ok(cli::Parsed::Run(options)) => options,
        Ok(cli::Parsed::Help) => {
            eprint!("{}", cli::USAGE);
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("mb-mcp: {message}\n");
            eprint!("{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    // Real text metrics and readable screenshots for a live session.
    options.agent.system_fonts = true;

    let (jobs_tx, jobs_rx) = mpsc::channel::<worker::Job>();
    let server_thread = std::thread::spawn(move || -> Result<(), String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("tokio runtime: {e}"))?;
        runtime.block_on(async move {
            let service = server::BrowserServer::new(jobs_tx)
                .serve(rmcp::transport::stdio())
                .await
                .map_err(|e| format!("mcp handshake failed: {e}"))?;
            service
                .waiting()
                .await
                .map_err(|e| format!("mcp server task failed: {e}"))?;
            Ok(())
        })
    });

    eprintln!("[mb-mcp] ready (stdio)");
    worker::run(options, jobs_rx);

    match server_thread.join() {
        Ok(Ok(())) => ExitCode::SUCCESS,
        Ok(Err(message)) => {
            eprintln!("mb-mcp: {message}");
            ExitCode::FAILURE
        }
        Err(_) => {
            eprintln!("mb-mcp: server thread panicked");
            ExitCode::FAILURE
        }
    }
}
