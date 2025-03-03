// Copyright (c) 2022, Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0
use crate::{
    block_remover::{
        BlockErrorType, BlockRemover, BlockRemoverCommand, BlockRemoverResult, DeleteBatchMessage,
        DeleteBatchResult, RemoveBlocksResponse, RequestKey,
    },
    common::create_db_stores,
    PrimaryWorkerMessage,
};
use bincode::deserialize;
use config::WorkerId;
use crypto::{
    ed25519::Ed25519PublicKey,
    traits::{Signer, VerifyingKey},
    Hash,
};
use futures::{future::try_join_all, stream::FuturesUnordered, StreamExt};
use network::SimpleSender;
use std::{collections::HashMap, net::SocketAddr, time::Duration};
use tokio::{
    net::TcpListener,
    sync::mpsc::{channel, Sender},
    task::JoinHandle,
    time::{sleep, timeout},
};
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use types::{
    test_utils::{
        certificate, fixture_batch_with_transactions, fixture_header_builder, keys,
        resolve_name_and_committee,
    },
    BatchDigest,
};

#[tokio::test]
async fn test_successful_blocks_delete() {
    // GIVEN
    let (header_store, certificate_store, payload_store) = create_db_stores();
    let (tx_commands, rx_commands) = channel(10);
    let (tx_remove_block, mut rx_remove_block) = channel(1);
    let (tx_delete_batches, rx_delete_batches) = channel(10);

    // AND the necessary keys
    let (name, committee) = resolve_name_and_committee(14000);

    BlockRemover::spawn(
        name.clone(),
        committee.clone(),
        certificate_store.clone(),
        header_store.clone(),
        payload_store.clone(),
        SimpleSender::new(),
        rx_commands,
        rx_delete_batches,
    );

    let mut block_ids = Vec::new();
    let mut header_ids = Vec::new();
    let handlers = FuturesUnordered::new();

    let key = keys().pop().unwrap();

    let mut worker_batches: HashMap<WorkerId, Vec<BatchDigest>> = HashMap::new();

    let worker_id_0 = 0;
    let worker_id_1 = 1;

    // AND generate headers with distributed batches between 2 workers (0 and 1)
    for headers in 0..5 {
        let batch_1 = fixture_batch_with_transactions(10);
        let batch_2 = fixture_batch_with_transactions(10);

        let header = fixture_header_builder()
            .with_payload_batch(batch_1.clone(), worker_id_0)
            .with_payload_batch(batch_2.clone(), worker_id_1)
            .build(|payload| key.sign(payload));

        let certificate = certificate(&header);
        let block_id = certificate.digest();

        // write the certificate
        certificate_store
            .write(certificate.digest(), certificate.clone())
            .await;

        // write the header
        header_store.write(header.clone().id, header.clone()).await;

        header_ids.push(header.clone().id);

        // write the batches to payload store
        payload_store
            .write_all(vec![
                ((batch_1.clone().digest(), worker_id_0), 0),
                ((batch_2.clone().digest(), worker_id_1), 0),
            ])
            .await
            .expect("couldn't store batches");

        block_ids.push(block_id);

        worker_batches
            .entry(worker_id_0)
            .or_insert_with(Vec::new)
            .push(batch_1.digest());

        worker_batches
            .entry(worker_id_1)
            .or_insert_with(Vec::new)
            .push(batch_2.digest());
    }

    // AND boostrap the workers
    for (worker_id, batch_digests) in worker_batches.clone() {
        let worker_address = committee
            .worker(&name, &worker_id)
            .unwrap()
            .primary_to_worker;

        let handle = worker_listener::<Ed25519PublicKey>(
            worker_address,
            batch_digests,
            tx_delete_batches.clone(),
        );
        handlers.push(handle);
    }

    tx_commands
        .send(BlockRemoverCommand::RemoveBlocks {
            ids: block_ids.clone(),
            sender: tx_remove_block,
        })
        .await
        .unwrap();

    if timeout(Duration::from_millis(4_000), try_join_all(handlers))
        .await
        .is_err()
    {
        panic!("workers haven't received expected delete batch requests")
    }

    // THEN we should expect to get back the result
    let timer = sleep(Duration::from_millis(5_000));
    tokio::pin!(timer);

    tokio::select! {
        Some(result) = rx_remove_block.recv() => {
            assert!(result.is_ok(), "Expected to receive a successful result, instead got error: {:?}", result.err().unwrap());

            let block = result.unwrap();

            assert_eq!(block.ids.len(), block_ids.len());

            // ensure that certificates have been deleted from store
            for block_id in block_ids {
                assert!(certificate_store.read(block_id).await.unwrap().is_none(), "Certificate shouldn't exist");
            }

            // ensure that headers have been deleted from store
            for header_id in header_ids {
                assert!(header_store.read(header_id).await.unwrap().is_none(), "Header shouldn't exist");
            }

            // ensure that batches have been deleted from the payload store
            for (worker_id, batch_digests) in worker_batches {
                for digest in batch_digests {
                   assert!(payload_store.read((digest, worker_id)).await.unwrap().is_none(), "Payload shouldn't exist");
                }
            }
        },
        () = &mut timer => {
            panic!("Timeout, no result has been received in time")
        }
    }
}

#[tokio::test]
async fn test_timeout() {
    // GIVEN
    let (header_store, certificate_store, payload_store) = create_db_stores();
    let (tx_commands, rx_commands) = channel(10);
    let (tx_remove_block, mut rx_remove_block) = channel(1);
    let (tx_delete_batches, rx_delete_batches) = channel(10);

    // AND the necessary keys
    let (name, committee) = resolve_name_and_committee(14001);

    BlockRemover::spawn(
        name.clone(),
        committee.clone(),
        certificate_store.clone(),
        header_store.clone(),
        payload_store.clone(),
        SimpleSender::new(),
        rx_commands,
        rx_delete_batches,
    );

    let mut block_ids = Vec::new();
    let mut header_ids = Vec::new();

    let key = keys().pop().unwrap();

    let mut worker_batches: HashMap<WorkerId, Vec<BatchDigest>> = HashMap::new();

    let worker_id_2 = 2;
    let worker_id_3 = 3;

    // AND generate headers with distributed batches between 2 workers (2 and 3)
    for headers in 0..5 {
        let batch_1 = fixture_batch_with_transactions(10);
        let batch_2 = fixture_batch_with_transactions(10);

        let header = fixture_header_builder()
            .with_payload_batch(batch_1.clone(), worker_id_2)
            .with_payload_batch(batch_2.clone(), worker_id_3)
            .build(|payload| key.sign(payload));

        let certificate = certificate(&header);
        let block_id = certificate.digest();

        // write the certificate
        certificate_store
            .write(certificate.digest(), certificate.clone())
            .await;

        // write the header
        header_store.write(header.clone().id, header.clone()).await;

        header_ids.push(header.clone().id);

        // write the batches to payload store
        payload_store
            .write_all(vec![
                ((batch_1.clone().digest(), worker_id_2), 0),
                ((batch_2.clone().digest(), worker_id_3), 0),
            ])
            .await
            .expect("couldn't store batches");

        block_ids.push(block_id);

        worker_batches
            .entry(worker_id_2)
            .or_insert_with(Vec::new)
            .push(batch_1.digest());

        worker_batches
            .entry(worker_id_3)
            .or_insert_with(Vec::new)
            .push(batch_2.digest());
    }

    // AND Don't boostrap any worker nodes

    // AND send the removal command
    tx_commands
        .send(BlockRemoverCommand::RemoveBlocks {
            ids: block_ids.clone(),
            sender: tx_remove_block,
        })
        .await
        .unwrap();

    // THEN we should expect to get back the result
    let timer = sleep(Duration::from_millis(5_000));
    tokio::pin!(timer);

    tokio::select! {
        Some(result) = rx_remove_block.recv() => {
            assert!(result.is_err(), "Expected to receive an error result, instead got: {:?}", result.ok().unwrap());

            let block_error = result.err().unwrap();

            assert_eq!(block_error.error, BlockErrorType::Timeout);

            assert_eq!(block_error.ids.len(), block_ids.len());

            // ensure that certificates have NOT been deleted from store
            for block_id in block_ids {
                assert!(certificate_store.read(block_id).await.unwrap().is_some(), "Certificate should exist");
            }

            // ensure that headers have NOT been deleted from store
            for header_id in header_ids {
                assert!(header_store.read(header_id).await.unwrap().is_some(), "Header should exist");
            }

            // ensure that batches have NOT been deleted from the payload store
            for (worker_id, batch_digests) in worker_batches {
                for digest in batch_digests {
                   assert!(payload_store.read((digest, worker_id)).await.unwrap().is_some(), "Payload should exist");
                }
            }
        },
        () = &mut timer => {
            panic!("Timeout, no result has been received in time")
        }
    }
}

#[tokio::test]
async fn test_unlocking_pending_requests() {
    // GIVEN
    let (header_store, certificate_store, payload_store) = create_db_stores();
    let (tx_commands, rx_commands) = channel(10);
    let (tx_delete_batches, rx_delete_batches) = channel(10);

    // AND the necessary keys
    let (name, committee) = resolve_name_and_committee(14001);

    let mut remover = BlockRemover {
        name,
        committee,
        certificate_store: certificate_store.clone(),
        header_store: header_store.clone(),
        payload_store: payload_store.clone(),
        network: SimpleSender::new(),
        rx_commands,
        pending_removal_requests: HashMap::new(),
        map_tx_removal_results: HashMap::new(),
        map_tx_worker_removal_results: HashMap::new(),
        rx_delete_batches,
    };

    let mut block_ids = Vec::new();
    let mut header_ids = Vec::new();

    let key = keys().pop().unwrap();

    let worker_id_0 = 0;

    let batch = fixture_batch_with_transactions(10);
    let header = fixture_header_builder()
        .with_payload_batch(batch.clone(), worker_id_0)
        .build(|payload| key.sign(payload));

    let certificate = certificate(&header);
    let block_id = certificate.digest();

    // write the certificate
    certificate_store
        .write(certificate.digest(), certificate.clone())
        .await;

    // write the header
    header_store.write(header.clone().id, header.clone()).await;

    header_ids.push(header.clone().id);

    // write the batches to payload store
    payload_store
        .write_all(vec![((batch.clone().digest(), worker_id_0), 0)])
        .await
        .expect("couldn't store batches");

    block_ids.push(block_id);

    // AND Don't boostrap any worker nodes

    // AND send the removal command
    let get_mock_sender = || {
        let (tx, _) = channel(1);
        tx
    };

    // AND we send a few commands
    for _ in 0..3 {
        remover
            .handle_command(BlockRemoverCommand::RemoveBlocks {
                ids: block_ids.clone(),
                sender: get_mock_sender(),
            })
            .await;
    }

    // AND we confirm that we have an internal pending request with 3 different senders
    let request_key: RequestKey =
        BlockRemover::<Ed25519PublicKey>::construct_blocks_request_key(&block_ids);

    assert_eq!(remover.pending_removal_requests.len(), 1);
    assert_eq!(
        remover
            .map_tx_removal_results
            .get(&request_key)
            .unwrap()
            .len(),
        3
    );

    assert_eq!(remover.map_tx_removal_results.len(), 1);

    // WHEN we send the delete response
    let result = BlockRemoverResult::Ok(RemoveBlocksResponse {
        ids: block_ids.clone(),
    });

    remover.handle_remove_waiting_result(result).await;

    // THEN ensure that internal state has been unlocked

    assert!(remover.pending_removal_requests.is_empty());
    assert!(remover.map_tx_removal_results.is_empty());
}

pub fn worker_listener<PublicKey: VerifyingKey>(
    address: SocketAddr,
    expected_batch_ids: Vec<BatchDigest>,
    tx_delete_batches: Sender<DeleteBatchResult>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        println!("[{}] Setting up server", &address);

        let listener = TcpListener::bind(&address).await.unwrap();
        let (socket, _) = listener.accept().await.unwrap();
        let transport = Framed::new(socket, LengthDelimitedCodec::new());

        let (_, mut reader) = transport.split();
        match reader.next().await {
            Some(Ok(received)) => {
                let message = received.freeze();
                match deserialize(&message) {
                    Ok(PrimaryWorkerMessage::<PublicKey>::DeleteBatches(ids)) => {
                        assert_eq!(
                            ids.clone(),
                            expected_batch_ids,
                            "Expected batch ids not the same for [{}]",
                            &address
                        );

                        tx_delete_batches
                            .send(Ok(DeleteBatchMessage { ids }))
                            .await
                            .unwrap();
                    }
                    _ => panic!("Unexpected request received"),
                };
            }
            _ => panic!("Failed to receive network message"),
        }
    })
}
