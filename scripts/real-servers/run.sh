#!/bin/sh
# Starts the servers of setup.sh in a container (Podman or Docker) with their ports on this
# machine, and copies what the tests need into OUT. Then:
#   SHARDLOAD_REAL_SERVERS=OUT cargo test -p shardload-engine --test real_servers
# Stop it with: podman rm -f shardload-real-servers
set -eu
OUT=${1:?usage: run.sh OUT}
ENGINE=${ENGINE:-podman}
DIR=$(dirname "$0")
"$ENGINE" build -q -t shardload-real-servers "$DIR" >/dev/null
"$ENGINE" rm -f shardload-real-servers >/dev/null 2>&1 || true
"$ENGINE" run -d --name shardload-real-servers \
    -p 9921:9921 -p 9990:9990 -p 9922:9922 -p 30000-30009:30000-30009 \
    shardload-real-servers >/dev/null
for _ in $(seq 60); do
    "$ENGINE" exec shardload-real-servers test -f /out/ready 2>/dev/null && break
    sleep 1
done
mkdir -p "$OUT"
for f in file.sha256 id_plain id_locked; do
    "$ENGINE" cp "shardload-real-servers:/out/$f" "$OUT/$f"
done
echo "servers up; SHARDLOAD_REAL_SERVERS=$OUT"
