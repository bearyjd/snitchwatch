//! `ConnectionsModel`'s grouped-mode switches: flat/grouped and the
//! program, domain and host expand/collapse toggles.

use super::*;

impl qobject::ConnectionsModel {
    /// Switch between grouped and flat views. A no-op if already in the
    /// requested mode. Bracketed by a reset since the entire visible row set
    /// changes shape.
    pub(super) fn set_grouped_mode(mut self: Pin<&mut Self>, grouped: bool) {
        if grouped == self.grouped {
            return;
        }
        // Buffered grouped-mode messages must apply before the mode flips so
        // the flat view starts from current data.
        self.as_mut().flush_grouped_messages();
        unsafe {
            self.as_mut().begin_reset_model();
        }
        self.as_mut().set_grouped(grouped);
        if grouped {
            self.as_mut().rebuild_grouped_projection();
        }
        unsafe {
            self.as_mut().end_reset_model();
        }
    }

    /// Toggle a process group's expand/collapse state and refresh the
    /// grouped projection. No-op when not currently in grouped mode.
    pub(super) fn toggle_process_group(mut self: Pin<&mut Self>, key: &QString) {
        if !self.grouped {
            return;
        }
        self.as_mut().flush_grouped_messages();
        self.as_mut()
            .rust_mut()
            .grouping
            .toggle_process(&key.to_string());
        let projection = self.compute_grouped_projection();
        self.as_mut().apply_grouped_projection(projection);
        self.refresh_counts();
    }

    /// Toggle a domain subgroup's expand/collapse state within its parent
    /// process group and refresh the grouped projection. No-op when not
    /// currently in grouped mode.
    pub(super) fn toggle_domain_group(
        mut self: Pin<&mut Self>,
        process_key: &QString,
        domain_key: &QString,
    ) {
        if !self.grouped {
            return;
        }
        self.as_mut().flush_grouped_messages();
        self.as_mut()
            .rust_mut()
            .grouping
            .toggle_domain(&process_key.to_string(), &domain_key.to_string());
        let projection = self.compute_grouped_projection();
        self.as_mut().apply_grouped_projection(projection);
        self.refresh_counts();
    }

    /// Toggle a host's expand/collapse state and refresh the grouped
    /// projection. No-op when not currently in grouped mode.
    pub(super) fn toggle_host_group(
        mut self: Pin<&mut Self>,
        process_key: &QString,
        domain_key: &QString,
        host_key: &QString,
    ) {
        if !self.grouped {
            return;
        }
        self.as_mut().flush_grouped_messages();
        self.as_mut().rust_mut().grouping.toggle_host(
            &process_key.to_string(),
            &domain_key.to_string(),
            &host_key.to_string(),
        );
        let projection = self.compute_grouped_projection();
        self.as_mut().apply_grouped_projection(projection);
        self.refresh_counts();
    }
}
