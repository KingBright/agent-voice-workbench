use crate::{Error, Result};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis().min(u64::MAX as u128) as u64
}

/// Cooperative cancellation. No claim of preempting a GPU kernel or model load.
#[derive(Debug, Clone)]
pub struct Control { cancelled: Arc<AtomicBool>, pub deadline_ms: u64 }
impl Control {
    pub fn new(deadline_ms: u64) -> Self { Self { cancelled: Arc::new(AtomicBool::new(false)), deadline_ms } }
    pub fn unbounded() -> Self { Self::new(u64::MAX) }
    pub fn cancel(&self) { self.cancelled.store(true, Ordering::Release); }
    pub fn check(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Acquire) { return Err(Error::Cancelled); }
        if now_ms() >= self.deadline_ms { return Err(Error::Deadline); }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn cloned_cancellation_is_shared() { let c = Control::unbounded(); c.clone().cancel(); assert!(matches!(c.check(), Err(Error::Cancelled))); }
    #[test] fn deadline_is_checked() { assert!(matches!(Control::new(0).check(), Err(Error::Deadline))); }
}
