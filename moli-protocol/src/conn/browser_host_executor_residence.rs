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
    next_turn_sequence: u64,
    _local_owner: PhantomData<Rc<()>>,
}

impl BrowserHostTurnExecutorOwner {
    pub fn for_application_owner_lane() -> Self {
        Self {
            next_turn_sequence: 1,
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
        let turn_sequence = self.next_turn_sequence;
        self.next_turn_sequence = self.next_turn_sequence.saturating_add(1);
        BrowserHostTurnExecution {
            connection,
            turn_sequence,
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
    turn_sequence: u64,
    _owner: PhantomData<&'a mut BrowserHostTurnExecutorOwner>,
}

impl BrowserHostTurnExecution<'_> {
    pub(crate) fn turn_sequence(&self) -> u64 {
        self.turn_sequence
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_binds_distinct_short_execution_turns() {
        let mut owner = BrowserHostTurnExecutorOwner::for_application_owner_lane();
        let mut connection = CdpConnection::new();

        let first_sequence = owner.bind_turn(&mut connection).turn_sequence();
        let second_sequence = owner.bind_turn(&mut connection).turn_sequence();

        assert_eq!(first_sequence, 1);
        assert_eq!(second_sequence, 2);
    }
}
