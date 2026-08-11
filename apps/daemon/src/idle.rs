//! Recording last-access times, and deciding what counts as idle.
//!
//! This is what keeps the running containers down to the ones actually in
//! use, however many worktrees pile up.

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

/// How often the idle sweep runs.
///
/// There is nothing to gain from going faster: the smallest `idle_timeout`
/// is measured in minutes, so every 30 seconds is plenty, and it spares
/// the runtime a stream of pointless queries.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// The last time each host was accessed.
///
/// The proxy calls [`IdleTracker::touch`] on every request, so this **has
/// to be fast**: keep writes short and do no I/O.
#[derive(Default)]
pub struct IdleTracker {
    last_access: RwLock<HashMap<String, Instant>>,
    /// Hosts currently starting. Keeps concurrent requests for the same
    /// host from starting it twice.
    starting: Mutex<HashMap<String, ()>>,
    /// Services someone is sitting at, and how many sessions each has.
    ///
    /// **Not a time, unlike everything else here.** Being attached to a
    /// service's terminal is not a moment of access to be measured
    /// against a timeout — it is a state that lasts, and lasts precisely
    /// as long as the session does. A count rather than a flag, because
    /// two people may watch one service and the first to leave must not
    /// take the second's claim with them.
    in_use: Mutex<HashMap<String, usize>>,
}

impl IdleTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records an access.
    pub fn touch(&self, host: &str) {
        let now = Instant::now();

        // A read lock would do for a key that is already there, but
        // updating the value needs a write lock. Hold it as briefly as
        // possible.
        if let Ok(mut guard) = self.last_access.write() {
            guard.insert(host.to_ascii_lowercase(), now);
        }
    }

    /// How long since the last access. `None` when there is no record.
    pub fn idle_for(&self, host: &str) -> Option<Duration> {
        let guard = self.last_access.read().ok()?;
        guard.get(&host.to_ascii_lowercase()).map(|at| at.elapsed())
    }

    /// Forgets a host. Called when its service stops.
    pub fn forget(&self, host: &str) {
        if let Ok(mut guard) = self.last_access.write() {
            guard.remove(&host.to_ascii_lowercase());
        }
    }

    /// Claims the right to start a host.
    ///
    /// However many requests arrive for the same host at once, only one
    /// start happens. A successful claim returns a [`StartGuard`], which
    /// releases on drop.
    pub fn begin_start(&self, host: &str) -> Option<StartGuard<'_>> {
        let key = host.to_ascii_lowercase();
        let mut guard = self.starting.lock().ok()?;

        if guard.contains_key(&key) {
            return None;
        }

        guard.insert(key.clone(), ());
        Some(StartGuard { tracker: self, key })
    }

    /// Whether a start is in flight for this host.
    ///
    /// **The sweep has to ask.** A host being woken has no endpoint on
    /// its route yet, so anything reading "not running" as "nobody is
    /// using it" would decide that against the very request doing the
    /// waking.
    pub fn is_starting(&self, host: &str) -> bool {
        let key = host.to_ascii_lowercase();

        self.starting
            .lock()
            .is_ok_and(|guard| guard.contains_key(&key))
    }

    fn finish_start(&self, key: &str) {
        if let Ok(mut guard) = self.starting.lock() {
            guard.remove(key);
        }
    }

    /// Says a service is being used by something that sends no requests.
    ///
    /// For an attached terminal. The sweep reads times, and someone
    /// watching a task runner produces none — so without this,
    /// scale-to-zero would stop the service out from under an open
    /// session after `idle_timeout` of deliberate use.
    ///
    /// The claim lasts until the returned guard is dropped.
    pub fn begin_use(&self, service: impl Into<String>) -> UseGuard<'_> {
        let key = service.into();

        if let Ok(mut guard) = self.in_use.lock() {
            *guard.entry(key.clone()).or_insert(0) += 1;
        }

        UseGuard { tracker: self, key }
    }

    /// Whether anyone is sitting at this service.
    pub fn is_in_use(&self, service: &str) -> bool {
        self.in_use
            .lock()
            .is_ok_and(|guard| guard.contains_key(service))
    }

    fn end_use(&self, key: &str) {
        if let Ok(mut guard) = self.in_use.lock()
            && let Some(count) = guard.get_mut(key)
        {
            *count -= 1;
            if *count == 0 {
                guard.remove(key);
            }
        }
    }
}

/// Marks a service as in use. Releases on drop.
pub struct UseGuard<'a> {
    tracker: &'a IdleTracker,
    key: String,
}

impl Drop for UseGuard<'_> {
    fn drop(&mut self) {
        self.tracker.end_use(&self.key);
    }
}

/// Marks a host as starting. Releases on drop.
pub struct StartGuard<'a> {
    tracker: &'a IdleTracker,
    key: String,
}

impl Drop for StartGuard<'_> {
    fn drop(&mut self) {
        self.tracker.finish_start(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_holds_a_service_open() {
        // The sweep asks this instead of a last-access time, because
        // someone typing at a task runner produces no requests to time.
        let tracker = IdleTracker::new();
        let key = "myapp/feat-1/dev";

        assert!(!tracker.is_in_use(key));

        let session = tracker.begin_use(key);
        assert!(tracker.is_in_use(key));

        drop(session);
        assert!(!tracker.is_in_use(key), "leaving gives it back");
    }

    #[test]
    fn one_person_leaving_does_not_evict_the_other() {
        // Two attachments to one service are allowed — they share the
        // terminal — so the claim has to be counted, not set.
        let tracker = IdleTracker::new();
        let key = "myapp/feat-1/dev";

        let first = tracker.begin_use(key);
        let second = tracker.begin_use(key);

        drop(first);
        assert!(tracker.is_in_use(key), "the second is still there");

        drop(second);
        assert!(!tracker.is_in_use(key));
    }

    #[test]
    fn records_and_reports_idle_time() {
        let tracker = IdleTracker::new();
        assert_eq!(tracker.idle_for("web.myapp.localhost"), None);

        tracker.touch("web.myapp.localhost");

        let idle = tracker
            .idle_for("web.myapp.localhost")
            .expect("has a record");
        assert!(idle < Duration::from_secs(1));
    }

    #[test]
    fn host_matching_ignores_case() {
        // The casing of a Host header is up to the client.
        let tracker = IdleTracker::new();
        tracker.touch("WEB.MyApp.localhost");

        assert!(tracker.idle_for("web.myapp.localhost").is_some());
    }

    #[test]
    fn idle_time_grows_from_the_last_touch() {
        // The idle sweep reads nothing else, so touching has to reset it.
        let tracker = IdleTracker::new();
        tracker.touch("web.myapp.localhost");
        std::thread::sleep(Duration::from_millis(20));

        let before = tracker
            .idle_for("web.myapp.localhost")
            .expect("has a record");
        assert!(before >= Duration::from_millis(20));

        tracker.touch("web.myapp.localhost");
        let after = tracker
            .idle_for("web.myapp.localhost")
            .expect("has a record");
        assert!(after < before);
    }

    #[test]
    fn forget_removes_the_record() {
        let tracker = IdleTracker::new();
        tracker.touch("web.myapp.localhost");
        tracker.forget("WEB.myapp.localhost");

        assert_eq!(tracker.idle_for("web.myapp.localhost"), None);
    }

    #[test]
    fn only_one_start_can_be_in_flight() {
        // Concurrent requests for one host must start it only once.
        let tracker = IdleTracker::new();

        let first = tracker.begin_start("web.myapp.localhost");
        assert!(first.is_some());

        let second = tracker.begin_start("web.myapp.localhost");
        assert!(second.is_none(), "the second claim fails");

        // A different host starts independently.
        assert!(tracker.begin_start("api.myapp.localhost").is_some());
    }

    #[test]
    fn a_start_in_flight_is_visible_to_the_sweep() {
        // The sweep decides on routes, and a host being woken has no
        // endpoint on its yet — so without this it reads as stopped, and
        // stopped is what the sweep treats as nobody using it.
        let tracker = IdleTracker::new();
        assert!(!tracker.is_starting("web.myapp.localhost"));

        {
            let _guard = tracker
                .begin_start("web.myapp.localhost")
                .expect("claims it");

            assert!(tracker.is_starting("web.myapp.localhost"));
            // Asked with the casing a client happened to send.
            assert!(tracker.is_starting("WEB.MyApp.localhost"));
            assert!(!tracker.is_starting("api.myapp.localhost"));
        }

        assert!(
            !tracker.is_starting("web.myapp.localhost"),
            "the start is done"
        );
    }

    #[test]
    fn dropping_the_guard_allows_the_next_start() {
        let tracker = IdleTracker::new();

        {
            let _guard = tracker
                .begin_start("web.myapp.localhost")
                .expect("claims it");
        }

        assert!(
            tracker.begin_start("web.myapp.localhost").is_some(),
            "after a failed start is released, the next one may try"
        );
    }
}
