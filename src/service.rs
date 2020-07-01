//! The "service" is where all the major components are plugged together:
//!
//! - The networking.
//! - The Wasm virtual machines.
//! - The storage and database.
//!
//! The service performs the following actions:
//!
//! - Tries to download all the active blocks (i.e. all blocks that descend from the latest
//! finalized block that have been announced) and put them in the database after having verified
//! their validity.
//! - Relays all block announces and transaction announces between the peers we're connected to.
//! - Announces our own locally-emitted transactions.
//! - Answers blocks requests made by remotes.
//!
//! At the moment, authoring blocks and running GrandPa isn't supported.

// # Implementation notes
//
// In terms of implementation, the service works by spawning various tasks that send messages to
// each other.
//
// Most of the magic happens at initialization, as that is the moment when we spawn the tasks.

use crate::network;

use alloc::sync::Arc;
use core::sync::atomic;
use futures::{channel::{mpsc, oneshot}, executor::ThreadPool, prelude::*};
use parity_scale_codec::DecodeAll as _;
use primitive_types::H256;

pub use builder::{builder, ServiceBuilder};

mod block_import_task;
mod builder;
mod database_task;
mod keystore_task;
mod network_task;
mod sync_task;

pub struct Service {
    /// Channel used by the background tasks to report what happens.
    /// Remember that this channel is bounded, and tasks will back-pressure if the user doesn't
    /// process events. This is an intended behaviour.
    events_in: mpsc::Receiver<Event>,

    /// Sender for messages towards the database task.
    to_database: mpsc::Sender<database_task::ToDatabase>,

    /// Number of transport-level (e.g. TCP/IP) network connections. Only updated by receiving
    /// events.
    num_network_connections: u64,
    /// `Arc` whose content is updated by the network task. Used to update
    /// [`Service::num_network_connections`].
    num_connections_store: Arc<atomic::AtomicU64>,

    /// Number of the best known block. Only updated by receiving events.
    best_block_number: u64,

    /// Hash of the best known block. Only updated by receiving events.
    best_block_hash: [u8; 32],

    /// Number of the latest finalized block. Only updated by receiving events.
    finalized_block_number: u64,

    /// Hash of the latest finalized block. Only updated by receiving events.
    finalized_block_hash: [u8; 32],

    /// Optional threads pool that is used to dispatch tasks and that we keep alive.
    _threads_pool: Option<ThreadPool>,
}

/// Event that happened on the service.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Event {
    /// Database state has been updated for the given block.
    NewChainHead {
        number: u64,
        hash: H256,
        head_update: ChainHeadUpdate,
    },

    /// The finalized block has been updated to a different one.
    NewFinalized {
        /// Number of the finalized block.
        number: u64,
        /// Hash of the finalized block.
        hash: H256,
    },

    /// Received a block announce from the network.
    BlockAnnounceReceived {
        /// Block number.
        number: u64,
        /// Block hash.
        hash: H256,
    },

    /// Networking has detected a new external address.
    NewNetworkExternalAddress {
        /// The address in question. Contains a `/p2p/` suffix.
        address: network::Multiaddr,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ChainHeadUpdate {
    NoUpdate,
    FastForward,
    Reorg,
}

impl Service {
    /// Returns the next event that happens in the service.
    pub async fn next_event(&mut self) -> Event {
        // The events channel is never closed unless the background tasks have all closed as well,
        // in which case it is totally appropriate to panic.
        let event = self.events_in.next().await.unwrap();

        self.num_network_connections = self.num_connections_store.load(atomic::Ordering::Relaxed);

        // Update the local state.
        match &event {
            Event::NewChainHead { number, hash, .. } => {
                self.best_block_number = *number;
                self.best_block_hash = (*hash).into();
            }
            Event::NewFinalized { number, hash } => {
                self.finalized_block_number = *number;
                self.finalized_block_hash = (*hash).into();
            }
            _ => {}
        }

        event
    }

    /// Returns the number of transport-level (e.g. TCP/IP) connections of the network. Only
    /// updated when calling [`Service::next_event`].
    pub fn num_network_connections(&self) -> u64 {
        self.num_network_connections
    }

    /// Returns the number of the best known block. Only updated when calling
    /// [`Service::next_event`].
    pub fn best_block_number(&self) -> u64 {
        self.best_block_number
    }

    /// Returns the hash of the best known block. Only updated when calling
    /// [`Service::next_event`].
    pub fn best_block_hash(&self) -> [u8; 32] {
        self.best_block_hash
    }

    /// Returns the number of the latest finalized block. Only updated when calling
    /// [`Service::next_event`].
    pub fn finalized_block_number(&self) -> u64 {
        self.finalized_block_number
    }

    /// Returns the hash of the latest finalized block. Only updated when calling
    /// [`Service::next_event`].
    pub fn finalized_block_hash(&self) -> [u8; 32] {
        self.finalized_block_hash
    }

    // TODO: crap API
    pub async fn best_effort_block_hash(&self, num: u64) -> Option<[u8; 32]> {
        let (tx, rx) = oneshot::channel();

        // TODO: don't clone the channel, it reserves an extra slot
        self.to_database.clone().send(database_task::ToDatabase::BlockHashGet {
            block_number: num,
            send_back: tx,
        }).await.unwrap();

        rx.await.unwrap()
    }
}
