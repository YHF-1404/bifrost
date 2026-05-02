//! Strongly-typed identifiers used across the control plane.
//!
//! Newtypes around integers prevent accidentally passing a `ConnId`
//! where a `SessionId` is expected; both are cheap `Copy`-able values.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Identifies a single accepted TCP connection (one per `ConnTask`).
///
/// Distinct from a session: one client may produce multiple `ConnId`s
/// over its lifetime (every reconnect creates a fresh one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnId(pub u64);

/// Identifies a virtual-network membership instance.
///
/// One `(client_uuid, net_uuid)` pair maps to exactly one `SessionId`
/// for its entire lifetime — across disconnect / reconnect — until the
/// session reaches the terminal `Dead` state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub u64);

impl fmt::Display for ConnId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "conn#{}", self.0)
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sid#{}", self.0)
    }
}

/// Monotonic-id allocator. Cheap; no contention beyond a single CAS.
#[derive(Debug, Default)]
pub struct IdAllocator {
    next: AtomicU64,
}

impl IdAllocator {
    /// Build an allocator that starts at `start` (inclusive).
    pub const fn starting_at(start: u64) -> Self {
        Self {
            next: AtomicU64::new(start),
        }
    }

    pub fn next_session(&self) -> SessionId {
        SessionId(self.next.fetch_add(1, Ordering::Relaxed))
    }

    pub fn next_conn(&self) -> ConnId {
        ConnId(self.next.fetch_add(1, Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocator_is_monotonic() {
        let a = IdAllocator::starting_at(1);
        assert_eq!(a.next_session(), SessionId(1));
        assert_eq!(a.next_conn(), ConnId(2));
        assert_eq!(a.next_session(), SessionId(3));
    }

    #[test]
    fn display_is_human_readable() {
        assert_eq!(format!("{}", SessionId(7)), "sid#7");
        assert_eq!(format!("{}", ConnId(7)), "conn#7");
    }
}
