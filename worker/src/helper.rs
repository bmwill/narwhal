// Copyright (c) 2021, Facebook, Inc. and its affiliates
// Copyright (c) 2022, Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0
use bytes::Bytes;
use config::{Committee, WorkerId};
use crypto::traits::VerifyingKey;
use network::SimpleSender;
use primary::BatchDigest;
use store::Store;
use tokio::sync::mpsc::Receiver;
use tracing::{error, warn};

use crate::processor::SerializedBatchMessage;

#[cfg(test)]
#[path = "tests/helper_tests.rs"]
pub mod helper_tests;

/// A task dedicated to help other authorities by replying to their batch requests.
pub struct Helper<PublicKey: VerifyingKey> {
    /// The id of this worker.
    id: WorkerId,
    /// The committee information.
    committee: Committee<PublicKey>,
    /// The persistent storage.
    store: Store<BatchDigest, SerializedBatchMessage>,
    /// Input channel to receive batch requests.
    rx_request: Receiver<(Vec<BatchDigest>, PublicKey)>,
    /// A network sender to send the batches to the other workers.
    network: SimpleSender,
}

impl<PublicKey: VerifyingKey> Helper<PublicKey> {
    pub fn spawn(
        id: WorkerId,
        committee: Committee<PublicKey>,
        store: Store<BatchDigest, SerializedBatchMessage>,
        rx_request: Receiver<(Vec<BatchDigest>, PublicKey)>,
    ) {
        tokio::spawn(async move {
            Self {
                id,
                committee,
                store,
                rx_request,
                network: SimpleSender::new(),
            }
            .run()
            .await;
        });
    }

    async fn run(&mut self) {
        while let Some((digests, origin)) = self.rx_request.recv().await {
            // TODO [issue #7]: Do some accounting to prevent bad nodes from monopolizing our resources.

            // get the requestors address.
            let address = match self.committee.worker(&origin, &self.id) {
                Ok(x) => x.worker_to_worker,
                Err(e) => {
                    warn!("Unexpected batch request: {}", e);
                    continue;
                }
            };

            // Reply to the request (the best we can).
            for digest in digests {
                match self.store.read(digest).await {
                    Ok(Some(data)) => self.network.send(address, Bytes::from(data)).await,
                    Ok(None) => (),
                    Err(e) => error!("{}", e),
                }
            }
        }
    }
}
