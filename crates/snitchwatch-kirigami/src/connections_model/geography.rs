//! Geographic scope, retained prepared facts, and selection queries.
use super::*;

impl qobject::ConnectionsModel {
    pub(super) fn set_geo_scope(mut self: Pin<&mut Self>, kind: &QString, key: &QString) -> bool {
        let (kind, key) = (kind.to_string(), key.to_string());
        let scope = if kind.is_empty() && key.is_empty() {
            None
        } else {
            let Some(scope) = GeoScope::parse(&kind, &key) else {
                return false;
            };
            Some(scope)
        };
        let mut filter = self.store.filter().clone();
        filter.set_geo_scope(scope);
        // Flush/rebuild also preserves all existing search/filter options.
        self.as_mut().apply_filter(filter);
        if kind == "ip" {
            let retained = self
                .store
                .row_by_id(&self.current_row_id)
                .filter(|row| self.store.filter().matches(row))
                .map(|row| row.id.clone());
            let selected = retained.or_else(|| {
                self.store
                    .rows()
                    .iter()
                    .find(|row| self.store.filter().matches(row))
                    .map(|row| row.id.clone())
            });
            if let Some(id) = selected {
                if self.grouped {
                    self.as_mut().rust_mut().grouping.reveal_row(&id);
                    let projection = self.compute_grouped_projection();
                    self.as_mut().apply_grouped_projection(projection);
                    self.as_mut().refresh_counts();
                }
                self.geographic_selection_requested(QString::from(&id));
            }
        }
        true
    }

    pub(super) fn destination_row_ids_json(&self, index: i32) -> QString {
        let Ok(index) = usize::try_from(index) else {
            return QString::from("[]");
        };
        let ids = if self.grouped {
            match self.grouped_projection.get(index) {
                Some(VisibleEntry::Row { id, .. }) => vec![id.clone()],
                Some(VisibleEntry::ProcessHeader { key, .. }) => {
                    self.grouping.row_ids_for_group(key, 0, "", "")
                }
                Some(VisibleEntry::DomainHeader {
                    process_key, key, ..
                }) => self.grouping.row_ids_for_group(process_key, 1, key, ""),
                Some(VisibleEntry::HostHeader {
                    process_key,
                    domain_key,
                    key,
                    ..
                }) => self
                    .grouping
                    .row_ids_for_group(process_key, 2, key, domain_key),
                None => Vec::new(),
            }
        } else {
            self.store
                .visible_row(index)
                .map(|row| vec![row.id.clone()])
                .unwrap_or_default()
        };
        // Resolve membership in one store pass. Calling row_by_id for every
        // descendant would make a large app selection quadratic on Qt.
        let selected: std::collections::HashSet<_> = ids.iter().map(String::as_str).collect();
        let matching: std::collections::HashSet<_> = self
            .store
            .rows()
            .iter()
            .filter(|row| selected.contains(row.id.as_str()) && self.store.filter().matches(row))
            .map(|row| row.id.as_str())
            .collect();
        // Keep the process/domain/host traversal order exposed before this
        // optimization; hash iteration never determines the returned order.
        let ids: Vec<_> = ids
            .into_iter()
            .filter(|id| matching.contains(id.as_str()))
            .collect();
        QString::from(&serde_json::to_string(&ids).expect("connection IDs serialize"))
    }

    pub(super) fn install_prepared_geo(
        mut self: Pin<&mut Self>,
        prepared: crate::geo::prepared::PreparedGeoMessage,
    ) {
        self.as_mut()
            .rust_mut()
            .store
            .merge_prepared_geo(prepared.reset, prepared.resolutions);
    }

    pub(super) fn prepare_fixture_geo(mut self: Pin<&mut Self>, message: &ServerMessage) {
        let prepared = self
            .as_mut()
            .rust_mut()
            .fixture_geo_preparer
            .get_or_insert_with(|| {
                crate::geo::prepared::GeoMessagePreparer::new(crate::geo::shared_resolver())
            })
            .prepare(0, message);
        self.install_prepared_geo(prepared);
    }
}
