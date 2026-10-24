#!/usr/bin/env bash
set -euo pipefail

# Host driver: compile and run inside the installed KDE SDK. Every D-Bus
# connection belongs to a fresh bus with no desktop service activation.
test_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(cd -- "$test_dir/../../../.." && pwd)
probe_build_dir=$(mktemp -d /tmp/snitchwatch-native-tray.XXXXXX)
cleanup() {
    rm -f -- "$probe_build_dir/probe" "$probe_build_dir/probe.moc" "$probe_build_dir/run-fixture.sh"
    rmdir -- "$probe_build_dir"
}
trap cleanup EXIT

cat > "$probe_build_dir/run-fixture.sh" <<'SDK_SCRIPT'
set -eu
test_dir=$1
build_dir=$2
qml_dir=$3
qt_version=$(pkg-config --modversion Qt6Gui)
qt_include=$(pkg-config --variable=includedir Qt6Gui)
qt_libexec=$(pkg-config --variable=libexecdir Qt6Core)
"$qt_libexec/moc" "$test_dir/probe.cpp" -o "$build_dir/probe.moc"
# -fPIC also avoids ELF copy relocations for Qt private static data.
c++ -fPIC -I"$build_dir" \
    -I"$qt_include/QtGui/$qt_version" -I"$qt_include/QtGui/$qt_version/QtGui" \
    -I"$qt_include/QtCore/$qt_version" -I"$qt_include/QtCore/$qt_version/QtCore" \
    "$test_dir/probe.cpp" -o "$build_dir/probe" \
    $(pkg-config --cflags --libs Qt6Widgets Qt6Qml Qt6Quick Qt6DBus)
exec "$build_dir/probe" "$qml_dir"
SDK_SCRIPT

timeout 60s dbus-run-session --config-file="$test_dir/session.conf" -- \
    flatpak run --user --socket=session-bus --nosocket=wayland --nosocket=x11 \
    --filesystem="$repo_dir:ro" --filesystem="$probe_build_dir" \
    --env=QT_QPA_PLATFORM=offscreen --env=QT_QPA_PLATFORMTHEME=kde \
    --env=XDG_CURRENT_DESKTOP=KDE --env=QT_NO_XDG_DESKTOP_PORTAL=1 \
    --env=QT_QUICK_BACKEND=software \
    --command=sh "${SNITCHWATCH_TRAY_TEST_SDK:-org.kde.Sdk//6.11}" \
    "$probe_build_dir/run-fixture.sh" "$test_dir" "$probe_build_dir" \
    "$repo_dir/crates/snitchwatch-kirigami/qml"
