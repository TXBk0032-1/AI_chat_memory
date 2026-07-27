use crate::sync::types::EntityVersion;

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

    pub fn tick(&mut self, now_ms: i64) -> EntityVersion {
        if now_ms > self.wall_ms {
            self.wall_ms = now_ms;
            self.counter = 0;
        } else {
            self.increment_counter(self.counter);
        }
        self.version()
    }

    pub fn observe(&mut self, remote: &EntityVersion, now_ms: i64) {
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

        self.wall_ms = next_wall;
        if let Some(counter) = counter_to_increment {
            self.increment_counter(counter);
        } else {
            self.counter = 0;
        }
    }

    fn increment_counter(&mut self, counter: i64) {
        if let Some(next_counter) = counter.checked_add(1) {
            self.counter = next_counter;
            return;
        }

        self.wall_ms = self
            .wall_ms
            .checked_add(1)
            .expect("hybrid logical clock exhausted");
        self.counter = 0;
    }

    fn version(&self) -> EntityVersion {
        EntityVersion::new(self.wall_ms, self.counter, self.device_id.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::HybridClock;
    use crate::sync::types::EntityVersion;

    #[test]
    fn hlc_never_moves_back_and_remote_versions_advance_it() {
        let mut clock = HybridClock::new("device-a".into(), 1000, 0);
        assert_eq!(clock.tick(900), EntityVersion::new(1000, 1, "device-a"));
        clock.observe(&EntityVersion::new(2000, 7, "device-b"), 1500);
        assert_eq!(clock.tick(1500), EntityVersion::new(2000, 9, "device-a"));
        assert!(EntityVersion::new(2000, 9, "device-b") > EntityVersion::new(2000, 9, "device-a"));
    }

    #[test]
    fn observe_applies_each_hlc_counter_branch() {
        let mut remote_ahead = HybridClock::new("device-a".into(), 1000, 4);
        remote_ahead.observe(&EntityVersion::new(2000, 7, "device-b"), 1500);
        assert_eq!(
            remote_ahead.tick(1500),
            EntityVersion::new(2000, 9, "device-a")
        );

        let mut local_ahead = HybridClock::new("device-a".into(), 2000, 4);
        local_ahead.observe(&EntityVersion::new(1000, 7, "device-b"), 1500);
        assert_eq!(
            local_ahead.tick(1500),
            EntityVersion::new(2000, 6, "device-a")
        );

        let mut physical_ahead = HybridClock::new("device-a".into(), 1000, 4);
        physical_ahead.observe(&EntityVersion::new(1500, 7, "device-b"), 2000);
        assert_eq!(
            physical_ahead.tick(2000),
            EntityVersion::new(2000, 1, "device-a")
        );

        let mut tied = HybridClock::new("device-a".into(), 2000, 4);
        tied.observe(&EntityVersion::new(2000, 7, "device-b"), 1500);
        assert_eq!(tied.tick(1500), EntityVersion::new(2000, 9, "device-a"));
    }

    #[test]
    fn logical_counter_overflow_advances_wall_time() {
        let mut clock = HybridClock::new("device-a".into(), 1000, i64::MAX);

        assert_eq!(clock.tick(900), EntityVersion::new(1001, 0, "device-a"));
    }

    #[test]
    #[should_panic(expected = "hybrid logical clock exhausted")]
    fn total_clock_overflow_has_consistent_failure() {
        let mut clock = HybridClock::new("device-a".into(), i64::MAX, i64::MAX);

        clock.tick(i64::MAX);
    }
}
