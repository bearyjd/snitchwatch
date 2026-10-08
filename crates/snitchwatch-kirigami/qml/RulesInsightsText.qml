// The wording of the Rules page's hit counts and rule analysis (P2.6), apart
// from the page so each stays readable. Plain strings only: the page shows
// them in PlainText labels, and none says a rule was or will be removed or
// changed. See RulesPage.qml's header comment for what each claims.
import QtQuick

QtObject {
    id: root

    function formatTime(ms) {
        return new Date(ms).toLocaleString(Qt.locale(), Locale.ShortFormat);
    }

    // Empty when this bridge sends no counts.
    function hitsSummaryText(info) {
        if (!info || !info.available) return "";
        if (!info.counting) {
            return "Hit counts start when the firewall first reports statistics.";
        }
        let text = "Hits counted by Snitchwatch since " + root.formatTime(info.sinceMs)
            + "; approximate.";
        if (info.lossy) {
            // The gap is a moment, not a state: hits before it may be missing,
            // and a long-past one says nothing is known since.
            if (info.lastGapMs > 0) {
                text += " Hits may be missing before " + root.formatTime(info.lastGapMs) + ".";
                if (Date.now() - info.lastGapMs >= 14 * 86400000) {
                    text += " No gap noticed since.";
                }
            } else {
                text += " Some hits may be missing.";
            }
        }
        return text;
    }

    function hitsStorageText(info) {
        if (!info || !info.available || info.persistent) return "";
        return info.storageReason.length > 0
            ? "Hit counts are not saved across restarts: " + info.storageReason
            : "Hit counts are not saved across restarts.";
    }

    function hitsRowText(counted, count, lastMs, note, badgeKind, badgeMs) {
        if (note.length > 0) return note;
        if (!counted) return "";
        if (count === 0) {
            switch (badgeKind) {
            case "unused":
                return "Unused: no hits counted in the last 14 days";
            case "since":
                return "No hits since " + root.formatTime(badgeMs);
            case "sinceMissed":
                return "No hits since " + root.formatTime(badgeMs)
                    + "; some may have been missed";
            default:
                return "No hits counted";
            }
        }
        return count + (count === 1 ? " hit" : " hits")
            + (lastMs > 0 ? ", last " + root.formatTime(lastMs) : "");
    }

    function analysisText(info) {
        if (!info) return "";
        switch (info.state) {
        case "running":
            return "Analyzing rules...";
        case "tooMany":
            return "Too many rules to analyze: " + info.enabled + " are enabled and the limit is "
                + info.limit + ".";
        case "stale":
            return "The rules changed after the analysis. Choose Analyze rules to run it again.";
        case "done": {
            const found = info.neverDecides + info.mayBeShadowed;
            const caveat = " Snitchwatch checks only conditions it can compare exactly.";
            return found === 0
                ? "No rules found that can never decide a connection." + caveat
                : found + (found === 1 ? " rule" : " rules")
                    + " may never decide a connection (marked below)." + caveat;
        }
        default:
            return "";
        }
    }
}
