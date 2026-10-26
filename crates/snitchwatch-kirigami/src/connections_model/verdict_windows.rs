//! The coarse tick behind the group headers' "last hour" / "today" verdict
//! counts (phase 0b). The tree keeps the window buckets
//! (`connections::grouping::windows`); this reads the wall clock, rolls the
//! cached projection's header figures forward and signals only the headers
//! whose figures moved, so a tick never rescans rows or resets the view.

use cxx_qt_lib::{QDateTime, QTime};

use super::*;
use crate::connections::grouping::{Clock, GroupCounts};

/// Now, and the local midnight that starts today, in local time, so a DST
/// change moves midnight correctly. Qt's local time is the C library's
/// (`TZ`, else the contents of `/etc/localtime`, symlink or not), the same
/// source QML's `Date` and `Qt.formatTime` use for every other local time the
/// Connections page shows. Not `QTimeZone::systemTimeZone()`: it needs an
/// IANA id and fell back to UTC in a sandbox whose `/etc/localtime` is a
/// plain file (the connections_verdict_counts_qml probe checks the two agree).
pub(super) fn local_clock() -> Clock {
    let mut midnight = QDateTime::current_date_time();
    let now_ms = midnight.to_msecs_since_epoch();
    midnight.set_time(QTime::new(0, 0, 0, 0));
    Clock {
        now_ms,
        today_start_ms: midnight.to_msecs_since_epoch(),
    }
}

/// `groupLastHour` / `groupToday` for a header.
pub(super) fn window_role(counts: &GroupCounts, role: i32) -> QVariant {
    let window = if role == ROLE_GROUP_TODAY {
        &counts.windows.today
    } else {
        &counts.windows.last_hour
    };
    QVariant::from(&QString::from(&window.segments_json()))
}

impl qobject::ConnectionsModel {
    pub(super) fn refresh_verdict_windows(mut self: Pin<&mut Self>) {
        // Buffered messages land first, as for a toggle, so the figures
        // and the projection agree.
        self.as_mut().flush_grouped_messages();
        let changed = {
            let mut rust = self.as_mut().rust_mut();
            let ConnectionsModelRust {
                grouping,
                grouped_projection,
                grouped,
                ..
            } = &mut *rust;
            grouping.set_clock(local_clock());
            if *grouped {
                grouping.refresh_window_figures(grouped_projection)
            } else {
                Vec::new()
            }
        };
        let mut roles = QList::<i32>::default();
        roles.append(ROLE_GROUP_LAST_HOUR);
        roles.append(ROLE_GROUP_TODAY);
        let no_parent = QModelIndex::default();
        for i in changed {
            let index = self.index(i as i32, 0, &no_parent);
            self.as_mut().data_changed(&index, &index, &roles);
        }
    }
}
