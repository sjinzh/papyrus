//! This module contains client that can request changes to [`Starknet`].
//!
//! [`Starknet`]: https://starknet.io/

pub mod objects;

#[cfg(test)]
mod starknet_gateway_client_test;

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::writer::objects::response::{DeclareResponse, DeployAccountResponse, InvokeResponse};
use crate::writer::objects::transaction::{
    DeclareTransaction, DeployAccountTransaction, InvokeTransaction,
};
use crate::{ClientCreationError, ClientError, RetryConfig, StarknetClient};

/// Errors that may be returned from a writer client.
pub type WriterClientError = ClientError;

pub type WriterClientResult<T> = Result<T, WriterClientError>;

/// A trait describing an object that can communicate with [`Starknet`] and make changes to it.
///
/// [`Starknet`]: https://starknet.io/
#[async_trait]
pub trait StarknetWriter {
    /// Add an invoke transaction to [`Starknet`].
    ///
    /// [`Starknet`]: https://starknet.io/
    async fn add_invoke_transaction(
        &self,
        tx: &InvokeTransaction,
    ) -> WriterClientResult<InvokeResponse>;

    /// Add a declare transaction to [`Starknet`].
    ///
    /// [`Starknet`]: https://starknet.io/
    async fn add_declare_transaction(
        &self,
        tx: &DeclareTransaction,
    ) -> WriterClientResult<DeclareResponse>;

    /// Add a deploy account transaction to [`Starknet`].
    ///
    /// [`Starknet`]: https://starknet.io/
    async fn add_deploy_account_transaction(
        &self,
        tx: &DeployAccountTransaction,
    ) -> WriterClientResult<DeployAccountResponse>;
}

const ADD_TRANSACTION_URL_SUFFIX: &str = "gateway/add_transaction";

/// A client for the [`Starknet`] gateway.
///
/// [`Starknet`]: https://starknet.io/
pub struct StarknetGatewayClient {
    add_transaction_url: Url,
    client: StarknetClient,
}

#[async_trait]
impl StarknetWriter for StarknetGatewayClient {
    async fn add_invoke_transaction(
        &self,
        tx: &InvokeTransaction,
    ) -> WriterClientResult<InvokeResponse> {
        self.add_transaction(&tx).await
    }

    async fn add_deploy_account_transaction(
        &self,
        tx: &DeployAccountTransaction,
    ) -> WriterClientResult<DeployAccountResponse> {
        self.add_transaction(&tx).await
    }

    async fn add_declare_transaction(
        &self,
        tx: &DeclareTransaction,
    ) -> WriterClientResult<DeclareResponse> {
        self.add_transaction(&tx).await
    }
}

impl StarknetGatewayClient {
    pub fn new(
        starknet_url: &str,
        http_headers: Option<HashMap<String, String>>,
        node_version: &'static str,
        retry_config: RetryConfig,
    ) -> Result<Self, ClientCreationError> {
        Ok(StarknetGatewayClient {
            add_transaction_url: Url::parse(starknet_url)?.join(ADD_TRANSACTION_URL_SUFFIX)?,
            client: StarknetClient::new(http_headers, node_version, retry_config)?,
        })
    }

    async fn add_transaction<Transaction: Serialize, Response: for<'a> Deserialize<'a>>(
        &self,
        tx: &Transaction,
    ) -> WriterClientResult<Response> {
        let response: String = self
            .client
            .request_with_retry(
                self.client
                    .internal_client
                    .post(self.add_transaction_url.clone())
                    .body(serde_json::to_string(&tx)?),
            )
            .await?;
        Ok(serde_json::from_str::<Response>(&response)?)
    }
}
