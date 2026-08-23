use std::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
    rc::Rc,
};

use super::CdpConnection;

/// Application-side residence for the capability to execute Browser Host
/// turns.
///
/// The Browser Host actor owns turn selection. This non-cloneable companion
/// lives beside that actor in the application owner lane and is required to
/// bind the remaining Protocol projection for one short execution turn.
/// `CdpConnection` deliberately does not contain this authority.
pub struct BrowserHostTurnExecutorOwner {
    _local_owner: PhantomData<Rc<()>>,
}

impl BrowserHostTurnExecutorOwner {
    pub fn for_application_owner_lane() -> Self {
        Self {
            _local_owner: PhantomData,
        }
    }

    /// Binds the migration-period Protocol projection for one exact Host
    /// execution turn.
    ///
    /// The mutable owner borrow prevents two Host executions from using the
    /// projection concurrently. Dropping a frontend adapter cannot create or
    /// retain this capability.
    pub(crate) fn bind_turn<'a>(
        &'a mut self,
        connection: &'a mut CdpConnection,
    ) -> BrowserHostTurnExecution<'a> {
        BrowserHostTurnExecution {
            connection,
            _owner: PhantomData,
        }
    }
}

/// One short application-authorized Browser Host execution over the remaining
/// Protocol projection.
///
/// This adapter is intentionally borrowing and cannot survive a turn or a
/// participant wait. Physical waits remain move-owned by
/// `PendingBrowserHostTurn` and are applied through a newly bound later turn.
pub(crate) struct BrowserHostTurnExecution<'a> {
    connection: &'a mut CdpConnection,
    _owner: PhantomData<&'a mut BrowserHostTurnExecutorOwner>,
}

impl Deref for BrowserHostTurnExecution<'_> {
    type Target = CdpConnection;

    fn deref(&self) -> &Self::Target {
        self.connection
    }
}

impl DerefMut for BrowserHostTurnExecution<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.connection
    }
}
