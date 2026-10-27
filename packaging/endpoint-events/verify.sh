#!/usr/bin/env bash
# Prove the endpoint-events recipe against real rsyslog. Run with
# `just endpoint-events-verify` (needs rootless podman and access to the
# Fedora mirrors; deliberately not part of package-check).
#
# Inside the digest-pinned Fedora 44 image from packaging/release/pins.env it:
#   1. installs the README's packages, checks the nftables table with
#      `nft -c` and loads it, and checks the drop-in with `rsyslogd -N1` under
#      Fedora's stock /etc/rsyslog.conf and under a minimal main config;
#   2. proves the table: a non-root connect to 127.0.0.1:514 is refused, a
#      root one accepted;
#   3. runs the drop-in under the STOCK rsyslog.conf, with a disk-only, slowed
#      main queue, against a local mutual-TLS receiver; sends daemon-shaped
#      JSON whose env/args/cwd/tree/checksums carry a fake token, plus a blank
#      frame, a non-JSON line, a task notification and invalid events
#      (oversize protocol, unknown action, port 70000, relative path); asserts
#      the receiver got exactly the one valid event, equal in values and types
#      to sample-event.json, and that no file under /var/log, /var/lib/rsyslog
#      or /var/spool holds the token;
#   4. sends worst-case and UTF-8 boundary events: a path or rule exactly at
#      its byte cap ending in a two-byte character is forwarded as strict
#      UTF-8 JSON; one byte over is dropped; every frame is under 8 KiB;
#   5. asserts mutual TLS: a server from another CA, or from the pinned CA
#      with the wrong name, gets nothing; the receiver (which requires this
#      host's client certificate by name) gets nothing from a sender whose
#      client certificate comes from another CA, or that has no certificate,
#      or no key;
#   6. runs the README's own merge and way-back snippets against a copy of the
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
    # NET_ADMIN is namespaced to the container's own network: it lets the
    # proof load the nftables table there, never on the host.
    # Optional: SW_VERIFY_DNF_CACHE=<podman volume name> keeps dnf's
    # metadata and packages between runs (slow mirrors); off by default.
    cache=()
    if [[ -n "${SW_VERIFY_DNF_CACHE:-}" ]]; then
        cache=(-v "$SW_VERIFY_DNF_CACHE:/var/cache/libdnf5" -e SW_VERIFY_DNF_CACHE=1)
    fi
    exec podman run --rm --cap-add=NET_ADMIN "${cache[@]}" \
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
client=verify-host.test
host_id=verify-host
pki=/etc/pki/snitchwatch-endpoint-events

echo "== install"
keepcache=()
[[ -n "${SW_VERIFY_DNF_CACHE:-}" ]] && keepcache=(--setopt=keepcache=True)
dnf -q -y install --setopt=install_weak_deps=False "${keepcache[@]}" \
    rsyslog rsyslog-mmjsonparse rsyslog-gnutls nftables openssl jq util-linux >/dev/null
rpm -q rsyslog rsyslog-mmjsonparse rsyslog-gnutls nftables
rsyslogd_path=$(command -v rsyslogd)
echo "rsyslogd: $rsyslogd_path -> $(readlink -f "$rsyslogd_path")"

echo "== certificates"
cd "$work"
make_ca() { # name
    openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj "/CN=$1" \
        -keyout "$1.key" -out "$1.pem" 2>/dev/null
}
make_cert() { # ca name cn
    openssl req -newkey rsa:2048 -nodes -subj "/CN=$3" \
        -addext "subjectAltName=DNS:$3" -keyout "$2.key" -out "$2.csr" 2>/dev/null
    openssl x509 -req -in "$2.csr" -CA "$1.pem" -CAkey "$1.key" -CAcreateserial \
        -days 2 -copy_extensions copy -out "$2.pem" 2>/dev/null
}
make_ca pinned-ca
make_ca rogue-ca
# Vigie's CA signs both its server certificate and this host's client one.
make_cert pinned-ca server "$peer"
make_cert pinned-ca host "$client"
make_cert rogue-ca rogue "$peer"
make_cert pinned-ca wrongname "other.test"
make_cert rogue-ca rogue-host "$client"
install_client() { # name: the client certificate the drop-in presents
    install -m 0644 "$work/$1.pem" "$pki/host-cert.pem"
    install -m 0600 "$work/$1.key" "$pki/host-key.pem"
}
mkdir -p "$pki"
cp pinned-ca.pem "$pki/vigie-ca.pem"
install_client host

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

echo "== nftables: only root may connect to 127.0.0.1:514"
nft -c -f "$src/snitchwatch-endpoint-events.nft"
nft -f "$src/snitchwatch-endpoint-events.nft"
nft -f "$src/snitchwatch-endpoint-events.nft" # reloading replaces, doesn't stack
nft list table inet snitchwatch_endpoint_events
[[ $(nft list table inet snitchwatch_endpoint_events | grep -c 'reject') -eq 2 ]] ||
    fail "the table doesn't hold exactly the two reject rules after a reload"
can_connect() { # uid
    setpriv --reuid="$1" --regid="$1" --clear-groups \
        bash -c 'exec 3<>/dev/tcp/127.0.0.1/514' 2>/dev/null
}

echo "== rsyslogd -N1, stock Fedora main config"
cp "$(drop_in 6514)" /etc/rsyslog.d/snitchwatch-endpoint-events.conf
rsyslogd -N1 -f /etc/rsyslog.conf 2>&1 | tee n1-stock.log
grep -qi 'error' n1-stock.log && fail "stock config check reported errors"
rm /etc/rsyslog.d/snitchwatch-endpoint-events.conf

# A minimal main config: the drop-in plus only what a test run needs.
# With a second argument `nocert` the drop-in loses its client certificate
# and key, with `nokey` only its key (the negative mutual-TLS cases).
sender_conf() { # port [nocert|nokey]
    local dir="$work/sender-$1" conf
    mkdir -p "$dir"
    conf=$(drop_in "$1")
    case "${2:-}" in
    nocert) sed -i -e '/streamDriver\.CertFile/d' -e '/streamDriver\.KeyFile/d' "$conf" ;;
    nokey) sed -i -e '/streamDriver\.KeyFile/d' "$conf" ;;
    esac
    if [[ -n "${2:-}" ]] && grep -q 'streamDriver\.KeyFile' "$conf"; then
        fail "client key still configured"
    fi
    printf 'global(workDirectory="%s")\ninclude(file="%s")\n' "$dir" "$conf" >"$dir.conf"
    echo "$dir.conf"
}
echo "== rsyslogd -N1, minimal main config"
rsyslogd -N1 -f "$(sender_conf 6514)" 2>&1 | tee n1-min.log
grep -qi 'error' n1-min.log && fail "minimal config check reported errors"

# A mutual-TLS receiver standing in for Vigie: it requires a client
# certificate from the pinned CA naming this host, and writes each frame's raw
# payload, one per line. Its own size limit is far above 8 KiB, so an
# oversize frame would show up whole rather than be cut.
receiver() { # name port
    mkdir -p "$work/$1"
    cat >"$work/$1.conf" <<EOF
global(workDirectory="$work/$1" maxMessageSize="256k"
       DefaultNetstreamDriver="gtls"
       DefaultNetstreamDriverCAFile="$work/pinned-ca.pem"
       DefaultNetstreamDriverCertFile="$work/$1.pem"
       DefaultNetstreamDriverKeyFile="$work/$1.key")
module(load="imtcp" StreamDriver.Name="gtls" StreamDriver.Mode="1"
       StreamDriver.AuthMode="x509/name" PermittedPeer="$client")
template(name="raw" type="string" string="%rawmsg%\n")
input(type="imtcp" address="127.0.0.1" port="$2" ruleset="r")
ruleset(name="r") { action(type="omfile" file="$work/$1.out" template="raw") }
EOF
    : >"$work/$1.out"
    rsyslogd -f "$work/$1.conf" -i "$work/$1.pid"
}
# Port `stock` runs Fedora's own /etc/rsyslog.conf with the drop-in in
# /etc/rsyslog.d, and a disk-only main queue slowed to one message a second:
# a raw event that went through the main queue would sit in its spool.
start_sender() { # port|stock [nocert|nokey]
    if [[ "$1" == stock ]]; then
        cp "$(drop_in 6514)" /etc/rsyslog.d/snitchwatch-endpoint-events.conf
        cat >/etc/rsyslog.d/00-verify-main-queue.conf <<'EOF'
main_queue(queue.type="Disk" queue.filename="verify-mainq"
           queue.dequeueSlowdown="1000000" queue.saveOnShutdown="on")
EOF
        rsyslogd -f /etc/rsyslog.conf -i "$work/sender.pid"
    else
        rsyslogd -f "$(sender_conf "$1" "${2:-}")" -i "$work/sender.pid"
    fi
    for _ in $(seq 50); do
        can_connect 0 && return 0
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
# Worst case for frame size: every capped field exactly at its cap, the path
# and rule in characters that each need six bytes of JSON escaping, the
# longest valid IPv6 addresses, plus a large environment.
big_event() {
    jq -cn --arg t "$token" '{
      Event: {protocol: "tcp", src_ip: ("f" * 45), src_port: 43211,
              dst_ip: ("f" * 45), dst_host: ("a" * 253), dst_port: 65535,
              user_id: 4294967295, process_id: 4243,
              process_path: ("/" + ("\u0001" * 767)),
              process_args: ["big", $t],
              process_env: {API_TOKEN: $t, BLOB: ("x" * 30000)}},
      Rule: ("\u0001" * 128), Action: "allow", Type: 0}'
}
# Events the validation must drop, each also carrying the token.
bad_events() {
    event 0 | jq -c '.Event.protocol = ("t" * 10000)'
    event 0 | jq -c '.Action = "allow-forged"'
    event 0 | jq -c '.Event.dst_port = 70000'
    event 0 | jq -c '.Event.process_path = "usr/bin/curl"'
    event 0 | jq -c '.Event.src_ip = "1.2.3.4\"}"'
    event 0 | jq -c '.Event.dst_host = ("a" * 254)'
}
# A value of exactly N bytes ending in "é" (2 bytes); a path starts with "/".
utf8_value() { # path|rule total-bytes
    local value
    case "$1" in
    path) value="/$(printf '%*s' $(($2 - 3)) '' | tr ' ' a)é" ;;
    rule) value="$(printf '%*s' $(($2 - 2)) '' | tr ' ' a)é" ;;
    esac
    [[ $(printf '%s' "$value" | wc -c) -eq $2 ]] || fail "boundary value is not $2 bytes"
    printf '%s' "$value"
}
# An event whose process_path, or rule, is N bytes ending in "é".
utf8_event() { # path|rule total-bytes
    case "$1" in
    path) event 0 | jq -c --arg v "$(utf8_value path "$2")" '.Event.process_path = $v' ;;
    rule) event 0 | jq -c --arg v "$(utf8_value rule "$2")" '.Rule = $v' ;;
    esac
}
send_all() {
    {
        printf '\n'
        printf 'not json %s\n\n' "$token"
        printf '%s\n\n' "$(event 2)"
        bad_events | while read -r line; do printf '%s\n\n' "$line"; done
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

echo "== nftables: non-root connect refused, root accepted"
receiver server 6514
start_sender stock
can_connect 0 || fail "root can't connect to the listener"
can_connect 65534 && fail "a non-root user can connect to 127.0.0.1:514"
echo "OK: uid 65534 refused, uid 0 accepted"

echo "== end to end: stock rsyslog.conf, pinned receiver"
send_all
# Look while the slowed main queue still holds whatever reached it: its spool
# files are deleted once drained.
sleep 2
in_spool=$(grep -rlsF "$token" /var/lib/rsyslog || true)
[[ -z "$in_spool" ]] || fail "the raw event went through the main queue's disk spool: $in_spool"
wait_for_lines "$work/server.out" 1 20 || fail "receiver got nothing"
sleep 3 # let any stray extra frame arrive before counting
stop_sender
sinks=$(grep -rlsF "$token" /var/log /var/lib/rsyslog /var/spool || true)
[[ -z "$sinks" ]] || fail "the raw event reached local files: $sinks"
echo "OK: no local sink (journal files, /var/log, main-queue spool) holds the token"
rm /etc/rsyslog.d/snitchwatch-endpoint-events.conf /etc/rsyslog.d/00-verify-main-queue.conf
echo "receiver got:"
cat "$work/server.out"
[[ $(grep -c . "$work/server.out") -eq 1 ]] || fail "expected exactly one event"
grep -q "$token" "$work/server.out" && fail "the fake token reached the receiver"
got_keys=$(jq -c 'keys' "$work/server.out")
# The hand-off sample's keys are the contract, so the sample can't drift.
want_keys=$(jq -c 'keys' "$src/sample-event.json")
[[ "$got_keys" == "$want_keys" ]] || fail "fields $got_keys, want $want_keys"
# Values and types, not just keys: the frame equals the sample but for time.
[[ $(jq -cS 'del(.time)' "$work/server.out") == $(jq -cS 'del(.time)' "$src/sample-event.json") ]] ||
    fail "forwarded event differs from sample-event.json"
jq -e '.time | test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+[+-][0-9]{2}:[0-9]{2}$")' \
    "$work/server.out" >/dev/null || fail "time is not RFC 3339"
echo "OK: one event (invalid ones dropped), equal to sample-event.json, no token"
echo "SAMPLE: $(head -n1 "$work/server.out")"

echo "== end to end: worst-case frame size"
raw_big=$(big_event)
echo "raw daemon line: ${#raw_big} bytes"
start_sender 6514
printf '%s\n\n' "$raw_big" >/dev/tcp/127.0.0.1/514
wait_for_lines "$work/server.out" 2 20 || fail "the large event wasn't forwarded"
stop_sender
frame=$(sed -n 2p "$work/server.out")
echo "forwarded frame: ${#frame} bytes"
[[ ${#frame} -lt 8192 ]] || fail "frame of ${#frame} bytes exceeds Vigie's 8 KiB"
grep -q "$token" "$work/server.out" && fail "the fake token reached the receiver"
jq -e '.src_port == 43211 and .action == "allow"' <<<"$frame" >/dev/null ||
    fail "the large event's frame is not the expected JSON"
echo "OK: the large event arrives in one frame under 8 KiB"

echo "== UTF-8 boundaries: at the cap forwarded intact, one byte over dropped"
before=$(grep -c . "$work/server.out")
start_sender 6514
for event_line in "$(utf8_event path 768)" "$(utf8_event rule 128)" \
    "$(utf8_event path 769)" "$(utf8_event rule 129)"; do
    printf '%s\n\n' "$event_line" >/dev/tcp/127.0.0.1/514
done
wait_for_lines "$work/server.out" $((before + 2)) 20 || fail "the at-cap events weren't forwarded"
sleep 3
stop_sender
[[ $(grep -c . "$work/server.out") -eq $((before + 2)) ]] || fail "an over-cap event was forwarded"
tail -n 2 "$work/server.out" >boundary.out
iconv -f UTF-8 -t UTF-8 boundary.out >/dev/null || fail "a frame is not valid UTF-8"
want_path=$(utf8_value path 768)
want_rule=$(utf8_value rule 128)
sed -n 1p boundary.out | jq -e --arg v "$want_path" '.process_path == $v' >/dev/null ||
    fail "the 768-byte path didn't arrive intact"
sed -n 2p boundary.out | jq -e --arg v "$want_rule" '.rule == $v' >/dev/null ||
    fail "the 128-byte rule didn't arrive intact"
echo "OK: 768-byte path and 128-byte rule intact as strict UTF-8; 769/129 dropped"

echo "== end to end: receiver from another CA"
receiver rogue 6515
start_sender 6515
send_all
sleep 8
stop_sender
[[ -s "$work/rogue.out" ]] && fail "a receiver outside the pinned CA got events"
echo # rsyslog's own TLS-rejection message above ends without a newline
echo "OK: the rogue receiver got nothing"

echo "== end to end: server from the pinned CA with the wrong name"
receiver wrongname 6516
start_sender 6516
send_all
sleep 8
stop_sender
[[ -s "$work/wrongname.out" ]] && fail "a server with the wrong name got events"
echo
echo "OK: the wrong-name server got nothing"

echo "== end to end: client certificate from another CA"
before=$(grep -c . "$work/server.out")
install_client rogue-host
start_sender 6514
send_all
sleep 8
stop_sender
install_client host
[[ $(grep -c . "$work/server.out") -eq $before ]] ||
    fail "the receiver accepted a client certificate outside its CA"
echo
echo "OK: the receiver rejected a client certificate from another CA"

echo "== end to end: no client certificate"
before=$(grep -c . "$work/server.out")
start_sender 6514 nocert
send_all
sleep 8
stop_sender
[[ $(grep -c . "$work/server.out") -eq $before ]] ||
    fail "the receiver accepted a sender without a client certificate"
echo
echo "OK: the receiver rejected a sender without a client certificate"

echo "== end to end: client certificate without its key"
before=$(grep -c . "$work/server.out")
start_sender 6514 nokey
send_all
sleep 8
stop_sender
[[ $(grep -c . "$work/server.out") -eq $before ]] ||
    fail "the receiver accepted a sender without a client key"
echo
echo "OK: no events from a sender without a client key"

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
