use maestro_store::{Paths, Store, TreeSource};
use std::path::PathBuf;

fn fixture() -> Store {
    let mut paths = Paths::under("/nonexistent/maestro-health-test");
    paths.fleet_meta = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/health");
    Store::new(paths, TreeSource::WorkingTree)
}

#[test]
fn telemetry_reads_each_fixture() {
    let store = fixture();
    assert_eq!(store.health_census().data.unwrap().host_load["mac"]["live"], 2);
    assert_eq!(store.health_capacity().data.unwrap().lanes.len(), 1);
    assert_eq!(store.health_loops().data.unwrap().loops["census"]["status"], "ok");
    assert_eq!(store.health_history().data.unwrap().len(), 2);
    assert!(store.health_log().data.unwrap().contains("green->red"));
}

#[test]
fn missing_and_malformed_sources_are_unavailable() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(Paths::under(temp.path()), TreeSource::WorkingTree);
    assert!(store.health_census().data.is_none());
    assert!(store.health_census().is_degraded());
    std::fs::create_dir_all(&store.paths.fleet_meta).unwrap();
    std::fs::write(store.paths.fleet_meta.join("capacity.json"), "{}").unwrap();
    assert!(store.health_capacity().data.is_none());
    std::fs::write(store.paths.fleet_meta.join("capacity-history.jsonl"), "broken\n").unwrap();
    let history = store.health_history();
    assert!(history.is_degraded());
    assert_eq!(history.errors.len(), 1);
}
