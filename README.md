# keep-vm-resolution

A small Windows background program (Rust, no runtime dependencies) for **VMware Windows guests**.
When someone opens the vSphere **web console** (or VMRC), VMware Tools resizes the guest
display to fit the console window ("autofit"). This program notices the change and switches
the display back to a **preset resolution**.

## First: the no-code fix (if you can edit the VM)

VMware/Broadcom supports turning autofit off in the VM configuration. If you can edit the VM,
do this first. You may not need the program at all.

Power off the VM, then in vCenter go to **Edit Settings → VM Options → Advanced → Edit Configuration Parameters**
and add the following (needs VMware Tools 10.2 or later; see Broadcom KB 311172):

```
guestInfo.svga.wddm.modeset             = "FALSE"
guestInfo.svga.wddm.modesetCCD          = "FALSE"
guestInfo.svga.wddm.modesetLegacySingle = "FALSE"
guestInfo.svga.wddm.modesetLegacyMulti  = "FALSE"
```

To also pin the boot resolution, add these (needs Tools 11.1.0 or later; see KB 331544):

```
guestInfo.svga.wddm.bootTopology               = "1920x1080"
guestInfo.svga.wddm.restrictModesToBootTopology = "TRUE"
```

> Windows 11 VMs encrypted for vTPM ignore these keys on older ESXi builds. The fix is in
> ESXi 7.0U3o / 8.0U1c and later (KB 423780). Use this program when you cannot change the VM
> configuration or the keys have no effect.

## How the program works

* It runs as a normal, invisible per-user process in the interactive session. A Windows
  service cannot do this job: it runs in session 0 and cannot change the logged-on user's display.
* A hidden top-level window receives `WM_DISPLAYCHANGE` and session notifications (logon,
  unlock, console connect). A slow poll (every 15 s by default) is the safety net.
* After a change it waits for things to settle (1.5 s debounce, because the console sends
  bursts of resizes). It then reads the **actual** current mode. If that differs from the
  preset, it validates the preset with `CDS_TEST` and applies it with `ChangeDisplaySettingsExW`.
* **Anti fight-loop.** If the resolution keeps getting changed back (for example the browser
  console re-asserts its size), it allows at most 5 restores per 60 s. After that it pauses,
  with exponential backoff: 120 s, 240 s, … up to 1 h. If the driver rejects the preset, it
  does not retry until the display mode changes.
* It does nothing in RDP sessions (RDP owns the resolution) or while the session is locked.

## Build

```powershell
cargo build --release
# target\release\keep-vm-resolution.exe     console CLI (the one you type commands into)
# target\release\keep-vm-resolution-bg.exe  windowless background instance (started at logon)
```

Both exes share `keep-vm-resolution.ini` in their folder. Copy both into the VM.

## Deploy to several VMs over SSH

PowerShell:

```powershell
.\deploy\deploy.ps1 192.0.2.11, 192.0.2.12, 192.0.2.13
.\deploy\deploy.ps1 -Hosts 192.0.2.20 -User admin -Resolution 1600x900 -Dest C:\Tools\KeepVMResolution
```

Bash (Git Bash / WSL):

```bash
deploy/deploy.sh -u admin -r 1920x1080 192.0.2.11 192.0.2.12 ...
```

For each host either script:

1. Stops any old instance.
2. Copies both exes to `C:\Tools\KeepVMResolution`.
3. Writes the preset.
4. Registers autostart in the HKCU Run key of the SSH user.
5. Starts the background exe in the user's **console session** with a one-off `schtasks /IT`
   task. This step is needed because SSH runs in its own non-interactive session, where the
   display APIs see no display.
6. Checks that the background exe is running.

It needs OpenSSH key authentication, and the user must be logged on to the console.

## Install by hand in the VM

Copy both exes to a folder the user can write to (for example
`C:\Tools\KeepVMResolution\`), then run these in a normal (non-admin) terminal **while the
display is at the resolution you want**:

```powershell
.\keep-vm-resolution.exe set 1920 1080      # or: set-current
.\keep-vm-resolution.exe install            # autostart at logon (HKCU Run) + start now
.\keep-vm-resolution.exe status
```

`install --all-users` writes to HKLM instead, so it starts for every user who logs on. It
needs an elevated prompt. Put both exes and the `.ini` in a folder all users can read.

## Commands

| Command | What it does |
|---|---|
| `start` | Start the invisible background instance (`keep-vm-resolution-bg.exe`). |
| `run` | Run in the foreground of this terminal with the log printed (debugging). |
| `set <W> <H> [--refresh Hz] [--bpp bits] [--device \\.\DISPLAYn]` | Save the preset and warn if the driver does not list that mode. |
| `set-current` | Save the current resolution as the preset. |
| `apply` | Restore the preset once, now. |
| `status` | Show the preset, displays, running state, autostart entries and log path. |
| `list-modes` | List the modes the display driver offers. |
| `pause` / `resume` | Suspend or resume enforcement (for example while you change the resolution on purpose). |
| `stop` | Stop the instance running in this session. |
| `install [--all-users] [--no-start]` / `uninstall [--all-users]` | Add or remove the autostart entry. `install` also starts the program now unless you pass `--no-start`; `uninstall` also stops it. |

Global option: `--config <file>` (default: `keep-vm-resolution.ini` next to the exe).
A running instance reads the config only at start, so after `set`, run `stop` and then `start`.
`stop`, `pause`, `resume` and `status` only see an instance in the *same* Windows session.

## Configuration (`keep-vm-resolution.ini`)

```ini
width = 1920
height = 1080
bpp = 0              # 0 = don't enforce
refresh = 0          # 0 = don't enforce
device =             # empty = primary display, or \\.\DISPLAY1
settle_ms = 1500     # debounce after the last change
poll_secs = 15       # fallback check, 0 = off
max_restores = 5     # anti fight-loop: max restores ...
rate_window_secs = 60    # ... per this window
cooldown_secs = 120  # first pause length (doubles each time, max 1 h)
persist = false      # true = CDS_UPDATEREGISTRY (store mode in the user profile)
skip_remote = true   # do nothing in RDP sessions
```

If the program starts without a config file, it creates one from the current resolution.
That may already be the resolution VMware set, so set the preset explicitly with `set`.

## Logs

`%LOCALAPPDATA%\KeepVMResolution\keep-vm-resolution.log` (rotates at 1 MB, one backup kept). Example:

```
[INFO] WM_DISPLAYCHANGE: 1536x730 (32 bpp)
[INFO] [event] \\.\DISPLAY1 was 1536x730 32bpp @60Hz; restored 1920x1080
```

## Known limits

* Nothing runs before a user logs on, so the resolution of the sign-in screen is not enforced.
* The web console keeps showing the guest at the preset size (scaled or with scrollbars);
  that is expected.
* If the console keeps re-sending its size after every restore, the rate limiter will pause
  the program. Check the log for `Pausing for`. In that case, use the VMX keys above.
* Only the configured display (by default the primary one) is enforced. Extra monitors that
  the console adds are left alone.

## Test checklist (in a real vSphere VM)

1. Open the web console. You should see one `restored` line in the log and the guest goes
   back to the preset.
2. Resize the browser window while the console is open. Reverts should be debounced, and
   after 5 per minute you should see a `Pausing for` warning.
3. Lock and unlock, log off and on, reboot. The program should start and enforce after logon.
4. Connect over RDP. You should see `RDP session detected`, and the program does nothing.
5. For deeper tracing, turn on VMware Tools logging (`tools.conf`: `vmusr`, `vmsvc` and
   `vmresset` at debug level) and look for `Resolution_Set` / `DisplayTopology_Set`.
