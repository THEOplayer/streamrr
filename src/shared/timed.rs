use chrono::{DateTime, Utc};

/// A value with a timestamp.
pub struct Timed<T> {
    pub value: T,
    pub time: DateTime<Utc>,
}

impl<T> Timed<T> {
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Timed<U> {
        Timed {
            value: f(self.value),
            time: self.time,
        }
    }

    pub fn and_then<U, E>(self, f: impl FnOnce(T) -> Result<U, E>) -> Result<Timed<U>, E> {
        Ok(Timed {
            value: f(self.value)?,
            time: self.time,
        })
    }
}
