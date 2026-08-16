//! Typed result of resolving one pending Page creation at its lifecycle boundary.
//!
//! The owner-local store keeps the checked-out Page entry private while it
//! observes the requested milestone and then either commits the latest page state,
//! restores stable residence, or retires the Page. Callers receive only this
//! result and therefore cannot expose the entry between those steps.

use super::terminal::PageNavigationOwnerFailure;
use crate::PageVmInitStage;
use crate::runtime::{RendererDocumentLifecycleIdentity, RendererLifecycleTerminationStamp};

pub(in crate::runtime) enum PageCreationResolution<Pending, Attached> {
    Finalized {
        attached: Attached,
        resume_parked_page_turn: bool,
    },
    Waiting {
        pending: Pending,
        document: RendererDocumentLifecycleIdentity,
    },
    /// The exact lifecycle target was reached, but a synchronous decider must
    /// run before page creation can either reply or follow another
    /// Document. This is an intra-owner-turn result, not a request to enqueue
    /// another turn.
    LifecycleDecisionRequired { pending: Pending },
    /// The owner-local store retired the Page before returning this result.
    Retired { failure: PageCreationRetirement },
    /// Checkout did not obtain the resident entry. The store remains its
    /// lifetime authority, so the caller must not retire it speculatively.
    EntryUnavailable { error: anyhow::Error },
}

pub(in crate::runtime) enum PageCreationRetirement {
    NavigationFailed(PageNavigationOwnerFailure),
    LifecycleInterrupted {
        target_stage: PageVmInitStage,
        termination: RendererLifecycleTerminationStamp,
    },
    MissingLifecycleResident {
        target_stage: PageVmInitStage,
        document: RendererDocumentLifecycleIdentity,
    },
    PageStateFailed(anyhow::Error),
}

impl PageCreationRetirement {
    pub(in crate::runtime) fn into_error(self) -> anyhow::Error {
        match self {
            Self::NavigationFailed(failure) => anyhow::anyhow!(failure.to_string()),
            Self::LifecycleInterrupted {
                target_stage,
                termination,
            } => anyhow::anyhow!(
                "renderer document lifecycle was interrupted before {target_stage:?}: {:?}",
                termination.reason
            ),
            Self::MissingLifecycleResident {
                target_stage,
                document,
            } => anyhow::anyhow!(
                "renderer document lifecycle resident disappeared while exact Document {document:?} was still pending before {target_stage:?}"
            ),
            Self::PageStateFailed(error) => error,
        }
    }
}
