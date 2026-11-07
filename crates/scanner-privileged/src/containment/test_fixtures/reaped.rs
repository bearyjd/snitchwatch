use std::process::Child;

/// A child that is always killed and reaped, whatever the test did.
pub(crate) struct Reaped(pub(crate) Child);
impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
