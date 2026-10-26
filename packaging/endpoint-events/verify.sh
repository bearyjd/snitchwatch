#!/usr/bin/env bash
# Prove the endpoint-events recipe against real rsyslog. Run with
# `just endpoint-events-verify` (needs rootless podman and access to the
# Fedora mirrors; deliberately not part of package-check).
#
# Inside the digest-pinned Fedora 44 image from packaging/release/pins.env it:
#   1. installs the README's packages and checks the drop-in with
#      `rsyslogd -N1`, both under Fedora's stock /etc/rsyslog.conf and under
#      a minimal main config;
#   2. starts the drop-in against a local TLS receiver (a second rsyslog),
#      sends daemon-shaped JSON whose process_env/process_args carry a fake
#      token, plus a blank frame, a non-JSON line and a task notification,
#      and asserts the receiver got exactly one event, holding only the
#      allowlisted fields, and no token anywhere;
#   3. points the drop-in at a receiver whose certificate comes from another
#      CA and asserts it gets nothing (the CA pin is enforced);
#   4. runs the README's own merge and way-back snippets against a copy of the
#      packaged daemon config, with sudo and systemctl stubbed.
#
# Everything runs in the container's own network namespace and filesystem:
# no sudo, no host /etc, no live daemon. The container's 127.0.0.1 is not the
# host's.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

if [[ "${1:-}" != "--inside" ]]; then
    repo_root="$(cd "$here/../.." && pwd -P)"
    # shellcheck source=/dev/null # pins.env: plain KEY=value lines (SW_*)
    source "$repo_root/packaging/release/pins.env"
    exec podman run --rm \
        -v "$repo_root/packaging:/src/packaging:ro,Z" \
        "$SW_BUILDER_IMAGE" bash /src/packaging/endpoint-events/verify.sh --inside
fi

# ---- inside the container ------------------------------------------------
fail() {
    echo "FAIL: $*" >&2
    exit 1
}
# It writes /etc and installs packages: never outside the container.
if [[ ! -e /run/.containerenv && ! -e /.dockerenv ]]; then
    fail "--inside runs only in the pinned container; run verify.sh without arguments"
fi
src=/src/packaging/endpoint-events
work=$(mktemp -d)
token="FAKE-TOKEN-$RANDOM$RANDOM"
peer=vigie.test
host_id=verify-host

echo "== install"
dnf -q -y install --setopt=install_weak_deps=False \
    rsyslog rsyslog-mmjsonparse rsyslog-gnutls openssl jq >/dev/null
rpm -q rsyslog rsyslog-mmjsonparse rsyslog-gnutls
rsyslogd_path=$(command -v rsyslogd)
echo "rsyslogd: $rsyslogd_path -> $(readlink -f "$rsyslogd_path")"

echo "== certificates"
cd "$work"
make_ca() { # name
    openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj "/CN=$1" \
        -keyout "$1.key" -out "$1.pem" 2>/dev/null
}
make_server() { # ca name
    openssl req -newkey rsa:2048 -nodes -subj "/CN=$peer" \
        -addext "subjectAltName=DNS:$peer" -keyout "$2.key" -out "$2.csr" 2>/dev/null
    openssl x509 -req -in "$2.csr" -CA "$1.pem" -CAkey "$1.key" -CAcreateserial \
        -days 2 -copy_extensions copy -out "$2.pem" 2>/dev/null
}
make_ca pinned-ca
make_ca rogue-ca
make_server pinned-ca server
make_server rogue-ca rogue
mkdir -p /etc/pki/snitchwatch-endpoint-events
cp pinned-ca.pem /etc/pki/snitchwatch-endpoint-events/vigie-ca.pem

# The drop-in with its placeholders filled the way the README says.
drop_in() { # port -> path
    sed -e "s/@VIGIE_ADDRESS@/127.0.0.1/" -e "s/@VIGIE_PORT@/$1/" \
        -e "s/@VIGIE_CERT_NAME@/$peer/" -e "s/@HOST_ID@/$host_id/" \
        "$src/snitchwatch-endpoint-events.conf" >"$work/drop-in-$1.conf"
    if grep -q '@[A-Z_]*@' "$work/drop-in-$1.conf"; then
        fail "unfilled placeholder in drop-in"
    fi
    echo "$work/drop-in-$1.conf"
}

echo "== rsyslogd -N1, stock Fedora main config"
cp "$(drop_in 6514)" /etc/rsyslog.d/snitchwatch-endpoint-events.conf
rsyslogd -N1 -f /etc/rsyslog.conf 2>&1 | tee n1-stock.log
grep -qi 'error' n1-stock.log && fail "stock config check reported errors"
rm /etc/rsyslog.d/snitchwatch-endpoint-events.conf

# A minimal main config: the drop-in plus only what a test run needs.
sender_conf() { # port
    local dir="$work/sender-$1"
    mkdir -p "$dir"
    printf 'global(workDirectory="%s")\ninclude(file="%s")\n' \
        "$dir" "$(drop_in "$1")" >"$dir.conf"
    echo "$dir.conf"
}
echo "== rsyslogd -N1, minimal main config"
rsyslogd -N1 -f "$(sender_conf 6514)" 2>&1 | tee n1-min.log
grep -qi 'error' n1-min.log && fail "minimal config check reported errors"

# A TLS receiver writing each frame's raw payload, one per line.
receiver() { # name port
    mkdir -p "$work/$1"
    cat >"$work/$1.conf" <<EOF
global(workDirectory="$work/$1"
       DefaultNetstreamDriver="gtls"
       DefaultNetstreamDriverCAFile="$work/pinned-ca.pem"
       DefaultNetstreamDriverCertFile="$work/$1.pem"
       DefaultNetstreamDriverKeyFile="$work/$1.key")
module(load="imtcp" StreamDriver.Name="gtls" StreamDriver.Mode="1"
       StreamDriver.AuthMode="anon")
template(name="raw" type="string" string="%rawmsg%\n")
input(type="imtcp" address="127.0.0.1" port="$2" ruleset="r")
ruleset(name="r") { action(type="omfile" file="$work/$1.out" template="raw") }
EOF
    : >"$work/$1.out"
    rsyslogd -f "$work/$1.conf" -i "$work/$1.pid"
}
start_sender() { # port
    rsyslogd -f "$(sender_conf "$1")" -i "$work/sender.pid"
    for _ in $(seq 50); do
        (exec 3<>/dev/tcp/127.0.0.1/514) 2>/dev/null && return 0
        sleep 0.2
    done
    fail "sender never listened on 127.0.0.1:514"
}
stop_sender() {
    kill "$(cat "$work/sender.pid")"
    for _ in $(seq 50); do
        [[ -e "$work/sender.pid" ]] || return 0
        sleep 0.2
    done
}

# Daemon-shaped events (log/formats/json.go: one object then "\n\n").
event() { # type
    jq -cn --arg t "$token" --argjson type "$1" '{
      Event: {protocol: "tcp", src_ip: "192.168.1.20", src_port: 43210,
              dst_ip: "93.184.215.14", dst_host: "example.com", dst_port: 443,
              user_id: 1000, process_id: 4242, process_path: "/usr/bin/curl",
              process_cwd: ("/home/u/" + $t),
              process_args: ["curl", "-H", ("Authorization: Bearer " + $t)],
              process_env: {API_TOKEN: $t, HOME: "/home/u"},
              process_checksums: {md5: $t},
              process_tree: [{key: ("/usr/bin/bash " + $t), value: 1}]},
      Rule: "<nil>", Action: "<nil>", Type: $type}'
}
send_all() {
    {
        printf '\n'
        printf 'not json %s\n\n' "$token"
        printf '%s\n\n' "$(event 2)"
        printf '%s\n\n' "$(event 0)"
    } >/dev/tcp/127.0.0.1/514
}
wait_for_lines() { # file count seconds
    for _ in $(seq $(($3 * 5))); do
        [[ $(grep -c . "$1" || true) -ge $2 ]] && return 0
        sleep 0.2
    done
    return 1
}

echo "== end to end: pinned receiver"
receiver server 6514
start_sender 6514
send_all
wait_for_lines "$work/server.out" 1 20 || fail "receiver got nothing"
sleep 2 # let any stray extra frame arrive before counting
stop_sender
echo "receiver got:"
cat "$work/server.out"
[[ $(grep -c . "$work/server.out") -eq 1 ]] || fail "expected exactly one event"
grep -q "$token" "$work/server.out" && fail "the fake token reached the receiver"
got_keys=$(jq -c 'keys' "$work/server.out")
want_keys='["action","dst_host","dst_ip","dst_port","host_id","process_path","protocol","rule","src_ip","src_port","time","type","user_id"]'
[[ "$got_keys" == "$want_keys" ]] || fail "fields $got_keys, want $want_keys"
jq -e --arg h "$host_id" '.src_port == 43210 and .dst_port == 443
    and .user_id == 1000 and .type == 0 and .host_id == $h
    and .process_path == "/usr/bin/curl" and .dst_host == "example.com"
    and .action == "<nil>" and .rule == "<nil>" and (.time | test("^[0-9]{4}-"))' \
    "$work/server.out" >/dev/null || fail "forwarded values are wrong"
echo "OK: one event, allowlisted fields only, no token"

echo "== end to end: receiver from another CA"
receiver rogue 6515
start_sender 6515
send_all
sleep 8
stop_sender
[[ -s "$work/rogue.out" ]] && fail "a receiver outside the pinned CA got events"
echo # rsyslog's own TLS-rejection message above ends without a newline
echo "OK: the rogue receiver got nothing"

echo "== README merge and way back"
mkdir -p /etc/opensnitchd stubs
printf '#!/bin/sh\nexec "$@"\n' >stubs/sudo
printf '#!/bin/sh\necho "systemctl $*" >>%s/systemctl.log\n' "$work" >stubs/systemctl
chmod +x stubs/*
cp /src/packaging/system/opensnitchd-default-config.json /etc/opensnitchd/default-config.json
chmod 600 /etc/opensnitchd/default-config.json
original=$(jq -S . /etc/opensnitchd/default-config.json)
# The fenced block after `<!-- verify.sh: NAME -->`, with the list item's
# indentation removed (both may sit inside a numbered step).
snippet() { # marker
    awk -v m="<!-- verify.sh: $1 -->" '
        { line = $0; sub(/^[ \t]+/, "", line) }
        line == m { armed = 1; next }
        armed && line ~ /^```/ {
            if (inside) exit
            inside = 1; indent = index($0, "`") - 1; next
        }
        inside { print substr($0, indent + 1) }' "$src/README.md"
}
for marker in enable way-back; do
    [[ -n "$(snippet "$marker")" ]] || fail "README has no '$marker' snippet"
done
run_snippet() { # marker
    (cd /src && PATH="$work/stubs:$PATH" bash -e -c "$(snippet "$1")")
}
loggers='.Server.Loggers | map(select(.Name == "remote" and .Server == "127.0.0.1:514")) | length'
run_snippet enable
run_snippet enable # a second run must not add a second entry
cfg=/etc/opensnitchd/default-config.json
[[ $(jq "$loggers" "$cfg") -eq 1 ]] || fail "merge didn't leave exactly one logger"
jq -e --slurpfile l "$src/opensnitchd-logger.json" '.Server.Loggers == $l' "$cfg" >/dev/null ||
    fail "merged entry differs from opensnitchd-logger.json"
[[ $(stat -c %a "$cfg") == 600 ]] || fail "merge changed the file mode"
[[ $(jq -S 'del(.Server.Loggers)' "$cfg") == "$original" ]] ||
    fail "merge changed more than Server.Loggers"
grep -q 'restart opensnitch.service' systemctl.log || fail "no daemon restart"
run_snippet way-back
[[ $(jq -S . "$cfg") == "$original" ]] || fail "way back didn't restore the config"
[[ $(stat -c %a "$cfg") == 600 ]] || fail "way back changed the file mode"
echo "OK: merge is idempotent, keeps 0600, and the way back restores the config"
echo "systemctl calls:"
cat systemctl.log

echo "ALL OK"
