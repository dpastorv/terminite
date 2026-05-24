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
        "tag" => Some(cmd_tag(
            args.get(1).and_then(|s| s.parse().ok()),
            args.get(2).and_then(|s| s.parse().ok()),
            args.get(3),
        )),
        "untag" => Some(cmd_untag(
            args.get(1).and_then(|s| s.parse().ok()),
            args.get(2).and_then(|s| s.parse().ok()),
            args.get(3),
        )),
        "cursor" => Some(cmd_cursor(
            args.get(1).and_then(|s| s.parse().ok()),
            args.get(2).and_then(|s| s.parse().ok()),
        )),
        "cursor-clear" => Some(cmd_cursor_clear(args.get(1).and_then(|s| s.parse().ok()))),
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
  terminite tag <tab> <id> <tag>     attach a tag to a block
  terminite untag <tab> <id> <tag>   remove a tag from a block
  terminite cursor <tab> <id>        move the AI cursor to a block
  terminite cursor-clear <tab>       drop the AI cursor from a tab
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

fn cmd_tag(tab_id: Option<u64>, block_id: Option<u32>, tag: Option<&String>) -> ExitCode {
    let (Some(tab_id), Some(block_id), Some(tag)) = (tab_id, block_id, tag) else {
        eprintln!("usage: terminite tag <tab_id> <block_id> <tag>");
        return ExitCode::from(2);
    };
    let tag = json_escape(tag);
    one_shot(&format!(
        r#"{{"id":1,"method":"set_tag","params":{{"tab_id":{tab_id},"block_id":{block_id},"tag":"{tag}"}}}}"#
    ))
}

fn cmd_untag(tab_id: Option<u64>, block_id: Option<u32>, tag: Option<&String>) -> ExitCode {
    let (Some(tab_id), Some(block_id), Some(tag)) = (tab_id, block_id, tag) else {
        eprintln!("usage: terminite untag <tab_id> <block_id> <tag>");
        return ExitCode::from(2);
    };
    let tag = json_escape(tag);
    one_shot(&format!(
        r#"{{"id":1,"method":"remove_tag","params":{{"tab_id":{tab_id},"block_id":{block_id},"tag":"{tag}"}}}}"#
    ))
}

fn cmd_cursor(tab_id: Option<u64>, block_id: Option<u32>) -> ExitCode {
    let (Some(tab_id), Some(block_id)) = (tab_id, block_id) else {
        eprintln!("usage: terminite cursor <tab_id> <block_id>");
        return ExitCode::from(2);
    };
    one_shot(&format!(
        r#"{{"id":1,"method":"cursor_at","params":{{"tab_id":{tab_id},"block_id":{block_id}}}}}"#
    ))
}

fn cmd_cursor_clear(tab_id: Option<u64>) -> ExitCode {
    let Some(tab_id) = tab_id else {
        eprintln!("usage: terminite cursor-clear <tab_id>");
        return ExitCode::from(2);
    };
    one_shot(&format!(
        r#"{{"id":1,"method":"cursor_clear","params":{{"tab_id":{tab_id}}}}}"#
    ))
}

/// Escape just enough for JSON-in-a-string: backslash, quote, and the
/// usual whitespace controls. Anything that arrives via argv is local
/// user input; bigger escape policies aren't needed at this surface.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
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
