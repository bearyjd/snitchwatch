# r15 E2: Kirigami file splits (#115)

Mechanical splits, no behaviour change. Pure moves: no logic, signature,
property or signal changes. Only visibility widens where a moved item is now
used across a module boundary (`pub(super)`), and the QML split adds the
property plumbing a separate QML file needs.

Test-set check: `cargo test -p snitchwatch-kirigami -- --list` before and
after (1101 tests before: 965 unit, 136 across the integration binaries).
Each moved test keeps its function name; where a split changes a module path
(`grouping::tests` -> `grouping::host_tests`, one integration binary -> several)
the report lists the mapping.

## Targets (each under 800 lines, aiming 200-500)

1. `src/connections_model.rs` (2343): the `#[cxx_qt::bridge]` block, the
   `ConnectionsModelRust` struct and the grouped-mode toggles stay. New
   submodules under `src/connections_model/`, joining `level_rule_actions`:
   - `roles.rs`: the `ROLE_*` ids.
   - `helpers.rs`: flush debounce, buffered-message helpers, row-to-role text,
     `grouped_entry_data`.
   - `qt_interface.rs`: row_count, data, role_names, setters, `start_bridge_feed`.
   - `apply.rs`: message application pipeline and row mutations.
   - `queries.rs`: row queries and rule invokables.
   - `tests.rs`: unit tests (same path `connections_model::tests`).
   Invokables move into child `impl qobject::ConnectionsModel` blocks as
   `pub(super)`; visible to the bridge module, so cxx-qt is unaffected.
2. `src/connections/grouping.rs` (1641): `grouping/keys.rs` (domain and key
   helpers, `registrable_domain` re-exported), `grouping/tests.rs` and
   `grouping/host_tests.rs`.
3. `src/connections/collapse.rs` (1189): tests to `collapse/tests.rs`.
4. `qml/RulesPage.qml` (882): the inspector sheet becomes
   `qml/RulesInspectorSheet.qml`, registered in `build.rs` and the guards'
   `ALL_QML`. It reaches the page through a `host` property; the page keeps
   the `inspector*` aliases the probes use. Every `objectName` is unchanged.
5. `src/bridge_dispatch.rs` (843): tests to `bridge_dispatch/tests.rs`.
6. `tests/honest_ui_qml_guards.rs` (1237): split by topic into several test
   files sharing `qml_guard_support`. Guards that grep `RulesPage.qml` also
   read the new inspector file; none is loosened.

## Out of scope (still over 800 lines)

`src/connections/row_store.rs` (988), `src/bridge_runtime.rs` and its tests,
`src/make_rule.rs`, `src/app_icons.rs`.
