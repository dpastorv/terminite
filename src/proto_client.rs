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
        "activities" => Some(cmd_activities(&args[1..])),
        "outbox" => Some(cmd_outbox(args.get(1))),
        "msg-status" => Some(cmd_msg_status(args.get(1).and_then(|s| s.parse().ok()))),
        "cancel" => Some(cmd_cancel(args.get(1).and_then(|s| s.parse().ok()))),
        "stop" => Some(cmd_stop(args.get(1))),
        "halt" => Some(cmd_halt(args.get(1))),
        "release" => Some(cmd_release(args.get(1))),
        "last-crash" => Some(cmd_last_crash()),
        "room-who" => Some(cmd_room_who()),
        "room-join" => Some(cmd_room_join(&args[1..])),
        "room-listen" => Some(cmd_room_listen(&args[1..])),
        "files" => Some(cmd_files(args.get(1).map(|s| s.as_str()))),
        "tool-emit-hook" => Some(cmd_tool_emit_hook()),
        "install" => Some(cmd_install(&args[1..])),
        "config" => Some(cmd_config()),
        "module" => Some(cmd_module(&args[1..])),
        "shell-init" => Some(cmd_shell_init(&args[1..])),
        "mcp" => {
            // The host spawns us as `terminite mcp --actor <slug>` so emits
            // are attributed to this agent.
            let actor = args
                .iter()
                .position(|a| a == "--actor")
                .and_then(|i| args.get(i + 1))
                .cloned();
            Some(crate::mcp::run(actor))
        }
        "channel" => {
            // The claude comms-base receiver: spawned by `claude --channels
            // server:lounge`, it pushes directed room messages into the running
            // session as channel events.
            Some(crate::mcp::run_channel())
        }
        "codex-bridge" => {
            // The codex comms-base receiver: a WebSocket client of codex's own
            // app-server daemon, turn/start-ing it on a pushed room message.
            Some(crate::codex_bridge::run_codex_bridge())
        }
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
  terminite room-who                 who is present in the room now
                                     (attendance) — each actor + its color
  terminite activities [actor]       the room's activity stream, in time
                                     order (all actors, or just <actor>;
                                     `to <slug>` reads <slug>'s inbox)
  terminite msg-status <id>          delivery fate of a directed message
                                     (queued/delivered/floor_typed/read/…)
  terminite outbox <actor>           an actor's sent messages + their states
  terminite cancel <id>              retract a message before it lands (the
                                     human can cancel any; agents only their own)
  terminite stop <actor>             interrupt an agent's current turn (Ctrl-C),
                                     bypassing busy — the orchestrator's halt
  terminite halt <actor>             bench an agent: interrupt + eject from the
                                     room until released (the reversible hard-stop)
  terminite release <actor>          lift a halt; the agent rejoins the room
  terminite install claude-terminite [--profile <name|dir>]
  terminite install codex-terminite  [--home <dir>]
  terminite install grok-terminite   [--home <dir>]
                                     make a plain agent terminite-aware —
                                     writes the room skill + MCP server into
                                     its profile (claude: ~/.claude; codex:
                                     ~/.codex; grok: ~/.grok). Reverse:
                                     `<cli> mcp remove lounge`
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

/// `terminite config` — show where the config lives and print every available
/// key with its docs + default, so the knobs are discoverable without grepping.
fn cmd_config() -> ExitCode {
    match crate::config::Config::path() {
        Some(p) => println!("# config file: {}", p.display()),
        None => println!("# config file: (no path — is $HOME set?)"),
    }
    println!();
    print!("{}", crate::config::documented_default());
    ExitCode::SUCCESS
}

fn cmd_blocks(tab_id: Option<u64>) -> ExitCode {
    let tab_id = tab_id.unwrap_or(0);
    one_shot(&format!(
        r#"{{"id":1,"method":"list_blocks","params":{{"tab_id":{tab_id}}}}}"#
    ))
}

fn cmd_room_who() -> ExitCode {
    one_shot(r#"{"id":1,"method":"room_who"}"#)
}

/// `terminite msg-status <id>` — the delivery fate of a directed message.
fn cmd_msg_status(id: Option<u64>) -> ExitCode {
    let Some(id) = id else {
        eprintln!("usage: terminite msg-status <message_id>");
        return ExitCode::from(2);
    };
    one_shot(&format!(
        r#"{{"id":1,"method":"room_message_status","params":{{"message_id":{id}}}}}"#
    ))
}

/// `terminite cancel <id>` — retract a message before it lands. Sent with NO
/// actor, so the host treats it as the human's (orchestrator) authority: the
/// human can cancel ANY message, where an agent can cancel only its own.
fn cmd_cancel(id: Option<u64>) -> ExitCode {
    let Some(id) = id else {
        eprintln!("usage: terminite cancel <message_id>");
        return ExitCode::from(2);
    };
    one_shot(&format!(
        r#"{{"id":1,"method":"room_message_cancel","params":{{"message_id":{id}}}}}"#
    ))
}

/// `terminite stop <actor>` — the priority lane: interrupt an agent's current
/// turn (Ctrl-C into its pane), bypassing busy. The orchestrator's halt for a
/// runaway. Interrupts the turn; doesn't kill or quarantine the agent.
fn cmd_stop(actor: Option<&String>) -> ExitCode {
    let Some(actor) = actor else {
        eprintln!("usage: terminite stop <actor>");
        return ExitCode::from(2);
    };
    one_shot(&format!(
        r#"{{"id":1,"method":"room_stop","params":{{"actor":"{}"}}}}"#,
        json_escape(actor)
    ))
}

/// `terminite halt <actor>` — bench an agent: interrupt it and eject it from the
/// room (no delivery, no room actions) until `release`. The reversible hard-stop.
fn cmd_halt(actor: Option<&String>) -> ExitCode {
    let Some(actor) = actor else {
        eprintln!("usage: terminite halt <actor>");
        return ExitCode::from(2);
    };
    one_shot(&format!(
        r#"{{"id":1,"method":"room_halt","params":{{"actor":"{}"}}}}"#,
        json_escape(actor)
    ))
}

/// `terminite release <actor>` — lift a halt; the agent rejoins the room.
fn cmd_release(actor: Option<&String>) -> ExitCode {
    let Some(actor) = actor else {
        eprintln!("usage: terminite release <actor>");
        return ExitCode::from(2);
    };
    one_shot(&format!(
        r#"{{"id":1,"method":"room_release","params":{{"actor":"{}"}}}}"#,
        json_escape(actor)
    ))
}

/// `terminite last-crash` — print the most recent crash dump to stdout.
fn cmd_last_crash() -> ExitCode {
    match crate::crash::last_crash_path() {
        Some(path) => {
            match std::fs::read_to_string(&path) {
                Ok(body) => println!("{}", body),
                Err(e) => eprintln!("couldn't read last-crash.log: {}", e),
            }
        }
        None => eprintln!("no crash log found"),
    }
    ExitCode::SUCCESS
}

/// `terminite outbox <actor>` — an agent's sent messages and their states.
fn cmd_outbox(actor: Option<&String>) -> ExitCode {
    let Some(actor) = actor else {
        eprintln!("usage: terminite outbox <actor>");
        return ExitCode::from(2);
    };
    one_shot(&format!(
        r#"{{"id":1,"method":"room_outbox","params":{{"actor":"{}"}}}}"#,
        json_escape(actor)
    ))
}

/// `terminite files` → every live file claim in the room; `terminite files
/// <path>` → who (if anyone) is working in that path. The human's window into
/// the co-editing layer — who's in what, so a clobber is a choice not a surprise.
fn cmd_files(path: Option<&str>) -> ExitCode {
    match path {
        Some(p) => one_shot(&format!(
            r#"{{"id":1,"method":"file_status","params":{{"path":{}}}}}"#,
            serde_json::Value::String(p.to_string())
        )),
        None => one_shot(r#"{"id":1,"method":"files"}"#),
    }
}

/// `terminite room-join --actor <base>` — a one-shot, SILENT room join, used by
/// a faculty's SessionStart hook so an agent is present from launch instead of
/// only when it first calls a room tool (the eager-presence path for per-call
/// CLIs like agy that don't hold the MCP socket open). It connects, joins,
/// reads the ack, and exits; the dropped connection then *lingers* in the
/// roster (see presence.rs) so the actor stays present.
///
/// "Am I actually in terminite?" is answered host-side, which is robust even if
/// the CLI scrubbed the env: if terminite isn't running the connect just fails
/// (silent no-op), and if the caller isn't inside a real terminite pane the
/// host derives no pane for it, so it won't linger — a plain agent running
/// elsewhere never sticks in the room. Always silent and exit 0: a hook's
/// stdout would be injected into the agent's context, and it must never fail it.
fn cmd_room_join(args: &[String]) -> ExitCode {
    let base = args
        .iter()
        .position(|a| a == "--actor")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
        .unwrap_or("agent");
    // Resolve + connect; any failure means "no terminite here" → do nothing.
    let Some(path) = socket_path() else { return ExitCode::SUCCESS };
    let Ok(mut stream) = UnixStream::connect(&path) else { return ExitCode::SUCCESS };
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
    let req = serde_json::json!({
        "id": 1, "method": "room_join", "params": { "base": base }
    })
    .to_string();
    if writeln!(stream, "{req}").is_err() {
        return ExitCode::SUCCESS;
    }
    // Read the ack so the host registers presence before we disconnect — the
    // disconnect is what arms the linger. The response is intentionally not
    // printed (silent).
    let mut line = String::new();
    let mut reader = BufReader::new(stream);
    let _ = reader.read_line(&mut line);
    ExitCode::SUCCESS
}

/// PostToolUse hook entry point (`terminite tool-emit-hook`). Reads the hook
/// JSON on stdin and reports the tool call to the room — the "see" half: peers
/// watch your work. Attribution is by pane: `$TERMINITE_PANE` if the CLI
/// forwarded it (claude), else terminite derives it from this hook process's
/// ancestry (codex scrubs the env). terminite drops the emit if it can't
/// attribute the call. Always **fail-open and silent** — a hook must never
/// crash the agent, and its stdout would be injected into the agent's context.
/// The hook JSON shape is shared between Claude Code and Codex (`tool_name` /
/// `tool_input`), so one parser serves both.
fn cmd_tool_emit_hook() -> ExitCode {
    let mut input = String::new();
    if std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).is_err() {
        return ExitCode::SUCCESS;
    }
    let v: serde_json::Value = match serde_json::from_str(input.trim()) {
        Ok(v) => v,
        Err(_) => return ExitCode::SUCCESS,
    };
    let tool = v.get("tool_name").and_then(|t| t.as_str()).unwrap_or("tool");
    // A short human detail: file path for file tools, command for Bash, etc.
    let detail = v
        .get("tool_input")
        .and_then(|ti| {
            // Grok uses target_file; Claude/Codex-style use file_path / command.
            ti.get("target_file")
                .or_else(|| ti.get("file_path"))
                .or_else(|| ti.get("command"))
                .or_else(|| ti.get("pattern"))
                .or_else(|| ti.get("path"))
                .or_else(|| ti.get("url"))
                .and_then(|x| x.as_str())
        })
        .unwrap_or("");
    let title = if detail.is_empty() {
        tool.to_string()
    } else {
        format!("{tool} {detail}")
    };
    let mut params = serde_json::json!({ "tool": tool, "title": title });
    if let Some(pane) = std::env::var("TERMINITE_PANE")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        params["pane"] = serde_json::json!(pane);
    }
    let req = serde_json::json!({ "id": 1, "method": "tool_emit", "params": params }).to_string();
    // Send silently, fail-open — don't read or print the response.
    if let Some(path) = socket_path() {
        if let Ok(mut stream) = UnixStream::connect(&path) {
            let _ = writeln!(stream, "{req}");
        }
    }
    ExitCode::SUCCESS
}

/// The validated claude-terminite skill, embedded so the binary is
/// self-contained — `terminite install` writes this to the target profile.
const CLAUDE_SKILL: &str = include_str!("../faculty/claude-terminite/SKILL.md");
const CODEX_SKILL: &str = include_str!("../faculty/codex-terminite/SKILL.md");
const KIMI_SKILL: &str = include_str!("../faculty/kimi-terminite/SKILL.md");
const QWEN_SKILL: &str = include_str!("../faculty/qwen-terminite/SKILL.md");
const AGY_SKILL: &str = include_str!("../faculty/agy-terminite/SKILL.md");
const GROK_SKILL: &str = include_str!("../faculty/grok-terminite/SKILL.md");

/// `terminite install <faculty> [...]` — write a faculty into an AI CLI's
/// profile so a plain agent becomes terminite-aware. Opt-in (the user runs it)
/// and reversible. Today: claude, codex, kimi, qwen, agy, grok — each a thin
/// per-vendor adapter over the same room (skill + MCP, see-half where the CLI
/// allows it).
fn cmd_install(args: &[String]) -> ExitCode {
    match args.first().map(|s| s.as_str()) {
        Some("claude-terminite") | Some("claude") => install_claude_terminite(&args[1..]),
        Some("claude-channel") => install_claude_channel(&args[1..]),
        Some("codex-terminite") | Some("codex") => install_codex_terminite(&args[1..]),
        Some("kimi-terminite") | Some("kimi") => install_kimi_terminite(&args[1..]),
        Some("qwen-terminite") | Some("qwen") => install_qwen_terminite(&args[1..]),
        Some("agy-terminite") | Some("agy") => install_agy_terminite(&args[1..]),
        Some("grok-terminite") | Some("grok") => install_grok_terminite(&args[1..]),
        Some(other) => {
            eprintln!(
                "terminite install: unknown faculty `{other}` — try claude / codex / kimi / qwen / agy / grok (-terminite)"
            );
            ExitCode::from(2)
        }
        None => {
            eprintln!("usage: terminite install <claude|codex|kimi|qwen|agy|grok>-terminite [--profile <name|dir>]");
            ExitCode::from(2)
        }
    }
}

/// Install the codex faculty: place the skill into `$CODEX_HOME/skills/` and
/// register the `lounge` MCP server via `codex mcp add`. codex joins the room
/// the same way claude does (`terminite mcp --actor codex`); only the install
/// surfaces differ. The see-half hook is a follow-up (codex's hook schema is
/// version-specific + trust-gated). `--home <dir>` overrides `$CODEX_HOME`.
fn install_codex_terminite(args: &[String]) -> ExitCode {
    let bin = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("terminite install: can't resolve own path: {e}");
            return ExitCode::from(1);
        }
    };
    let explicit_home = args.iter().position(|a| a == "--home").and_then(|i| args.get(i + 1));
    let codex_home = match explicit_home {
        Some(dir) => PathBuf::from(dir),
        None => match std::env::var_os("CODEX_HOME") {
            Some(d) => PathBuf::from(d),
            None => match std::env::var_os("HOME") {
                Some(h) => PathBuf::from(h).join(".codex"),
                None => {
                    eprintln!("terminite install: no HOME / CODEX_HOME to install into");
                    return ExitCode::from(1);
                }
            },
        },
    };

    // 1. Place the skill.
    let skill_dir = codex_home.join("skills/terminite-room");
    if let Err(e) = std::fs::create_dir_all(&skill_dir) {
        eprintln!("terminite install: can't create {}: {e}", skill_dir.display());
        return ExitCode::from(1);
    }
    let skill_path = skill_dir.join("SKILL.md");
    if let Err(e) = std::fs::write(&skill_path, CODEX_SKILL) {
        eprintln!("terminite install: can't write {}: {e}", skill_path.display());
        return ExitCode::from(1);
    }

    // 2. Register the lounge MCP via codex's own CLI (writes [mcp_servers.lounge]
    //    to config.toml). Remove-then-add so re-install is clean.
    // 2. Add the see-half hook → $CODEX_HOME/hooks.json (PostToolUse, matcher
    //    "" = all tools). codex's hook JSON shares Claude's shape, so the same
    //    `tool-emit-hook` serves it. NOTE: codex requires the hook to be TRUSTED
    //    before it runs (its in-app /hooks review, or `--dangerously-bypass-hook-trust`).
    let hook_cmd = format!("{} tool-emit-hook", bin.display());
    let hook_added = match install_hook(&codex_home.join("hooks.json"), "", &hook_cmd) {
        Ok(added) => added,
        Err(e) => {
            eprintln!("terminite install: warning — couldn't add the see-half hook ({e})");
            false
        }
    };

    // 3. Register the lounge MCP via codex's own CLI.
    let manual = format!(
        "codex mcp add lounge -- {} mcp --actor codex",
        bin.display()
    );
    let with_home = |cmd: &mut std::process::Command| {
        if explicit_home.is_some() || std::env::var_os("CODEX_HOME").is_some() {
            cmd.env("CODEX_HOME", &codex_home);
        }
    };
    let mut rm = std::process::Command::new("codex");
    with_home(&mut rm);
    let _ = rm
        .args(["mcp", "remove", "lounge"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let mut cmd = std::process::Command::new("codex");
    with_home(&mut cmd);
    let status = cmd
        .args(["mcp", "add", "lounge", "--"])
        .arg(&bin)
        .args(["mcp", "--actor", "codex"])
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("installed codex-terminite into {}", codex_home.display());
            println!("  skill: {}", skill_path.display());
            println!(
                "  hook:  PostToolUse → {} tool-emit-hook{}",
                bin.display(),
                if hook_added { "" } else { " (already present)" }
            );
            println!("  mcp:   lounge → {} mcp --actor codex", bin.display());
            println!("\ncodex in a terminite pane now joins the room + streams its work.");
            println!("NOTE: codex must TRUST the hook before it runs — approve it in codex's");
            println!("      /hooks review, or launch codex with --dangerously-bypass-hook-trust.");
            println!(
                "reverse: codex mcp remove lounge; rm -r {}; remove the tool-emit-hook entry from {}/hooks.json",
                skill_dir.display(),
                codex_home.display()
            );
            ExitCode::SUCCESS
        }
        Ok(s) => {
            eprintln!("terminite install: skill placed, but `codex mcp add` failed ({s}).");
            eprintln!("add the MCP server yourself:\n  {manual}");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("terminite install: skill placed, but couldn't run `codex` ({e}) — is it on PATH?");
            eprintln!("add the MCP server yourself:\n  {manual}");
            ExitCode::from(1)
        }
    }
}

/// Add a `[[hooks]]` PostToolUse entry to kimi's `config.toml` (its hook config
/// is TOML, not JSON). kimi's `HookDef` is `{event, command, matcher}` and its
/// PostToolUse stdin payload carries `tool_name`/`tool_input` — the same
/// contract claude/codex use — so `tool-emit-hook` serves it unchanged.
/// Non-destructive (toml_edit preserves the rest of the file) + idempotent.
fn install_kimi_hook(config_file: &std::path::Path, command: &str) -> Result<bool, String> {
    use toml_edit::{Array, DocumentMut, InlineTable, Item, Value};
    let text = std::fs::read_to_string(config_file).unwrap_or_default();
    let mut doc = text
        .parse::<DocumentMut>()
        .map_err(|e| format!("parse {}: {e}", config_file.display()))?;
    if doc.get("hooks").and_then(|i| i.as_array()).is_none() {
        doc["hooks"] = Item::Value(Value::Array(Array::new()));
    }
    let arr = doc["hooks"].as_array_mut().ok_or("`hooks` is not an array")?;
    let already = arr.iter().any(|v| {
        v.as_inline_table()
            .and_then(|t| t.get("command"))
            .and_then(|c| c.as_str())
            == Some(command)
    });
    if already {
        return Ok(false);
    }
    let mut entry = InlineTable::new();
    entry.insert("event", "PostToolUse".into());
    entry.insert("command", command.into());
    entry.insert("matcher", "".into());
    arr.push(Value::InlineTable(entry));
    if let Some(parent) = config_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    crate::io_util::atomic_write(config_file, doc.to_string().as_bytes(), 0o644)
        .map_err(|e| format!("write {}: {e}", config_file.display()))?;
    Ok(true)
}

/// Install the kimi faculty: place the skill into `$KIMI_SHARE_DIR/skills/`
/// (kimi also reads `~/.claude/` and `~/.codex/` skills, so the room skill is
/// doubly discoverable) and register the `lounge` MCP via `kimi mcp add`. kimi
/// scrubs nothing special; pane is derived from the connecting process like
/// every other CLI. `--home <dir>` overrides `$KIMI_SHARE_DIR` (for testing).
fn install_kimi_terminite(args: &[String]) -> ExitCode {
    let bin = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("terminite install: can't resolve own path: {e}");
            return ExitCode::from(1);
        }
    };
    let explicit_home = args.iter().position(|a| a == "--home").and_then(|i| args.get(i + 1));
    let kimi_home = match explicit_home {
        Some(dir) => PathBuf::from(dir),
        None => match std::env::var_os("KIMI_SHARE_DIR") {
            Some(d) => PathBuf::from(d),
            None => match std::env::var_os("HOME") {
                Some(h) => PathBuf::from(h).join(".kimi"),
                None => {
                    eprintln!("terminite install: no HOME / KIMI_SHARE_DIR to install into");
                    return ExitCode::from(1);
                }
            },
        },
    };

    // 1. Place the skill.
    let skill_dir = kimi_home.join("skills/terminite-room");
    if let Err(e) = std::fs::create_dir_all(&skill_dir) {
        eprintln!("terminite install: can't create {}: {e}", skill_dir.display());
        return ExitCode::from(1);
    }
    let skill_path = skill_dir.join("SKILL.md");
    if let Err(e) = std::fs::write(&skill_path, KIMI_SKILL) {
        eprintln!("terminite install: can't write {}: {e}", skill_path.display());
        return ExitCode::from(1);
    }

    // 2. Register the lounge MCP via kimi's own CLI (stdio command after `--`).
    //    Remove-then-add so re-install is clean.
    let manual = format!("kimi mcp add lounge -- {} mcp --actor kimi", bin.display());
    let with_home = |cmd: &mut std::process::Command| {
        if explicit_home.is_some() || std::env::var_os("KIMI_SHARE_DIR").is_some() {
            cmd.env("KIMI_SHARE_DIR", &kimi_home);
        }
    };
    let mut rm = std::process::Command::new("kimi");
    with_home(&mut rm);
    let _ = rm
        .args(["mcp", "remove", "lounge"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let mut cmd = std::process::Command::new("kimi");
    with_home(&mut cmd);
    let status = cmd
        .args(["mcp", "add", "lounge", "--"])
        .arg(&bin)
        .args(["mcp", "--actor", "kimi"])
        .status();
    match status {
        Ok(s) if s.success() => {
            // See-half: add a [[hooks]] PostToolUse entry to kimi's config.toml.
            let hook_cmd = format!("{} tool-emit-hook", bin.display());
            let see = match install_kimi_hook(&kimi_home.join("config.toml"), &hook_cmd) {
                Ok(true) => "added",
                Ok(false) => "already present",
                Err(e) => {
                    eprintln!("terminite install: warning — couldn't add see-half hook ({e})");
                    "skipped"
                }
            };
            println!("installed kimi-terminite into {}", kimi_home.display());
            println!("  skill:    {}", skill_path.display());
            println!("  mcp:      lounge → {} mcp --actor kimi", bin.display());
            println!("  see-half: PostToolUse → {} tool-emit-hook ({see})", bin.display());
            println!("\nkimi in a terminite pane now joins the room + streams its work.");
            println!("reverse: kimi mcp remove lounge; rm -r {}", skill_dir.display());
            ExitCode::SUCCESS
        }
        Ok(s) => {
            eprintln!("terminite install: skill placed, but `kimi mcp add` failed ({s}).");
            eprintln!("add the MCP server yourself:\n  {manual}");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("terminite install: skill placed, but couldn't run `kimi` ({e}) — is it on PATH?");
            eprintln!("add the MCP server yourself:\n  {manual}");
            ExitCode::from(1)
        }
    }
}

/// Install the qwen faculty: place the skill into `~/.qwen/skills/` and register
/// the `lounge` MCP via `qwen mcp add <name> <command> [args]` (note: no `--`;
/// qwen takes the command as a positional). qwen has no config-relocate env, so
/// `--home <dir>` works by pointing `HOME` at `<dir>` (its `~/.qwen` then lives
/// under it) — used for isolated testing.
fn install_qwen_terminite(args: &[String]) -> ExitCode {
    let bin = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("terminite install: can't resolve own path: {e}");
            return ExitCode::from(1);
        }
    };
    let explicit_home = args.iter().position(|a| a == "--home").and_then(|i| args.get(i + 1));
    let qwen_dir = match explicit_home {
        Some(dir) => PathBuf::from(dir).join(".qwen"),
        None => match std::env::var_os("HOME") {
            Some(h) => PathBuf::from(h).join(".qwen"),
            None => {
                eprintln!("terminite install: no HOME to install into");
                return ExitCode::from(1);
            }
        },
    };

    // 1. Place the skill.
    let skill_dir = qwen_dir.join("skills/terminite-room");
    if let Err(e) = std::fs::create_dir_all(&skill_dir) {
        eprintln!("terminite install: can't create {}: {e}", skill_dir.display());
        return ExitCode::from(1);
    }
    let skill_path = skill_dir.join("SKILL.md");
    if let Err(e) = std::fs::write(&skill_path, QWEN_SKILL) {
        eprintln!("terminite install: can't write {}: {e}", skill_path.display());
        return ExitCode::from(1);
    }

    // 2. Register the lounge MCP via qwen's own CLI (positional command, no `--`).
    //    `--trust` so the room tools don't prompt. qwen writes user scope into
    //    `~/.qwen/settings.json`. No `mcp remove` guard needed — re-add replaces.
    let manual = format!("qwen mcp add lounge {} mcp --actor qwen", bin.display());
    let with_home = |cmd: &mut std::process::Command| {
        if let Some(dir) = explicit_home {
            cmd.env("HOME", dir);
        }
    };
    let mut rm = std::process::Command::new("qwen");
    with_home(&mut rm);
    let _ = rm
        .args(["mcp", "remove", "lounge"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let mut cmd = std::process::Command::new("qwen");
    with_home(&mut cmd);
    let status = cmd
        .args(["mcp", "add", "--trust", "lounge"])
        .arg(&bin)
        .args(["mcp", "--actor", "qwen"])
        .status();
    match status {
        Ok(s) if s.success() => {
            // See-half: qwen reads claude-shaped `hooks.PostToolUse` from
            // settings.json (verified in its cli.js), so the same install_hook
            // serves it. Done after `mcp add` so we merge into the settings.json
            // qwen just wrote. Same stdin contract (tool_name/tool_input).
            let hook_cmd = format!("{} tool-emit-hook", bin.display());
            let see = match install_hook(&qwen_dir.join("settings.json"), "", &hook_cmd) {
                Ok(true) => "added",
                Ok(false) => "already present",
                Err(e) => {
                    eprintln!("terminite install: warning — couldn't add see-half hook ({e})");
                    "skipped"
                }
            };
            println!("installed qwen-terminite into {}", qwen_dir.display());
            println!("  skill:    {}", skill_path.display());
            println!("  mcp:      lounge → {} mcp --actor qwen", bin.display());
            println!("  see-half: PostToolUse → {} tool-emit-hook ({see})", bin.display());
            println!("\nqwen in a terminite pane now joins the room + streams its work.");
            println!("reverse: qwen mcp remove lounge; rm -r {}", skill_dir.display());
            ExitCode::SUCCESS
        }
        Ok(s) => {
            eprintln!("terminite install: skill placed, but `qwen mcp add` failed ({s}).");
            eprintln!("add the MCP server yourself:\n  {manual}");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("terminite install: skill placed, but couldn't run `qwen` ({e}) — is it on PATH?");
            eprintln!("add the MCP server yourself:\n  {manual}");
            ExitCode::from(1)
        }
    }
}

/// Install the grok faculty: place the skill into `~/.grok/skills/`, register
/// the `lounge` MCP via `grok mcp add`, and write a see-half PostToolUse hook
/// under `~/.grok/hooks/` (Grok's hook discovery merges those JSON files).
/// Grok joins as `terminite mcp --actor grok`. `--home <dir>` overrides the
/// grok config root (default `~/.grok`).
fn install_grok_terminite(args: &[String]) -> ExitCode {
    let bin = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("terminite install: can't resolve own path: {e}");
            return ExitCode::from(1);
        }
    };
    let explicit_home = args.iter().position(|a| a == "--home").and_then(|i| args.get(i + 1));
    let grok_dir = match explicit_home {
        Some(dir) => PathBuf::from(dir),
        None => match std::env::var_os("GROK_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".grok")))
        {
            Some(p) => p,
            None => {
                eprintln!("terminite install: no HOME / GROK_HOME to install into");
                return ExitCode::from(1);
            }
        },
    };

    // 1. Place the skill.
    let skill_dir = grok_dir.join("skills/terminite-room");
    if let Err(e) = std::fs::create_dir_all(&skill_dir) {
        eprintln!("terminite install: can't create {}: {e}", skill_dir.display());
        return ExitCode::from(1);
    }
    let skill_path = skill_dir.join("SKILL.md");
    if let Err(e) = std::fs::write(&skill_path, GROK_SKILL) {
        eprintln!("terminite install: can't write {}: {e}", skill_path.display());
        return ExitCode::from(1);
    }

    // 2. See-half + optional SessionStart join — dedicated hook file so we
    //    don't have to merge into settings.json (Grok loads ~/.grok/hooks/*.json).
    let hook_cmd = format!("{} tool-emit-hook", bin.display());
    let join_cmd = format!("{} room-join --actor grok", bin.display());
    let hooks_dir = grok_dir.join("hooks");
    let hook_path = hooks_dir.join("terminite-lounge.json");
    let see = match install_grok_hooks(&hook_path, &hook_cmd, &join_cmd) {
        Ok(true) => "added",
        Ok(false) => "already present",
        Err(e) => {
            eprintln!("terminite install: warning — couldn't add see-half hook ({e})");
            "skipped"
        }
    };

    // 3. Register the lounge MCP via grok's CLI when installing into the live
    //    user config. For a custom --home, write the [mcp_servers.lounge]
    //    section into that tree's config.toml directly.
    let manual = format!(
        "grok mcp add lounge -s user -- {} mcp --actor grok",
        bin.display()
    );
    if explicit_home.is_some() {
        match install_grok_mcp_toml(&grok_dir.join("config.toml"), &bin) {
            Ok(()) => {
                println!("installed grok-terminite into {}", grok_dir.display());
                println!("  skill:    {}", skill_path.display());
                println!("  mcp:      lounge → {} mcp --actor grok (config.toml)", bin.display());
                println!("  see-half: PostToolUse → {} ({see})", hook_cmd);
                println!("  join:     SessionStart → {} ({see})", join_cmd);
                println!("\ngrok in a terminite pane now joins the room + streams its work.");
                println!(
                    "reverse: remove [mcp_servers.lounge] from {}; rm -r {}; rm {}",
                    grok_dir.join("config.toml").display(),
                    skill_dir.display(),
                    hook_path.display()
                );
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("terminite install: skill + hooks placed, but config.toml MCP write failed ({e}).");
                eprintln!("add the MCP server yourself:\n  {manual}");
                ExitCode::from(1)
            }
        }
    } else {
        let _ = std::process::Command::new("grok")
            .args(["mcp", "remove", "lounge"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let status = std::process::Command::new("grok")
            .args(["mcp", "add", "lounge", "-s", "user", "--"])
            .arg(&bin)
            .args(["mcp", "--actor", "grok"])
            .status();
        match status {
            Ok(s) if s.success() => {
                println!("installed grok-terminite into {}", grok_dir.display());
                println!("  skill:    {}", skill_path.display());
                println!("  mcp:      lounge → {} mcp --actor grok", bin.display());
                println!("  see-half: PostToolUse → {} ({see})", hook_cmd);
                println!("  join:     SessionStart → {} ({see})", join_cmd);
                println!("\nplain `grok` in a terminite pane now joins the room + streams its work.");
                println!(
                    "reverse: grok mcp remove lounge; rm -r {}; rm {}",
                    skill_dir.display(),
                    hook_path.display()
                );
                ExitCode::SUCCESS
            }
            Ok(s) => {
                eprintln!("terminite install: skill + hooks placed, but `grok mcp add` failed ({s}).");
                eprintln!("add the MCP server yourself:\n  {manual}");
                ExitCode::from(1)
            }
            Err(e) => {
                eprintln!(
                    "terminite install: skill + hooks placed, but couldn't run `grok` ({e}) — is it on PATH?"
                );
                eprintln!("add the MCP server yourself:\n  {manual}");
                ExitCode::from(1)
            }
        }
    }
}

/// Write (or refresh) `~/.grok/hooks/terminite-lounge.json` with PostToolUse
/// see-half + SessionStart room-join. Idempotent: returns Ok(false) when the
/// file already carries both commands.
fn install_grok_hooks(
    hook_path: &std::path::Path,
    emit_cmd: &str,
    join_cmd: &str,
) -> Result<bool, String> {
    if let Some(parent) = hook_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let existing = std::fs::read_to_string(hook_path).unwrap_or_default();
    if existing.contains(emit_cmd) && existing.contains(join_cmd) {
        return Ok(false);
    }
    let body = serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "hooks": [
                    { "type": "command", "command": join_cmd }
                ]
            }],
            "PostToolUse": [{
                "hooks": [
                    { "type": "command", "command": emit_cmd }
                ]
            }]
        }
    });
    let pretty = serde_json::to_string_pretty(&body).map_err(|e| format!("serialize: {e}"))?;
    crate::io_util::atomic_write(hook_path, pretty.as_bytes(), 0o644)
        .map_err(|e| format!("write {}: {e}", hook_path.display()))?;
    Ok(true)
}

/// Write (or refresh) a Claude hooks file with PostToolUse see-half +
/// SessionStart room-join. Same JSON shape as Grok's hooks but written to
/// the Claude profile's `hooks/` directory. Idempotent: returns Ok(false)
/// when both commands are already present.
fn install_claude_hooks(
    hook_path: &std::path::Path,
    emit_cmd: &str,
    join_cmd: &str,
) -> Result<bool, String> {
    if let Some(parent) = hook_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let existing = std::fs::read_to_string(hook_path).unwrap_or_default();
    if existing.contains(emit_cmd) && existing.contains(join_cmd) {
        return Ok(false);
    }
    let body = serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "hooks": [
                    { "type": "command", "command": join_cmd }
                ]
            }],
            "PostToolUse": [{
                "hooks": [
                    { "type": "command", "command": emit_cmd }
                ]
            }]
        }
    });
    let pretty = serde_json::to_string_pretty(&body).map_err(|e| format!("serialize: {e}"))?;
    crate::io_util::atomic_write(hook_path, pretty.as_bytes(), 0o644)
        .map_err(|e| format!("write {}: {e}", hook_path.display()))?;
    Ok(true)
}

/// For offline / custom-home installs: ensure `[mcp_servers.lounge]` points at
/// this terminite binary. Replaces an existing lounge block; leaves other
/// servers alone. Uses line-oriented edit so we don't pull toml_edit into a
/// second writer path — the section is small and machine-owned.
fn install_grok_mcp_toml(config_path: &std::path::Path, bin: &std::path::Path) -> Result<(), String> {
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let bin_s = bin.display().to_string().replace('\\', "\\\\").replace('"', "\\\"");
    let section = format!(
        "\n[mcp_servers.lounge]\ncommand = \"{bin_s}\"\nargs = [\"mcp\", \"--actor\", \"grok\"]\nenabled = true\n"
    );
    let original = std::fs::read_to_string(config_path).unwrap_or_default();
    let mut out = String::new();
    let mut skipping = false;
    for line in original.lines() {
        let trimmed = line.trim();
        if trimmed == "[mcp_servers.lounge]" {
            skipping = true;
            continue;
        }
        if skipping {
            // Leave the old lounge block when the next table header appears.
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                skipping = false;
            } else {
                continue;
            }
        }
        if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !out.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&section);
    crate::io_util::atomic_write(config_path, out.as_bytes(), 0o644)
        .map_err(|e| format!("write {}: {e}", config_path.display()))?;
    Ok(())
}

/// Merge entries into agy's permission allow-list
/// (`~/.gemini/antigravity-cli/settings.json` → `permissions.allow`) so agy
/// stops prompting for terminite's own lounge tools and runs the faculty's hook
/// commands unattended (agy gates every command + MCP call on this list).
/// Non-destructive + idempotent: preserves model/trustedWorkspaces/existing
/// entries, only appends what's missing. Returns how many were newly added.
fn seed_agy_permissions(settings_file: &std::path::Path, entries: &[String]) -> Result<usize, String> {
    let mut root: serde_json::Value = match std::fs::read_to_string(settings_file) {
        Ok(s) if !s.trim().is_empty() => serde_json::from_str(&s)
            .map_err(|e| format!("parse {}: {e}", settings_file.display()))?,
        _ => serde_json::json!({}),
    };
    let allow = root
        .as_object_mut()
        .ok_or("settings is not a JSON object")?
        .entry("permissions")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or("`permissions` is not an object")?
        .entry("allow")
        .or_insert_with(|| serde_json::json!([]));
    let arr = allow.as_array_mut().ok_or("`permissions.allow` is not an array")?;
    let existing: std::collections::HashSet<String> =
        arr.iter().filter_map(|v| v.as_str().map(String::from)).collect();
    let mut added = 0;
    for e in entries {
        if !existing.contains(e) {
            arr.push(serde_json::json!(e));
            added += 1;
        }
    }
    if added > 0 {
        if let Some(parent) = settings_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let pretty = serde_json::to_string_pretty(&root).map_err(|e| format!("serialize: {e}"))?;
        crate::io_util::atomic_write(settings_file, pretty.as_bytes(), 0o644)
            .map_err(|e| format!("write {}: {e}", settings_file.display()))?;
    }
    Ok(added)
}

/// Install the agy (Antigravity) faculty. agy has no `mcp add` — it imports
/// *plugins*. So terminite stages a claude/gemini-style plugin (the layout agy
/// expects: `plugin.json` manifest + `mcp_config.json` for the MCP + `skills/`)
/// and hands it to `agy plugin install`, which copies it into agy's plugin dir
/// (`~/.gemini/config/plugins/`). Reverse: `agy plugin uninstall terminite-room`.
/// `--home <dir>` points agy's GeminiDir under `<dir>` (for testing).
fn install_agy_terminite(args: &[String]) -> ExitCode {
    let bin = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("terminite install: can't resolve own path: {e}");
            return ExitCode::from(1);
        }
    };
    let explicit_home = args.iter().position(|a| a == "--home").and_then(|i| args.get(i + 1));

    // Stage the plugin in a temp dir: plugin.json + mcp_config.json + skill +
    // hooks/hooks.json (the layout agy's `plugin install` expects).
    let stage = std::env::temp_dir().join("terminite-agy-faculty");
    let skill_dir = stage.join("skills/terminite-room");
    let hooks_dir = stage.join("hooks");
    if let Err(e) = std::fs::create_dir_all(&skill_dir).and_then(|_| std::fs::create_dir_all(&hooks_dir)) {
        eprintln!("terminite install: can't stage plugin at {}: {e}", stage.display());
        return ExitCode::from(1);
    }
    let manifest = serde_json::json!({
        "name": "terminite-room",
        "version": "0.1.0",
        "description": "Presence and coordination in a shared terminite room."
    });
    let mcp_config = serde_json::json!({
        "mcpServers": {
            "lounge": { "command": bin.to_string_lossy(), "args": ["mcp", "--actor", "agy"] }
        }
    });
    // hooks/hooks.json (agy's named-hooks array, learned from its own validator):
    //  - SessionStart → join the room on launch, so agy is present from the start
    //    rather than only when it first calls a tool (it spawns its MCP per-call).
    //    `room-join` is silent and self-guards on being in terminite (host-side).
    //  - PreToolUse → the see-half: agy's tool calls stream into the room.
    let hooks = serde_json::json!({
        "hooks": [
            { "name": "terminite-join", "event": "SessionStart",
              "command": format!("{} room-join --actor agy", bin.display()) },
            { "name": "terminite-see-half", "event": "PreToolUse",
              "command": format!("{} tool-emit-hook", bin.display()) }
        ]
    });
    let writes = [
        (stage.join("plugin.json"), serde_json::to_string_pretty(&manifest).unwrap_or_default()),
        (stage.join("mcp_config.json"), serde_json::to_string_pretty(&mcp_config).unwrap_or_default()),
        (skill_dir.join("SKILL.md"), AGY_SKILL.to_string()),
        (hooks_dir.join("hooks.json"), serde_json::to_string_pretty(&hooks).unwrap_or_default()),
    ];
    for (path, body) in &writes {
        if let Err(e) = std::fs::write(path, body) {
            eprintln!("terminite install: can't write {}: {e}", path.display());
            return ExitCode::from(1);
        }
    }

    // Hand the staged plugin to agy. It validates + copies it into its own
    // plugin dir; re-install replaces in place.
    let manual = format!("agy plugin install {}", stage.display());
    let mut cmd = std::process::Command::new("agy");
    if let Some(dir) = explicit_home {
        cmd.env("HOME", dir);
    }
    let status = cmd.args(["plugin", "install"]).arg(&stage).status();
    match status {
        Ok(s) if s.success() => {
            // Pre-seed agy's permission allow-list: agy gates every command +
            // MCP call on it, so without this agy prompts relentlessly and skips
            // the faculty's hook commands. Allow the lounge tools the skill uses
            // and the two hook commands.
            let settings = explicit_home
                .map(|d| PathBuf::from(d.as_str()))
                .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
                .map(|h| h.join(".gemini/antigravity-cli/settings.json"));
            let allow = [
                "mcp(lounge/terminite_room_who)".to_string(),
                "mcp(lounge/terminite_activities_list)".to_string(),
                "mcp(lounge/terminite_activity_emit)".to_string(),
                format!("command({} room-join --actor agy)", bin.display()),
                format!("command({} tool-emit-hook)", bin.display()),
            ];
            let perms = match settings.as_ref().map(|p| seed_agy_permissions(p, &allow)) {
                Some(Ok(n)) => format!("{n} added"),
                Some(Err(e)) => {
                    eprintln!("terminite install: warning — couldn't seed agy permissions ({e})");
                    "skipped".into()
                }
                None => "no HOME — skipped".to_string(),
            };
            println!("installed agy-terminite (plugin → agy's ~/.gemini/config/plugins/)");
            println!("  skill:    bundled (skills/terminite-room/SKILL.md)");
            println!("  mcp:      lounge → {} mcp --actor agy", bin.display());
            println!("  presence: SessionStart hook → joins the room on launch (eager)");
            println!("  see-half: PreToolUse hook → agy's tool calls stream into the room");
            println!("  perms:    pre-allowed lounge tools + hook commands ({perms})");
            println!("\nagy in a terminite pane now joins the room on launch + streams its work.");
            println!("reverse: agy plugin uninstall terminite-room");
            ExitCode::SUCCESS
        }
        Ok(s) => {
            eprintln!("terminite install: staged the plugin, but `agy plugin install` failed ({s}).");
            eprintln!("install it yourself:\n  {manual}");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("terminite install: staged the plugin, but couldn't run `agy` ({e}) — is it on PATH?");
            eprintln!("install it yourself:\n  {manual}");
            ExitCode::from(1)
        }
    }
}

/// Wire claude's comms-base RECEIVER — the channel. Registers a `lounge-channel`
/// MCP server (`terminite channel`) so claude can spawn it, and prints the
/// wrapped launch that turns it on. The wake costs a special launch (the dev
/// channel flag) — that's the per-vendor cost the source-dive found, not faked.
/// PREVIEW: the channel flag/protocol is a Claude Code research preview and may
/// change; this needs live confirmation against the installed claude version.
fn install_claude_channel(args: &[String]) -> ExitCode {
    let bin = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("terminite install: can't resolve own path: {e}");
            return ExitCode::from(1);
        }
    };
    let config_dir = match resolve_profile_dir(args) {
        Some(d) => d,
        None => {
            eprintln!("terminite install: no HOME / CLAUDE_CONFIG_DIR to install into");
            return ExitCode::from(1);
        }
    };
    let explicit_profile = args.iter().any(|a| a == "--profile");
    let manual = format!("claude mcp add --scope user lounge-channel -- {} channel", bin.display());
    let mut rm = std::process::Command::new("claude");
    if explicit_profile {
        rm.env("CLAUDE_CONFIG_DIR", &config_dir);
    }
    let _ = rm
        .args(["mcp", "remove", "lounge-channel"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let mut cmd = std::process::Command::new("claude");
    if explicit_profile {
        cmd.env("CLAUDE_CONFIG_DIR", &config_dir);
    }
    let status = cmd
        .args(["mcp", "add", "--scope", "user", "lounge-channel", "--"])
        .arg(&bin)
        .args(["channel"])
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("registered the claude channel receiver: lounge-channel → {} channel", bin.display());
            println!("\nTurn the wake ON by launching claude with the channel flag:");
            println!("  claude --dangerously-load-development-channels server:lounge-channel");
            println!("\nThen a directed room message to this claude is PUSHED into its");
            println!("running session as a <channel> event — it wakes without polling.");
            println!("CAVEATS (Claude Code research preview): Anthropic auth only (no");
            println!("Bedrock/Vertex), interactive only (not -p), v2.1.80+. The flag/protocol");
            println!("may change. reverse: claude mcp remove lounge-channel");
            ExitCode::SUCCESS
        }
        Ok(s) => {
            eprintln!("terminite install: `claude mcp add` failed ({s}).");
            eprintln!("register it yourself:\n  {manual}");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("terminite install: couldn't run `claude` ({e}) — is it on PATH?");
            eprintln!("register it yourself:\n  {manual}");
            ExitCode::from(1)
        }
    }
}

fn install_claude_terminite(args: &[String]) -> ExitCode {
    let bin = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("terminite install: can't resolve own path: {e}");
            return ExitCode::from(1);
        }
    };
    // Profile dir: `--profile <name|dir>` → a bare name `bivoo` means
    // `~/.claude-bivoo`; a path is used as-is. Else $CLAUDE_CONFIG_DIR, else
    // ~/.claude.
    let config_dir = match resolve_profile_dir(args) {
        Some(d) => d,
        None => {
            eprintln!("terminite install: no HOME / CLAUDE_CONFIG_DIR to install into");
            return ExitCode::from(1);
        }
    };

    // 1. Place the skill (the context carrier).
    let skill_dir = config_dir.join("skills/terminite-room");
    if let Err(e) = std::fs::create_dir_all(&skill_dir) {
        eprintln!("terminite install: can't create {}: {e}", skill_dir.display());
        return ExitCode::from(1);
    }
    let skill_path = skill_dir.join("SKILL.md");
    if let Err(e) = std::fs::write(&skill_path, CLAUDE_SKILL) {
        eprintln!("terminite install: can't write {}: {e}", skill_path.display());
        return ExitCode::from(1);
    }

    // 2. Add the see-half hook (PostToolUse → tool-emit-hook) so peers see
    //    this claude's tool calls, not just its messages. Non-destructive +
    //    idempotent; optional (skill + mcp still install if it fails).
    let hook_cmd = format!("{} tool-emit-hook", bin.display());
    let hook_added = match install_hook(
        &config_dir.join("settings.json"),
        "Edit|Write|Read|Bash|Grep|Glob|NotebookEdit",
        &hook_cmd,
    ) {
        Ok(added) => added,
        Err(e) => {
            eprintln!("terminite install: warning — couldn't add the see-half hook ({e})");
            false
        }
    };

    // 2b. SessionStart room-join — a separate hooks file so Claude auto-joins
    //     the room when its session starts. Idempotent; non-destructive.
    let join_cmd = format!("{} room-join --actor claude", bin.display());
    let hooks_dir = config_dir.join("hooks");
    let hook_file = hooks_dir.join("terminite-lounge.json");
    let join_added = match install_claude_hooks(&hook_file, &hook_cmd, &join_cmd) {
        Ok(added) => added,
        Err(e) => {
            eprintln!("terminite install: warning — couldn't add SessionStart hook ({e})");
            false
        }
    };

    // 3. Register the MCP server via claude's own CLI, so the config edit is
    //    claude's (robust) not ours.
    let manual = format!(
        "claude mcp add --scope user lounge -- {} mcp --actor claude",
        bin.display()
    );
    let explicit_profile = args.iter().any(|a| a == "--profile");
    // Idempotent: drop any prior `lounge` so a re-install cleanly replaces it
    // instead of erroring "already exists".
    let mut rm = std::process::Command::new("claude");
    if explicit_profile {
        rm.env("CLAUDE_CONFIG_DIR", &config_dir);
    }
    let _ = rm
        .args(["mcp", "remove", "lounge"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let mut cmd = std::process::Command::new("claude");
    // Only override CLAUDE_CONFIG_DIR for an explicit `--profile`. For the
    // default profile we must NOT set it: a plain `claude` reads its user MCP
    // config from `~/.claude.json`, but forcing `CLAUDE_CONFIG_DIR=~/.claude`
    // makes `claude mcp add` write to `~/.claude/.claude.json` instead — a
    // file the default claude never reads, so the server would silently never
    // load.
    if explicit_profile {
        cmd.env("CLAUDE_CONFIG_DIR", &config_dir);
    }
    let status = cmd
        .args(["mcp", "add", "--scope", "user", "lounge", "--"])
        .arg(&bin)
        .args(["mcp", "--actor", "claude"])
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("installed claude-terminite into {}", config_dir.display());
            println!("  skill:     {}", skill_path.display());
            println!(
                "  hook:      PostToolUse → {} tool-emit-hook{}",
                bin.display(),
                if hook_added { "" } else { " (already present)" }
            );
            println!(
                "  join:      SessionStart → {}{}",
                join_cmd,
                if join_added { "" } else { " (already present)" }
            );
            println!("  mcp:       lounge → {} mcp --actor claude", bin.display());
            println!("\nplain `claude` in a terminite pane now joins the room + streams its work.");
            println!(
                "reverse with: claude mcp remove lounge   (and rm -r {} {})",
                skill_dir.display(),
                hooks_dir.display()
            );
            ExitCode::SUCCESS
        }
        Ok(s) => {
            eprintln!("terminite install: skill placed, but `claude mcp add` failed ({s}).");
            eprintln!("add the MCP server yourself:\n  {manual}");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("terminite install: skill placed, but couldn't run `claude` ({e}) — is it on PATH?");
            eprintln!("add the MCP server yourself:\n  {manual}");
            ExitCode::from(1)
        }
    }
}

/// Add the see-half `PostToolUse` hook to the profile's `settings.json`,
/// non-destructively and idempotently. Returns `Ok(true)` if newly added,
/// `Ok(false)` if our command was already present.
/// Add a `PostToolUse` command hook to a hook file (claude's `settings.json`,
/// codex's `hooks.json` — same `hooks.PostToolUse` shape), non-destructively
/// and idempotently. Returns `Ok(true)` if newly added.
fn install_hook(hooks_file: &std::path::Path, matcher: &str, command: &str) -> Result<bool, String> {
    let mut root: serde_json::Value = match std::fs::read_to_string(hooks_file) {
        Ok(s) if !s.trim().is_empty() => serde_json::from_str(&s)
            .map_err(|e| format!("parse {}: {e}", hooks_file.display()))?,
        _ => serde_json::json!({}),
    };
    let post = root
        .as_object_mut()
        .ok_or("hook file is not a JSON object")?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or("`hooks` is not an object")?
        .entry("PostToolUse")
        .or_insert_with(|| serde_json::json!([]));
    let arr = post.as_array_mut().ok_or("`hooks.PostToolUse` is not an array")?;
    let already = arr.iter().any(|group| {
        group
            .get("hooks")
            .and_then(|h| h.as_array())
            .is_some_and(|hs| {
                hs.iter()
                    .any(|h| h.get("command").and_then(|c| c.as_str()) == Some(command))
            })
    });
    if already {
        return Ok(false);
    }
    arr.push(serde_json::json!({
        "matcher": matcher,
        "hooks": [ { "type": "command", "command": command } ]
    }));
    let pretty = serde_json::to_string_pretty(&root).map_err(|e| format!("serialize: {e}"))?;
    crate::io_util::atomic_write(hooks_file, pretty.as_bytes(), 0o644)
        .map_err(|e| format!("write {}: {e}", hooks_file.display()))?;
    Ok(true)
}

/// Resolve the target Claude profile dir from `--profile`, else
/// `$CLAUDE_CONFIG_DIR`, else `~/.claude`. A bare `--profile bivoo` maps to
/// `~/.claude-bivoo` (the convention); a value with a `/` is a literal path.
fn resolve_profile_dir(args: &[String]) -> Option<PathBuf> {
    if let Some(i) = args.iter().position(|a| a == "--profile") {
        if let Some(val) = args.get(i + 1) {
            if val.contains('/') {
                return Some(PathBuf::from(val));
            }
            let home = std::env::var_os("HOME")?;
            return Some(PathBuf::from(home).join(format!(".claude-{val}")));
        }
    }
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(d));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".claude"))
}

fn cmd_activities(args: &[String]) -> ExitCode {
    // terminite activities            → the whole room, time order
    // terminite activities <actor>    → one actor's activity
    // terminite activities to <slug>  → messages addressed to <slug> (inbox)
    let params = match args.first().map(|s| s.as_str()) {
        Some("to") => match args.get(1) {
            Some(slug) => format!(r#"{{"to":"{slug}"}}"#),
            None => {
                eprintln!("usage: terminite activities to <slug>");
                return ExitCode::from(2);
            }
        },
        Some(actor) => format!(r#"{{"actor":"{actor}"}}"#),
        None => "{}".to_string(),
    };
    one_shot(&format!(
        r#"{{"id":1,"method":"activities_list","params":{params}}}"#
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

/// `terminite room-listen --actor <slug>` — subscribe to the comms base and
/// stream directed messages pushed to that actor (delivery, live, no poll). The
/// test harness for the held-connection transport, and the basis every per-CLI
/// receiver builds on: hold this open, and on each pushed `room_message`,
/// surface it into the agent and `room_ack` it.
fn cmd_room_listen(args: &[String]) -> ExitCode {
    let actor = args
        .iter()
        .position(|a| a == "--actor")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
        .unwrap_or("agent");
    let mut stream = connect_or_exit();
    let req = serde_json::json!({
        "id": 1, "method": "room_subscribe", "params": { "actor": actor }
    })
    .to_string();
    if writeln!(stream, "{req}").is_err() {
        eprintln!("terminite: room_subscribe write failed");
        return ExitCode::from(1);
    }
    // Stream every pushed line to stdout, flushing so a piped reader sees
    // deliveries as they happen.
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
    // The id must be a single safe path component. Without this, a value like
    // `..` or `../victim` escapes the modules dir and `remove_dir_all` deletes
    // arbitrary directories. Reject traversal, separators, and absolutes.
    let is_one_normal_component = std::path::Path::new(id)
        .components()
        .eq(std::iter::once(std::path::Component::Normal(id.as_ref())));
    if id.is_empty() || !is_one_normal_component {
        eprintln!("terminite: invalid module id '{id}' (must be a single name)");
        return ExitCode::from(1);
    }
    let dst = dst_root.join(id);
    // Belt and suspenders: the join must stay a direct child of the root.
    if dst.parent() != Some(dst_root.as_path()) {
        eprintln!("terminite: refusing to remove a path outside the modules dir");
        return ExitCode::from(1);
    }
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
    // Atomic + mode-preserving: a private rc (0600) stays private, and a
    // crash mid-write can't truncate the user's shell config. New rc → 0644.
    if let Err(e) = crate::io_util::atomic_write(&rc, new_content.as_bytes(), 0o644) {
        eprintln!("terminite: shell-init: write {} failed: {e}", rc.display());
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
