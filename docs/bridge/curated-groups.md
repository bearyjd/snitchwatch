# Curated default groups

Wire: `SetCuratedDefaults` carries `groups` (id, title, description, in display
order) and each entry a `group` id. Advertised as capability `curatedGroupsV1`.
Both fields are optional on the wire: an older bridge sends neither, and an older
GUI ignores them. Entry ids, and so installed rule names, never change when
entries are regrouped.

Groups (trust boundaries): `system-services` (NetworkManager check, chronyc,
chronyd NTP, Avahi mDNS), `app-updates` (Flatpak), `tailscale`, `name-resolution`
(the broad resolver rule, alone, behind its consent ticket).

Enabling a group = one `setCuratedDefaults {ids, on: true}` with the ids of the
entries whose `group` matches. A broad entry is always alone in its group, so a
group switch can never carry it.

Tailscale: only the coordination and log servers can be named narrowly. DERP
relays and WireGuard peers have no fixed name or port, so those asks stay.

GUI follow-up (not in this change): at first run list `groups`, show each group's
entries (`allows`, `why`), offer one switch per group; keep the broad group on its
own ticketed flow; fall back to the flat list when `groups` is absent.
