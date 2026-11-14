#!/usr/bin/env bash
# Start throwaway SSH servers for the live end-to-end tests
# (src-tauri/src/live_tests.rs) and print the environment that runs them.
#
#   sudo scripts/live-ssh-servers.sh /tmp/ishell-live > live.env
#   set -a; . ./live.env; set +a
#   cd src-tauri && xvfb-run -a cargo test live_ -- --nocapture
#
# It creates two local accounts (ishell-live, and ishell-sftp restricted to
# SFTP) and two sshd instances on 127.0.0.1: port 2222 with SFTP and port 2223
# without it. Meant for CI runners and disposable machines only: the tests
# record the servers' host keys in the running user's ~/.ssh/known_hosts, and
# a later run with new host keys is refused until those entries are removed.
set -euo pipefail

dir=${1:?usage: live-ssh-servers.sh <work-dir>}
password=ishell-live-$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')
mkdir -p "$dir" /run/sshd
chmod 755 "$dir"
sshd_bin=$(command -v sshd || echo /usr/sbin/sshd)

for user in ishell-live ishell-sftp; do
  id "$user" >/dev/null 2>&1 || useradd -m -s /bin/bash "$user"
  echo "$user:$password" | chpasswd
done

[ -f "$dir/host_key" ] || ssh-keygen -q -t ed25519 -N '' -f "$dir/host_key"
[ -f "$dir/client_key" ] || ssh-keygen -q -t ed25519 -N '' -f "$dir/client_key"
# The tests run as the invoking user, who must be able to read the key.
chown "${SUDO_USER:-$(id -un)}" "$dir/client_key" "$dir/client_key.pub"
home=$(getent passwd ishell-live | cut -d: -f6)
install -d -m 700 -o ishell-live -g ishell-live "$home/.ssh"
install -m 600 -o ishell-live -g ishell-live "$dir/client_key.pub" "$home/.ssh/authorized_keys"

common="ListenAddress 127.0.0.1
HostKey $dir/host_key
PasswordAuthentication yes
KbdInteractiveAuthentication no
PubkeyAuthentication yes
UsePAM no
PermitRootLogin no
AllowTcpForwarding yes
MaxSessions 64
MaxStartups 64:30:128"

cat > "$dir/sshd_sftp" <<CONF
Port 2222
PidFile $dir/sshd_sftp.pid
$common
Subsystem sftp internal-sftp
Match User ishell-sftp
    ForceCommand internal-sftp
CONF

cat > "$dir/sshd_plain" <<CONF
Port 2223
PidFile $dir/sshd_plain.pid
$common
CONF

for name in sftp plain; do
  [ -f "$dir/sshd_$name.pid" ] && kill "$(cat "$dir/sshd_$name.pid")" 2>/dev/null || true
done
sleep 0.3
"$sshd_bin" -f "$dir/sshd_sftp" -E "$dir/sshd_sftp.log"
"$sshd_bin" -f "$dir/sshd_plain" -E "$dir/sshd_plain.log"

cat <<ENV
ISHELL_LIVE_SSH=127.0.0.1:2222
ISHELL_LIVE_NO_SFTP_PORT=2223
ISHELL_LIVE_USER=ishell-live
ISHELL_LIVE_SFTP_ONLY_USER=ishell-sftp
ISHELL_LIVE_PASSWORD=$password
ISHELL_LIVE_KEY=$dir/client_key
ENV
