//! Compare durable per-event commits with the batched ingestion path.
//! Run: cargo run -p archon-monitor --release --example bench_store
use archon_monitor::EventStore;
use archon_monitor_common::{EventKind, SyscallEvent};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join(format!(
        "archon-store-bench-{}-{}.db",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    let store = EventStore::open(&path)?;
    let id = store.start_build("benchmark", 0)?;
    let events: Vec<_> = (0..1000)
        .map(|_| (SyscallEvent::zeroed(EventKind::Open), None))
        .collect();
    let start = Instant::now();
    for (event, reason) in &events {
        store.record_event(id, event, reason.as_ref())?;
    }
    let individual = start.elapsed();
    let start = Instant::now();
    for chunk in events.chunks(100) {
        store.record_batch(id, chunk)?;
    }
    let batches = start.elapsed();
    assert_eq!(store.events_for_build(id)?.len(), 2000);
    println!("1000 events · individual commits: {individual:?} · 10 batches: {batches:?}");
    drop(store);
    std::fs::remove_file(path)?;
    Ok(())
}
