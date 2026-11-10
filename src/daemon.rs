//! Background loop: a hidden top-level window receives WM_DISPLAYCHANGE broadcasts
//! and session notifications; after a short settle delay the preset is restored.
//!
//! The guest cannot tell *who* asked for a resize (web console, VMRC, API): VMware
//! Tools receives them all as the same RPC. So we only watch the result - the
//! Windows display mode - and treat every event purely as a trigger to re-check.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetMessageW, GetSystemMetrics, KillTimer, MSG, MSGFLT_ALLOW, PostQuitMessage, RegisterClassW,
    SM_REMOTESESSION, SetTimer, TranslateMessage, WM_APP, WM_CLOSE, WM_DESTROY, WM_DISPLAYCHANGE,
    WM_TIMER, WM_WTSSESSION_CHANGE, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP, WTS_CONSOLE_CONNECT,
    WTS_REMOTE_CONNECT, WTS_SESSION_LOCK, WTS_SESSION_LOGON, WTS_SESSION_UNLOCK,
};
use windows::core::{PCWSTR, w};

use crate::config::Config;
use crate::display::{self, Device, Mode, Target};

/// Window class used to find a running instance (for `stop`, `pause`, `resume`).
pub const WINDOW_CLASS: PCWSTR = w!("KeepVMResolution.Hidden");
const MUTEX_NAME: PCWSTR = w!("Local\\KeepVMResolution.SingleInstance");

/// Messages other instances post to the running one.
pub const MSG_PAUSE: u32 = WM_APP + 1;
pub const MSG_RESUME: u32 = WM_APP + 2;

/// WTS_SESSION_DESKTOP_READY (0xF) is not exported by the windows crate.
const WTS_SESSION_DESKTOP_READY: u32 = 0xF;

const TIMER_SETTLE: usize = 1;
const TIMER_POLL: usize = 2;
const TIMER_WTS_RETRY: usize = 3;
const WTS_RETRY_MS: u32 = 5_000;
const WTS_MAX_RETRIES: u32 = 24;
/// Upper bound for the exponential cooldown.
const MAX_COOLDOWN: Duration = Duration::from_secs(3600);

struct State {
    cfg: Config,
    target: Target,
    /// Times of recent restore attempts (for the rate limiter).
    restores: VecDeque<Instant>,
    /// Rate limiter tripped: do nothing until this instant.
    paused_until: Option<Instant>,
    /// How many times in a row the rate limiter tripped (exponential backoff).
    trips: u32,
    /// When the most recent cooldown ends (for resetting `trips`).
    pause_end: Option<Instant>,
    /// The driver rejected the preset while the display was in this mode;
    /// don't retry until the mode changes or a session event happens.
    rejected_in: Option<Mode>,
    /// Session is locked (secure desktop shown): don't touch the display.
    locked: bool,
    /// Paused by the user (`keep-vm-resolution pause`).
    user_paused: bool,
    wts_registered: bool,
    wts_retries: u32,
    last_skip_reason: Option<String>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

impl State {
    fn new(cfg: Config) -> State {
        let target = Target {
            width: cfg.width,
            height: cfg.height,
            bpp: cfg.bpp,
            refresh: cfg.refresh,
        };
        State {
            cfg,
            target,
            restores: VecDeque::new(),
            paused_until: None,
            trips: 0,
            pause_end: None,
            rejected_in: None,
            locked: false,
            user_paused: false,
            wts_registered: false,
            wts_retries: 0,
            last_skip_reason: None,
        }
    }

    /// Log a "skipping" reason only when it changes, so polling doesn't spam the log.
    fn skip(&mut self, reason: String) {
        if self.last_skip_reason.as_deref() != Some(reason.as_str()) {
            info!("{reason}");
            self.last_skip_reason = Some(reason);
        }
    }

    /// A fresh situation (logon, unlock, console connect, resume): forget
    /// rejections so the preset is tried again.
    fn reset_for_new_situation(&mut self) {
        self.rejected_in = None;
    }

    fn cooldown(&self) -> Duration {
        let base = Duration::from_secs(self.cfg.cooldown_secs.max(1).into());
        base.saturating_mul(1u32 << self.trips.saturating_sub(1).min(10))
            .min(MAX_COOLDOWN)
    }

    /// Decide what to do. Never calls ChangeDisplaySettingsExW itself: that call
    /// can synchronously re-enter wndproc (WM_DISPLAYCHANGE) while STATE is borrowed.
    fn check(&mut self) -> Decision {
        if self.user_paused {
            self.skip("paused by user".into());
            return Decision::Nothing;
        }
        if self.locked {
            self.skip("session locked; waiting for unlock".into());
            return Decision::Nothing;
        }
        if self.cfg.skip_remote && unsafe { GetSystemMetrics(SM_REMOTESESSION) } != 0 {
            self.skip("RDP session detected; not touching the resolution".into());
            return Decision::Nothing;
        }
        let dev = match display::resolve(&self.cfg.device) {
            Ok(d) => d,
            Err(e) => {
                self.skip(format!("cannot resolve display device: {e}"));
                return Decision::Nothing;
            }
        };
        let cur = match display::current_mode(&dev) {
            Ok(m) => m,
            Err(e) => {
                self.skip(format!("cannot read current mode: {e}"));
                return Decision::Nothing;
            }
        };

        let now = Instant::now();
        let window = Duration::from_secs(self.cfg.rate_window_secs.into());
        while self
            .restores
            .front()
            .is_some_and(|t| now.duration_since(*t) > window)
        {
            self.restores.pop_front();
        }

        // Backoff reset: a full rate window without restores after the last pause ended.
        if self.trips > 0
            && self.restores.is_empty()
            && self.pause_end.is_none_or(|e| now >= e + window)
        {
            self.trips = 0;
            self.pause_end = None;
        }

        if self.target.matches(&cur) {
            self.last_skip_reason = None;
            self.rejected_in = None;
            if self.paused_until.is_some_and(|u| now >= u) {
                self.paused_until = None;
            }
            return Decision::Nothing;
        }
        if self.rejected_in == Some(cur) {
            return Decision::Nothing; // already logged when it was rejected
        }

        if let Some(until) = self.paused_until {
            if now < until {
                return Decision::Retry(until - now);
            }
            self.paused_until = None;
            info!("cooldown over, resuming");
        }
        if self.restores.len() >= self.cfg.max_restores as usize {
            self.trips += 1;
            let cooldown = self.cooldown();
            warn!(
                "{} restores within {}s - something keeps changing the resolution (VMware console autofit?). Pausing for {}s.",
                self.restores.len(),
                self.cfg.rate_window_secs,
                cooldown.as_secs()
            );
            // Start the next round with a fresh budget, so the first check after the
            // cooldown actually tries a restore instead of tripping again at once.
            self.restores.clear();
            self.paused_until = Some(now + cooldown);
            self.pause_end = Some(now + cooldown);
            return Decision::Retry(cooldown);
        }

        self.restores.push_back(now);
        self.last_skip_reason = None;
        Decision::Apply {
            dev,
            cur,
            target: self.target,
            persist: self.cfg.persist,
        }
    }
}

enum Decision {
    Nothing,
    /// Re-check after this delay (cooldown running).
    Retry(Duration),
    Apply {
        dev: Device,
        cur: Mode,
        target: Target,
        persist: bool,
    },
}

/// Run `f` on the daemon state. Returns None (instead of panicking) if the state
/// is already borrowed further up the stack by a re-entrant window message.
fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|cell| {
        cell.try_borrow_mut()
            .ok()
            .and_then(|mut s| s.as_mut().map(f))
    })
}

fn schedule_check(hwnd: HWND, delay_ms: u32) {
    // Re-arming the same timer id resets it, which debounces bursts of changes.
    unsafe { SetTimer(Some(hwnd), TIMER_SETTLE, delay_ms.max(10), None) };
}

fn run_check(hwnd: HWND, why: &str) {
    match with_state(|s| s.check()) {
        Some(Decision::Retry(delay)) => {
            schedule_check(
                hwnd,
                delay.as_millis().min(u32::MAX as u128 - 100) as u32 + 50,
            );
        }
        Some(Decision::Apply {
            dev,
            cur,
            target,
            persist,
        }) => {
            // STATE is not borrowed here, so re-entrant messages are safe.
            match display::apply(&dev, &target, persist) {
                Ok(()) => info!("[{why}] {dev} was {cur}; restored {target}"),
                Err(e) if e.rejected => {
                    error!(
                        "[{why}] {dev} is {cur}; {e}. Not retrying until the display mode changes."
                    );
                    with_state(|s| s.rejected_in = Some(cur));
                }
                Err(e) => error!("[{why}] {dev} is {cur}; restore failed: {e}"),
            }
        }
        _ => {}
    }
}

fn settle_ms() -> u32 {
    with_state(|s| s.cfg.settle_ms).unwrap_or(1500)
}

fn register_wts(hwnd: HWND) {
    match unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) } {
        Ok(()) => {
            unsafe { KillTimer(Some(hwnd), TIMER_WTS_RETRY).ok() };
            let retried = with_state(|s| {
                s.wts_registered = true;
                s.wts_retries
            })
            .unwrap_or(0);
            if retried > 0 {
                info!("session notifications registered after {retried} retries");
            }
        }
        Err(e) => {
            // Early at logon the terminal-services RPC endpoint may not be up yet
            // (RPC_S_INVALID_BINDING); retry for a while, polling covers the gap.
            let tries = with_state(|s| {
                s.wts_retries += 1;
                s.wts_retries
            })
            .unwrap_or(WTS_MAX_RETRIES);
            if tries == 1 {
                warn!(
                    "WTSRegisterSessionNotification failed ({e}); retrying every {}s",
                    WTS_RETRY_MS / 1000
                );
                unsafe { SetTimer(Some(hwnd), TIMER_WTS_RETRY, WTS_RETRY_MS, None) };
            } else if tries >= WTS_MAX_RETRIES {
                warn!(
                    "giving up on session notifications ({e}); relying on display-change events and polling"
                );
                unsafe { KillTimer(Some(hwnd), TIMER_WTS_RETRY).ok() };
            }
        }
    }
}

fn on_session_change(hwnd: HWND, event: u32) {
    let name = match event {
        WTS_CONSOLE_CONNECT => "console connect",
        WTS_REMOTE_CONNECT => "remote connect",
        WTS_SESSION_LOGON => "logon",
        WTS_SESSION_UNLOCK => "unlock",
        WTS_SESSION_LOCK => "lock",
        WTS_SESSION_DESKTOP_READY => "desktop ready",
        _ => return,
    };
    info!("session event: {name}");
    with_state(|s| {
        s.locked = event == WTS_SESSION_LOCK;
        s.reset_for_new_situation();
    });
    if event != WTS_SESSION_LOCK {
        schedule_check(hwnd, settle_ms());
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_DISPLAYCHANGE => {
            // lParam is only informational; the real mode is re-read when the timer fires.
            let (w, h) = (
                (lparam.0 & 0xFFFF) as u32,
                ((lparam.0 >> 16) & 0xFFFF) as u32,
            );
            info!("WM_DISPLAYCHANGE: {w}x{h} ({} bpp)", wparam.0);
            schedule_check(hwnd, settle_ms());
            LRESULT(0)
        }
        WM_WTSSESSION_CHANGE => {
            on_session_change(hwnd, wparam.0 as u32);
            LRESULT(0)
        }
        WM_TIMER => {
            match wparam.0 {
                TIMER_SETTLE => {
                    unsafe { KillTimer(Some(hwnd), TIMER_SETTLE).ok() };
                    run_check(hwnd, "event");
                }
                TIMER_POLL => run_check(hwnd, "poll"),
                TIMER_WTS_RETRY => register_wts(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        MSG_PAUSE => {
            info!("paused by user");
            with_state(|s| s.user_paused = true);
            LRESULT(1)
        }
        MSG_RESUME => {
            info!("resumed by user");
            with_state(|s| {
                s.user_paused = false;
                s.paused_until = None;
                s.trips = 0;
                s.pause_end = None;
                s.restores.clear();
                s.reset_for_new_situation();
            });
            schedule_check(hwnd, 10);
            LRESULT(1)
        }
        WM_CLOSE => {
            info!("close requested");
            unsafe { DestroyWindow(hwnd).ok() };
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe {
                if with_state(|s| s.wts_registered).unwrap_or(false) {
                    let _ = WTSUnRegisterSessionNotification(hwnd);
                }
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Returns None when another instance already runs in this session.
fn acquire_single_instance() -> Result<Option<HANDLE>, String> {
    let h = unsafe { CreateMutexW(None, false, MUTEX_NAME) }
        .map_err(|e| format!("CreateMutexW: {e}"))?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return Ok(None);
    }
    Ok(Some(h)) // held (never closed) for the life of the process
}

pub fn run(cfg: Config) -> Result<(), String> {
    let Some(_mutex) = acquire_single_instance()? else {
        info!("another instance is already running in this session; exiting");
        return Ok(());
    };

    info!(
        "starting (pid {}): preset {}, device '{}', settle {} ms, poll {} s, max {} restores/{}s, cooldown {}s",
        std::process::id(),
        Target {
            width: cfg.width,
            height: cfg.height,
            bpp: cfg.bpp,
            refresh: cfg.refresh
        },
        if cfg.device.is_empty() {
            "<primary>"
        } else {
            &cfg.device
        },
        cfg.settle_ms,
        cfg.poll_secs,
        cfg.max_restores,
        cfg.rate_window_secs,
        cfg.cooldown_secs
    );
    let poll_secs = cfg.poll_secs;
    let initial_delay = cfg.settle_ms;
    STATE.with_borrow_mut(|s| *s = Some(State::new(cfg)));

    unsafe {
        let hinstance = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: WINDOW_CLASS,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(format!("RegisterClassW failed: {:?}", GetLastError()));
        }
        // A real (never shown) top-level window: message-only windows do not
        // receive broadcast messages such as WM_DISPLAYCHANGE.
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            WINDOW_CLASS,
            w!("KeepVMResolution"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance.into()),
            None,
        )
        .map_err(|e| format!("CreateWindowExW: {e}"))?;

        // Let `stop` / `pause` / `resume` from a normal (medium-integrity) prompt
        // reach us even if this instance was started elevated (UIPI).
        for m in [WM_CLOSE, MSG_PAUSE, MSG_RESUME] {
            let _ = ChangeWindowMessageFilterEx(hwnd, m, MSGFLT_ALLOW, None);
        }

        register_wts(hwnd);
        if poll_secs > 0 {
            SetTimer(Some(hwnd), TIMER_POLL, poll_secs.saturating_mul(1000), None);
        }
        // Initial check shortly after start (e.g. right after logon).
        schedule_check(hwnd, initial_delay);

        let mut msg = MSG::default();
        loop {
            let r = GetMessageW(&mut msg, None, 0, 0);
            if r.0 == 0 || r.0 == -1 {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    info!("stopped");
    Ok(())
}
