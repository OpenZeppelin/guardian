/// Pins tracing-core out of its single-dispatcher fast path for the duration
/// of a log-capture test. Hold the returned dispatch across
/// `tracing::subscriber::with_default`.
///
/// While at most one dispatcher is registered, tracing-core computes a
/// callsite's interest via the *current thread's* default dispatcher
/// (`Rebuilder::JustOne`). A parallel test that hits a shared callsite first
/// — with no subscriber on its thread — then caches `Interest::never`, and
/// the capturing thread's own events at that callsite are silently dropped.
/// With a second dispatcher registered, interest is computed against the
/// dispatcher registry, which includes the subscriber under test, so shared
/// callsites resolve to at least `Interest::sometimes`.
pub fn dispatcher_registry_pin() -> tracing::Dispatch {
    tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default())
}
