pub struct CompatHost;

impl CompatHost {
    pub fn with_registry() -> Self {
        Self
    }

    pub fn set_wired_app(&self) {}

    pub fn set_unwired(&self) {}

    pub fn set_unwired_cfg_test(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cfg_test_caller_only() {
        let host = CompatHost;
        host.set_unwired_cfg_test();
        host.set_wired_app();
    }
}
