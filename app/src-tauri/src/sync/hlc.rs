use crate::sync::types::EntityVersion;
use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HlcError {
    Exhausted,
}

impl fmt::Display for HlcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exhausted => formatter.write_str("hybrid logical clock exhausted"),
        }
    }
}

impl Error for HlcError {}

#[derive(Debug, Clone)]
pub struct HybridClock {
    device_id: String,
    wall_ms: i64,
    counter: i64,
}

impl HybridClock {
    pub fn new(device_id: String, wall_ms: i64, counter: i64) -> Self {
        Self {
            device_id,
            wall_ms,
            counter,
        }
    }

    pub fn tick(&mut self, now_ms: i64) -> Result<EntityVersion, HlcError> {
        let (next_wall, next_counter) = if now_ms > self.wall_ms {
            (now_ms, 0)
        } else {
            Self::increment(self.wall_ms, self.counter)?
        };

        self.wall_ms = next_wall;
        self.counter = next_counter;
        Ok(self.version())
    }

    pub fn observe(&mut self, remote: &EntityVersion, now_ms: i64) -> Result<(), HlcError> {
        let next_wall = self.wall_ms.max(remote.wall_ms).max(now_ms);
        let counter_to_increment = if next_wall == self.wall_ms && next_wall == remote.wall_ms {
            Some(self.counter.max(remote.counter))
        } else if next_wall == self.wall_ms {
            Some(self.counter)
        } else if next_wall == remote.wall_ms {
            Some(remote.counter)
        } else {
            None
        };

        let (next_wall, next_counter) = if let Some(counter) = counter_to_increment {
            Self::increment(next_wall, counter)?
        } else {
            (next_wall, 0)
        };

        self.wall_ms = next_wall;
        self.counter = next_counter;
        Ok(())
    }

    pub fn state(&self) -> (i64, i64) {
        (self.wall_ms, self.counter)
    }

    fn increment(wall_ms: i64, counter: i64) -> Result<(i64, i64), HlcError> {
        if let Some(next_counter) = counter.checked_add(1) {
            return Ok((wall_ms, next_counter));
        }

        wall_ms
            .checked_add(1)
            .map(|next_wall| (next_wall, 0))
            .ok_or(HlcError::Exhausted)
    }

    fn version(&self) -> EntityVersion {
        EntityVersion::new(self.wall_ms, self.counter, self.device_id.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::{HlcError, HybridClock};
    use crate::sync::types::EntityVersion;

    #[test]
    fn hlc_never_moves_back_and_remote_versions_advance_it() {
        let mut clock = HybridClock::new("device-a".into(), 1000, 0);
        assert_eq!(
            clock.tick(900).unwrap(),
            EntityVersion::new(1000, 1, "device-a")
        );
        clock
            .observe(&EntityVersion::new(2000, 7, "device-b"), 1500)
            .unwrap();
        assert_eq!(
            clock.tick(1500).unwrap(),
            EntityVersion::new(2000, 9, "device-a")
        );
        assert!(EntityVersion::new(2000, 9, "device-b") > EntityVersion::new(2000, 9, "device-a"));
    }

    #[test]
    fn observe_applies_each_hlc_counter_branch() {
        let mut remote_ahead = HybridClock::new("device-a".into(), 1000, 4);
        remote_ahead
            .observe(&EntityVersion::new(2000, 7, "device-b"), 1500)
            .unwrap();
        assert_eq!(
            remote_ahead.tick(1500).unwrap(),
            EntityVersion::new(2000, 9, "device-a")
        );

        let mut local_ahead = HybridClock::new("device-a".into(), 2000, 4);
        local_ahead
            .observe(&EntityVersion::new(1000, 7, "device-b"), 1500)
            .unwrap();
        assert_eq!(
            local_ahead.tick(1500).unwrap(),
            EntityVersion::new(2000, 6, "device-a")
        );

        let mut physical_ahead = HybridClock::new("device-a".into(), 1000, 4);
        physical_ahead
            .observe(&EntityVersion::new(1500, 7, "device-b"), 2000)
            .unwrap();
        assert_eq!(
            physical_ahead.tick(2000).unwrap(),
            EntityVersion::new(2000, 1, "device-a")
        );

        let mut tied = HybridClock::new("device-a".into(), 2000, 4);
        tied.observe(&EntityVersion::new(2000, 7, "device-b"), 1500)
            .unwrap();
        assert_eq!(
            tied.tick(1500).unwrap(),
            EntityVersion::new(2000, 9, "device-a")
        );
    }

    #[test]
    fn logical_counter_overflow_advances_wall_time() {
        let mut clock = HybridClock::new("device-a".into(), 1000, i64::MAX);

        assert_eq!(
            clock.tick(900).unwrap(),
            EntityVersion::new(1001, 0, "device-a")
        );
    }

    #[test]
    fn exhausted_clock_returns_typed_error() {
        let mut clock = HybridClock::new("device-a".into(), i64::MAX, i64::MAX);

        assert_eq!(clock.tick(i64::MAX), Err(HlcError::Exhausted));
    }

    #[test]
    fn maximum_counter_transitions_are_explicit_and_never_panic() {
        let terminal = EntityVersion::new(i64::MAX, i64::MAX, "device-a");

        let mut ticking = HybridClock::new("device-a".into(), i64::MAX, i64::MAX - 1);
        assert_eq!(ticking.tick(i64::MAX), Ok(terminal.clone()));
        assert_eq!(ticking.tick(i64::MAX), Err(HlcError::Exhausted));

        let mut observing = HybridClock::new("device-a".into(), 1000, 0);
        observing
            .observe(
                &EntityVersion::new(i64::MAX, i64::MAX - 1, "device-b"),
                1000,
            )
            .unwrap();
        assert_eq!(observing.tick(i64::MAX), Err(HlcError::Exhausted));

        let mut terminal_remote = HybridClock::new("device-a".into(), 1000, 0);
        assert_eq!(
            terminal_remote.observe(&terminal, 1000),
            Err(HlcError::Exhausted)
        );
    }

    #[test]
    fn entity_version_ordering_uses_wall_counter_then_device_id() {
        assert!(EntityVersion::new(2, 0, "device-a") > EntityVersion::new(1, i64::MAX, "device-z"));
        assert!(EntityVersion::new(1, 2, "device-a") > EntityVersion::new(1, 1, "device-z"));
        assert!(EntityVersion::new(1, 1, "device-b") > EntityVersion::new(1, 1, "device-a"));
    }
}
