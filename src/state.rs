//! Plugin state tracking.
//!
//! Tracks which plugins are loaded, pending, or errored, along with timing
//! information for each load operation.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The lifecycle state of a single plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginStatus {
    /// Registered but not yet loaded (waiting for a lazy trigger or not yet
    /// reached in the load order).
    Pending,

    /// Successfully loaded.
    Loaded {
        /// How long it took to call `require()` and initialize.
        load_time: Duration,
    },

    /// Failed to load.
    Errored {
        /// Human-readable error message.
        message: String,
    },
}

impl std::fmt::Display for PluginStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Loaded { load_time } => write!(f, "loaded ({load_time:.2?})"),
            Self::Errored { message } => write!(f, "error: {message}"),
        }
    }
}

/// Per-plugin tracking entry.
#[derive(Debug, Clone)]
pub struct PluginEntry {
    pub name: String,
    pub status: PluginStatus,
}

/// Global plugin state, protected by a mutex for thread safety.
///
/// Access via [`global()`].
#[derive(Debug, Default)]
pub struct State {
    entries: HashMap<String, PluginEntry>,
}

/// Global singleton state instance.
static GLOBAL: OnceLock<Mutex<State>> = OnceLock::new();

/// Returns a reference to the global plugin state mutex.
pub fn global() -> &'static Mutex<State> {
    GLOBAL.get_or_init(|| Mutex::new(State::default()))
}

impl State {
    /// Register a plugin as pending.
    pub fn register(&mut self, name: &str) {
        self.entries.insert(
            name.to_string(),
            PluginEntry {
                name: name.to_string(),
                status: PluginStatus::Pending,
            },
        );
    }

    /// Mark a plugin as loaded with the given load time.
    pub fn mark_loaded(&mut self, name: &str, load_time: Duration) {
        if let Some(entry) = self.entries.get_mut(name) {
            entry.status = PluginStatus::Loaded { load_time };
        }
    }

    /// Mark a plugin as errored.
    pub fn mark_errored(&mut self, name: &str, message: impl Into<String>) {
        if let Some(entry) = self.entries.get_mut(name) {
            entry.status = PluginStatus::Errored {
                message: message.into(),
            };
        }
    }

    /// Reset a plugin back to pending (used before reload).
    pub fn reset(&mut self, name: &str) {
        if let Some(entry) = self.entries.get_mut(name) {
            entry.status = PluginStatus::Pending;
        }
    }

    /// Remove a plugin from tracking entirely.
    pub fn remove(&mut self, name: &str) {
        self.entries.remove(name);
    }

    /// Get the status of a specific plugin.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&PluginEntry> {
        self.entries.get(name)
    }

    /// Get all entries, sorted by name.
    #[must_use]
    pub fn all_sorted(&self) -> Vec<&PluginEntry> {
        let mut entries: Vec<&PluginEntry> = self.entries.values().collect();
        entries.sort_by_key(|e| &e.name);
        entries
    }

    /// Count plugins by status category.
    #[must_use]
    pub fn counts(&self) -> StatusCounts {
        let mut counts = StatusCounts::default();
        for entry in self.entries.values() {
            match &entry.status {
                PluginStatus::Pending => counts.pending += 1,
                PluginStatus::Loaded { .. } => counts.loaded += 1,
                PluginStatus::Errored { .. } => counts.errored += 1,
            }
        }
        counts
    }

    /// Total number of tracked plugins.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no plugins are tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Total load time across all successfully loaded plugins.
    #[must_use]
    pub fn total_load_time(&self) -> Duration {
        self.entries
            .values()
            .filter_map(|e| match &e.status {
                PluginStatus::Loaded { load_time } => Some(*load_time),
                _ => None,
            })
            .sum()
    }
}

/// Summary counts by status.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StatusCounts {
    pub loaded: usize,
    pub pending: usize,
    pub errored: usize,
}

/// Measures the duration of a closure, returning both the result and elapsed
/// time. Used by the loader to time plugin loads.
pub fn timed<F, T>(f: F) -> (T, Duration)
where
    F: FnOnce() -> T,
{
    let start = Instant::now();
    let result = f();
    (result, start.elapsed())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_get() {
        let mut state = State::default();
        state.register("foo");
        let entry = state.get("foo").unwrap();
        assert_eq!(entry.status, PluginStatus::Pending);
    }

    #[test]
    fn mark_loaded() {
        let mut state = State::default();
        state.register("foo");
        state.mark_loaded("foo", Duration::from_millis(42));
        let entry = state.get("foo").unwrap();
        assert_eq!(
            entry.status,
            PluginStatus::Loaded {
                load_time: Duration::from_millis(42)
            }
        );
    }

    #[test]
    fn mark_errored() {
        let mut state = State::default();
        state.register("foo");
        state.mark_errored("foo", "dlopen failed");
        let entry = state.get("foo").unwrap();
        assert_eq!(
            entry.status,
            PluginStatus::Errored {
                message: "dlopen failed".into()
            }
        );
    }

    #[test]
    fn reset_to_pending() {
        let mut state = State::default();
        state.register("foo");
        state.mark_loaded("foo", Duration::from_millis(10));
        state.reset("foo");
        assert_eq!(state.get("foo").unwrap().status, PluginStatus::Pending);
    }

    #[test]
    fn remove_plugin() {
        let mut state = State::default();
        state.register("foo");
        state.remove("foo");
        assert!(state.get("foo").is_none());
    }

    #[test]
    fn counts() {
        let mut state = State::default();
        state.register("a");
        state.register("b");
        state.register("c");
        state.mark_loaded("a", Duration::from_millis(10));
        state.mark_errored("c", "fail");

        let counts = state.counts();
        assert_eq!(counts.loaded, 1);
        assert_eq!(counts.pending, 1);
        assert_eq!(counts.errored, 1);
    }

    #[test]
    fn all_sorted() {
        let mut state = State::default();
        state.register("charlie");
        state.register("alpha");
        state.register("bravo");

        let sorted = state.all_sorted();
        let names: Vec<&str> = sorted.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "bravo", "charlie"]);
    }

    #[test]
    fn total_load_time() {
        let mut state = State::default();
        state.register("a");
        state.register("b");
        state.register("c");
        state.mark_loaded("a", Duration::from_millis(10));
        state.mark_loaded("b", Duration::from_millis(20));
        // c stays pending — should not contribute.

        assert_eq!(state.total_load_time(), Duration::from_millis(30));
    }

    #[test]
    fn len_and_is_empty() {
        let mut state = State::default();
        assert!(state.is_empty());
        assert_eq!(state.len(), 0);

        state.register("a");
        assert!(!state.is_empty());
        assert_eq!(state.len(), 1);
    }

    #[test]
    fn display_status() {
        assert_eq!(format!("{}", PluginStatus::Pending), "pending");
        let loaded = PluginStatus::Loaded {
            load_time: Duration::from_millis(42),
        };
        assert!(format!("{loaded}").contains("loaded"));
        let errored = PluginStatus::Errored {
            message: "oops".into(),
        };
        assert!(format!("{errored}").contains("oops"));
    }

    #[test]
    fn timed_measures_duration() {
        let (result, duration) = timed(|| 42);
        assert_eq!(result, 42);
        // Duration should be non-negative (it always is, but sanity check).
        assert!(duration.as_nanos() < 1_000_000_000);
    }

    #[test]
    fn mark_nonexistent_is_noop() {
        let mut state = State::default();
        // Should not panic.
        state.mark_loaded("nonexistent", Duration::from_millis(1));
        state.mark_errored("nonexistent", "fail");
        state.reset("nonexistent");
        state.remove("nonexistent");
    }
}
