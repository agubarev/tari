// Copyright 2022 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

#![recursion_limit = "2048"]
// Some functions have a large amount of dependencies (e.g. services) and historically this warning
// has lead to bundling of dependencies into a resources struct, which is then overused and is the
// wrong abstraction
#![allow(clippy::too_many_arguments)]

#[macro_use]
mod macros;
pub mod base_node_service;
pub mod connectivity_service;
pub mod error;
mod operation_id;
pub mod output_manager_service;
pub mod storage;
pub mod test_utils;
pub mod transaction_service;
pub mod types;

pub use types::WalletHasher; // For use externally to the code base
pub mod util;
pub mod wallet;

pub use operation_id::OperationId;

#[macro_use]
extern crate diesel;
#[macro_use]
extern crate diesel_migrations;

mod config;
pub mod schema;
pub mod utxo_scanner_service;

pub use config::{TransactionStage, WalletConfig};
use tari_contacts::contacts_service::storage::sqlite_db::ContactsServiceSqliteDatabase;
use tari_key_manager::key_manager_service::storage::sqlite_db::KeyManagerSqliteDatabase;
pub use wallet::Wallet;

use crate::{
    output_manager_service::storage::sqlite_db::OutputManagerSqliteDatabase,
    storage::{sqlite_db::wallet::WalletSqliteDatabase, sqlite_utilities::WalletDbConnection},
    transaction_service::storage::sqlite_db::TransactionServiceSqliteDatabase,
};

mod consts {
    // Import the auto-generated const values from the Manifest and Git
    include!(concat!(env!("OUT_DIR"), "/consts.rs"));
}

pub type WalletSqlite = Wallet<
    WalletSqliteDatabase,
    TransactionServiceSqliteDatabase,
    OutputManagerSqliteDatabase,
    ContactsServiceSqliteDatabase<WalletDbConnection>,
    KeyManagerSqliteDatabase<WalletDbConnection>,
>;
