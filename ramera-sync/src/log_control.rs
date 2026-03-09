use std::sync::atomic::{AtomicBool, Ordering};

static ZERO_LOG: AtomicBool = AtomicBool::new(false);

pub fn set_zero_log(enabled: bool) {
    ZERO_LOG.store(enabled, Ordering::Relaxed);
}

pub fn is_zero_log() -> bool {
    ZERO_LOG.load(Ordering::Relaxed)
}

#[macro_export]
macro_rules! progress {
    ($($arg:tt)*) => {{
        if !$crate::log_control::is_zero_log() {
            eprintln!($($arg)*);
        }
    }};
}
