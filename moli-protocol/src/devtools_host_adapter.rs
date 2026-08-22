use std::ops::{Deref, DerefMut};

use crate::CdpConnection;

/// Application-owned DevTools adapter for one Browser Host.
///
/// This is deliberately distinct from a CDP socket frontend. The application
/// owner task keeps this adapter alive beside the Browser Host owner lane,
/// while any number of short-lived frontend endpoints may attach and detach.
/// Browser Core remains authoritative for browser identity, topology,
/// navigation, Page lifetime and browser-global policy; the wrapped
/// [`CdpConnection`] retains the migration-period renderer/DevTools projection
/// needed to translate commands and facts.
///
/// The wrapper is intentionally not `Clone`. There is one mutable adapter
/// residence per owner task, and a frontend endpoint never receives it.
pub struct DevToolsHostAdapter {
    connection: CdpConnection,
}

impl DevToolsHostAdapter {
    /// Transfers a protocol adapter into its application owner residence.
    pub fn for_application_owner(connection: CdpConnection) -> Self {
        Self { connection }
    }

    #[cfg(test)]
    pub(crate) fn into_connection_for_test(self) -> CdpConnection {
        self.connection
    }
}

impl Deref for DevToolsHostAdapter {
    type Target = CdpConnection;

    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

impl DerefMut for DevToolsHostAdapter {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.connection
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_owner_adapter_preserves_the_exact_browser_host_residence() {
        let connection = CdpConnection::new();
        let browser_instance_id = connection
            .browser_host_state()
            .navigation_owner()
            .browser_instance_id();

        let adapter = DevToolsHostAdapter::for_application_owner(connection);

        assert_eq!(
            adapter
                .browser_host_state()
                .navigation_owner()
                .browser_instance_id(),
            browser_instance_id
        );
        assert_eq!(
            adapter
                .into_connection_for_test()
                .browser_host_state()
                .navigation_owner()
                .browser_instance_id(),
            browser_instance_id
        );
    }
}
