//! CLI side of the module protocol. The `terminite` binary doubles as
//! its own client — running `terminite tabs / blocks / block / watch`
//! connects to `~/.terminite/socket`, speaks the JSON protocol, and
//! prints results. No external dependency: introspecting the terminal
//! is part of the terminal.
//!
//! Subcommands are dispatched from `main.rs`'s `main()`; this module
//! owns the connection and protocol mechanics.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;

/// Where to find a running terminite's socket. Mirrors `proto::socket_path`
/// — kept duplicated so the client doesn't pull the server module in.
fn socket_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("TERMINITE_SOCKET") {
        return Some(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".terminite/socket"))
}

fn connect_or_exit() -> UnixStream {
    let Some(path) = socket_path() else {
        eprintln!("terminite: no socket path — set $TERMINITE_SOCKET or $HOME");
        std::process::exit(1);
    };
    match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "terminite: can't connect to {} — is terminite running? ({e})",
                path.display()
            );
            std::process::exit(1);
        }
    }
}

/// Top-level dispatch. Returns the process exit code. `None` means
/// "no subcommand given — start the window app instead."
pub fn dispatch(args: &[String]) -> Option<ExitCode> {
    let Some(cmd) = args.first().map(|s| s.as_str()) else {
        return None;
    };
    match cmd {
        "tabs" => Some(cmd_tabs()),
        "blocks" => Some(cmd_blocks(args.get(1).and_then(|s| s.parse().ok()))),
        "block" => Some(cmd_block(
            args.get(1).and_then(|s| s.parse().ok()),
            args.get(2).and_then(|s| s.parse().ok()),
        )),
        "watch" => Some(cmd_watch()),
        "help" | "--help" | "-h" => {
            print_usage();
            Some(ExitCode::SUCCESS)
        }
        other => {
            eprintln!("terminite: unknown subcommand `{other}`");
            print_usage();
            Some(ExitCode::from(2))
        }
    }
}

fn print_usage() {
    eprintln!(
        "\
terminite — terminal for the human-AI pair.

USAGE
  terminite                          launch the window
  terminite tabs                     list open tabs
  terminite blocks [tab_id]          list blocks in a tab (default 0)
  terminite block <tab_id> <id>      print one block's command + output
  terminite watch                    stream block_opened / block_closed events
  terminite help                     this message

ENV
  TERMINITE_SOCKET                   override the socket path
                                     (default: ~/.terminite/socket)
"
    );
}

fn cmd_tabs() -> ExitCode {
    one_shot(r#"{"id":1,"method":"list_tabs"}"#)
}

fn cmd_blocks(tab_id: Option<u64>) -> ExitCode {
    let tab_id = tab_id.unwrap_or(0);
    one_shot(&format!(
        r#"{{"id":1,"method":"list_blocks","params":{{"tab_id":{tab_id}}}}}"#
    ))
}

fn cmd_block(tab_id: Option<u64>, block_id: Option<u32>) -> ExitCode {
    let (Some(tab_id), Some(block_id)) = (tab_id, block_id) else {
        eprintln!("usage: terminite block <tab_id> <block_id>");
        return ExitCode::from(2);
    };
    one_shot(&format!(
        r#"{{"id":1,"method":"get_block","params":{{"tab_id":{tab_id},"block_id":{block_id}}}}}"#
    ))
}

fn cmd_watch() -> ExitCode {
    let mut stream = connect_or_exit();
    if writeln!(stream, r#"{{"id":1,"method":"subscribe"}}"#).is_err() {
        eprintln!("terminite: socket write failed");
        return ExitCode::from(1);
    }
    // Stream every line back to stdout, flushing each so a piped reader
    // (`watch | jq`, `watch | grep`) sees events as they happen.
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        match line {
            Ok(l) => {
                if writeln!(out, "{l}").is_err() || out.flush().is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    ExitCode::SUCCESS
}

fn one_shot(req: &str) -> ExitCode {
    let mut stream = connect_or_exit();
    if writeln!(stream, "{req}").is_err() {
        eprintln!("terminite: socket write failed");
        return ExitCode::from(1);
    }
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => {
            eprintln!("terminite: socket closed before response");
            ExitCode::from(1)
        }
        Ok(_) => {
            print!("{line}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("terminite: read failed: {e}");
            ExitCode::from(1)
        }
    }
}
