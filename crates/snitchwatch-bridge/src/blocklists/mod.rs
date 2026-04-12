//! Bridge-owned blocklist subscriptions, fetch loop, and rule materialization.
//!
//! opensnitchd has no native blocklist concept. The bridge stores subscription
//! URLs in SQLite, fetches them on a schedule, parses hosts-file / domains /
//! ABP-style lists, and materializes each entry as an opensnitchd deny rule in
//! the `900–999` specificity band tagged `__source: blocklist:<id>`.

pub mod fetcher;
pub mod format;
pub mod materializer;
pub mod store;

#[cfg(test)]
mod tests {
    #[test]
    fn module_exports_compile() {
        // Pure compile-time guarantee — fails at link if any submodule is missing.
        let _ = std::any::type_name::<super::store::Subscription>();
    }
}
