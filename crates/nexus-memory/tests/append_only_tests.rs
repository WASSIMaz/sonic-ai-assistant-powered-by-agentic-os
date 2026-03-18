//! append_only_tests.rs — Exit gate tests for the APPEND_ONLY region.

/// Write 151 snapshots with sequential timestamp_ns.
/// Call recent(150). Verify first element has timestamp_ns == 2.
/// Verify last element has timestamp_ns == 151 (slot 0 silently overwritten).
#[test]
fn test_ring_wrap_silently_overwrites_oldest() {
    todo!()
}

/// One writer at full speed. One reader calling recent(10) every 1ms for 5 seconds.
/// Verify SeqLock retry triggers when ring laps the reader.
/// Verify no returned snapshot has timestamps violating monotonic order.
#[test]
fn test_seqlock_detects_and_retries_lapped_read() {
    todo!()
}

/// Write 150 snapshots. Snapshot write_index. Sleep for 150+ more writes.
/// Attempt to read original slot. Verify ReadResult::RingLapped is returned.
/// Verify partial data from overwritten slot is never returned.
#[test]
fn test_full_lap_returns_ring_lapped_not_partial_data() {
    todo!()
}

/// Write 150 events to EventLogBuffer without draining.
/// Start flush thread. Wait 600ms (one full flush cycle + buffer).
/// Verify all 150 events appear in security_events table in nexus.db.
/// Confirms sqlite_ready gate and flush coordination work end-to-end.
#[test]
fn test_flush_thread_drains_to_sqlite_within_500ms() {
    todo!()
}

/// Pre-SQLite emergency buffer promotion:
/// Pause boot sequence after APPEND_ONLY mapped but before sqlite_ready.
/// Trigger NamespaceViolation USE event (accesses emergency buffer).
/// Release boot pause. Let SQLite initialize.
/// Verify USE event promoted as first operation before flush thread fires.
/// Query security_events — event present with all fields intact.
#[test]
fn test_emergency_buffer_promoted_before_first_flush_cycle() {
    todo!()
}