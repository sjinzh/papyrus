use std::sync::Arc;

use jsonrpsee::core::RpcResult;
use jsonrpsee::types::ErrorObjectOwned;
use jsonrpsee::RpcModule;
use papyrus_common::SyncingState;
use papyrus_storage::body::events::{EventIndex, EventsReader};
use papyrus_storage::body::{BodyStorageReader, TransactionIndex};
use papyrus_storage::state::StateStorageReader;
use papyrus_storage::StorageReader;
use starknet_api::block::{BlockNumber, BlockStatus};
use starknet_api::core::{ChainId, ClassHash, ContractAddress, GlobalRoot, Nonce};
use starknet_api::hash::{StarkFelt, StarkHash, GENESIS_HASH};
use starknet_api::state::{StateNumber, StorageKey};
use starknet_api::transaction::{
    EventIndexInTransactionOutput, TransactionExecutionStatus, TransactionHash,
    TransactionOffsetInBlock,
};
use tokio::sync::RwLock;
use tracing::instrument;

use super::super::block::{Block, BlockHeader};
use super::super::state::StateUpdate;
use super::super::transaction::{
    Event, Transaction, TransactionOutput, TransactionReceipt, TransactionReceiptWithStatus,
    TransactionWithHash, Transactions,
};
use super::{
    BlockHashAndNumber, BlockId, EventFilter, EventsChunk, GatewayContractClass,
    JsonRpcV0_3_0Server,
};
use crate::api::{BlockHashOrNumber, ContinuationToken, JsonRpcError, JsonRpcServerImpl};
use crate::block::get_block_header_by_number;
use crate::transaction::{get_block_tx_hashes_by_number, get_block_txs_by_number};
use crate::{
    get_block_number, get_block_status, get_latest_block_number, internal_server_error,
    ContinuationTokenAsStruct,
};

/// Rpc server.
pub struct JsonRpcServerV0_3_0Impl {
    pub chain_id: ChainId,
    pub storage_reader: StorageReader,
    pub max_events_chunk_size: usize,
    pub max_events_keys: usize,
    pub shared_syncing_state: Arc<RwLock<SyncingState>>,
}

impl JsonRpcV0_3_0Server for JsonRpcServerV0_3_0Impl {
    #[instrument(skip(self), level = "debug", err, ret)]
    fn block_number(&self) -> RpcResult<BlockNumber> {
        let txn = self.storage_reader.begin_ro_txn().map_err(internal_server_error)?;
        get_latest_block_number(&txn)?.ok_or_else(|| ErrorObjectOwned::from(JsonRpcError::NoBlocks))
    }

    #[instrument(skip(self), level = "debug", err, ret)]
    fn block_hash_and_number(&self) -> RpcResult<BlockHashAndNumber> {
        let txn = self.storage_reader.begin_ro_txn().map_err(internal_server_error)?;
        let block_number = get_latest_block_number(&txn)?
            .ok_or_else(|| ErrorObjectOwned::from(JsonRpcError::NoBlocks))?;
        let header: BlockHeader = get_block_header_by_number(&txn, block_number)?;

        Ok(BlockHashAndNumber { block_hash: header.block_hash, block_number })
    }

    #[instrument(skip(self), level = "debug", err, ret)]
    fn get_block_w_transaction_hashes(&self, block_id: BlockId) -> RpcResult<Block> {
        let txn = self.storage_reader.begin_ro_txn().map_err(internal_server_error)?;
        let block_number = get_block_number(&txn, block_id)?;
        let status = get_block_status(&txn, block_number)?;
        let header = get_block_header_by_number(&txn, block_number)?;
        let transaction_hashes = get_block_tx_hashes_by_number(&txn, block_number)?;

        Ok(Block { status, header, transactions: Transactions::Hashes(transaction_hashes) })
    }

    #[instrument(skip(self), level = "debug", err, ret)]
    fn get_block_w_full_transactions(&self, block_id: BlockId) -> RpcResult<Block> {
        let txn = self.storage_reader.begin_ro_txn().map_err(internal_server_error)?;
        let block_number = get_block_number(&txn, block_id)?;
        let status = get_block_status(&txn, block_number)?;
        let header = get_block_header_by_number(&txn, block_number)?;
        // TODO(dvir): consider create a vector of (transaction, transaction_index) first and get
        // the transaction hashes by the index.
        let transactions = get_block_txs_by_number(&txn, block_number)?;
        let transaction_hashes = get_block_tx_hashes_by_number(&txn, block_number)?;
        let transactions_with_hash = transactions
            .into_iter()
            .zip(transaction_hashes)
            .map(|(transaction, transaction_hash)| TransactionWithHash {
                transaction,
                transaction_hash,
            })
            .collect();

        Ok(Block { status, header, transactions: Transactions::Full(transactions_with_hash) })
    }

    #[instrument(skip(self), level = "debug", err, ret)]
    fn get_storage_at(
        &self,
        contract_address: ContractAddress,
        key: StorageKey,
        block_id: BlockId,
    ) -> RpcResult<StarkFelt> {
        let txn = self.storage_reader.begin_ro_txn().map_err(internal_server_error)?;

        // Check that the block is valid and get the state number.
        let block_number = get_block_number(&txn, block_id)?;
        let state = StateNumber::right_after_block(block_number);
        let state_reader = txn.get_state_reader().map_err(internal_server_error)?;

        // Check that the contract exists.
        state_reader
            .get_class_hash_at(state, &contract_address)
            .map_err(internal_server_error)?
            .ok_or_else(|| ErrorObjectOwned::from(JsonRpcError::ContractNotFound))?;

        state_reader.get_storage_at(state, &contract_address, &key).map_err(internal_server_error)
    }

    #[instrument(skip(self), level = "debug", err, ret)]
    fn get_transaction_by_hash(
        &self,
        transaction_hash: TransactionHash,
    ) -> RpcResult<TransactionWithHash> {
        let txn = self.storage_reader.begin_ro_txn().map_err(internal_server_error)?;

        let transaction_index = txn
            .get_transaction_idx_by_hash(&transaction_hash)
            .map_err(internal_server_error)?
            .ok_or_else(|| ErrorObjectOwned::from(JsonRpcError::TransactionHashNotFound))?;

        let (transaction, _execution_status) = txn
            .get_transaction(transaction_index)
            .map_err(internal_server_error)?
            .ok_or_else(|| ErrorObjectOwned::from(JsonRpcError::TransactionHashNotFound))?;

        Ok(TransactionWithHash { transaction: transaction.into(), transaction_hash })
    }

    #[instrument(skip(self), level = "debug", err, ret)]
    fn get_transaction_by_block_id_and_index(
        &self,
        block_id: BlockId,
        index: TransactionOffsetInBlock,
    ) -> RpcResult<TransactionWithHash> {
        let txn = self.storage_reader.begin_ro_txn().map_err(internal_server_error)?;
        let block_number = get_block_number(&txn, block_id)?;

        let tx_index = TransactionIndex(block_number, index);
        let (transaction, _execution_status) = txn
            .get_transaction(tx_index)
            .map_err(internal_server_error)?
            .ok_or_else(|| ErrorObjectOwned::from(JsonRpcError::InvalidTransactionIndex))?;
        let transaction_hash = txn
            .get_transaction_hash_by_idx(&tx_index)
            .map_err(internal_server_error)?
            .ok_or_else(|| ErrorObjectOwned::from(JsonRpcError::InvalidTransactionIndex))?;

        Ok(TransactionWithHash { transaction: transaction.into(), transaction_hash })
    }

    #[instrument(skip(self), level = "debug", err, ret)]
    fn get_block_transaction_count(&self, block_id: BlockId) -> RpcResult<usize> {
        let txn = self.storage_reader.begin_ro_txn().map_err(internal_server_error)?;
        let block_number = get_block_number(&txn, block_id)?;
        let transactions: Vec<Transaction> = get_block_txs_by_number(&txn, block_number)?;

        Ok(transactions.len())
    }

    #[instrument(skip(self), level = "debug", err, ret)]
    fn get_state_update(&self, block_id: BlockId) -> RpcResult<StateUpdate> {
        let txn = self.storage_reader.begin_ro_txn().map_err(internal_server_error)?;

        // Get the block header for the block hash and state root.
        let block_number = get_block_number(&txn, block_id)?;
        let header: BlockHeader = get_block_header_by_number(&txn, block_number)?;

        // Get the old root.
        let parent_block_number = get_block_number(
            &txn,
            BlockId::HashOrNumber(BlockHashOrNumber::Hash(header.parent_hash)),
        );
        let mut old_root =
            GlobalRoot(StarkHash::try_from(GENESIS_HASH).map_err(internal_server_error)?);
        if parent_block_number.is_ok() {
            let parent_header: BlockHeader =
                get_block_header_by_number(&txn, parent_block_number.unwrap())?;
            old_root = parent_header.new_root;
        }

        // Get the block state diff.
        let thin_state_diff = txn
            .get_state_diff(block_number)
            .map_err(internal_server_error)?
            .ok_or_else(|| ErrorObjectOwned::from(JsonRpcError::BlockNotFound))?;

        Ok(StateUpdate {
            block_hash: header.block_hash,
            new_root: header.new_root,
            old_root,
            state_diff: thin_state_diff.into(),
        })
    }

    #[instrument(skip(self), level = "debug", err, ret)]
    fn get_transaction_receipt(
        &self,
        transaction_hash: TransactionHash,
    ) -> RpcResult<TransactionReceiptWithStatus> {
        let txn = self.storage_reader.begin_ro_txn().map_err(internal_server_error)?;

        let transaction_index = txn
            .get_transaction_idx_by_hash(&transaction_hash)
            .map_err(internal_server_error)?
            .ok_or_else(|| ErrorObjectOwned::from(JsonRpcError::TransactionHashNotFound))?;

        let block_number = transaction_index.0;
        let status = get_block_status(&txn, block_number)?;

        // rejected blocks should not be a part of the API so we early return here.
        // this assumption also holds for the conversion from block status to transaction status
        // where we set rejected blocks to unreachable.
        if status == BlockStatus::Rejected {
            return Err(ErrorObjectOwned::from(JsonRpcError::BlockNotFound))?;
        }

        let block_hash = get_block_header_by_number::<_, BlockHeader>(&txn, block_number)
            .map_err(internal_server_error)?
            .block_hash;

        let (_, transaction_execution_status) = txn
            .get_transaction(transaction_index)
            .map_err(internal_server_error)?
            .ok_or_else(|| ErrorObjectOwned::from(JsonRpcError::TransactionHashNotFound))?;

        // starting from starknet v0.12.1 blocks can have reverted transactions (transactions that
        // failed execution). RPC API v0.3 does not support these transactions therefore we
        // return here with an error.
        if transaction_execution_status == TransactionExecutionStatus::Reverted {
            return Err(ErrorObjectOwned::from(JsonRpcError::TransactionReverted))?;
        }

        let thin_tx_output = txn
            .get_transaction_output(transaction_index)
            .map_err(internal_server_error)?
            .ok_or_else(|| ErrorObjectOwned::from(JsonRpcError::TransactionHashNotFound))?;

        let events = txn
            .get_transaction_events(transaction_index)
            .map_err(internal_server_error)?
            .ok_or_else(|| ErrorObjectOwned::from(JsonRpcError::TransactionHashNotFound))?;

        let output = TransactionOutput::from_thin_transaction_output(thin_tx_output, events);

        // todo: nevo - check what the expected behavior is when the transaction is reverted
        // todo: nevo - check the meaning of the rejected status
        Ok(TransactionReceiptWithStatus {
            receipt: TransactionReceipt { transaction_hash, block_hash, block_number, output },
            status: status.into(),
        })
    }

    #[instrument(skip(self), level = "debug", err, ret)]
    fn get_class(
        &self,
        block_id: BlockId,
        class_hash: ClassHash,
    ) -> RpcResult<GatewayContractClass> {
        let txn = self.storage_reader.begin_ro_txn().map_err(internal_server_error)?;

        let block_number = get_block_number(&txn, block_id)?;
        let state_number = StateNumber::right_after_block(block_number);
        let state_reader = txn.get_state_reader().map_err(internal_server_error)?;

        // The class might be a deprecated class. Search it first in the declared classes and if not
        // found, search in the deprecated classes.
        if let Some(class) = state_reader
            .get_class_definition_at(state_number, &class_hash)
            .map_err(internal_server_error)?
        {
            Ok(GatewayContractClass::Sierra(class.try_into().map_err(internal_server_error)?))
        } else {
            let class = state_reader
                .get_deprecated_class_definition_at(state_number, &class_hash)
                .map_err(internal_server_error)?
                .ok_or_else(|| ErrorObjectOwned::from(JsonRpcError::ClassHashNotFound))?;
            Ok(GatewayContractClass::Cairo0(class.try_into().map_err(internal_server_error)?))
        }
    }

    #[instrument(skip(self), level = "debug", err, ret)]
    fn get_class_at(
        &self,
        block_id: BlockId,
        contract_address: ContractAddress,
    ) -> RpcResult<GatewayContractClass> {
        let txn = self.storage_reader.begin_ro_txn().map_err(internal_server_error)?;

        let block_number = get_block_number(&txn, block_id)?;
        let state_number = StateNumber::right_after_block(block_number);
        let state_reader = txn.get_state_reader().map_err(internal_server_error)?;

        let class_hash = state_reader
            .get_class_hash_at(state_number, &contract_address)
            .map_err(internal_server_error)?
            .ok_or_else(|| ErrorObjectOwned::from(JsonRpcError::ContractNotFound))?;

        if let Some(class) = state_reader
            .get_class_definition_at(state_number, &class_hash)
            .map_err(internal_server_error)?
        {
            Ok(GatewayContractClass::Sierra(class.try_into().map_err(internal_server_error)?))
        } else {
            let class = state_reader
                .get_deprecated_class_definition_at(state_number, &class_hash)
                .map_err(internal_server_error)?
                .ok_or_else(|| ErrorObjectOwned::from(JsonRpcError::ContractNotFound))?;
            Ok(GatewayContractClass::Cairo0(class.try_into().map_err(internal_server_error)?))
        }
    }

    #[instrument(skip(self), level = "debug", err, ret)]
    fn get_class_hash_at(
        &self,
        block_id: BlockId,
        contract_address: ContractAddress,
    ) -> RpcResult<ClassHash> {
        let txn = self.storage_reader.begin_ro_txn().map_err(internal_server_error)?;

        let block_number = get_block_number(&txn, block_id)?;
        let state = StateNumber::right_after_block(block_number);
        let state_reader = txn.get_state_reader().map_err(internal_server_error)?;

        state_reader
            .get_class_hash_at(state, &contract_address)
            .map_err(internal_server_error)?
            .ok_or_else(|| ErrorObjectOwned::from(JsonRpcError::ContractNotFound))
    }

    #[instrument(skip(self), level = "debug", err, ret)]
    fn get_nonce(&self, block_id: BlockId, contract_address: ContractAddress) -> RpcResult<Nonce> {
        let txn = self.storage_reader.begin_ro_txn().map_err(internal_server_error)?;

        let block_number = get_block_number(&txn, block_id)?;
        let state = StateNumber::right_after_block(block_number);
        let state_reader = txn.get_state_reader().map_err(internal_server_error)?;

        state_reader
            .get_nonce_at(state, &contract_address)
            .map_err(internal_server_error)?
            .ok_or_else(|| ErrorObjectOwned::from(JsonRpcError::ContractNotFound))
    }

    #[instrument(skip(self), level = "debug", err, ret)]
    fn chain_id(&self) -> RpcResult<String> {
        Ok(self.chain_id.as_hex())
    }

    #[instrument(skip(self), level = "debug", err, ret)]
    fn get_events(&self, filter: EventFilter) -> RpcResult<EventsChunk> {
        // Check the chunk size.
        if filter.chunk_size > self.max_events_chunk_size {
            return Err(ErrorObjectOwned::from(JsonRpcError::PageSizeTooBig));
        }
        // Check the number of keys.
        if filter.keys.len() > self.max_events_keys {
            return Err(ErrorObjectOwned::from(JsonRpcError::TooManyKeysInFilter));
        }

        // Get the requested block numbers.
        let txn = self.storage_reader.begin_ro_txn().map_err(internal_server_error)?;
        let from_block_number = filter
            .from_block
            .map_or(Ok(BlockNumber(0)), |block_id| get_block_number(&txn, block_id))?;
        let maybe_to_block_number =
            filter.to_block.map_or(get_latest_block_number(&txn), |block_id| {
                get_block_number(&txn, block_id).map(Some)
            })?;
        if maybe_to_block_number.is_none() {
            // There are no blocks.
            return Ok(EventsChunk { events: vec![], continuation_token: None });
        }
        let to_block_number = maybe_to_block_number.unwrap();
        if from_block_number > to_block_number {
            return Ok(EventsChunk { events: vec![], continuation_token: None });
        }

        // Get the event index. If there's a continuation token we take the event index from there.
        // Otherwise, we take the first index in the from_block_number.
        let event_index = match filter.continuation_token {
            Some(token) => token.parse()?.0,
            None => EventIndex(
                TransactionIndex(from_block_number, TransactionOffsetInBlock(0)),
                EventIndexInTransactionOutput(0),
            ),
        };

        // Collect the requested events.
        // Once we collected enough events, we continue to check if there are any more events
        // corresponding to the requested filter. If there are, we return a continuation token
        // pointing to the next relevant event. Otherwise, we return a continuation token None.
        let mut filtered_events = vec![];
        for ((from_address, event_index), content) in txn
            .iter_events(filter.address, event_index, to_block_number)
            .map_err(internal_server_error)?
        {
            let block_number = (event_index.0).0;
            if block_number > to_block_number {
                break;
            }
            if filter.address.is_some() && from_address != filter.address.unwrap() {
                break;
            }
            // TODO: Consider changing empty sets in the filer keys to None.
            if filter.keys.iter().enumerate().all(|(i, keys)| {
                content.keys.len() > i && (keys.is_empty() || keys.contains(&content.keys[i]))
            }) {
                if filtered_events.len() == filter.chunk_size {
                    return Ok(EventsChunk {
                        events: filtered_events,
                        continuation_token: Some(ContinuationToken::new(
                            ContinuationTokenAsStruct(event_index),
                        )?),
                    });
                }
                let header: BlockHeader = get_block_header_by_number(&txn, block_number)
                    .map_err(internal_server_error)?;
                let transaction_hash = txn
                    .get_transaction_hash_by_idx(&event_index.0)
                    .map_err(internal_server_error)?
                    .ok_or_else(|| internal_server_error("Unknown internal error."))?;
                let emitted_event = Event {
                    block_hash: header.block_hash,
                    block_number,
                    transaction_hash,
                    event: starknet_api::transaction::Event { from_address, content },
                };
                filtered_events.push(emitted_event);
            }
        }

        Ok(EventsChunk { events: filtered_events, continuation_token: None })
    }

    #[instrument(skip(self), level = "debug", err, ret)]
    fn syncing(&self) -> RpcResult<SyncingState> {
        // TODO(omri): This is temporary. Implement syncing logic.
        Ok(SyncingState::Synced)
    }
}

impl JsonRpcServerImpl for JsonRpcServerV0_3_0Impl {
    fn new(
        chain_id: ChainId,
        storage_reader: StorageReader,
        max_events_chunk_size: usize,
        max_events_keys: usize,
        shared_syncing_state: Arc<RwLock<SyncingState>>,
    ) -> Self {
        Self {
            chain_id,
            storage_reader,
            max_events_chunk_size,
            max_events_keys,
            shared_syncing_state,
        }
    }

    fn into_rpc_module(self) -> RpcModule<Self> {
        self.into_rpc()
    }
}
