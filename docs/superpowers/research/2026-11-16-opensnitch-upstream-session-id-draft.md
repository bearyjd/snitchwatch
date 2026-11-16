# DRAFT (not filed): per-process identity in opensnitchd's Subscribe `ClientConfig`

Status: draft for the owner to read. Nothing has been filed upstream. Context: Snitchwatch issue #356 (owner
decision 2026-11-15: accept the 3 s grace in the bridge for r16.3 and ask upstream for a field).

## Problem
A UI server cannot tell whether a `Subscribe` call comes from a new opensnitchd process or from the same process
reconnecting after a transport error. `ClientConfig.id` is the only candidate, and it does not identify the
process: the daemon fills it per `Subscribe` message, not per process
(`daemon/ui/notifications.go:52`, `Id: uint64(ts.UnixNano())`, taken when the config message is built). It changes
on every reconnect, so it cannot distinguish "daemon restarted" from "stream reconnected".

## Why a UI cares
State a UI derives from one daemon run (for example "the process monitor fell back from eBPF to /proc", a
fallback alert) must be dropped when the daemon is replaced and kept when only the stream reconnected. Snitchwatch
works around it today with a 3 s grace on the age of the alert (`on_daemon_subscribe`), which is a heuristic.

## Proposal
Add one field to `message ClientConfig` in `proto/ui.proto` (next free number, 9), for example
`string instanceId = 9;`, set once at daemon start (random 128-bit value or boot id + pid + start time) and sent
unchanged on every `Subscribe` for the life of that process. Old servers ignore it (proto3, additive); old daemons
leave it empty, which servers treat as "unknown, use the fallback heuristic".

## Alternatives considered
- Reading the daemon log for its start line: needs log access the UI does not have, and the log level may hide it.
- Using `ClientConfig.name`/`version`: identical across restarts.
- Server-side connection tracking on the gRPC peer address: a Unix-socket peer has no stable address across
  reconnects.

## Notes for the filer
Snitchwatch's vendored reference is master `a1353848` (`ui.proto` unchanged from v1.8.0). Upstream has had one
active maintainer and no release since v1.8.0, so keep the patch small and offer a PR with the field plus a
test. The owner has three other open upstream PRs (#1699, #1707, #1708); check their state before filing.
