// mb-mcp: mini-browser as an MCP server over stdio (docs/concept.md G2),
// or over HTTP with `--http` (G3b: the person starts it, agents attach).
//
// Two threads:
//   main   — the browser (`AgentBrowser` is !Send: Rc DOM + JS runtime).
//   tokio  — the rmcp server reading stdin / writing stdout.
// They talk only through `worker::Job`s. When the client closes stdin the
// server ends, its job sender drops, and the browser loop returns. Over
// HTTP the server never ends on its own: the process lives until the
// person closes the window (or, headless, until it is killed).
//
// stdout is the JSON-RPC channel. Nothing else may write to it: every log
// line in the workspace goes to stderr (AGENTS.md "stdout 금지").

#![deny(clippy::print_stdout)]

mod cli;
mod http;
mod overlay;
mod prompts;
mod replay;
mod server;
mod session_log;
mod window_mode;
mod worker;

use std::process::ExitCode;
use std::sync::mpsc;

use rmcp::ServiceExt;

fn main() -> ExitCode {
    let mut options = match cli::parse(std::env::args().skip(1)) {
        Ok(cli::Parsed::Run(options)) => options,
        Ok(cli::Parsed::Replay(replay)) => return replay::run(&replay),
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

    if let Some(path) = options.preview.clone() {
        // `--http --preview`: show the attach section (no listener).
        if let Some(port) = options.http_port {
            options.attach = Some(http::Attach {
                url: format!("http://127.0.0.1:{port}{}", http::PATH),
                token: http::token_from_env_or_random().unwrap_or_default(),
            });
        }
        let clicks = options.preview_clicks.clone();
        return match window_mode::preview(options, &path, &clicks) {
            Ok(()) => {
                eprintln!("[mb-mcp] wrote {}", path.display());
                ExitCode::SUCCESS
            }
            Err(message) => {
                eprintln!("mb-mcp: {message}");
                ExitCode::FAILURE
            }
        };
    }

    let (jobs_tx, jobs_rx) = mpsc::channel::<worker::Job>();
    let wake_slot: window_mode::WakeSlot = Default::default();
    let server_wake = wake_slot.clone();

    if let Some(port) = options.http_port {
        return run_http(options, port, jobs_tx, jobs_rx, wake_slot);
    }

    let server_thread = std::thread::spawn(move || -> Result<(), String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("tokio runtime: {e}"))?;
        runtime.block_on(async move {
            let service = server::BrowserServer::new(jobs_tx, server_wake)
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

    if options.window {
        eprintln!("[mb-mcp] ready (stdio, observation window)");
        window_mode::run(options, jobs_rx, wake_slot);
    } else {
        eprintln!("[mb-mcp] ready (stdio)");
        worker::run(options, jobs_rx);
    }

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

/// `--http`: bind 127.0.0.1, print where to attach, serve until the
/// window closes (or forever, headless).
fn run_http(
    mut options: cli::Options,
    port: u16,
    jobs_tx: mpsc::Sender<worker::Job>,
    jobs_rx: mpsc::Receiver<worker::Job>,
    wake_slot: window_mode::WakeSlot,
) -> ExitCode {
    let token = match http::token_from_env_or_random() {
        Ok(token) => token,
        Err(message) => {
            eprintln!("mb-mcp: {message}");
            return ExitCode::from(2);
        }
    };
    // Bound here, before the window opens, so a busy port is a clear error.
    let listener = match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("mb-mcp: cannot listen on 127.0.0.1:{port}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(port);
    let attach = http::Attach {
        url: format!("http://127.0.0.1:{port}{}", http::PATH),
        token: token.clone(),
    };
    // The person launched this from their terminal: tell them how to
    // attach. (The window shows the token masked, with a copy button.)
    eprintln!("[mb-mcp] ready (http) at {}", attach.url);
    eprintln!("[mb-mcp] attach with: {}", attach.setup_command());
    options.attach = Some(attach);

    let window_wake = wake_slot.clone();
    std::thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            listener
                .set_nonblocking(true)
                .map_err(|e| format!("listener: {e}"))?;
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("tokio runtime: {e}"))?;
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener)
                    .map_err(|e| format!("listener: {e}"))?;
                http::serve(listener, token, move || {
                    server::BrowserServer::new(jobs_tx.clone(), wake_slot.clone())
                })
                .await
                .map_err(|e| format!("http server: {e}"))
            })
        })();
        if let Err(message) = result {
            eprintln!("mb-mcp: {message}");
            std::process::exit(1);
        }
    });

    if options.window {
        window_mode::run(options, jobs_rx, window_wake);
    } else {
        worker::run(options, jobs_rx);
    }
    ExitCode::SUCCESS
}
