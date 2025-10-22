use chrono::{DateTime, Utc};

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
    fn now_rfc3339(&self) -> String {
        self.now().to_rfc3339()
    }
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[cfg(test)]
pub mod test {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    pub struct MockClock {
        time: Arc<Mutex<DateTime<Utc>>>,
    }

    impl MockClock {
        pub fn new(time: DateTime<Utc>) -> Self {
            Self {
                time: Arc::new(Mutex::new(time)),
            }
        }

        pub fn fixed(timestamp: &str) -> Self {
            let time = DateTime::parse_from_rfc3339(timestamp)
                .expect("Invalid timestamp")
                .with_timezone(&Utc);
            Self::new(time)
        }

        pub fn set_time(&self, time: DateTime<Utc>) {
            *self.time.lock().unwrap() = time;
        }

        pub fn advance_secs(&self, seconds: i64) {
            let mut time = self.time.lock().unwrap();
            *time = *time + chrono::Duration::seconds(seconds);
        }
    }

    impl Clock for MockClock {
        fn now(&self) -> DateTime<Utc> {
            *self.time.lock().unwrap()
        }
    }

    impl Default for MockClock {
        fn default() -> Self {
            Self::fixed("2024-01-01T00:00:00Z")
        }
    }
}
