use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

pub const fn to_log_level(level: LogLevel) -> log::Level {
    match level {
        LogLevel::Debug => log::Level::Debug,
        LogLevel::Info => log::Level::Info,
        LogLevel::Warn => log::Level::Warn,
        LogLevel::Error | LogLevel::Fatal => log::Level::Error,
    }
}

pub trait Log {
    fn is_enabled(&self, level: LogLevel) -> bool;

    fn log_message(&self, level: LogLevel, message: &str, err: Option<&dyn std::error::Error>);

    fn log_fmt(&self, level: LogLevel, args: fmt::Arguments<'_>) {
        if self.is_enabled(level) {
            self.log_message(level, &args.to_string(), None);
        }
    }

    fn debug(&self, message: &str) {
        self.log_message(LogLevel::Debug, message, None);
    }

    fn info(&self, message: &str) {
        self.log_message(LogLevel::Info, message, None);
    }

    fn warn(&self, message: &str) {
        self.log_message(LogLevel::Warn, message, None);
    }

    fn error(&self, message: &str) {
        self.log_message(LogLevel::Error, message, None);
    }

    fn fatal(&self, message: &str) {
        self.log_message(LogLevel::Fatal, message, None);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Logger {
    target: &'static str,
}

impl Log for Logger {
    fn is_enabled(&self, level: LogLevel) -> bool {
        log::log_enabled!(target: self.target, to_log_level(level))
    }

    fn log_message(&self, level: LogLevel, message: &str, err: Option<&dyn std::error::Error>) {
        match err {
            Some(e) => log::log!(target: self.target, to_log_level(level), "{message}: {e}"),
            None => log::log!(target: self.target, to_log_level(level), "{message}"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LogConfig {
    pub max_level: Option<log::LevelFilter>,
    pub log_file_name: Option<String>,
}

pub struct LogManager;

impl LogManager {
    pub fn get_logger(target: &'static str) -> Logger {
        Logger { target }
    }

    pub fn configure(config: &LogConfig) {
        if let Some(max) = config.max_level {
            log::set_max_level(max);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_mapping() {
        assert_eq!(to_log_level(LogLevel::Debug), log::Level::Debug);
        assert_eq!(to_log_level(LogLevel::Info), log::Level::Info);
        assert_eq!(to_log_level(LogLevel::Warn), log::Level::Warn);
        assert_eq!(to_log_level(LogLevel::Error), log::Level::Error);
        assert_eq!(to_log_level(LogLevel::Fatal), log::Level::Error);
    }

    #[test]
    fn logger_enabled_follows_max_level() {
        struct TestLog;
        impl log::Log for TestLog {
            fn enabled(&self, _: &log::Metadata<'_>) -> bool {
                true
            }
            fn log(&self, _: &log::Record<'_>) {}
            fn flush(&self) {}
        }
        static TEST_LOG: TestLog = TestLog;
        let _ = log::set_logger(&TEST_LOG);
        LogManager::configure(&LogConfig {
            max_level: Some(log::LevelFilter::Info),
            log_file_name: None,
        });
        let l = LogManager::get_logger("autors_util.test");
        assert!(l.is_enabled(LogLevel::Info));
        assert!(l.is_enabled(LogLevel::Warn));
        assert!(l.is_enabled(LogLevel::Error));
        assert!(l.is_enabled(LogLevel::Fatal));
        assert!(!l.is_enabled(LogLevel::Debug));
        l.info("hello");
        l.log_fmt(LogLevel::Info, format_args!("v={}", 42));
        l.log_message(
            LogLevel::Error,
            "with err",
            Some(&std::io::Error::other("x")),
        );
        LogManager::configure(&LogConfig {
            max_level: Some(log::LevelFilter::Trace),
            log_file_name: None,
        });
        assert!(l.is_enabled(LogLevel::Debug));
    }
}
