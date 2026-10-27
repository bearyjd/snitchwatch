#!/usr/bin/env bash
# Prove the endpoint-events recipe against real rsyslog, nftables and
# systemd. Run with `just endpoint-events-verify` (needs rootless podman and
# access to the Fedora mirrors; deliberately not part of package-check).
#
# The packages are installed once into a container from the digest-pinned
# Fedora 44 image (packaging/release/pins.env), committed to a throwaway local
# image, and removed afterwards. Two phases run on it.
#
# Phase 1 (--inside, plain container):
#   1. nftables: `nft -c`, load, reload (still exactly four rejects); uid
#      65534 can't connect to 127.0.0.1:514, root can; best effort, a peer
#      network namespace routing to 127.0.0.1 over a route_localnet veth is
#      rejected by the input rule (positive control: accepted without it).
#   2. the instance's config passes `rsyslogd -N1`; a deliberately broken one
#      fails -N1, and rsyslogd refuses to start with it (abortOnUncleanConfig)
#      and never opens 127.0.0.1:514.
#   3. end to end, with Fedora's stock host rsyslog running alongside, to a
#      local mutual-TLS receiver: daemon-shaped JSON whose env/args/cwd/tree/
#      checksums carry a fake token, the producer's "Kernel connection" and
#      comm path values, a blank frame, a non-JSON line, a task notification
#      and invalid events. Exactly the three valid events arrive; the first
#      equals sample-event.json in values and types; no file under /var/log,
#      /var/lib or /var/spool holds the token (read errors fail the check).
#   4. worst-case and UTF-8 boundary events: at the cap forwarded as strict
#      UTF-8 JSON, one byte over dropped, every frame under 8 KiB.
#   5. mutual TLS: nothing is delivered to a server from another CA or with
#      the wrong name, and the receiver accepts nothing from a client
#      certificate of another CA, or a sender with no certificate or key.
#      Every receiver is first proven ready and able to record (a positive
#      control), so "nothing delivered" is never an idle receiver.
#   6. the README's merge and way-back snippets, sudo/systemctl stubbed.
#
# Phase 2 (--systemd-inside, the same image booted with systemd):
#   the units as the README installs them: a failed table load means the
#   listener never starts; while the table's unit restarts there is no moment
#   with a listener and no table; a reload keeps both; stopping the table
#   stops the listener; a broken config means no listener; and nothing sent
#   to the listener reaches the live journal.
#
# NET_ADMIN and SYS_ADMIN are granted inside the container's own namespaces
# (its network, its user namespace): nothing touches the host's firewall,
# /etc or daemons, and the container's 127.0.0.1 is not the host's.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
packages=(rsyslog rsyslog-mmjsonparse rsyslog-gnutls nftables openssl jq
    util-linux iproute systemd)

if [[ -z "${1:-}" ]]; then
    repo_root="$(cd "$here/../.." && pwd -P)"
    # shellcheck source=/dev/null # pins.env: plain KEY=value lines (SW_*)
    source "$repo_root/packaging/release/pins.env"
    # Optional: SW_VERIFY_DNF_CACHE=<podman volume name> keeps dnf's
    # metadata and packages between runs (slow mirrors); off by default.
    cache=()
    if [[ -n "${SW_VERIFY_DNF_CACHE:-}" ]]; then
        cache=(-v "$SW_VERIFY_DNF_CACHE:/var/cache/libdnf5")
    fi
    run_id="snitchwatch-ee-verify-$$"
    image="localhost/$run_id:tmp"
    # shellcheck disable=SC2329 # invoked by the EXIT trap
    cleanup() {
        podman rm -f "$run_id-prep" "$run_id-systemd" >/dev/null 2>&1 || true
        podman rmi -f "$image" >/dev/null 2>&1 || true
    }
    trap cleanup EXIT
    echo "== install (into a throwaway image from $SW_BUILDER_IMAGE)"
    podman run --name "$run_id-prep" "${cache[@]}" "$SW_BUILDER_IMAGE" \
        dnf -q -y install --setopt=install_weak_deps=False --setopt=keepcache=True \
        "${packages[@]}" >/dev/null
    podman commit -q "$run_id-prep" "$image" >/dev/null
    mount=(-v "$repo_root/packaging:/src/packaging:ro,Z")
    podman run --rm --cap-add=NET_ADMIN --cap-add=SYS_ADMIN \
        --sysctl net.ipv4.conf.all.route_localnet=1 --sysctl net.ipv4.conf.all.rp_filter=0 \
        --sysctl net.ipv4.conf.default.rp_filter=0 "${mount[@]}" "$image" \
        bash /src/packaging/endpoint-events/verify.sh --inside
    podman run -d --name "$run_id-systemd" --systemd=always --cap-add=NET_ADMIN \
        "${mount[@]}" "$image" /usr/sbin/init >/dev/null
    podman exec "$run_id-systemd" bash /src/packaging/endpoint-events/verify.sh --systemd-inside
    echo "ALL OK"
    exit 0
fi

# ---- inside a container --------------------------------------------------
fail() {
    echo "FAIL: $*" >&2
    exit 1
}
# It writes /etc and starts daemons: never outside the container.
if [[ ! -e /run/.containerenv && ! -e /.dockerenv ]]; then
    fail "$1 runs only in the verify container; run verify.sh without arguments"
fi
src=/src/packaging/endpoint-events
work=$(mktemp -d)
token="FAKE-TOKEN-$RANDOM$RANDOM"
peer=vigie.test
client=verify-host.test
host_id=verify-host
pki=/etc/pki/snitchwatch-endpoint-events
instance_conf=/etc/snitchwatch-endpoint-events/rsyslog.conf

rpm -q rsyslog rsyslog-mmjsonparse rsyslog-gnutls nftables
rsyslogd_path=$(command -v rsyslogd)
echo "rsyslogd: $rsyslogd_path -> $(readlink -f "$rsyslogd_path")"

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
install_client() { # name: the client certificate the instance presents
    install -m 0644 "$work/$1.pem" "$pki/host-cert.pem"
    install -m 0600 "$work/$1.key" "$pki/host-key.pem"
}
mkdir -p "$pki"
cp pinned-ca.pem "$pki/vigie-ca.pem"
install_client host

# The instance's config with its placeholders filled the way the README says.
# Variants for the negative cases:
#   nocert      no client certificate or key
#   nokey       a client certificate without its key
#   trust-rogue pins rogue-ca instead (positive control for the rogue server)
#   name-other  permits other.test (positive control for the wrong-name one)
fill_conf() { # port [variant] -> path
    local out="$work/instance-$1-${2:-plain}.conf"
    sed -e "s/@VIGIE_ADDRESS@/127.0.0.1/" -e "s/port=\"6514\" protocol=\"tcp\"/port=\"$1\" protocol=\"tcp\"/" \
        -e "s/@VIGIE_CERT_NAME@/$peer/" -e "s/@HOST_ID@/$host_id/" \
        "$src/snitchwatch-endpoint-events.conf" >"$out"
    case "${2:-}" in
    nocert) sed -i -e '/streamDriver\.CertFile/d' -e '/streamDriver\.KeyFile/d' "$out" ;;
    nokey) sed -i -e '/streamDriver\.KeyFile/d' "$out" ;;
    trust-rogue) sed -i "s|$pki/vigie-ca.pem|$work/rogue-ca.pem|" "$out" ;;
    name-other) sed -i "s/StreamDriverPermittedPeers=\"$peer\"/StreamDriverPermittedPeers=\"other.test\"/" "$out" ;;
    esac
    if grep -q '@[A-Z_]*@' "$out"; then
        fail "unfilled placeholder in the instance's config"
    fi
    grep -q "port=\"$1\" protocol=\"tcp\"" "$out" || fail "forward port not set to $1"
    if [[ "${2:-}" == nocert || "${2:-}" == nokey ]] && grep -q 'streamDriver\.KeyFile' "$out"; then
        fail "client key still configured"
    fi
    echo "$out"
}
# The config with its ruleset name misspelt in the input: rsyslog's own
# fallback would route the listener to the default ruleset.
broken_conf() { # -> path
    local out="$work/instance-broken.conf"
    sed 's/ruleset="snitchwatch_endpoint_events"/ruleset="snitchwatch_endpoint_eventz"/' \
        "$(fill_conf 6514)" >"$out"
    grep -q 'snitchwatch_endpoint_eventz' "$out" || fail "broken config isn't broken"
    echo "$out"
}

can_connect() { # uid [port]
    setpriv --reuid="$1" --regid="$1" --clear-groups \
        bash -c "exec 3<>/dev/tcp/127.0.0.1/${2:-514}" 2>/dev/null
}
wait_port() { # port
    for _ in $(seq 50); do
        can_connect 0 "$1" && return 0
        sleep 0.2
    done
    return 1
}
lines() { # file
    grep -c . "$1" || true
}
wait_for_lines() { # file count seconds
    for _ in $(seq $(($3 * 5))); do
        [[ $(lines "$1") -ge $2 ]] && return 0
        sleep 0.2
    done
    return 1
}
# No file under the given directories holds the token; a read error fails.
no_token_in() { # dir...
    local rc=0 hits
    hits=$(grep -rlF -- "$token" "$@") || rc=$?
    [[ $rc -eq 1 ]] || fail "token in local files (rc $rc): $hits"
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
# Producer-derived path values (vendor/opensnitch/daemon/procmon/details.go:
# 258-275): a kernel task, and a process whose exe couldn't be read (its
# comm). Both are root, so user_id is omitted (protobuf omitempty).
producer_events() {
    event 0 | jq -c '.Event.process_path = "Kernel connection" | del(.Event.user_id)
        | .Event.protocol = "udp6" | .Event.src_ip = "fd00::20" | .Event.dst_ip = "2001:db8::1"'
    event 0 | jq -c '.Event.process_path = "kworker/0:1" | del(.Event.user_id)
        | .Action = "allow" | .Rule = "000-snitchwatch-bridge-fetch"'
}
# comm may hold any byte (prctl PR_SET_NAME): checked by length only, and
# jsonf must escape it into valid JSON.
comm_values=($'tab\there' 'quo"te' 'back\\slash' 'é1' 'aaaaaaaaaaaaaa�')
comm_events() {
    local comm
    for comm in "${comm_values[@]}"; do
        event 0 | jq -c --arg c "$comm" '.Event.process_path = $c'
    done
}
# Events the validation must drop, each also carrying the token.
bad_events() {
    event 0 | jq -c '.Event.protocol = ("t" * 10000)'
    event 0 | jq -c '.Event.protocol = "gre"'
    event 0 | jq -c '.Action = "allow-forged"'
    event 0 | jq -c '.Event.dst_port = 70000'
    event 0 | jq -c '.Event.user_id = 4294967296'
    event 0 | jq -c '.Event.src_ip = "999.1.1.1"'
    event 0 | jq -c '.Event.dst_ip = "deadbeef"'
    event 0 | jq -c '.Event.process_path = ("c" * 46)'
    event 0 | jq -c '.Event.src_ip = "1.2.3.4\"}"'
    event 0 | jq -c '.Event.dst_host = ("a" * 254)'
}
send_lines() { # every line of stdin as one daemon frame
    while read -r line; do printf '%s\n\n' "$line"; done >/dev/tcp/127.0.0.1/514
}
send_all() {
    {
        echo
        echo "not json $token"
        event 2
        bad_events
        event 0
        producer_events
        comm_events
    } | send_lines
}
# Worst case for frame size: every capped field exactly at its cap, the path
# and rule in characters that each need six bytes of JSON escaping, the
# longest valid IPv6 addresses, plus a large environment.
big_event() {
    jq -cn --arg t "$token" '{
      Event: {protocol: "sctp6", src_ip: ("f" * 44 + ":"), src_port: 43211,
              dst_ip: ("f" * 44 + ":"), dst_host: ("a" * 253), dst_port: 65535,
              user_id: 4294967295, process_id: 4243,
              process_path: ("/" + ("\u0001" * 767)),
              process_args: ["big", $t],
              process_env: {API_TOKEN: $t, BLOB: ("x" * 30000)}},
      Rule: ("\u0001" * 128), Action: "allow", Type: 0}'
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
utf8_event() { # path|rule total-bytes
    case "$1" in
    path) event 0 | jq -c --arg v "$(utf8_value path "$2")" '.Event.process_path = $v' ;;
    rule) event 0 | jq -c --arg v "$(utf8_value rule "$2")" '.Rule = $v' ;;
    esac
}

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
for marker in prepare check start enable way-back rollback; do
    [[ -n "$(snippet "$marker")" ]] || fail "README has no '$marker' snippet"
done
# sudo runs the command (we are root); restorecon is a no-op with SELinux
# off in the container, recorded so the order can be checked.
mkdir -p "$work/stubs"
printf '#!/bin/sh\nexec "$@"\n' >"$work/stubs/sudo"
printf '#!/bin/sh\necho "restorecon $*" >>%s/restorecon.log\n' "$work" >"$work/stubs/restorecon"
# semanage: the recipe never runs it (owner decision on #233: 6514 only).
# The stub records any call, and the run fails on one.
printf '#!/bin/sh\necho "semanage $*" >>%s/semanage.calls\nexit 1\n' "$work" >"$work/stubs/semanage"
chmod +x "$work/stubs/"*
no_semanage_calls() {
    [[ ! -e "$work/semanage.calls" ]] || fail "something ran semanage: $(cat "$work/semanage.calls")"
}
run_snippet() { # marker [extra PATH dir]
    (cd /src && PATH="${2:+$2:}$work/stubs:$PATH" bash -e -c "$(snippet "$1")")
}
# The README's fresh install, verbatim and in order, with nothing created
# beforehand except what step 2 has the operator place by hand (the
# certificates, already in $pki) and the filled-in placeholders.
fresh_install() {
    [[ ! -e /var/lib/rsyslog/snitchwatch-endpoint-events ]] || fail "work dir pre-exists"
    [[ ! -e "$instance_conf" ]] || fail "instance config pre-exists"
    run_snippet prepare
    [[ $(stat -c %a /var/lib/rsyslog/snitchwatch-endpoint-events) == 700 ]] ||
        fail "work dir is not 0700"
    grep -q 'restorecon -RFv /var/lib/rsyslog/snitchwatch-endpoint-events' "$work/restorecon.log" ||
        fail "prepare didn't restore labels"
    run_snippet check && fail "check accepted unfilled placeholders"
    cp "$(fill_conf 6514)" "$instance_conf"
    run_snippet check
    echo "OK: README prepare and check ran in order on a fresh system"
}

if [[ "$1" == --systemd-inside ]]; then
    # ---- phase 2: the units under real systemd ---------------------------
    echo "== systemd: waiting for boot"
    for _ in $(seq 60); do
        state=$(systemctl is-system-running 2>/dev/null || true)
        [[ "$state" == running || "$state" == degraded ]] && break
        sleep 1
    done
    echo "system state: $state"
    echo "== systemd: the README's fresh install, verbatim"
    fresh_install
    run_snippet start
    units=(snitchwatch-endpoint-events-nft.service snitchwatch-endpoint-events.service)
    systemctl is-active -q "${units[@]}" || fail "README start left the units inactive"
    can_connect 0 || fail "README start: no listener"
    can_connect 65534 && fail "README start: uid 65534 can connect"
    echo "OK: README start: both units active, root connects, uid 65534 refused"
    table_up() { nft list table inet snitchwatch_endpoint_events >/dev/null 2>&1; }
    reset() {
        systemctl stop "${units[@]}" 2>/dev/null || true
        systemctl reset-failed "${units[@]}" 2>/dev/null || true
    }

    echo "== systemd: a failed table load means no listener"
    systemctl stop "${units[@]}"
    cp /etc/nftables/snitchwatch-endpoint-events.nft "$work/good.nft"
    echo "this is not nft syntax" >>/etc/nftables/snitchwatch-endpoint-events.nft
    systemctl start snitchwatch-endpoint-events.service && fail "the listener started without its table"
    systemctl is-active -q snitchwatch-endpoint-events.service && fail "listener active"
    table_up && fail "a broken table file loaded"
    can_connect 0 && fail "something listens on 514 without the table"
    echo "OK: table load failed, listener never started"
    cp "$work/good.nft" /etc/nftables/snitchwatch-endpoint-events.nft
    reset

    echo "== systemd: starting the listener brings the table up first"
    systemctl start snitchwatch-endpoint-events.service
    systemctl is-active -q "${units[@]}" || fail "units not active"
    wait_port 514 || fail "no listener"
    table_up || fail "no table"
    can_connect 65534 && fail "uid 65534 can connect"
    echo "OK: both active, table loaded, uid 65534 refused"

    echo "== systemd: no listener without the table while the table's unit restarts"
    : >"$work/window.log"
    (
        end=$((SECONDS + 20))
        while ((SECONDS < end)); do
            if ! table_up && can_connect 0; then
                echo "listener up without the table at $(date +%T.%N)" >>"$work/window.log"
            fi
        done
    ) &
    poller=$!
    for _ in 1 2 3; do
        systemctl restart snitchwatch-endpoint-events-nft.service
        sleep 2
    done
    wait "$poller"
    [[ -s "$work/window.log" ]] && fail "$(head -n3 "$work/window.log")"
    systemctl is-active -q "${units[@]}" || fail "units not active after the restarts (PartOf)"
    wait_port 514 || fail "listener not back after the restarts"
    echo "OK: three restarts, no unprotected moment, listener back each time"

    echo "== systemd: a reload keeps the table and the listener"
    pid=$(systemctl show -p MainPID --value snitchwatch-endpoint-events.service)
    systemctl reload snitchwatch-endpoint-events-nft.service
    table_up || fail "reload deleted the table"
    [[ $(nft list table inet snitchwatch_endpoint_events | grep -c reject) -eq 4 ]] ||
        fail "reload didn't leave exactly four rejects"
    [[ $(systemctl show -p MainPID --value snitchwatch-endpoint-events.service) == "$pid" ]] ||
        fail "reload restarted the listener"
    echo "OK: reload kept the table (four rejects) and the listener (same PID)"

    echo "== systemd: nothing sent to the listener reaches the journal"
    { event 0; producer_events; } | send_lines
    sleep 3
    journalctl --sync 2>/dev/null || true
    rc=0
    journalctl --no-pager -q -o cat --grep="$token" >"$work/journal-hits" 2>"$work/journal-err" || rc=$?
    [[ $rc -eq 1 || ($rc -eq 0 && ! -s "$work/journal-hits") ]] ||
        fail "journal holds the token or can't be read (rc $rc): $(head -c 300 "$work/journal-hits" "$work/journal-err")"
    [[ -s "$work/journal-hits" ]] && fail "the journal holds the token"
    [[ $(journalctl --no-pager -q -u snitchwatch-endpoint-events.service | wc -l) -gt 0 ]] ||
        fail "the journal shows nothing for the unit: the check above proves nothing"
    no_token_in /var/log /var/lib /var/spool
    echo "OK: the live journal and /var hold no token"

    echo "== systemd: stopping the table's unit stops the listener first"
    systemctl stop snitchwatch-endpoint-events-nft.service
    systemctl is-active -q snitchwatch-endpoint-events.service && fail "listener survived its table"
    can_connect 0 && fail "something still listens on 514"
    echo "OK: listener stopped with its table"
    reset

    echo "== systemd: a broken config means no listener"
    cp "$(broken_conf)" "$instance_conf"
    systemctl start snitchwatch-endpoint-events.service && fail "started with a broken config"
    can_connect 0 && fail "something listens on 514 with a broken config"
    journalctl --no-pager -q -u snitchwatch-endpoint-events.service | grep -m1 -i 'error' ||
        fail "no config error logged"
    echo "OK: ExecStartPre refused the broken config, no listener"
    reset

    echo "== systemd: the complete way back leaves nothing the recipe added"
    cp "$(fill_conf 6514)" "$instance_conf"
    run_snippet start
    mkdir -p /etc/opensnitchd "$work/opensnitch-stub"
    cp /src/packaging/system/opensnitchd-default-config.json /etc/opensnitchd/default-config.json
    chmod 600 /etc/opensnitchd/default-config.json
    original=$(jq -S . /etc/opensnitchd/default-config.json)
    # Only opensnitch.service is absent here; everything else is real systemd.
    cat >"$work/opensnitch-stub/systemctl" <<'EOF'
#!/bin/bash
for arg; do [[ "$arg" == opensnitch.service ]] && exit 0; done
exec /usr/bin/systemctl "$@"
EOF
    chmod +x "$work/opensnitch-stub/systemctl"
    for marker in enable way-back rollback; do
        run_snippet "$marker" "$work/opensnitch-stub"
    done
    for path in /etc/systemd/system/snitchwatch-endpoint-events.service \
        /etc/systemd/system/snitchwatch-endpoint-events-nft.service \
        /etc/nftables/snitchwatch-endpoint-events.nft /etc/snitchwatch-endpoint-events \
        /var/lib/rsyslog/snitchwatch-endpoint-events "$pki"; do
        [[ ! -e "$path" ]] || fail "the way back left $path"
    done
    systemctl cat snitchwatch-endpoint-events.service >/dev/null 2>&1 && fail "listener unit still known"
    systemctl cat snitchwatch-endpoint-events-nft.service >/dev/null 2>&1 && fail "table unit still known"
    table_up && fail "the way back left the table"
    can_connect 0 && fail "the way back left a listener"
    [[ $(jq -S . /etc/opensnitchd/default-config.json) == "$original" ]] ||
        fail "the way back left the logger entry"
    no_semanage_calls
    echo "OK: units, table, listener, config, work dir, certificates and logger gone; no semanage call"
    echo "SYSTEMD OK"
    exit 0
fi

# ---- phase 1: plain container ----------------------------------------------
echo "== nftables: only root may connect to 127.0.0.1:514"
nft -c -f "$src/snitchwatch-endpoint-events.nft"
nft -f "$src/snitchwatch-endpoint-events.nft"
nft -f "$src/snitchwatch-endpoint-events.nft" # reloading replaces, doesn't stack
nft list table inet snitchwatch_endpoint_events
[[ $(nft list table inet snitchwatch_endpoint_events | grep -c 'reject') -eq 4 ]] ||
    fail "the table doesn't hold exactly the four reject rules after a reload"

echo "== the README's fresh install (prepare, check)"
fresh_install

no_semanage_calls
rsyslogd -N1 -f "$instance_conf" 2>&1 | tee n1.log
grep -qi 'error' n1.log && fail "config check reported errors"

echo "== a broken config: -N1 fails, rsyslogd refuses to start, no listener"
broken=$(broken_conf)
rsyslogd -N1 -f "$broken" >n1-broken.log 2>&1 && fail "-N1 accepted the broken config"
grep -m1 -i 'error\|not found' n1-broken.log
rc=0
timeout 20 rsyslogd -n -iNONE -f "$broken" >broken-run.log 2>&1 &
broken_pid=$!
for _ in $(seq 20); do
    can_connect 0 && fail "the broken config opened 127.0.0.1:514"
    kill -0 "$broken_pid" 2>/dev/null || break
    sleep 0.25
done
wait "$broken_pid" || rc=$?
[[ $rc -ne 0 && $rc -ne 124 ]] || fail "rsyslogd ran with the broken config (rc $rc)"
echo "OK: refused (rc $rc), port 514 never opened"

# A mutual-TLS receiver standing in for Vigie: it requires a client
# certificate from the pinned CA naming this host, and writes each frame's raw
# payload, one per line. Its own size limit is far above 8 KiB, so an
# oversize frame would show up whole rather than be cut. It must be ready
# before any test relies on it.
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
    wait_port "$2" || fail "receiver $1 never listened on $2"
}
start_sender() { # port [variant]
    rsyslogd -f "$(fill_conf "$1" "${2:-}")" -i "$work/sender.pid"
    wait_port 514 || fail "sender never listened on 127.0.0.1:514"
}
stop_sender() {
    kill "$(cat "$work/sender.pid")"
    for _ in $(seq 50); do
        [[ -e "$work/sender.pid" ]] || return 0
        sleep 0.2
    done
}
# Nothing reaches `name` from a sender of `variant`, then a sender of
# `control` does: the receiver was able to record all along.
expect_refused() { # name port variant control label
    local out="$work/$1.out" before
    before=$(lines "$out")
    start_sender "$2" "$3"
    event 0 | send_lines
    sleep 8
    stop_sender
    [[ $(lines "$out") -eq $before ]] || fail "$5: delivered"
    start_sender "$2" "$4"
    event 0 | send_lines
    wait_for_lines "$out" $((before + 1)) 20 || fail "$5: positive control got nothing"
    stop_sender
    echo
    echo "OK: $5: nothing delivered; positive control delivered"
}

echo "== nftables: non-root connect refused, root accepted"
receiver server 6514
# The host's own rsyslog, as Fedora ships it, runs alongside the instance.
mkdir -p /var/spool/rsyslog
rsyslogd -f /etc/rsyslog.conf -i /run/host-rsyslog.pid
start_sender 6514
can_connect 0 || fail "root can't connect to the listener"
can_connect 65534 && fail "a non-root user can connect to 127.0.0.1:514"
echo "OK: uid 65534 refused, uid 0 accepted"

echo "== nftables: packets routed to 127.0.0.1 from another interface (best effort)"
# The container's own netns plays the host (route_localnet=1 and rp_filter=0
# set by podman --sysctl, as netavark would on a bridge); a peer netns plays a
# container routing 127.0.0.1 to it over a veth. /proc/sys is read-only here,
# so the peer's own sysctls can't be changed, and `ip netns exec` can't mount
# /sys: nsenter enters only the peer's network.
routed_connect() {
    nsenter --net=/run/netns/peer timeout 3 bash -c 'exec 3<>/dev/tcp/127.0.0.1/514' 2>/dev/null
}
if ip netns add peer 2>/dev/null &&
    ip link add vh type veth peer name vp &&
    ip link set vp netns peer &&
    ip addr add 10.99.0.1/24 dev vh && ip link set vh up &&
    nsenter --net=/run/netns/peer sh -c '
        ip addr add 10.99.0.2/24 dev vp && ip link set vp up &&
        ip route add 127.0.0.1/32 via 10.99.0.1 dev vp table 100 &&
        ip rule add to 127.0.0.1 lookup 100 priority 10' 2>/dev/null; then
    nft delete chain inet snitchwatch_endpoint_events input
    if routed_connect; then
        nft -f "$src/snitchwatch-endpoint-events.nft"
        routed_connect && fail "a routed packet for 127.0.0.1:514 got through the input rule"
        echo "OK: routed connect accepted without the input rule, refused with it"
    else
        nft -f "$src/snitchwatch-endpoint-events.nft"
        echo "UNREPRODUCED: the routed path didn't reach the listener even without the input rule"
    fi
else
    echo "UNREPRODUCED: couldn't build the routed peer in this container"
fi
[[ $(nft list table inet snitchwatch_endpoint_events | grep -c 'reject') -eq 4 ]] ||
    fail "the table isn't back to four rejects"

echo "== end to end: host rsyslog alongside, pinned receiver"
send_all
wait_for_lines "$work/server.out" 8 20 || fail "receiver got $(lines "$work/server.out") of 8 events"
sleep 3 # let any stray extra frame arrive before counting
stop_sender
kill "$(cat /run/host-rsyslog.pid)"
echo "receiver got:"
cat "$work/server.out"
[[ $(lines "$work/server.out") -eq 8 ]] || fail "expected exactly eight events"
grep -q "$token" "$work/server.out" && fail "the fake token reached the receiver"
# Values and types, not just keys: the frame equals the sample but for time.
[[ $(head -n1 "$work/server.out" | jq -cS 'del(.time)') == $(jq -cS 'del(.time)' "$src/sample-event.json") ]] ||
    fail "forwarded event differs from sample-event.json"
jq -e '.time | test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+[+-][0-9]{2}:[0-9]{2}$")' \
    <(head -n1 "$work/server.out") >/dev/null || fail "time is not RFC 3339"
sed -n 2p "$work/server.out" | jq -e '.process_path == "Kernel connection" and .user_id == 0
    and .protocol == "udp6" and .dst_ip == "2001:db8::1"' >/dev/null ||
    fail "the kernel-connection event isn't as sent"
sed -n 3p "$work/server.out" | jq -e '.process_path == "kworker/0:1" and .user_id == 0
    and .action == "allow" and .rule == "000-snitchwatch-bridge-fetch"' >/dev/null ||
    fail "the comm-path event isn't as sent"
for i in "${!comm_values[@]}"; do
    line=$(sed -n "$((i + 4))p" "$work/server.out")
    jq -e --arg c "${comm_values[$i]}" '.process_path == $c' <<<"$line" >/dev/null ||
        fail "comm $(printf %q "${comm_values[$i]}") didn't arrive as valid JSON with its value"
done
grep -q $'\t' "$work/server.out" && fail "a raw tab reached a frame (not escaped)"
iconv -f UTF-8 -t UTF-8 "$work/server.out" >/dev/null || fail "a frame is not valid UTF-8"
no_token_in /var/log /var/lib /var/spool
echo "OK: tab, quote, backslash, UTF-8 and U+FFFD-replaced comm values arrive escaped, as valid JSON"
echo "OK: eight events (invalid ones dropped); the first equals sample-event.json;"
echo "    no token in frames or under /var/log, /var/lib, /var/spool"
echo "SAMPLE: $(head -n1 "$work/server.out")"

echo "== end to end: worst-case frame size"
raw_big=$(big_event)
echo "raw daemon line: ${#raw_big} bytes"
before=$(lines "$work/server.out")
start_sender 6514
echo "$raw_big" | send_lines
wait_for_lines "$work/server.out" $((before + 1)) 20 || fail "the large event wasn't forwarded"
stop_sender
frame=$(tail -n1 "$work/server.out")
echo "forwarded frame: ${#frame} bytes"
[[ ${#frame} -lt 8192 ]] || fail "frame of ${#frame} bytes exceeds Vigie's 8 KiB"
grep -q "$token" "$work/server.out" && fail "the fake token reached the receiver"
jq -e '.src_port == 43211 and .action == "allow" and .user_id == 4294967295' <<<"$frame" >/dev/null ||
    fail "the large event's frame is not the expected JSON"
echo "OK: the large event arrives in one frame under 8 KiB"

echo "== UTF-8 boundaries: at the cap forwarded intact, one byte over dropped"
before=$(lines "$work/server.out")
start_sender 6514
{
    utf8_event path 768
    utf8_event rule 128
    utf8_event path 769
    utf8_event rule 129
} | send_lines
wait_for_lines "$work/server.out" $((before + 2)) 20 || fail "the at-cap events weren't forwarded"
sleep 3
stop_sender
[[ $(lines "$work/server.out") -eq $((before + 2)) ]] || fail "an over-cap event was forwarded"
tail -n 2 "$work/server.out" >boundary.out
iconv -f UTF-8 -t UTF-8 boundary.out >/dev/null || fail "a frame is not valid UTF-8"
sed -n 1p boundary.out | jq -e --arg v "$(utf8_value path 768)" '.process_path == $v' >/dev/null ||
    fail "the 768-byte path didn't arrive intact"
sed -n 2p boundary.out | jq -e --arg v "$(utf8_value rule 128)" '.rule == $v' >/dev/null ||
    fail "the 128-byte rule didn't arrive intact"
echo "OK: 768-byte path and 128-byte rule intact as strict UTF-8; 769/129 dropped"

echo "== mutual TLS"
receiver rogue 6515
expect_refused rogue 6515 "" trust-rogue "server from another CA"
receiver wrongname 6516
expect_refused wrongname 6516 "" name-other "server from the pinned CA with the wrong name"
# The client's certificate is a file, not a config variant: swap it for the
# negative half and back for the positive control.
before=$(lines "$work/server.out")
install_client rogue-host
start_sender 6514
event 0 | send_lines
sleep 8
stop_sender
install_client host
[[ $(lines "$work/server.out") -eq $before ]] || fail "client certificate from another CA: delivered"
start_sender 6514
event 0 | send_lines
wait_for_lines "$work/server.out" $((before + 1)) 20 ||
    fail "client certificate from another CA: positive control got nothing"
stop_sender
echo
echo "OK: client certificate from another CA: nothing delivered; positive control delivered"
expect_refused server 6514 nocert "" "sender without a client certificate"
expect_refused server 6514 nokey "" "sender without a client key"

echo "== README merge and way back"
mkdir -p /etc/opensnitchd "$work/stub-systemctl"
printf '#!/bin/sh\necho "systemctl $*" >>%s/systemctl.log\n' "$work" >"$work/stub-systemctl/systemctl"
chmod +x "$work/stub-systemctl/systemctl"
cp /src/packaging/system/opensnitchd-default-config.json /etc/opensnitchd/default-config.json
chmod 600 /etc/opensnitchd/default-config.json
original=$(jq -S . /etc/opensnitchd/default-config.json)
loggers='.Server.Loggers | map(select(.Name == "remote" and .Server == "127.0.0.1:514")) | length'
run_snippet enable "$work/stub-systemctl"
run_snippet enable "$work/stub-systemctl" # a second run must not add a second entry
cfg=/etc/opensnitchd/default-config.json
[[ $(jq "$loggers" "$cfg") -eq 1 ]] || fail "merge didn't leave exactly one logger"
jq -e --slurpfile l "$src/opensnitchd-logger.json" '.Server.Loggers == $l' "$cfg" >/dev/null ||
    fail "merged entry differs from opensnitchd-logger.json"
[[ $(stat -c %a "$cfg") == 600 ]] || fail "merge changed the file mode"
[[ $(jq -S 'del(.Server.Loggers)' "$cfg") == "$original" ]] ||
    fail "merge changed more than Server.Loggers"
grep -q 'restart opensnitch.service' systemctl.log || fail "no daemon restart"
run_snippet way-back "$work/stub-systemctl"
[[ $(jq -S . "$cfg") == "$original" ]] || fail "way back didn't restore the config"
[[ $(stat -c %a "$cfg") == 600 ]] || fail "way back changed the file mode"
echo "OK: merge is idempotent, keeps 0600, and the way back restores the config"

echo "PHASE 1 OK"
