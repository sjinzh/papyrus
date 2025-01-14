mod api;
mod block;
mod gateway_metrics;
#[cfg(test)]
mod gateway_test;
mod middleware;
#[cfg(test)]
mod test_utils;
mod transaction;
mod v0_3_0;
mod v0_4_0;
mod version_config;
#[cfg(test)]
mod version_config_test;

use std::collections::BTreeMap;
use std::fmt::Display;
use std::net::SocketAddr;
use std::sync::Arc;

use gateway_metrics::MetricLogger;
use jsonrpsee::server::{ServerBuilder, ServerHandle};
use jsonrpsee::types::error::ErrorCode::InternalError;
use jsonrpsee::types::error::INTERNAL_ERROR_MSG;
use jsonrpsee::types::ErrorObjectOwned;
use papyrus_common::SyncingState;
use papyrus_config::dumping::{ser_param, SerializeConfig};
use papyrus_config::{ParamPath, SerializedParam};
use papyrus_storage::base_layer::BaseLayerStorageReader;
use papyrus_storage::body::events::EventIndex;
use papyrus_storage::db::TransactionKind;
use papyrus_storage::header::HeaderStorageReader;
use papyrus_storage::{StorageReader, StorageTxn};
use serde::{Deserialize, Serialize};
use starknet_api::block::{BlockNumber, BlockStatus};
use starknet_api::core::ChainId;
use tokio::sync::RwLock;
use tracing::{debug, error, info, instrument};

use crate::api::{
    get_methods_from_supported_apis, BlockHashOrNumber, BlockId, ContinuationToken, JsonRpcError,
    Tag,
};
use crate::middleware::{deny_requests_with_unsupported_path, proxy_rpc_request};

/// Maximum size of a supported transaction body - 10MB.
pub const SERVER_MAX_BODY_SIZE: u32 = 10 * 1024 * 1024;
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct GatewayConfig {
    pub chain_id: ChainId,
    pub server_address: String,
    pub max_events_chunk_size: usize,
    pub max_events_keys: usize,
    pub collect_metrics: bool,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        GatewayConfig {
            chain_id: ChainId("SN_MAIN".to_string()),
            server_address: String::from("0.0.0.0:8080"),
            max_events_chunk_size: 1000,
            max_events_keys: 100,
            collect_metrics: false,
        }
    }
}

impl SerializeConfig for GatewayConfig {
    fn dump(&self) -> BTreeMap<ParamPath, SerializedParam> {
        BTreeMap::from_iter([
            ser_param("chain_id", &self.chain_id, "The chain to follow. For more details see https://docs.starknet.io/documentation/architecture_and_concepts/Blocks/transactions/#chain-id."),
            ser_param("server_address", &self.server_address, "IP:PORT of the node`s JSON-RPC server."),
            ser_param("max_events_chunk_size", &self.max_events_chunk_size, "Maximum chunk size supported by the node in get_events requests."),
            ser_param("max_events_keys", &self.max_events_keys, "Maximum number of keys supported by the node in get_events requests."),
            ser_param("collect_metrics", &self.collect_metrics, "If true, collect metrics for the gateway."),
        ])
    }
}

impl From<JsonRpcError> for ErrorObjectOwned {
    fn from(err: JsonRpcError) -> Self {
        ErrorObjectOwned::owned(err as i32, err.to_string(), None::<()>)
    }
}

fn internal_server_error(err: impl Display) -> ErrorObjectOwned {
    error!("{}: {}", INTERNAL_ERROR_MSG, err);
    ErrorObjectOwned::owned(InternalError.code(), INTERNAL_ERROR_MSG, None::<()>)
}

fn get_block_number<Mode: TransactionKind>(
    txn: &StorageTxn<'_, Mode>,
    block_id: BlockId,
) -> Result<BlockNumber, ErrorObjectOwned> {
    Ok(match block_id {
        BlockId::HashOrNumber(BlockHashOrNumber::Hash(block_hash)) => txn
            .get_block_number_by_hash(&block_hash)
            .map_err(internal_server_error)?
            .ok_or_else(|| ErrorObjectOwned::from(JsonRpcError::BlockNotFound))?,
        BlockId::HashOrNumber(BlockHashOrNumber::Number(block_number)) => {
            // Check that the block exists.
            let last_block_number = get_latest_block_number(txn)?
                .ok_or_else(|| ErrorObjectOwned::from(JsonRpcError::BlockNotFound))?;
            if block_number > last_block_number {
                return Err(ErrorObjectOwned::from(JsonRpcError::BlockNotFound));
            }
            block_number
        }
        BlockId::Tag(Tag::Latest) => get_latest_block_number(txn)?
            .ok_or_else(|| ErrorObjectOwned::from(JsonRpcError::BlockNotFound))?,
        BlockId::Tag(Tag::Pending) => {
            todo!("Pending tag is not supported yet.")
        }
    })
}

fn get_latest_block_number<Mode: TransactionKind>(
    txn: &StorageTxn<'_, Mode>,
) -> Result<Option<BlockNumber>, ErrorObjectOwned> {
    Ok(txn.get_header_marker().map_err(internal_server_error)?.prev())
}

fn get_block_status<Mode: TransactionKind>(
    txn: &StorageTxn<'_, Mode>,
    block_number: BlockNumber,
) -> Result<BlockStatus, ErrorObjectOwned> {
    let base_layer_tip = txn.get_base_layer_block_marker().map_err(internal_server_error)?;
    let status = if block_number < base_layer_tip {
        BlockStatus::AcceptedOnL1
    } else {
        BlockStatus::AcceptedOnL2
    };

    Ok(status)
}
struct ContinuationTokenAsStruct(EventIndex);

impl ContinuationToken {
    fn parse(&self) -> Result<ContinuationTokenAsStruct, ErrorObjectOwned> {
        let ct = serde_json::from_str(&self.0)
            .map_err(|_| ErrorObjectOwned::from(JsonRpcError::InvalidContinuationToken))?;

        Ok(ContinuationTokenAsStruct(ct))
    }

    fn new(ct: ContinuationTokenAsStruct) -> Result<Self, ErrorObjectOwned> {
        Ok(Self(serde_json::to_string(&ct.0).map_err(internal_server_error)?))
    }
}

#[instrument(skip(storage_reader), level = "debug", err)]
pub async fn run_server(
    config: &GatewayConfig,
    shared_syncing_state: Arc<RwLock<SyncingState>>,
    storage_reader: StorageReader,
) -> anyhow::Result<(SocketAddr, ServerHandle)> {
    debug!("Starting gateway.");
    let methods = get_methods_from_supported_apis(
        &config.chain_id,
        storage_reader,
        config.max_events_chunk_size,
        config.max_events_keys,
        shared_syncing_state,
    );
    let addr;
    let handle;
    let server_builder =
        ServerBuilder::default().max_request_body_size(SERVER_MAX_BODY_SIZE).set_middleware(
            tower::ServiceBuilder::new()
                .filter_async(deny_requests_with_unsupported_path)
                .filter_async(proxy_rpc_request),
        );

    if config.collect_metrics {
        let server = server_builder
            .set_logger(MetricLogger::new(&methods))
            .build(&config.server_address)
            .await?;
        addr = server.local_addr()?;
        handle = server.start(methods)?;
    } else {
        let server = server_builder.build(&config.server_address).await?;
        addr = server.local_addr()?;
        handle = server.start(methods)?;
    }
    info!(local_address = %addr, "Gateway is running.");
    Ok((addr, handle))
}
