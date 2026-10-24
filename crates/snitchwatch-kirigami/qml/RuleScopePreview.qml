// One description for the same level/program/key and duration sent to Rust.
import QtQuick
import com.snitchwatch.shell

QtObject {
    function describe(model, level, program, key, duration) {
        const scope = model && typeof model.levelRuleScopeText === "function"
            ? model.levelRuleScopeText(level, program, key) : "Scope unavailable";
        return scope + ". Duration: " + Durations.labelFor(duration) + ".";
    }
}
