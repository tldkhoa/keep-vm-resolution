#!/usr/bin/env bash
# Deploy keep-vm-resolution to Windows VMs over OpenSSH (key auth) and start it in
# the logged-on user's console session.
#
#   deploy/deploy.sh [-u admin] [-r 1920x1080] [-d C:\Tools\KeepVMResolution] host1 [host2 ...]
#
# Per host: stop any running instance, copy both exes, write the preset, register
# autostart (HKCU Run of the SSH user), start the background exe in the
# interactive session through a one-off scheduled task (/IT), then verify.
set -u

USER_NAME=admin
RES=1920x1080
DEST='C:\Tools\KeepVMResolution'
while getopts "u:r:d:" o; do
  case $o in
    u) USER_NAME=$OPTARG ;;
    r) RES=$OPTARG ;;
    d) DEST=$OPTARG ;;
    *) exit 2 ;;
  esac
done
shift $((OPTIND - 1))
[ $# -gt 0 ] || { echo "usage: $0 [-u user] [-r WxH] [-d dir] host..." >&2; exit 2; }
W=${RES%x*}; H=${RES#*x}

HERE=$(cd "$(dirname "$0")/.." && pwd)
CLI="$HERE/target/release/keep-vm-resolution.exe"
BG="$HERE/target/release/keep-vm-resolution-bg.exe"
[ -f "$CLI" ] && [ -f "$BG" ] || { echo "build first: cargo build --release" >&2; exit 1; }

SSH_BIN=ssh; SCP_BIN=scp
if [ -x /c/Windows/System32/OpenSSH/ssh.exe ]; then
  SSH_BIN=/c/Windows/System32/OpenSSH/ssh.exe; SCP_BIN=/c/Windows/System32/OpenSSH/scp.exe
fi
OPTS=(-o BatchMode=yes -o ConnectTimeout=10 -o StrictHostKeyChecking=accept-new)
DEST_FWD=${DEST//\\//}

failed=()
for host in "$@"; do
  echo "=== $host"
  t="$USER_NAME@$host"
  r() { "$SSH_BIN" "${OPTS[@]}" "$t" "$@"; }

  if ! r "if not exist \"$DEST\" mkdir \"$DEST\"" ; then failed+=("$host (ssh)"); continue; fi
  # Stop a running instance (old single-exe builds too) so the files can be replaced.
  r "\"$DEST\\keep-vm-resolution.exe\" stop >nul 2>&1 & taskkill /F /IM keep-vm-resolution-bg.exe >nul 2>&1 & taskkill /F /IM keep-vm-resolution.exe >nul 2>&1 & schtasks /Delete /F /TN KVR-Daemon >nul 2>&1 & schtasks /Delete /F /TN KVR-Resize >nul 2>&1 & exit 0"
  if ! "$SCP_BIN" "${OPTS[@]}" -q "$CLI" "$BG" "$t:$DEST_FWD/"; then failed+=("$host (copy)"); continue; fi

  r "cd /d \"$DEST\" && keep-vm-resolution.exe set $W $H && keep-vm-resolution.exe install --no-start" || { failed+=("$host (install)"); continue; }

  # Start in the interactive console session of $USER_NAME (SSH runs in a separate, non-interactive session).
  r "schtasks /Create /F /TN KVR-Start /SC ONCE /ST 23:59 /RU $USER_NAME /IT /TR \"\\\"$DEST\\keep-vm-resolution-bg.exe\\\"\" >nul && schtasks /Run /TN KVR-Start >nul && ping -n 4 127.0.0.1 >nul & schtasks /Delete /F /TN KVR-Start >nul"

  out=$(r "powershell -NoProfile -Command \"Get-Process keep-vm-resolution-bg -EA SilentlyContinue | ForEach-Object { 'running pid ' + \$_.Id + ' session ' + \$_.SessionId }; Get-Content \$env:LOCALAPPDATA\\KeepVMResolution\\keep-vm-resolution.log -Tail 3\"")
  echo "$out"
  echo "$out" | grep -q "running pid" || failed+=("$host (not running)")
done

if [ ${#failed[@]} -gt 0 ]; then
  echo; echo "FAILED: ${failed[*]}"; exit 1
fi
echo; echo "all hosts deployed"
