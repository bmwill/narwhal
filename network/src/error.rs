// Copyright (c) 2021, Facebook, Inc. and its affiliates
// Copyright (c) 2022, Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0
use std::{fmt::Debug, net::SocketAddr};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum NetworkError {
    #[error("Failed to connect to {0} (retry {1}): {2}")]
    FailedToConnect(SocketAddr, u16, std::io::Error),

    #[error("Failed to accept connection: {0}")]
    FailedToListen(std::io::Error),

    #[error("Failed to send message to {0}: {1}")]
    FailedToSendMessage(SocketAddr, std::io::Error),

    #[error("Failed to receive message from {0}: {1}")]
    FailedToReceiveMessage(SocketAddr, std::io::Error),

    #[error("Failed to receive ACK from {0}")]
    FailedToReceiveAck(SocketAddr),

    #[error("Receive unexpected ACK from {0}")]
    UnexpectedAck(SocketAddr),
}
