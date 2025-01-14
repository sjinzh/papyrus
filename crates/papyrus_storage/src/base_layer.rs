//! Interface for handling data related to the base layer.
//!
//! The base layer is the blockchain that is used for the final verification of the StarkNet state.
//!
//! The common use case is Ethereum, but it can be other blockchains as well (including another
//! Starknet network).
//!
//! Import [`BaseLayerStorageReader`] and [`BaseLayerStorageWriter`] to read and write data related
//! to the base layer using a [`StorageTxn`].
//! # Example
//! ```
//! use papyrus_storage::base_layer::{BaseLayerStorageReader, BaseLayerStorageWriter};
//! use papyrus_storage::open_storage;
//! # use papyrus_storage::db::DbConfig;
//! # use starknet_api::core::ChainId;
//! use starknet_api::block::BlockNumber;
//!
//! # let dir_handle = tempfile::tempdir().unwrap();
//! # let dir = dir_handle.path().to_path_buf();
//! # let db_config = DbConfig {
//! #     path_prefix: dir,
//! #     chain_id: ChainId("SN_MAIN".to_owned()),
//! #     min_size: 1 << 20,    // 1MB
//! #     max_size: 1 << 35,    // 32GB
//! #     growth_step: 1 << 26, // 64MB
//! # };
//! let (reader, mut writer) = open_storage(db_config)?;
//! writer
//!     .begin_rw_txn()?                                    // Start a RW transaction.
//!     .update_base_layer_block_marker(&BlockNumber(3))?    //Update the base layer block marker.
//!     .commit()?; // Commit the transaction.
//! let block_number = reader.begin_ro_txn()?.get_base_layer_block_marker()?;
//! assert_eq!(block_number, BlockNumber(3));
//! # Ok::<(), papyrus_storage::StorageError>(())
//! ```
#[cfg(test)]
#[path = "base_layer_test.rs"]
mod base_layer_test;

use starknet_api::block::BlockNumber;

use crate::db::{TransactionKind, RW};
use crate::{MarkerKind, StorageResult, StorageTxn};

/// Interface for reading data related to the base layer.
pub trait BaseLayerStorageReader {
    /// The block number marker is the first block number that doesn't exist yet in the base layer.
    fn get_base_layer_block_marker(&self) -> StorageResult<BlockNumber>;
}

/// Interface for writing data related to the base layer.
pub trait BaseLayerStorageWriter
where
    Self: Sized,
{
    /// Updates the block marker of the base layer.
    // To enforce that no commit happen after a failure, we consume and return Self on success.
    fn update_base_layer_block_marker(self, block_number: &BlockNumber) -> StorageResult<Self>;
}

impl<'env, Mode: TransactionKind> BaseLayerStorageReader for StorageTxn<'env, Mode> {
    fn get_base_layer_block_marker(&self) -> StorageResult<BlockNumber> {
        let markers_table = self.txn.open_table(&self.tables.markers)?;
        Ok(markers_table.get(&self.txn, &MarkerKind::BaseLayerBlock)?.unwrap_or_default())
    }
}

impl<'env> BaseLayerStorageWriter for StorageTxn<'env, RW> {
    fn update_base_layer_block_marker(self, block_number: &BlockNumber) -> StorageResult<Self> {
        let markers_table = self.txn.open_table(&self.tables.markers)?;
        markers_table.upsert(&self.txn, &MarkerKind::BaseLayerBlock, block_number)?;
        Ok(self)
    }
}
