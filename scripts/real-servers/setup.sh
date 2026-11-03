#!/bin/sh
# Starts real servers for the tests in crates/engine/tests/real_servers.rs, as root, on a
# Debian or Ubuntu machine with vsftpd and openssh-server installed (CI runs it on the runner;
# elsewhere run.sh runs it in a container):
#   vsftpd, FTPS with implicit TLS on 9990 and explicit TLS (AUTH TLS) on 9921,
#   sshd on 9922, with a key without a passphrase and one with,
# for the user "shard" (password "shard-pass") who has pub/file.bin.
# Writes what the tests need into OUT: the file's SHA-256 and the two private keys.
set -eu
OUT=${1:?usage: setup.sh OUT}
mkdir -p "$OUT"
rm -f "$OUT/ready"
id shard >/dev/null 2>&1 || useradd -m -s /bin/sh shard
echo 'shard:shard-pass' | chpasswd
mkdir -p /home/shard/pub /home/shard/.ssh
# 13 MiB: four parts of at least 4 MiB less one, and not a multiple of a block.
head -c 13631489 /dev/urandom > /home/shard/pub/file.bin
sha256sum /home/shard/pub/file.bin | cut -d' ' -f1 > "$OUT/file.sha256"
rm -f "$OUT/id_plain" "$OUT/id_plain.pub" "$OUT/id_locked" "$OUT/id_locked.pub"
ssh-keygen -q -t ed25519 -N '' -C plain -f "$OUT/id_plain"
ssh-keygen -q -t ed25519 -N 'key-pass' -C locked -f "$OUT/id_locked"
cat "$OUT/id_plain.pub" "$OUT/id_locked.pub" > /home/shard/.ssh/authorized_keys
chown -R shard:shard /home/shard
chmod 700 /home/shard/.ssh
chmod 600 /home/shard/.ssh/authorized_keys
chmod 644 "$OUT"/*

mkdir -p /etc/real-servers /var/run/vsftpd/empty /run/sshd
openssl req -x509 -newkey rsa:2048 -nodes -days 30 -subj /CN=localhost \
    -keyout /etc/real-servers/key.pem -out /etc/real-servers/cert.pem 2>/dev/null

ftps() { # port, implicit (YES/NO)
    cat > "/etc/real-servers/vsftpd-$1.conf" <<CONF
listen=YES
listen_port=$1
anonymous_enable=NO
local_enable=YES
write_enable=NO
pam_service_name=vsftpd
secure_chroot_dir=/var/run/vsftpd/empty
seccomp_sandbox=NO
pasv_enable=YES
pasv_address=127.0.0.1
pasv_min_port=30000
pasv_max_port=30009
ssl_enable=YES
implicit_ssl=$2
force_local_logins_ssl=YES
force_local_data_ssl=YES
rsa_cert_file=/etc/real-servers/cert.pem
rsa_private_key_file=/etc/real-servers/key.pem
background=YES
CONF
    vsftpd "/etc/real-servers/vsftpd-$1.conf"
}
ftps 9990 YES
ftps 9921 NO

ssh-keygen -A >/dev/null
cat > /etc/real-servers/sshd_config <<CONF
Port 9922
HostKey /etc/ssh/ssh_host_ed25519_key
PasswordAuthentication yes
KbdInteractiveAuthentication no
PubkeyAuthentication yes
UsePAM yes
Subsystem sftp internal-sftp
PidFile /run/sshd-real-servers.pid
CONF
/usr/sbin/sshd -f /etc/real-servers/sshd_config
touch "$OUT/ready"
echo "real servers up: FTPS 9990 (implicit), 9921 (explicit), SSH 9922; keys in $OUT"
