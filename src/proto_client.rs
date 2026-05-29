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
        "export" => Some(cmd_export(&args[1..])),
        "stats" => Some(cmd_stats()),
        "module" => Some(cmd_module(&args[1..])),
        "shell-init" => Some(cmd_shell_init(&args[1..])),
        "mcp" => Some(crate::mcp::run()),
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
  terminite export <tab> [--json]    write the tab's blocks as markdown
                                     (or JSON with --json)
                                     [--since <id>] starts from block id
  terminite stats                    snapshot of internal state
                                     (frames, tabs, blocks, memory)
  terminite module list              registered modules (extension surface)
  terminite module add <dir>         install a module from <dir>
  terminite module remove <id>       uninstall a module
  terminite module reload            re-discover modules without relaunch
  terminite shell-init [--shell S]   print shell integration snippet for
                                     zsh or bash (default: $SHELL).
                                     `eval \"$(terminite shell-init)\"`
                                     in your rc, or pass --install to
                                     append it idempotently for you.
  terminite mcp                      run the Model Context Protocol
                                     server on stdio. Add to your AI
                                     client's MCP config so it
                                     auto-discovers terminite's tools.
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

fn cmd_stats() -> ExitCode {
    one_shot(r#"{"id":1,"method":"stats"}"#)
}

fn cmd_module(args: &[String]) -> ExitCode {
    match args.first().map(|s| s.as_str()) {
        Some("list") => one_shot(r#"{"id":1,"method":"list_modules"}"#),
        Some("reload") => one_shot(r#"{"id":1,"method":"reload_modules"}"#),
        Some("add") => match args.get(1) {
            Some(src) => module_add(src),
            None => {
                eprintln!("usage: terminite module add <source-dir>");
                ExitCode::from(2)
            }
        },
        Some("remove") => match args.get(1) {
            Some(id) => module_remove(id),
            None => {
                eprintln!("usage: terminite module remove <id>");
                ExitCode::from(2)
            }
        },
        _ => {
            eprintln!(
                "usage:\n  \
                 terminite module list\n  \
                 terminite module add <source-dir>\n  \
                 terminite module remove <id>\n  \
                 terminite module reload"
            );
            ExitCode::from(2)
        }
    }
}

fn modules_dir() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("TERMINITE_MODULES_DIR") {
        return Some(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".terminite/modules"))
}

fn module_add(src: &str) -> ExitCode {
    let src_path = PathBuf::from(src);
    if !src_path.is_dir() {
        eprintln!("terminite: {src} is not a directory");
        return ExitCode::from(1);
    }
    if !src_path.join("manifest.toml").is_file() {
        eprintln!("terminite: {src}/manifest.toml not found");
        return ExitCode::from(1);
    }
    let Some(name) = src_path.file_name().and_then(|s| s.to_str()) else {
        eprintln!("terminite: can't extract module id from path");
        return ExitCode::from(1);
    };
    let Some(dst_root) = modules_dir() else {
        eprintln!("terminite: no $HOME, can't locate modules dir");
        return ExitCode::from(1);
    };
    let dst = dst_root.join(name);
    if let Err(e) = std::fs::create_dir_all(&dst_root) {
        eprintln!("terminite: can't create {}: {e}", dst_root.display());
        return ExitCode::from(1);
    }
    if dst.exists() {
        eprintln!(
            "terminite: {} already exists — remove first with `terminite module remove {name}`",
            dst.display()
        );
        return ExitCode::from(1);
    }
    if let Err(e) = copy_dir_recursive(&src_path, &dst) {
        eprintln!("terminite: copy failed: {e}");
        return ExitCode::from(1);
    }
    println!("installed module → {}", dst.display());
    println!("run `terminite module reload` to make it selectable in the dropdown");
    ExitCode::SUCCESS
}

fn module_remove(id: &str) -> ExitCode {
    let Some(dst_root) = modules_dir() else {
        eprintln!("terminite: no $HOME, can't locate modules dir");
        return ExitCode::from(1);
    };
    let dst = dst_root.join(id);
    if !dst.exists() {
        eprintln!("terminite: {} doesn't exist", dst.display());
        return ExitCode::from(1);
    }
    if let Err(e) = std::fs::remove_dir_all(&dst) {
        eprintln!("terminite: remove failed: {e}");
        return ExitCode::from(1);
    }
    println!("removed module → {}", dst.display());
    println!("run `terminite module reload` to drop it from the dropdown");
    ExitCode::SUCCESS
}

/// Recursive copy that preserves the executable bit. std::fs has no
/// recursive copy; this is the smallest version that suffices for
/// module installation.
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ft.is_file() {
            std::fs::copy(&from, &to)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&from)?.permissions().mode();
                std::fs::set_permissions(
                    &to,
                    std::fs::Permissions::from_mode(mode),
                )?;
            }
        }
    }
    Ok(())
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

fn cmd_export(args: &[String]) -> ExitCode {
    // Parse: positional tab_id, then `--json` and `--since <id>` flags
    // in any order. Hand-rolled — clap would be a heavier dep than
    // this four-arg surface justifies.
    let mut tab_id: Option<u64> = None;
    let mut since: Option<u32> = None;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => {
                json = true;
                i += 1;
            }
            "--since" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<u32>().ok()) {
                    Some(n) => since = Some(n),
                    None => {
                        eprintln!("usage: terminite export <tab_id> [--json] [--since <block_id>]");
                        return ExitCode::from(2);
                    }
                }
                i += 1;
            }
            other => {
                if tab_id.is_none() {
                    tab_id = other.parse().ok();
                }
                i += 1;
            }
        }
    }
    let Some(tab_id) = tab_id else {
        eprintln!("usage: terminite export <tab_id> [--json] [--since <block_id>]");
        return ExitCode::from(2);
    };

    let req = match since {
        Some(s) => format!(
            r#"{{"id":1,"method":"export_tab","params":{{"tab_id":{tab_id},"since":{s}}}}}"#
        ),
        None => format!(
            r#"{{"id":1,"method":"export_tab","params":{{"tab_id":{tab_id}}}}}"#
        ),
    };

    let mut stream = connect_or_exit();
    if writeln!(stream, "{req}").is_err() {
        eprintln!("terminite: socket write failed");
        return ExitCode::from(1);
    }
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let read = reader.read_line(&mut line);
    if matches!(read, Ok(0) | Err(_)) {
        eprintln!("terminite: socket closed before response");
        return ExitCode::from(1);
    }

    if json {
        print!("{line}");
        return ExitCode::SUCCESS;
    }

    // Parse the response and render markdown locally. Server-side
    // formatting would mean the server cares about presentation; this
    // way the protocol stays format-agnostic and the CLI owns the look.
    let value: serde_json::Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("terminite: response wasn't valid JSON: {e}");
            return ExitCode::from(1);
        }
    };
    if value.get("kind").and_then(|v| v.as_str()) == Some("error") {
        let msg = value.get("message").and_then(|v| v.as_str()).unwrap_or("");
        eprintln!("terminite: {msg}");
        return ExitCode::from(1);
    }
    print_markdown(&value);
    ExitCode::SUCCESS
}

/// Render an `export_tab` response as a markdown session log to stdout.
/// Each block becomes a section: `## Bn`, the command in backticks with
/// its exit code, any tags, then the output in a fenced block. Blank
/// blocks (no command and no output) are skipped so a half-formed
/// open block doesn't litter the bottom.
fn print_markdown(value: &serde_json::Value) {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let tab_id = value.get("tab_id").and_then(|v| v.as_u64()).unwrap_or(0);
    let _ = writeln!(out, "# terminite tab {tab_id}\n");

    let empty: Vec<serde_json::Value> = Vec::new();
    let blocks = value
        .get("blocks")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);

    let mut first = true;
    for block in blocks {
        let id = block.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
        let command = block
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let output = block
            .get("output")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim_end()
            .to_string();
        if command.is_empty() && output.is_empty() {
            continue;
        }
        let exit = block
            .get("exit_code")
            .and_then(|v| v.as_i64())
            .map(|c| c.to_string())
            .unwrap_or_else(|| "?".into());
        let tags: Vec<&str> = block
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        if !first {
            let _ = writeln!(out, "---\n");
        }
        first = false;

        let _ = writeln!(out, "## B{id}");
        if !command.is_empty() {
            let _ = writeln!(out, "`{command}` — exit {exit}");
        } else {
            let _ = writeln!(out, "exit {exit}");
        }
        if !tags.is_empty() {
            let _ = writeln!(out, "tags: {}", tags.join(", "));
        }
        if !output.is_empty() {
            let _ = writeln!(out, "\n```");
            let _ = writeln!(out, "{output}");
            let _ = writeln!(out, "```");
        }
        let _ = writeln!(out);
    }
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

// ── shell-init ────────────────────────────────────────────────────────────

/// Markers terminite owns inside the user's rc. Repeat installs replace
/// content between these two lines verbatim — never duplicates, never
/// touches the user's other rc content.
const SHELL_INIT_BEGIN: &str = "# >>> terminite shell integration >>>";
const SHELL_INIT_END: &str = "# <<< terminite shell integration <<<";

const ZSH_SNIPPET: &str = "\
# OSC 133 marks so terminite can show your commands as blocks.
preexec() { printf '\\e]133;C\\e\\\\' }
precmd() {
  local code=$?
  printf '\\e]133;D;%d\\e\\\\' \"$code\"
  printf '\\e]133;A\\e\\\\'
}
";

const BASH_SNIPPET: &str = "\
# OSC 133 marks so terminite can show your commands as blocks.
__terminite_precmd() {
  local code=$?
  printf '\\e]133;D;%d\\e\\\\' \"$code\"
  printf '\\e]133;A\\e\\\\'
}
__terminite_preexec() { printf '\\e]133;C\\e\\\\'; }
trap '__terminite_preexec' DEBUG
case \"$PROMPT_COMMAND\" in
  *__terminite_precmd*) ;;
  *) PROMPT_COMMAND=\"__terminite_precmd${PROMPT_COMMAND:+; $PROMPT_COMMAND}\" ;;
esac
";

fn detect_shell() -> &'static str {
    if let Ok(s) = std::env::var("SHELL") {
        if s.contains("zsh") { return "zsh"; }
        if s.contains("bash") { return "bash"; }
    }
    "zsh"
}

fn snippet_for(shell: &str) -> Option<&'static str> {
    match shell {
        "zsh" => Some(ZSH_SNIPPET),
        "bash" => Some(BASH_SNIPPET),
        _ => None,
    }
}

fn rc_path_for(shell: &str) -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let home = std::path::PathBuf::from(home);
    match shell {
        "zsh" => {
            if let Ok(z) = std::env::var("ZDOTDIR") {
                let mut p = std::path::PathBuf::from(z);
                p.push(".zshrc");
                return Some(p);
            }
            let mut p = home.clone();
            p.push(".zshrc");
            Some(p)
        }
        "bash" => {
            // Prefer ~/.bashrc — it's what most users have. .bash_profile
            // is login-only and won't run for interactive non-login shells.
            let mut p = home.clone();
            p.push(".bashrc");
            Some(p)
        }
        _ => None,
    }
}

fn cmd_shell_init(args: &[String]) -> ExitCode {
    // Parse: [--shell zsh|bash] [--install]
    let mut shell: Option<String> = None;
    let mut install = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--shell" => {
                if let Some(v) = args.get(i + 1) {
                    shell = Some(v.clone());
                    i += 2;
                    continue;
                }
                eprintln!("terminite: --shell needs an argument");
                return ExitCode::from(1);
            }
            "--install" => {
                install = true;
                i += 1;
            }
            other => {
                eprintln!("terminite: shell-init: unknown flag {other}");
                return ExitCode::from(1);
            }
        }
    }
    let shell = shell.unwrap_or_else(|| detect_shell().to_string());
    let Some(snippet) = snippet_for(&shell) else {
        eprintln!("terminite: shell-init: unsupported shell {shell} (zsh, bash)");
        return ExitCode::from(1);
    };

    if !install {
        // Default mode — print the snippet to stdout so callers can
        // pipe it through `eval` from their rc.
        print!("{snippet}");
        return ExitCode::SUCCESS;
    }

    let Some(rc) = rc_path_for(&shell) else {
        eprintln!("terminite: shell-init: can't resolve rc path");
        return ExitCode::from(1);
    };

    // Read existing rc (or treat absent as empty). Replace any prior
    // block between our markers; otherwise append. Cap the read so a
    // pathological rc can't OOM the install — 1 MB is two orders of
    // magnitude past any reasonable shell rc.
    const MAX_RC_BYTES: u64 = 1024 * 1024;
    let existing = match std::fs::metadata(&rc) {
        Ok(m) if m.len() > MAX_RC_BYTES => {
            eprintln!(
                "terminite: shell-init: refusing to edit {} — file is {} bytes (> {} cap)",
                rc.display(),
                m.len(),
                MAX_RC_BYTES,
            );
            return ExitCode::from(1);
        }
        _ => std::fs::read_to_string(&rc).unwrap_or_default(),
    };
    let block = format!("{SHELL_INIT_BEGIN}\n{snippet}{SHELL_INIT_END}\n");
    let new_content = if let (Some(start), Some(end)) =
        (existing.find(SHELL_INIT_BEGIN), existing.find(SHELL_INIT_END))
    {
        // Replace between markers, keeping the rest of the file intact.
        let end = end + SHELL_INIT_END.len();
        let mut out = String::with_capacity(existing.len());
        out.push_str(&existing[..start]);
        out.push_str(&block.trim_end_matches('\n'));
        // Skip a trailing newline if one was already after the END marker.
        let rest = &existing[end..];
        if !rest.starts_with('\n') {
            out.push('\n');
        }
        out.push_str(rest);
        out
    } else {
        // Append. Leave a blank line before the block if the rc doesn't
        // already end with one — readable diff against the user's file.
        let mut out = existing.clone();
        if !out.is_empty() && !out.ends_with("\n\n") {
            if !out.ends_with('\n') { out.push('\n'); }
            out.push('\n');
        }
        out.push_str(&block);
        out
    };

    if let Some(parent) = rc.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = rc.with_extension(format!(
        "{}.terminite.tmp",
        rc.extension()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    ));
    if let Err(e) = std::fs::write(&tmp, &new_content) {
        eprintln!("terminite: shell-init: write {} failed: {e}", tmp.display());
        return ExitCode::from(1);
    }
    if let Err(e) = std::fs::rename(&tmp, &rc) {
        eprintln!("terminite: shell-init: rename to {} failed: {e}", rc.display());
        return ExitCode::from(1);
    }
    eprintln!(
        "installed terminite shell integration → {}\n\
         open a new shell (or run `source {}`) to activate it.\n\
         after that, every command runs as a labeled block in the gutter.",
        rc.display(),
        rc.display(),
    );
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
