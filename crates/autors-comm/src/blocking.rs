//! Synchronous facade over the async parts of the [`CommKernel`] registry.
//! [`BlockingCommKernel`] mirrors the [`CommKernel`] API: the async keep-alive
//! round [`CommKernel::poll_clients`] is driven to completion on the calling
//! thread via [`autors_runtime::block_on`], while the inherently synchronous
//! registry operations delegate directly.
//! Do not call [`BlockingCommKernel::poll_clients`] from within async code
//! running on the shared runtime: [`autors_runtime::block_on`] panics on its
//! executor threads, the same restriction as `tokio::runtime::Runtime::block_on`.

use std::sync::{Arc, Mutex};

use crate::base::{CommKernel, CommMasterHandle};
use crate::error::Result;

/// Synchronous wrapper around the [`CommKernel`] registry.
/// Like [`CommKernel`] this is a unit struct: all operations work on the
/// process-wide client registry.
pub struct BlockingCommKernel;

impl BlockingCommKernel {
    /// Delegates to [`CommKernel::register_client`] (inherently synchronous).
    pub fn register_client<H: CommMasterHandle + 'static>(master: &Arc<Mutex<H>>) -> Result<()> {
        CommKernel::register_client(master)
    }

    /// Delegates to [`CommKernel::deregister_client`] (inherently synchronous).
    pub fn deregister_client<H: CommMasterHandle + 'static>(master: &Arc<Mutex<H>>) -> bool {
        CommKernel::deregister_client(master)
    }

    /// Delegates to [`CommKernel::is_registered`] (inherently synchronous).
    pub fn is_registered<H: CommMasterHandle + 'static>(master: &Arc<Mutex<H>>) -> bool {
        CommKernel::is_registered(master)
    }

    /// Delegates to [`CommKernel::registered_count`] (inherently synchronous).
    pub fn registered_count() -> usize {
        CommKernel::registered_count()
    }

    /// Drives [`CommKernel::poll_clients`] to completion on the calling thread.
    pub fn poll_clients() {
        autors_runtime::block_on(CommKernel::poll_clients())
    }
}
