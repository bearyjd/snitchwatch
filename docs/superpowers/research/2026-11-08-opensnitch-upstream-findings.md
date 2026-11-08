# OpenSnitch upstream findings (local notes)

Date: 2026-11-08. Source: `/var/home/user/w2/vendor/opensnitch`, read-only
code reading. `git rev-parse HEAD` returned
`a1353848ba1b660320e90cefea782c3fba272c00`, which matches the expected master
`a1353848`. Nothing was built or run, except two tiny shell checks for finding
4 (`expr` and `sort` behavior). Paths below are relative to that checkout.

Two corrections to the suspected-defect descriptions: there is no scheduler
under `daemon/rule`. It lives in `daemon/tasks/scheduler/` and is configured
by the IOC scanner task's `schedule` list, not by a rule's operator list. The
`println` is in `daemon/log/formats/rfc3164.go`, not `loggers/`.

## 1. Scheduler nil dereference on a bad "time" entry

Verdict: CONFIRMED. Severity guess: medium (daemon crash from a malformed
task config, no remote trigger).

- `daemon/tasks/scheduler/daily.go:46-56`: `calcDailyTicker` returns
  `nil, time.Millisecond` when the string parses as neither `15:04:05` nor
  `15:04`.
- `daily.go:77-80`: `NewDailyTicker` passes that straight to
  `time.NewTicker(wait)`, so the returned `*time.Ticker` is never nil while
  `tms` is nil.
- `daily.go:116`: the guard checks `tck == nil`, not `tms == nil`. It can
  never fire, so the "invalid timer" return is dead code.
- `daily.go:149` -> `daily.go:19`: after the 1 ms wait,
  `timeHasDrifted(&realNow, tms)` evaluates `tms.Minute()` on a nil
  `*time.Time`, which panics. It runs in a goroutine with no recover, so the
  whole daemon dies.

Trigger: an IOC scanner task config with `"schedule": [{"weekday":[0,1,2,3,4,5,6],
"time":["9am"]}]` (any unparsable `time`, including `"25:00"`). `Start()`
only launches `SetupDailyTimers` when `len(Config.Time) > 0`
(`scheduler.go:68`). The config struct is `scheduler.Config`
(`scheduler.go:15`), used at `tasks/iocscanner/config/config.go:190`.
Nothing validates the strings on load (only the callers above were checked).
A side effect is that the bad ticker was stored in `s.Tickers[id]` first.

## 2. Data race on a map in Stop()

Verdict: CONFIRMED at code level (unsynchronized access). The race window is
narrow, so severity is low. The race is real but I did not reproduce it.

- `daemon/tasks/scheduler/scheduler.go:155-169`: `Scheduler.Stop()` ranges
  over `s.Tickers`, deletes from it and reassigns it, without taking `s.mu`.
- Writers hold `s.mu`: `daily.go:121-123` (`s.Tickers[id] = tck`) and
  `daily.go:82-87` (`stopTimer` delete). Stop() never takes that lock, so the
  lock does not protect it.
- Go's runtime aborts on concurrent map read/write with an unrecoverable
  `fatal error`. Reassigning the field at `scheduler.go:164` is a plain
  field race too.
- Same Stop() reads `s.Ticker` (`scheduler.go:166`), which is assigned in
  the `Start()` goroutine (`scheduler.go:88`) without a lock.

Trigger: `tasks/iocscanner/main.go:141-143` calls `sched.Stop()` right after
`ctx.Done()`. A daily-timer goroutine that is in the drift/reschedule path
at that moment (`daily.go:130` or `142-143` -> `Reschedule` -> line 122) can
write `s.Tickers` while `Stop()` iterates it. This needs a clock drift (for
example resume from suspend) coinciding with shutdown or a config reload.
`go test -race` with a drift injected would show it; no scheduler test file
exists in the tree.

`LoggerManager.Stop()` (`daemon/log/loggers/logger.go:129`) was also glanced
at, since it also resets a map (line 140). It takes `l.mu` for the whole
function, so no race is evident there. Other `Stop()` methods were not
audited.

## 3. Stray println in rfc3164 formatter

Verdict: CONFIRMED. Severity guess: low (debug leftover, log noise).

- `daemon/log/formats/rfc3164.go:50`: `println("TSK:", out)` inside the
  `case taskBase.TaskNotification:` branch of the argument loop, after the
  `TASK=... DATA=...` fragment is appended to `out`.
- `println` writes to stderr, bypassing the daemon's logger and log level.
  Each task notification formatted as RFC3164 emits one line to the
  daemon's stderr (usually the journal) containing the partial message,
  and `DATA` is `fmt.Sprint(tsk.Data)`, which may include scan results.
  Whether that is a privacy concern depends on the task, not verified.

Trigger: configure an RFC3164 (syslog) logger and have any task (for
example IOC scanner, node/pid monitor) emit a `TaskNotification`.
The matching RFC5424 formatter has no such `println` (checked with a grep of
`daemon/log` and `daemon/tasks/scheduler`).

## 4. Makefile version handling as text

Verdict: CONFIRMED (both parts). Severity guess: low (unneeded flags on
6.2 to 6.9, likely harmless; needed flags dropped on a 6.100+/10.x kernel; the
wrong kernel is picked when several are installed; the compile impact itself
was not tested).

- `ebpf_prog/Makefile:6`: `KERNEL_VER ?= $(shell find /lib/modules/* ... | sort | tail -1 | cut -d/ -f4)`.
  Plain `sort` is lexicographic, so with `6.9.1` and `6.19.4` installed it
  picks `6.9.1` (checked with `sort`). `sort -V` would be correct. Line 10
  repeats the same pattern for `KERNEL_DIR`.
- `ebpf_prog/Makefile:17`: `KERNEL_6_19_CHECK = $(shell expr "$(KERNEL_VER)" \>= "6.19")`.
  Neither operand is an integer, so `expr` falls back to string comparison.
  Local results: `6.9.1`->1 (wrong), `6.2.0`->1 (wrong), `6.19.4`->1,
  `7.2.11`->1, `6.100.0`->0 (would be wrong), `10.1`->0 (wrong).
- Effect at `Makefile:57-59`: `-Wno-microsoft-anon-tag -fms-extensions` is
  added to `EXTRA_FLAGS` when the check is 1. Kernels 6.2 to 6.9 wrongly
  get the flags (6.0, 6.1 and 6.10 to 6.18 compare correctly: checked with
  `expr`). A hypothetical 6.100 or 10.x would wrongly lose them.

Trigger: building with several kernels installed under `/lib/modules`
(wrong one selected), or building against any 6.2 to 6.9 kernel
(flag added although unneeded). The top-level `Makefile` and
`daemon/Makefile` have no `KERNEL_VER` handling.

Local note only; nothing filed upstream.
