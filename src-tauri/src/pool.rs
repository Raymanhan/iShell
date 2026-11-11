#[derive(Default)]
pub struct SshPool;

impl SshPool {
    /// Drop any connection kept open for this server, so the next operation
    /// connects afresh (after its address or credentials changed, say).
    pub fn invalidate(&self, id: &str) {
        crate::transport::invalidate(id);
    }
}
