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

#[test]
fn timestamps_and_bad_rows_do_not_disappear() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(Paths::under(temp.path()), TreeSource::WorkingTree);
    std::fs::create_dir_all(&store.paths.fleet_meta).unwrap();
    std::fs::write(store.paths.fleet_meta.join("capacity-history.jsonl"),
        "{\"at\":\"invalid\",\"lane\":\"x\",\"used\":20,\"readable\":true}\n{\"at\":\"2026-11-23T12:00:00Z\",\"lane\":\"x\",\"used\":20,\"readable\":true}\n").unwrap();
    let rows = store.health_history();
    assert!(rows.is_degraded());
    assert_eq!(rows.errors.len(), 1);
    assert_eq!(rows.data.unwrap().len(), 1);
    std::fs::write(
        store.paths.fleet_meta.join("signals.jsonl"),
        "{\"at\":\"2026-11-23T12:00:00Z\",\"host\":\"mac\",\"instance\":\"codex\",\"to\":\"down\"}\nbroken\n",
    )
    .unwrap();
    let signals = store.health_signals();
    assert!(signals.is_degraded());
    assert_eq!(signals.data.unwrap().len(), 1);
}

#[test]
fn growing_telemetry_is_bounded_and_invalid_utf8_is_unavailable() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(Paths::under(temp.path()), TreeSource::WorkingTree);
    std::fs::create_dir_all(&store.paths.fleet_meta).unwrap();
    let log = store.paths.fleet_meta.join("capacity.log");
    std::fs::write(&log, format!("{}\nlatest\n", "x".repeat(2 * 1024 * 1024))).unwrap();
    let read = store.health_log();
    assert!(read.source.contains("earlier records not read"));
    assert_eq!(read.data.unwrap(), "latest\n");
    std::fs::write(&log, [255, 254, b'\n']).unwrap();
    assert!(store.health_log().data.is_none());
    std::fs::write(store.paths.fleet_meta.join("capacity.json"), "x".repeat(maestro_store::tree::MAX_READ + 1))
        .unwrap();
    assert!(store.health_capacity().data.is_none());
}
