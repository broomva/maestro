//! The one clock the flow ledger reads: the wall clock. The storage layer stamps each event's
//! `at` with [`now`], and the kernel's writers judge leases and freshness by it and stamp a run's
//! start with it. No write verb takes a clock from its caller, so nothing a caller passes can make
//! an observation or a lease look fresher or less expired than the wall clock says.
//!
//! [`now`] is one code path in every build: it reads the wall clock. Only in this crate's own
//! unit-test binary (`cfg(test)`) does it then pass that reading through a test clock, which a
//! test thread installs to set or shift what [`now`] reads on that thread (`kernel::clocked`).
//! `cfg(test)` is set for no other build: not a dependent crate, not its tests, not this crate's
//! integration tests, which link the library as every caller does. None of them can name or reach
//! the test clock.

use jiff::Timestamp;

/// The wall clock, as the storage layer and the writers read it.
pub fn now() -> Timestamp {
    let wall = Timestamp::now();
    // The crate's own unit tests only: what the calling thread's test clock reads instead.
    #[cfg(test)]
    let wall = test_clock::read(wall);
    wall
}

#[cfg(test)]
pub(crate) use test_clock::TestClock;

#[cfg(test)]
mod test_clock {
    use jiff::{SignedDuration, Timestamp};
    use std::cell::RefCell;
    use std::sync::{Arc, Mutex, PoisonError};

    #[derive(Debug, Clone, Copy)]
    enum Reading {
        /// The wall clock, shifted.
        Wall(SignedDuration),
        /// A fixed time, until set or shifted again.
        Fixed(Timestamp),
    }

    thread_local! {
        static INSTALLED: RefCell<Option<TestClock>> = const { RefCell::new(None) };
    }

    /// A clock for tests: the wall clock until [`TestClock::set`] fixes it or
    /// [`TestClock::shift`] moves it. Clones share it, so a thread a test spawns can move the
    /// clock while a command runs on the test's own thread.
    #[derive(Debug, Clone)]
    pub struct TestClock(Arc<Mutex<Reading>>);

    impl TestClock {
        /// A clock that reads the wall clock, installed on the calling thread in place of any
        /// other.
        pub fn install() -> TestClock {
            let clock = TestClock(Arc::new(Mutex::new(Reading::Wall(SignedDuration::ZERO))));
            INSTALLED.with(|c| *c.borrow_mut() = Some(clock.clone()));
            clock
        }

        /// Reads `at` from now on, until set or shifted again.
        pub fn set(&self, at: Timestamp) {
            *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Reading::Fixed(at);
        }

        /// Moves the clock by `by`, forward or back.
        pub fn shift(&self, by: SignedDuration) {
            let mut reading = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            *reading = match *reading {
                Reading::Wall(skew) => Reading::Wall(skew.saturating_add(by)),
                Reading::Fixed(at) => Reading::Fixed(at.checked_add(by).unwrap_or(at)),
            };
        }

        fn at(&self, wall: Timestamp) -> Timestamp {
            match *self.0.lock().unwrap_or_else(PoisonError::into_inner) {
                Reading::Wall(skew) => wall.checked_add(skew).unwrap_or(wall),
                Reading::Fixed(at) => at,
            }
        }
    }

    /// What the calling thread's clock reads: its installed [`TestClock`], or the wall clock.
    pub(super) fn read(wall: Timestamp) -> Timestamp {
        INSTALLED.with(|c| c.borrow().as_ref().map_or(wall, |clock| clock.at(wall)))
    }
}
