//! Command-line front end shared by both executables:
//! * `keep-vm-resolution.exe`    - console subsystem: the CLI you type commands into.
//! * `keep-vm-resolution-bg.exe` - windows subsystem: the invisible background
//!   instance started at logon (no console window flashes).

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetSystemMetrics, PostMessageW, SM_REMOTESESSION, WM_CLOSE,
};

use crate::config::{self, Config};
use crate::display::{self, Target};
use crate::{autostart, daemon, log};

/// File name of the windowless background executable (next to the CLI exe).
pub const BG_EXE: &str = "keep-vm-resolution-bg.exe";

const HELP: &str = "\
keep-vm-resolution - restore a preset resolution after VMware changes it

USAGE:
  keep-vm-resolution [--config <file>] <command> [options]

COMMANDS:
  set <W> <H> [--refresh <Hz>] [--bpp <bits>] [--device <\\\\.\\DISPLAYn>]
                         Save the preset resolution to the config file.
  set-current            Save the current resolution as the preset.
  install [--all-users] [--no-start]
                         Start automatically at logon (HKCU Run key; HKLM with
                         --all-users, needs admin) and start it now (unless
                         --no-start, e.g. when installing over SSH).
  uninstall [--all-users]  Remove the autostart entry and stop the running instance.
  start                  Start the invisible background instance (keep-vm-resolution-bg.exe).
  stop                   Stop the instance running in this session.
  pause | resume         Temporarily stop / restart enforcing (e.g. while you
                         deliberately change the resolution).
  apply                  Restore the preset once, right now.
  status                 Show preset, current mode, autostart and log location.
  list-modes             List the modes the display driver supports.
  run                    Run in the foreground of this terminal, logging to it (debugging).
  help                   Show this help.

The config file defaults to keep-vm-resolution.ini next to the executables.
The log is written to %LOCALAPPDATA%\\KeepVMResolution\\keep-vm-resolution.log.
";

struct Args {
    config: PathBuf,
    command: String,
    rest: Vec<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut config = None;
    let mut command = None;
    let mut rest = Vec::new();
    while let Some(a) = it.next() {
        if a == "--config" || a == "-c" {
            config = Some(PathBuf::from(it.next().ok_or("--config needs a path")?));
        } else if command.is_none() && (a == "-h" || a == "--help" || a == "/?") {
            command = Some("help".into());
        } else if command.is_none() && !a.starts_with('-') {
            command = Some(a.to_ascii_lowercase());
        } else {
            rest.push(a);
        }
    }
    let config = match config {
        Some(p) if p.is_relative() => std::env::current_dir().map(|d| d.join(&p)).unwrap_or(p),
        Some(p) => p,
        None => config::default_path(),
    };
    Ok(Args {
        config,
        command: command.unwrap_or_else(|| "run".into()),
        rest,
    })
}

fn has_flag(rest: &[String], flag: &str) -> bool {
    rest.iter().any(|a| a.eq_ignore_ascii_case(flag))
}

fn flag_value(rest: &[String], flag: &str) -> Result<Option<String>, String> {
    match rest.iter().position(|a| a.eq_ignore_ascii_case(flag)) {
        Some(i) => rest
            .get(i + 1)
            .cloned()
            .map(Some)
            .ok_or_else(|| format!("{flag} needs a value")),
        None => Ok(None),
    }
}

/// Reject options a command does not understand instead of silently ignoring them.
fn check_options(rest: &[String], allowed: &[&str]) -> Result<(), String> {
    for a in rest.iter().filter(|a| a.starts_with("--")) {
        if !allowed.iter().any(|x| a.eq_ignore_ascii_case(x)) {
            return Err(format!("unknown option '{a}' (try: help)"));
        }
    }
    Ok(())
}

fn load_or_default(path: &Path) -> Result<Config, String> {
    if path.exists() {
        Config::load(path)
    } else {
        Ok(Config::default())
    }
}

fn target_of(c: &Config) -> Target {
    Target {
        width: c.width,
        height: c.height,
        bpp: c.bpp,
        refresh: c.refresh,
    }
}

fn is_remote_session() -> bool {
    unsafe { GetSystemMetrics(SM_REMOTESESSION) != 0 }
}

/// Path of the background executable, which sits next to this one.
fn bg_exe() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate executable: {e}"))?;
    let bg = exe.with_file_name(BG_EXE);
    if bg.exists() {
        Ok(bg)
    } else {
        Err(format!(
            "{} not found (it must sit next to this exe)",
            bg.display()
        ))
    }
}

fn bg_command_line(cfg_path: &Path) -> Result<String, String> {
    let bg = bg_exe()?;
    let mut cmd = format!("\"{}\"", bg.display());
    if cfg_path != config::default_path_for(&bg) {
        cmd.push_str(&format!(" --config \"{}\"", cfg_path.display()));
    }
    Ok(cmd)
}

/// Launch the background instance fully detached from this terminal: no
/// inherited pipes (a caller capturing our output would otherwise never see EOF).
fn spawn_background(cfg_path: &Path) -> Result<(), String> {
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    std::process::Command::new(bg_exe()?)
        .arg("--config")
        .arg(cfg_path)
        .arg("run")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not start background instance: {e}"))
}

enum Post {
    NotRunning,
    Posted,
    Blocked(String),
}

/// Post `msg` to the instance running in this session.
fn post_to_running(msg: u32) -> Post {
    match unsafe { FindWindowW(daemon::WINDOW_CLASS, None) } {
        Ok(hwnd) if !hwnd.is_invalid() => {
            match unsafe { PostMessageW(Some(hwnd), msg, WPARAM(0), LPARAM(0)) } {
                Ok(()) => Post::Posted,
                Err(e) => Post::Blocked(format!(
                    "instance found but the message was refused ({}); if it runs elevated, use an elevated prompt",
                    e.message()
                )),
            }
        }
        _ => Post::NotRunning,
    }
}

fn is_running() -> bool {
    matches!(unsafe { FindWindowW(daemon::WINDOW_CLASS, None) }, Ok(h) if !h.is_invalid())
}

fn stop_running() -> Result<&'static str, String> {
    match post_to_running(WM_CLOSE) {
        Post::Posted => Ok("running instance stopped"),
        Post::NotRunning => Ok("no running instance in this session"),
        Post::Blocked(e) => Err(e),
    }
}

fn cmd_run(args: &Args, gui: bool) -> Result<(), String> {
    log::init(true, !gui);
    let cfg = if args.config.exists() {
        Config::load(&args.config).inspect_err(|e| error!("invalid config: {e}"))?
    } else if is_remote_session() {
        // The RDP window size is not a meaningful console preset.
        let c = Config::default();
        warn!(
            "no config found in an RDP session; using {}x{} without saving (run `set <W> <H>`)",
            c.width, c.height
        );
        c
    } else {
        // First start without a config: adopt the current resolution as the preset.
        let mut c = Config::default();
        if let Ok(m) = display::resolve("").and_then(|d| display::current_mode(&d)) {
            c.width = m.width;
            c.height = m.height;
        }
        match c.save(&args.config) {
            Ok(()) => warn!(
                "no config found; created {} with preset {}x{}",
                args.config.display(),
                c.width,
                c.height
            ),
            Err(e) => warn!(
                "no config found and could not create one ({e}); using {}x{}",
                c.width, c.height
            ),
        }
        c
    };
    daemon::run(cfg).inspect_err(|e| error!("{e}"))
}

fn cmd_set(args: &Args) -> Result<(), String> {
    check_options(&args.rest, &["--refresh", "--bpp", "--device"])?;
    let nums: Vec<&String> = args
        .rest
        .iter()
        .take_while(|a| !a.starts_with("--"))
        .collect();
    let [w, h] = nums.as_slice() else {
        return Err(
            "usage: set <WIDTH> <HEIGHT> [--refresh Hz] [--bpp bits] [--device name]".into(),
        );
    };
    let mut c = load_or_default(&args.config)?;
    c.width = w.parse().map_err(|_| format!("bad width '{w}'"))?;
    c.height = h.parse().map_err(|_| format!("bad height '{h}'"))?;
    if let Some(v) = flag_value(&args.rest, "--refresh")? {
        c.refresh = v.parse().map_err(|_| format!("bad refresh '{v}'"))?;
    }
    if let Some(v) = flag_value(&args.rest, "--bpp")? {
        c.bpp = v.parse().map_err(|_| format!("bad bpp '{v}'"))?;
    }
    if let Some(v) = flag_value(&args.rest, "--device")? {
        c.device = v;
    }
    c.validate()?;
    let t = target_of(&c);
    match display::resolve(&c.device) {
        Ok(dev) if !display::list_modes(&dev).iter().any(|m| t.matches(m)) => {
            println!(
                "warning: {t} is not in the mode list of {dev}; the driver may reject it (see list-modes)"
            )
        }
        Ok(_) => {}
        Err(e) => println!("warning: could not check the mode list ({e})"),
    }
    c.save(&args.config)?;
    println!("preset {t} saved to {}", args.config.display());
    if is_running() {
        println!("the running instance picks it up after a restart: run `stop`, then `start`");
    }
    Ok(())
}

fn cmd_set_current(args: &Args) -> Result<(), String> {
    if is_remote_session() {
        println!(
            "warning: this is an RDP session; its size is the RDP window, not the VMware console mode"
        );
    }
    let mut c = load_or_default(&args.config)?;
    let dev = display::resolve(&c.device)?;
    let m = display::current_mode(&dev)?;
    c.width = m.width;
    c.height = m.height;
    c.save(&args.config)?;
    println!(
        "preset {}x{} (current mode of {dev}: {m}) saved to {}",
        m.width,
        m.height,
        args.config.display()
    );
    Ok(())
}

fn cmd_apply(args: &Args) -> Result<(), String> {
    let c = Config::load(&args.config)?;
    let dev = display::resolve(&c.device)?;
    let cur = display::current_mode(&dev)?;
    let t = target_of(&c);
    if t.matches(&cur) {
        println!("{dev} already at {cur}");
        return Ok(());
    }
    display::apply(&dev, &t, c.persist).map_err(|e| e.to_string())?;
    println!("{dev}: {cur} -> {t}");
    Ok(())
}

fn cmd_status(args: &Args) -> Result<(), String> {
    println!("config file : {}", args.config.display());
    match Config::load(&args.config) {
        Ok(c) => println!("preset      : {}", target_of(&c)),
        Err(e) => println!("preset      : <none> ({e})"),
    }
    let devices: Vec<_> = display::devices()
        .into_iter()
        .filter(|d| d.attached)
        .collect();
    if devices.is_empty() {
        println!("display     : none visible (non-interactive session such as SSH?)");
    }
    for d in devices {
        let cur = display::current_mode(&d.device)
            .map(|m| m.to_string())
            .unwrap_or_else(|e| e);
        println!(
            "display     : {}{} [{}] {cur}",
            d.device,
            if d.primary { " (primary)" } else { "" },
            d.description
        );
    }
    println!(
        "running     : {}",
        if is_running() {
            "yes (this session)"
        } else {
            "no (in this session)"
        }
    );
    println!(
        "autostart   : HKCU {} / HKLM {}",
        autostart::query(false).as_deref().unwrap_or("-"),
        autostart::query(true).as_deref().unwrap_or("-")
    );
    if let Some(p) = log::log_path() {
        println!("log file    : {}", p.display());
    }
    Ok(())
}

fn cmd_list_modes(args: &Args) -> Result<(), String> {
    let c = load_or_default(&args.config)?;
    let dev = display::resolve(&c.device)?;
    println!("modes of {dev}:");
    for m in display::list_modes(&dev) {
        println!("  {m}");
    }
    Ok(())
}

fn cmd_start(args: &Args) -> Result<(), String> {
    Config::load(&args.config).map_err(|e| {
        format!(
            "config {} is not usable: {e} (run `set <W> <H>` first)",
            args.config.display()
        )
    })?;
    if is_running() {
        println!("already running in this session");
        return Ok(());
    }
    spawn_background(&args.config)?;
    println!(
        "background instance started; log: {}",
        log::log_path()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    );
    Ok(())
}

fn cmd_install(args: &Args) -> Result<(), String> {
    let all = has_flag(&args.rest, "--all-users");
    if args.config.exists() {
        Config::load(&args.config)
            .map_err(|e| format!("config {} is invalid: {e}", args.config.display()))?;
    } else if is_remote_session() {
        return Err(
            "no config yet and this is an RDP session: run `set <W> <H>` first \
                    (the RDP window size is not a console mode)"
                .into(),
        );
    } else {
        cmd_set_current(args)?;
    }
    println!(
        "{}",
        autostart::install(all, &bg_command_line(&args.config)?)?
    );
    if has_flag(&args.rest, "--no-start") {
        println!("not started now (--no-start); it starts at the next logon");
        return Ok(());
    }
    cmd_start(args)
}

fn cmd_uninstall(args: &Args) -> Result<(), String> {
    println!(
        "{}",
        autostart::uninstall(has_flag(&args.rest, "--all-users"))?
    );
    println!("{}", stop_running()?);
    Ok(())
}

/// Entry point for both executables. `gui` is true for the windows-subsystem
/// background exe, which defaults to `run` and has no console of its own.
pub fn main(gui: bool) -> ExitCode {
    unsafe {
        // Physical pixels for window metrics; the GDI mode APIs are not DPI-virtualised anyway.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let parsed = parse_args();
    if gui && !matches!(&parsed, Ok(a) if a.command == "run") {
        // Best effort: the background exe was given a CLI command.
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS).ok() };
    }
    let args = match parsed {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let options_ok = match args.command.as_str() {
        "install" => check_options(&args.rest, &["--all-users", "--no-start"]),
        "uninstall" => check_options(&args.rest, &["--all-users"]),
        "set" => Ok(()), // checked in cmd_set
        _ => check_options(&args.rest, &[]),
    };
    let result = options_ok.and_then(|()| match args.command.as_str() {
        "run" => cmd_run(&args, gui),
        "start" => cmd_start(&args),
        "set" => cmd_set(&args),
        "set-current" => cmd_set_current(&args),
        "apply" => cmd_apply(&args),
        "status" => cmd_status(&args),
        "list-modes" => cmd_list_modes(&args),
        "install" => cmd_install(&args),
        "uninstall" => cmd_uninstall(&args),
        "pause" | "resume" => {
            let msg = if args.command == "pause" {
                daemon::MSG_PAUSE
            } else {
                daemon::MSG_RESUME
            };
            match post_to_running(msg) {
                Post::Posted => {
                    println!("{} requested", args.command);
                    Ok(())
                }
                Post::NotRunning => Err("no running instance in this session".into()),
                Post::Blocked(e) => Err(e),
            }
        }
        "stop" => stop_running().map(|m| println!("{m}")),
        "help" => {
            println!("{HELP}");
            Ok(())
        }
        other => Err(format!("unknown command '{other}' (try: help)")),
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if gui && args.command == "run" {
                error!("{e}");
            }
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
