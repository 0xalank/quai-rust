//! Minimal leveled logging to standard error. Results go to standard output.
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}
impl Level {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "error" => Some(Self::Error),
            "warn" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            _ => None,
        }
    }
    fn tag(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN ",
            Self::Info => "INFO ",
            Self::Debug => "DEBUG",
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Log {
    pub level: Level,
}
impl Log {
    pub fn new(level: Level) -> Self {
        Self { level }
    }
    pub fn enabled(&self, level: Level) -> bool {
        level <= self.level
    }
    pub fn write(&self, level: Level, message: &str) {
        if !self.enabled(level) {
            return;
        }
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let mut err = std::io::stderr().lock();
        let _ = writeln!(err, "{} {t:.3} {message}", level.tag());
    }
    pub fn error(&self, m: &str) {
        self.write(Level::Error, m)
    }
    pub fn warn(&self, m: &str) {
        self.write(Level::Warn, m)
    }
    pub fn info(&self, m: &str) {
        self.write(Level::Info, m)
    }
    pub fn debug(&self, m: &str) {
        self.write(Level::Debug, m)
    }
}
