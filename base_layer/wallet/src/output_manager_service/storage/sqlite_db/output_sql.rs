//  Copyright 2021. The Tari Project
//
//  Redistribution and use in source and binary forms, with or without modification, are permitted provided that the
//  following conditions are met:
//
//  1. Redistributions of source code must retain the above copyright notice, this list of conditions and the following
//  disclaimer.
//
//  2. Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the
//  following disclaimer in the documentation and/or other materials provided with the distribution.
//
//  3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote
//  products derived from this software without specific prior written permission.
//
//  THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES,
//  INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
//  DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
//  SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
//  SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
//  WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE
//  USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

use std::convert::{TryFrom, TryInto};

use borsh::BorshDeserialize;
use chacha20poly1305::XChaCha20Poly1305;
use chrono::NaiveDateTime;
use derivative::Derivative;
use diesel::{prelude::*, sql_query, SqliteConnection};
use log::*;
use tari_common_sqlite::util::diesel_ext::ExpectedRowsExtension;
use tari_common_types::{
    encryption::{decrypt_bytes_integral_nonce, encrypt_bytes_integral_nonce, Encryptable},
    transaction::TxId,
    types::{ComAndPubSignature, Commitment, FixedHash, PrivateKey, PublicKey},
};
use tari_core::transactions::{
    tari_amount::MicroTari,
    transaction_components::{EncryptedData, OutputFeatures, OutputType, UnblindedOutput},
    CryptoFactories,
};
use tari_crypto::{commitment::HomomorphicCommitmentFactory, tari_utilities::ByteArray};
use tari_script::{ExecutionStack, TariScript};
use tari_utilities::Hidden;
use zeroize::Zeroize;

use crate::{
    output_manager_service::{
        error::OutputManagerStorageError,
        input_selection::{UtxoSelectionCriteria, UtxoSelectionMode},
        service::Balance,
        storage::{
            database::{OutputBackendQuery, SortDirection},
            models::DbUnblindedOutput,
            sqlite_db::{UpdateOutput, UpdateOutputSql},
            OutputSource,
            OutputStatus,
        },
        UtxoSelectionFilter,
        UtxoSelectionOrdering,
    },
    schema::outputs,
};

const LOG_TARGET: &str = "wallet::output_manager_service::database::wallet";

#[derive(Clone, Derivative, Queryable, Identifiable, PartialEq, QueryableByName)]
#[derivative(Debug)]
#[diesel(table_name = outputs)]
pub struct OutputSql {
    pub id: i32, // Auto inc primary key
    pub commitment: Option<Vec<u8>>,
    #[derivative(Debug = "ignore")]
    pub spending_key: Vec<u8>,
    pub value: i64,
    pub output_type: i32,
    pub maturity: i64,
    pub status: i32,
    pub hash: Option<Vec<u8>>,
    pub script: Vec<u8>,
    pub input_data: Vec<u8>,
    #[derivative(Debug = "ignore")]
    pub script_private_key: Vec<u8>,
    pub script_lock_height: i64,
    pub sender_offset_public_key: Vec<u8>,
    pub metadata_signature_ephemeral_commitment: Vec<u8>,
    pub metadata_signature_ephemeral_pubkey: Vec<u8>,
    pub metadata_signature_u_a: Vec<u8>,
    pub metadata_signature_u_x: Vec<u8>,
    pub metadata_signature_u_y: Vec<u8>,
    pub mined_height: Option<i64>,
    pub mined_in_block: Option<Vec<u8>>,
    pub mined_mmr_position: Option<i64>,
    pub marked_deleted_at_height: Option<i64>,
    pub marked_deleted_in_block: Option<Vec<u8>>,
    pub received_in_tx_id: Option<i64>,
    pub spent_in_tx_id: Option<i64>,
    pub coinbase_block_height: Option<i64>,
    pub coinbase_extra: Option<Vec<u8>>,
    pub features_json: String,
    pub spending_priority: i32,
    pub covenant: Vec<u8>,
    pub mined_timestamp: Option<NaiveDateTime>,
    pub encrypted_data: Vec<u8>,
    pub minimum_value_promise: i64,
    pub source: i32,
    pub last_validation_timestamp: Option<NaiveDateTime>,
}

impl OutputSql {
    /// Return all outputs
    pub fn index(conn: &mut SqliteConnection) -> Result<Vec<OutputSql>, OutputManagerStorageError> {
        Ok(outputs::table.load::<OutputSql>(conn)?)
    }

    /// Return all outputs with a given status
    pub fn index_status(
        statuses: Vec<OutputStatus>,
        conn: &mut SqliteConnection,
    ) -> Result<Vec<OutputSql>, OutputManagerStorageError> {
        Ok(outputs::table
            .filter(outputs::status.eq_any::<Vec<i32>>(statuses.into_iter().map(|s| s as i32).collect()))
            .load(conn)?)
    }

    /// Retrieves UTXOs by a set of given rules
    // TODO: maybe use a shorthand macros
    #[allow(clippy::cast_sign_loss)]
    pub fn fetch_outputs_by(
        q: OutputBackendQuery,
        conn: &mut SqliteConnection,
    ) -> Result<Vec<OutputSql>, OutputManagerStorageError> {
        let mut query = outputs::table
            .into_boxed()
            .filter(outputs::script_lock_height.le(q.tip_height))
            .filter(outputs::maturity.le(q.tip_height));

        if let Some((offset, limit)) = q.pagination {
            query = query.offset(offset).limit(limit);
        }

        // filtering by OutputStatus
        query = match q.status.len() {
            0 => query,
            1 => query.filter(outputs::status.eq(q.status[0] as i32)),
            _ => query.filter(outputs::status.eq_any::<Vec<i32>>(q.status.into_iter().map(|s| s as i32).collect())),
        };

        // filtering by Commitment
        if !q.commitments.is_empty() {
            query = match q.commitments.len() {
                0 => query,
                1 => query.filter(outputs::commitment.eq(q.commitments[0].to_vec())),
                _ => query.filter(
                    outputs::commitment.eq_any::<Vec<Vec<u8>>>(q.commitments.into_iter().map(|c| c.to_vec()).collect()),
                ),
            };
        }

        // if set, filtering by minimum value
        if let Some((min, is_inclusive)) = q.value_min {
            query = if is_inclusive {
                query.filter(outputs::value.ge(min))
            } else {
                query.filter(outputs::value.gt(min))
            };
        }

        // if set, filtering by max value
        if let Some((max, is_inclusive)) = q.value_max {
            query = if is_inclusive {
                query.filter(outputs::value.le(max))
            } else {
                query.filter(outputs::value.lt(max))
            };
        }

        use SortDirection::{Asc, Desc};
        Ok(q.sorting
            .into_iter()
            .fold(query, |query, s| match s {
                ("value", d) => match d {
                    Asc => query.then_order_by(outputs::value.asc()),
                    Desc => query.then_order_by(outputs::value.desc()),
                },
                ("mined_height", d) => match d {
                    Asc => query.then_order_by(outputs::mined_height.asc()),
                    Desc => query.then_order_by(outputs::mined_height.desc()),
                },
                _ => query,
            })
            .load(conn)?)
    }

    /// Retrieves UTXOs than can be spent, sorted by priority, then value from smallest to largest.
    #[allow(clippy::cast_sign_loss)]
    pub fn fetch_unspent_outputs_for_spending(
        selection_criteria: &UtxoSelectionCriteria,
        amount: u64,
        tip_height: Option<u64>,
        conn: &mut SqliteConnection,
    ) -> Result<Vec<OutputSql>, OutputManagerStorageError> {
        let i64_tip_height = tip_height.and_then(|h| i64::try_from(h).ok()).unwrap_or(i64::MAX);

        let mut query = outputs::table
            .into_boxed()
            .filter(outputs::status.eq(OutputStatus::Unspent as i32))
            .order_by(outputs::spending_priority.desc());

        // NOTE: Safe mode presets `script_lock_height` and `maturity` filters for all queries
        if selection_criteria.mode == UtxoSelectionMode::Safe {
            query = query
                .filter(outputs::script_lock_height.le(i64_tip_height))
                .filter(outputs::maturity.le(i64_tip_height));
        };

        match &selection_criteria.filter {
            UtxoSelectionFilter::Standard => {
                query = query.filter(
                    outputs::output_type
                        .eq(i32::from(OutputType::Standard.as_byte()))
                        .or(outputs::output_type.eq(i32::from(OutputType::Coinbase.as_byte()))),
                );

                if selection_criteria.excluding_onesided {
                    query = query.filter(outputs::source.ne(OutputSource::OneSided as i32));
                }
            },

            UtxoSelectionFilter::SpecificOutputs { commitments } => {
                query = match commitments.len() {
                    0 => query,
                    1 => query.filter(outputs::commitment.eq(commitments[0].to_vec())),
                    _ => query.filter(
                        outputs::commitment.eq_any::<Vec<Vec<u8>>>(commitments.iter().map(|c| c.to_vec()).collect()),
                    ),
                };
            },
        }

        for exclude in &selection_criteria.excluding {
            query = query.filter(outputs::commitment.ne(exclude.as_bytes()));
        }

        query = match selection_criteria.ordering {
            UtxoSelectionOrdering::SmallestFirst => query.then_order_by(outputs::value.asc()),
            UtxoSelectionOrdering::LargestFirst => query.then_order_by(outputs::value.desc()),
            UtxoSelectionOrdering::Default => {
                // NOTE: keeping filtering by `script_lock_height` and `maturity` for all modes
                // lets get the max value for all utxos
                let max: Option<i64> = outputs::table
                    .filter(outputs::status.eq(OutputStatus::Unspent as i32))
                    .filter(outputs::script_lock_height.le(i64_tip_height))
                    .filter(outputs::maturity.le(i64_tip_height))
                    .order(outputs::value.desc())
                    .select(outputs::value)
                    .first(conn)
                    .optional()?;

                match max {
                    // Want to reduce the number of inputs to reduce fees
                    Some(max) if amount > max as u64 => query.then_order_by(outputs::value.desc()),

                    // Use the smaller utxos to make up this transaction.
                    _ => query.then_order_by(outputs::value.asc()),
                }
            },
        };

        // debug!(
        //     target: LOG_TARGET,
        //     "Executing UTXO select query: {}",
        //     diesel::debug_query(&query)
        // );

        Ok(query.load(conn)?)
    }

    /// Return all unspent outputs that have a maturity above the provided chain tip
    #[allow(clippy::cast_possible_wrap)]
    pub fn index_time_locked(
        tip: u64,
        conn: &mut SqliteConnection,
    ) -> Result<Vec<OutputSql>, OutputManagerStorageError> {
        Ok(outputs::table
            .filter(outputs::status.eq(OutputStatus::Unspent as i32))
            .filter(outputs::maturity.gt(tip as i64))
            .load(conn)?)
    }

    pub fn index_unconfirmed(conn: &mut SqliteConnection) -> Result<Vec<OutputSql>, OutputManagerStorageError> {
        Ok(outputs::table
            .filter(
                outputs::status
                    .eq(OutputStatus::UnspentMinedUnconfirmed as i32)
                    .or(outputs::mined_in_block.is_null())
                    .or(outputs::mined_height.is_null()),
            )
            .order(outputs::id.asc())
            .load(conn)?)
    }

    pub fn index_by_output_type(
        output_type: OutputType,
        conn: &mut SqliteConnection,
    ) -> Result<Vec<OutputSql>, OutputManagerStorageError> {
        let res = diesel::sql_query("SELECT * FROM outputs where output_type & $1 = $1 ORDER BY id;")
            .bind::<diesel::sql_types::Integer, _>(i32::from(output_type.as_byte()))
            .load(conn)?;
        Ok(res)
    }

    pub fn index_unspent(conn: &mut SqliteConnection) -> Result<Vec<OutputSql>, OutputManagerStorageError> {
        Ok(outputs::table
            .filter(outputs::status.eq(OutputStatus::Unspent as i32))
            .order(outputs::id.asc())
            .load(conn)?)
    }

    pub fn index_marked_deleted_in_block_is_null(
        conn: &mut SqliteConnection,
    ) -> Result<Vec<OutputSql>, OutputManagerStorageError> {
        Ok(outputs::table
            // Return outputs not marked as deleted or confirmed
            .filter(outputs::marked_deleted_in_block.is_null().or(outputs::status.eq(OutputStatus::SpentMinedUnconfirmed as i32)))
            // Only return mined
            .filter(outputs::mined_in_block.is_not_null().and(outputs::mined_height.is_not_null()))
            .order(outputs::id.asc())
            .load(conn)?)
    }

    pub fn index_invalid(
        timestamp: &NaiveDateTime,
        conn: &mut SqliteConnection,
    ) -> Result<Vec<OutputSql>, OutputManagerStorageError> {
        Ok(outputs::table
            .filter(
                outputs::status
                    .eq(OutputStatus::Invalid as i32)
                    .or(outputs::status.eq(OutputStatus::CancelledInbound as i32)),
            )
            .filter(
                outputs::last_validation_timestamp
                    .le(timestamp)
                    .or(outputs::last_validation_timestamp.is_null()),
            )
            .order(outputs::id.asc())
            .load(conn)?)
    }

    pub fn first_by_mined_height_desc(
        conn: &mut SqliteConnection,
    ) -> Result<Option<OutputSql>, OutputManagerStorageError> {
        Ok(outputs::table
            .filter(outputs::mined_height.is_not_null())
            .order(outputs::mined_height.desc())
            .first(conn)
            .optional()?)
    }

    pub fn first_by_marked_deleted_height_desc(
        conn: &mut SqliteConnection,
    ) -> Result<Option<OutputSql>, OutputManagerStorageError> {
        Ok(outputs::table
            .filter(outputs::marked_deleted_at_height.is_not_null())
            .order(outputs::marked_deleted_at_height.desc())
            .first(conn)
            .optional()?)
    }

    /// Find a particular Output, if it exists
    pub fn find(spending_key: &[u8], conn: &mut SqliteConnection) -> Result<OutputSql, OutputManagerStorageError> {
        Ok(outputs::table
            .filter(outputs::spending_key.eq(spending_key))
            .first::<OutputSql>(conn)?)
    }

    pub fn find_by_tx_id(
        tx_id: TxId,
        conn: &mut SqliteConnection,
    ) -> Result<Vec<OutputSql>, OutputManagerStorageError> {
        Ok(outputs::table
            .filter(
                outputs::received_in_tx_id
                    .eq(tx_id.as_i64_wrapped())
                    .or(outputs::spent_in_tx_id.eq(tx_id.as_i64_wrapped())),
            )
            .load(conn)?)
    }

    /// Return the available, time locked, pending incoming and pending outgoing balance
    #[allow(clippy::cast_possible_wrap)]
    pub fn get_balance(
        current_tip_for_time_lock_calculation: Option<u64>,
        conn: &mut SqliteConnection,
    ) -> Result<Balance, OutputManagerStorageError> {
        #[derive(QueryableByName, Clone)]
        struct BalanceQueryResult {
            #[diesel(sql_type = diesel::sql_types::BigInt)]
            amount: i64,
            #[diesel(sql_type = diesel::sql_types::Text)]
            category: String,
        }
        let balance_query_result = if let Some(current_tip) = current_tip_for_time_lock_calculation {
            let balance_query = sql_query(
                "SELECT coalesce(sum(value), 0) as amount, 'available_balance' as category \
                 FROM outputs WHERE status = ? \
                 UNION ALL \
                 SELECT coalesce(sum(value), 0) as amount, 'time_locked_balance' as category \
                 FROM outputs WHERE status = ? AND maturity > ? OR script_lock_height > ? \
                 UNION ALL \
                 SELECT coalesce(sum(value), 0) as amount, 'pending_incoming_balance' as category \
                 FROM outputs WHERE source != ? AND status = ? OR status = ? OR status = ? \
                 UNION ALL \
                 SELECT coalesce(sum(value), 0) as amount, 'pending_outgoing_balance' as category \
                 FROM outputs WHERE status = ? OR status = ? OR status = ?",
            )
                // available_balance
                .bind::<diesel::sql_types::Integer, _>(OutputStatus::Unspent as i32)
                // time_locked_balance
                .bind::<diesel::sql_types::Integer, _>(OutputStatus::Unspent as i32)
                .bind::<diesel::sql_types::BigInt, _>(current_tip as i64)
                .bind::<diesel::sql_types::BigInt, _>(current_tip as i64)
                // pending_incoming_balance
                .bind::<diesel::sql_types::Integer, _>(OutputSource::Coinbase as i32)
                .bind::<diesel::sql_types::Integer, _>(OutputStatus::EncumberedToBeReceived as i32)
                .bind::<diesel::sql_types::Integer, _>(OutputStatus::ShortTermEncumberedToBeReceived as i32)
                .bind::<diesel::sql_types::Integer, _>(OutputStatus::UnspentMinedUnconfirmed as i32)
                // pending_outgoing_balance
                .bind::<diesel::sql_types::Integer, _>(OutputStatus::EncumberedToBeSpent as i32)
                .bind::<diesel::sql_types::Integer, _>(OutputStatus::ShortTermEncumberedToBeSpent as i32)
                .bind::<diesel::sql_types::Integer, _>(OutputStatus::SpentMinedUnconfirmed as i32);
            balance_query.load::<BalanceQueryResult>(conn)?
        } else {
            let balance_query = sql_query(
                "SELECT coalesce(sum(value), 0) as amount, 'available_balance' as category \
                 FROM outputs WHERE status = ? \
                 UNION ALL \
                 SELECT coalesce(sum(value), 0) as amount, 'pending_incoming_balance' as category \
                 FROM outputs WHERE source != ? AND status = ? OR status = ? OR status = ? \
                 UNION ALL \
                 SELECT coalesce(sum(value), 0) as amount, 'pending_outgoing_balance' as category \
                 FROM outputs WHERE status = ? OR status = ? OR status = ?",
            )
                // available_balance
                .bind::<diesel::sql_types::Integer, _>(OutputStatus::Unspent as i32)
                // pending_incoming_balance
                .bind::<diesel::sql_types::Integer, _>(OutputSource::Coinbase as i32)
                .bind::<diesel::sql_types::Integer, _>(OutputStatus::EncumberedToBeReceived as i32)
                .bind::<diesel::sql_types::Integer, _>(OutputStatus::ShortTermEncumberedToBeReceived as i32)
                .bind::<diesel::sql_types::Integer, _>(OutputStatus::UnspentMinedUnconfirmed as i32)
                // pending_outgoing_balance
                .bind::<diesel::sql_types::Integer, _>(OutputStatus::EncumberedToBeSpent as i32)
                .bind::<diesel::sql_types::Integer, _>(OutputStatus::ShortTermEncumberedToBeSpent as i32)
                .bind::<diesel::sql_types::Integer, _>(OutputStatus::SpentMinedUnconfirmed as i32);
            balance_query.load::<BalanceQueryResult>(conn)?
        };
        let mut available_balance = None;
        let mut time_locked_balance = Some(None);
        let mut pending_incoming_balance = None;
        let mut pending_outgoing_balance = None;
        for balance in balance_query_result {
            match balance.category.as_str() {
                "available_balance" => available_balance = Some(MicroTari::from(balance.amount as u64)),
                "time_locked_balance" => time_locked_balance = Some(Some(MicroTari::from(balance.amount as u64))),
                "pending_incoming_balance" => pending_incoming_balance = Some(MicroTari::from(balance.amount as u64)),
                "pending_outgoing_balance" => pending_outgoing_balance = Some(MicroTari::from(balance.amount as u64)),
                _ => {
                    return Err(OutputManagerStorageError::UnexpectedResult(
                        "Unexpected category in balance query".to_string(),
                    ))
                },
            }
        }

        Ok(Balance {
            available_balance: available_balance.ok_or_else(|| {
                OutputManagerStorageError::UnexpectedResult("Available balance could not be calculated".to_string())
            })?,
            time_locked_balance: time_locked_balance.ok_or_else(|| {
                OutputManagerStorageError::UnexpectedResult("Time locked balance could not be calculated".to_string())
            })?,
            pending_incoming_balance: pending_incoming_balance.ok_or_else(|| {
                OutputManagerStorageError::UnexpectedResult(
                    "Pending incoming balance could not be calculated".to_string(),
                )
            })?,
            pending_outgoing_balance: pending_outgoing_balance.ok_or_else(|| {
                OutputManagerStorageError::UnexpectedResult(
                    "Pending outgoing balance could not be calculated".to_string(),
                )
            })?,
        })
    }

    pub fn find_by_commitment(
        commitment: &[u8],
        conn: &mut SqliteConnection,
    ) -> Result<OutputSql, OutputManagerStorageError> {
        Ok(outputs::table
            .filter(outputs::commitment.eq(commitment))
            .first::<OutputSql>(conn)?)
    }

    pub fn find_by_commitments_excluding_status(
        commitments: Vec<&[u8]>,
        status: OutputStatus,
        conn: &mut SqliteConnection,
    ) -> Result<Vec<OutputSql>, OutputManagerStorageError> {
        Ok(outputs::table
            .filter(outputs::commitment.eq_any(commitments))
            .filter(outputs::status.ne(status as i32))
            .load(conn)?)
    }

    pub fn update_by_commitments(
        commitments: Vec<&[u8]>,
        updated_output: UpdateOutput,
        conn: &mut SqliteConnection,
    ) -> Result<usize, OutputManagerStorageError> {
        Ok(
            diesel::update(outputs::table.filter(outputs::commitment.eq_any(commitments)))
                .set(UpdateOutputSql::from(updated_output))
                .execute(conn)?,
        )
    }

    pub fn find_by_commitment_and_cancelled(
        commitment: &[u8],
        cancelled: bool,
        conn: &mut SqliteConnection,
    ) -> Result<OutputSql, OutputManagerStorageError> {
        let cancelled_flag = OutputStatus::CancelledInbound as i32;

        let mut request = outputs::table.filter(outputs::commitment.eq(commitment)).into_boxed();
        if cancelled {
            request = request.filter(outputs::status.eq(cancelled_flag))
        } else {
            request = request.filter(outputs::status.ne(cancelled_flag))
        };

        Ok(request.first::<OutputSql>(conn)?)
    }

    pub fn find_by_tx_id_and_status(
        tx_id: TxId,
        status: OutputStatus,
        conn: &mut SqliteConnection,
    ) -> Result<Vec<OutputSql>, OutputManagerStorageError> {
        Ok(outputs::table
            .filter(
                outputs::received_in_tx_id
                    .eq(Some(tx_id.as_u64() as i64))
                    .or(outputs::spent_in_tx_id.eq(Some(tx_id.as_u64() as i64))),
            )
            .filter(outputs::status.eq(status as i32))
            .load(conn)?)
    }

    /// Find outputs via tx_id that are encumbered. Any outputs that are encumbered cannot be marked as spent.
    pub fn find_by_tx_id_and_encumbered(
        tx_id: TxId,
        conn: &mut SqliteConnection,
    ) -> Result<Vec<OutputSql>, OutputManagerStorageError> {
        Ok(outputs::table
            .filter(
                outputs::received_in_tx_id
                    .eq(Some(tx_id.as_u64() as i64))
                    .or(outputs::spent_in_tx_id.eq(Some(tx_id.as_u64() as i64))),
            )
            .filter(
                outputs::status
                    .eq(OutputStatus::EncumberedToBeReceived as i32)
                    .or(outputs::status.eq(OutputStatus::EncumberedToBeSpent as i32))
                    .or(outputs::status.eq(OutputStatus::ShortTermEncumberedToBeReceived as i32))
                    .or(outputs::status.eq(OutputStatus::ShortTermEncumberedToBeSpent as i32)),
            )
            .load(conn)?)
    }

    /// Find a particular Output, if it exists and is in the specified Spent state
    pub fn find_status(
        spending_key: &[u8],
        status: OutputStatus,
        conn: &mut SqliteConnection,
    ) -> Result<OutputSql, OutputManagerStorageError> {
        Ok(outputs::table
            .filter(outputs::status.eq(status as i32))
            .filter(outputs::spending_key.eq(spending_key))
            .first::<OutputSql>(conn)?)
    }

    /// Find a particular Output, if it exists and is in the specified Spent state
    pub fn find_by_hash(
        hash: &[u8],
        status: OutputStatus,
        conn: &mut SqliteConnection,
    ) -> Result<OutputSql, OutputManagerStorageError> {
        Ok(outputs::table
            .filter(outputs::status.eq(status as i32))
            .filter(outputs::hash.eq(Some(hash)))
            .first::<OutputSql>(conn)?)
    }

    /// Find a particular Output, if it exists and is in the specified Spent state
    pub fn find_pending_coinbase_at_block_height(
        block_height: u64,
        conn: &mut SqliteConnection,
    ) -> Result<OutputSql, OutputManagerStorageError> {
        Ok(outputs::table
            .filter(outputs::status.ne(OutputStatus::Unspent as i32))
            .filter(outputs::coinbase_block_height.eq(block_height as i64))
            .first::<OutputSql>(conn)?)
    }

    pub fn delete(&self, conn: &mut SqliteConnection) -> Result<(), OutputManagerStorageError> {
        let num_deleted =
            diesel::delete(outputs::table.filter(outputs::spending_key.eq(&self.spending_key))).execute(conn)?;

        if num_deleted == 0 {
            return Err(OutputManagerStorageError::ValuesNotFound);
        }

        Ok(())
    }

    pub fn update(
        &self,
        updated_output: UpdateOutput,
        conn: &mut SqliteConnection,
    ) -> Result<OutputSql, OutputManagerStorageError> {
        diesel::update(outputs::table.filter(outputs::id.eq(&self.id)))
            .set(UpdateOutputSql::from(updated_output))
            .execute(conn)
            .num_rows_affected_or_not_found(1)?;

        OutputSql::find(&self.spending_key, conn)
    }

    #[allow(clippy::too_many_lines)]
    pub fn to_db_unblinded_output(
        self,
        cipher: &XChaCha20Poly1305,
    ) -> Result<DbUnblindedOutput, OutputManagerStorageError> {
        let mut o = self.decrypt(cipher).map_err(OutputManagerStorageError::AeadError)?;

        let features: OutputFeatures =
            serde_json::from_str(&o.features_json).map_err(|s| OutputManagerStorageError::ConversionError {
                reason: format!("Could not convert json into OutputFeatures:{}", s),
            })?;

        let covenant = BorshDeserialize::deserialize(&mut o.covenant.as_bytes()).map_err(|e| {
            error!(
                target: LOG_TARGET,
                "Could not create Covenant from stored bytes ({}), They might be encrypted", e
            );
            OutputManagerStorageError::ConversionError {
                reason: "Covenant could not be converted from bytes".to_string(),
            }
        })?;

        let encrypted_data = EncryptedData::from_bytes(&o.encrypted_data)?;
        let unblinded_output = UnblindedOutput::new_current_version(
            MicroTari::from(o.value as u64),
            PrivateKey::from_vec(&o.spending_key).map_err(|_| {
                error!(
                    target: LOG_TARGET,
                    "Could not create PrivateKey from stored bytes, They might be encrypted"
                );
                OutputManagerStorageError::ConversionError {
                    reason: "PrivateKey could not be converted from bytes".to_string(),
                }
            })?,
            features,
            TariScript::from_bytes(o.script.as_slice())?,
            ExecutionStack::from_bytes(o.input_data.as_slice())?,
            PrivateKey::from_vec(&o.script_private_key).map_err(|_| {
                error!(
                    target: LOG_TARGET,
                    "Could not create PrivateKey from stored bytes, They might be encrypted"
                );
                OutputManagerStorageError::ConversionError {
                    reason: "PrivateKey could not be converted from bytes".to_string(),
                }
            })?,
            PublicKey::from_vec(&o.sender_offset_public_key).map_err(|_| {
                error!(
                    target: LOG_TARGET,
                    "Could not create PublicKey from stored bytes, They might be encrypted"
                );
                OutputManagerStorageError::ConversionError {
                    reason: "PrivateKey could not be converted from bytes".to_string(),
                }
            })?,
            ComAndPubSignature::new(
                Commitment::from_vec(&o.metadata_signature_ephemeral_commitment).map_err(|_| {
                    error!(
                        target: LOG_TARGET,
                        "Could not create Commitment from stored bytes, They might be encrypted"
                    );
                    OutputManagerStorageError::ConversionError {
                        reason: "Commitment could not be converted from bytes".to_string(),
                    }
                })?,
                PublicKey::from_vec(&o.metadata_signature_ephemeral_pubkey).map_err(|_| {
                    error!(
                        target: LOG_TARGET,
                        "Could not create PublicKey from stored bytes, They might be encrypted"
                    );
                    OutputManagerStorageError::ConversionError {
                        reason: "PublicKey could not be converted from bytes".to_string(),
                    }
                })?,
                PrivateKey::from_vec(&o.metadata_signature_u_a).map_err(|_| {
                    error!(
                        target: LOG_TARGET,
                        "Could not create PrivateKey from stored bytes, They might be encrypted"
                    );
                    OutputManagerStorageError::ConversionError {
                        reason: "PrivateKey could not be converted from bytes".to_string(),
                    }
                })?,
                PrivateKey::from_vec(&o.metadata_signature_u_x).map_err(|_| {
                    error!(
                        target: LOG_TARGET,
                        "Could not create PrivateKey from stored bytes, They might be encrypted"
                    );
                    OutputManagerStorageError::ConversionError {
                        reason: "PrivateKey could not be converted from bytes".to_string(),
                    }
                })?,
                PrivateKey::from_vec(&o.metadata_signature_u_y).map_err(|_| {
                    error!(
                        target: LOG_TARGET,
                        "Could not create PrivateKey from stored bytes, They might be encrypted"
                    );
                    OutputManagerStorageError::ConversionError {
                        reason: "PrivateKey could not be converted from bytes".to_string(),
                    }
                })?,
            ),
            o.script_lock_height as u64,
            covenant,
            encrypted_data,
            MicroTari::from(o.minimum_value_promise as u64),
        );

        // we manually zeroize the sensitive data associated with OuptputSql, to avoid any leaks
        o.spending_key.zeroize();
        o.script_private_key.zeroize();

        let factories = CryptoFactories::default();
        let commitment = match o.commitment {
            None => factories
                .commitment
                .commit(&unblinded_output.spending_key, &unblinded_output.value.into()),
            Some(c) => Commitment::from_vec(&c)?,
        };
        let hash = match o.hash {
            None => unblinded_output.hash(&factories),
            Some(v) => match <Vec<u8> as TryInto<FixedHash>>::try_into(v) {
                Ok(v) => v,
                Err(e) => {
                    error!(target: LOG_TARGET, "Malformed output hash: {}", e);
                    return Err(OutputManagerStorageError::ConversionError {
                        reason: "Malformed output hash".to_string(),
                    });
                },
            },
        };
        let spending_priority = (o.spending_priority as u32).into();
        let mined_in_block = match o.mined_in_block {
            Some(v) => match v.try_into() {
                Ok(v) => Some(v),
                Err(_) => None,
            },
            None => None,
        };
        let marked_deleted_in_block = match o.marked_deleted_in_block {
            Some(v) => match v.try_into() {
                Ok(v) => Some(v),
                Err(_) => None,
            },
            None => None,
        };
        Ok(DbUnblindedOutput {
            commitment,
            unblinded_output,
            hash,
            status: o.status.try_into()?,
            mined_height: o.mined_height.map(|mh| mh as u64),
            mined_in_block,
            mined_mmr_position: o.mined_mmr_position.map(|mp| mp as u64),
            mined_timestamp: o.mined_timestamp,
            marked_deleted_at_height: o.marked_deleted_at_height.map(|d| d as u64),
            marked_deleted_in_block,
            spending_priority,
            source: o.source.try_into()?,
            received_in_tx_id: o.received_in_tx_id.map(|d| (d as u64).into()),
            spent_in_tx_id: o.spent_in_tx_id.map(|d| (d as u64).into()),
        })
    }
}

impl Encryptable<XChaCha20Poly1305> for OutputSql {
    fn domain(&self, field_name: &'static str) -> Vec<u8> {
        // WARNING: using `OUTPUT` for both NewOutputSql and OutputSql due to later transition without re-encryption
        [Self::OUTPUT, self.script.as_slice(), field_name.as_bytes()]
            .concat()
            .to_vec()
    }

    fn encrypt(mut self, cipher: &XChaCha20Poly1305) -> Result<Self, String> {
        self.spending_key = encrypt_bytes_integral_nonce(
            cipher,
            self.domain("spending_key"),
            Hidden::hide(self.spending_key.clone()),
        )?;

        self.script_private_key = encrypt_bytes_integral_nonce(
            cipher,
            self.domain("script_private_key"),
            Hidden::hide(self.script_private_key),
        )?;

        Ok(self)
    }

    fn decrypt(mut self, cipher: &XChaCha20Poly1305) -> Result<Self, String> {
        self.spending_key = decrypt_bytes_integral_nonce(cipher, self.domain("spending_key"), &self.spending_key)?;

        self.script_private_key =
            decrypt_bytes_integral_nonce(cipher, self.domain("script_private_key"), &self.script_private_key)?;

        Ok(self)
    }
}
