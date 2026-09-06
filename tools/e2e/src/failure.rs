//! The one error type a scenario returns. A failure names what was
//! expected in words a reader who did not run the scenario can act on.

use std::fmt::{self, Display};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub message: String,
}

impl Failure {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Failure {}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        Self::new(message)
    }
}

impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        Self::new(format!("io error: {error}"))
    }
}

impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        Self::new(format!("json error: {error}"))
    }
}

/// `fail!("S3: {} intents captured", n)` returns early with a [`Failure`].
#[macro_export]
macro_rules! fail {
    ($($arg:tt)*) => {
        return Err($crate::Failure::new(format!($($arg)*)))
    };
}

/// `ensure!(cond, "message {}", x)` fails the scenario when `cond` is false.
#[macro_export]
macro_rules! ensure {
    ($cond:expr, $($arg:tt)*) => {
        if !($cond) {
            return Err($crate::Failure::new(format!($($arg)*)));
        }
    };
}
