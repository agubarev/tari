// Copyright 2019. The Tari Project
//
// Redistribution and use in source and binary forms, with or without modification, are permitted provided that the
// following conditions are met:
//
// 1. Redistributions of source code must retain the above copyright notice, this list of conditions and the following
// disclaimer.
//
// 2. Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the
// following disclaimer in the documentation and/or other materials provided with the distribution.
//
// 3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote
// products derived from this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES,
// INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
// DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
// SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
// SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
// WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE
// USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

use std::{collections::HashMap, fmt};

use serde::{Deserialize, Serialize};
use tari_common_types::{
    transaction::TxId,
    types::{FixedHash, PrivateKey, PublicKey, Signature},
};

use crate::transactions::{
    crypto_factories::CryptoFactories,
    transaction_components::{EncryptedData, TransactionOutput},
    transaction_protocol::{
        sender::{SingleRoundSenderData as SD, TransactionSenderMessage},
        single_receiver::SingleReceiverTransactionProtocol,
        TransactionMetadata,
        TransactionProtocolError,
    },
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum RecipientState {
    Finalized(Box<RecipientSignedMessage>),
    Failed(TransactionProtocolError),
}

impl fmt::Display for RecipientState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use RecipientState::{Failed, Finalized};
        match self {
            Finalized(signed_message) => write!(
                f,
                "Finalized({:?}, maturity = {})",
                signed_message.output.features.output_type, signed_message.output.features.maturity
            ),
            Failed(err) => write!(f, "Failed({:?})", err),
        }
    }
}

/// An enum describing the types of information that a recipient can send back to the receiver
#[derive(Debug, Clone, PartialEq)]
pub(super) enum RecipientInfo {
    None,
    Single(Option<Box<RecipientSignedMessage>>),
    Multiple(HashMap<u64, MultiRecipientInfo>),
}

#[allow(clippy::derivable_impls)]
impl Default for RecipientInfo {
    fn default() -> Self {
        RecipientInfo::Single(None)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct MultiRecipientInfo {
    pub commitment: FixedHash,
    pub data: RecipientSignedMessage,
}

/// This is the message containing the public data that the Receiver will send back to the Sender
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipientSignedMessage {
    pub tx_id: TxId,
    pub output: TransactionOutput,
    pub public_spend_key: PublicKey,
    pub partial_signature: Signature,
    pub tx_metadata: TransactionMetadata,
}

/// The generalised transaction recipient protocol. A different state transition network is followed depending on
/// whether this is a single recipient or one of many.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReceiverTransactionProtocol {
    pub state: RecipientState,
}

/// Initiate a new recipient protocol state.
///
/// It takes as input the transaction message from the sender (which will indicate how many rounds the transaction
/// protocol will undergo, the recipient's nonce and spend key, as well as the output features for this recipient's
/// transaction output.
///
/// The function returns the protocol in the relevant state. If this is a single-round protocol, the state will
/// already be finalised, and the return message will be accessible from the `get_signed_data` method.
impl ReceiverTransactionProtocol {
    pub fn new(
        info: TransactionSenderMessage,
        nonce: PrivateKey,
        spending_key: PrivateKey,
        factories: &CryptoFactories,
    ) -> ReceiverTransactionProtocol {
        let state = match info {
            TransactionSenderMessage::None => RecipientState::Failed(TransactionProtocolError::InvalidStateError),
            TransactionSenderMessage::Single(v) => {
                ReceiverTransactionProtocol::single_round(nonce, spending_key, &v, factories, &EncryptedData::default())
            },
            TransactionSenderMessage::Multiple => Self::multi_round(),
        };
        ReceiverTransactionProtocol { state }
    }

    /// This function creates a new Receiver Transaction Protocol where the resulting receiver output can be recovered
    pub fn new_with_recoverable_output(
        info: TransactionSenderMessage,
        nonce: PrivateKey,
        spending_key: PrivateKey,
        factories: &CryptoFactories,
        encrypted_data: &EncryptedData,
    ) -> ReceiverTransactionProtocol {
        let state = match info {
            TransactionSenderMessage::None => RecipientState::Failed(TransactionProtocolError::InvalidStateError),
            TransactionSenderMessage::Single(v) => {
                ReceiverTransactionProtocol::single_round(nonce, spending_key, &v, factories, encrypted_data)
            },
            TransactionSenderMessage::Multiple => Self::multi_round(),
        };
        ReceiverTransactionProtocol { state }
    }

    /// Returns true if the recipient protocol is finalised, and the signature data is ready to be sent to the sender.
    pub fn is_finalized(&self) -> bool {
        matches!(self.state, RecipientState::Finalized(_))
    }

    /// Method to determine if the transaction protocol has failed
    pub fn is_failed(&self) -> bool {
        matches!(&self.state, RecipientState::Failed(_))
    }

    /// Method to return the error behind a failure, if one has occurred
    pub fn failure_reason(&self) -> Option<TransactionProtocolError> {
        match &self.state {
            RecipientState::Failed(e) => Some(e.clone()),
            _ => None,
        }
    }

    /// Retrieve the final signature data to be returned to the sender to complete the transaction.
    pub fn get_signed_data(&self) -> Result<&RecipientSignedMessage, TransactionProtocolError> {
        match &self.state {
            RecipientState::Finalized(data) => Ok(data),
            _ => Err(TransactionProtocolError::InvalidStateError),
        }
    }

    /// Run the single-round recipient protocol, which can immediately construct an output and sign the data
    fn single_round(
        nonce: PrivateKey,
        key: PrivateKey,
        data: &SD,
        factories: &CryptoFactories,
        encrypted_data: &EncryptedData,
    ) -> RecipientState {
        let signer = SingleReceiverTransactionProtocol::create(data, nonce, key, factories, encrypted_data);
        match signer {
            Ok(signed_data) => RecipientState::Finalized(Box::new(signed_data)),
            Err(e) => RecipientState::Failed(e),
        }
    }

    fn multi_round() -> RecipientState {
        RecipientState::Failed(TransactionProtocolError::UnsupportedError(
            "Multiple recipients aren't supported yet".into(),
        ))
    }

    /// Create an empty SenderTransactionProtocol that can be used as a placeholder in data structures that do not
    /// require a well formed version
    pub fn new_placeholder() -> Self {
        ReceiverTransactionProtocol {
            state: RecipientState::Failed(TransactionProtocolError::IncompleteStateError(
                "This is a placeholder protocol".to_string(),
            )),
        }
    }
}

#[cfg(test)]
mod test {
    use rand::rngs::OsRng;
    use tari_common_types::types::{PrivateKey, PublicKey, Signature};
    use tari_crypto::{
        commitment::HomomorphicCommitmentFactory,
        keys::{PublicKey as PK, SecretKey as SecretKeyTrait},
    };
    use tari_script::TariScript;

    use crate::{
        covenants::Covenant,
        transactions::{
            crypto_factories::CryptoFactories,
            tari_amount::*,
            test_helpers::TestParams,
            transaction_components::{EncryptedData, OutputFeatures, TransactionKernel, TransactionKernelVersion},
            transaction_protocol::{
                sender::{SingleRoundSenderData, TransactionSenderMessage},
                RecoveryData,
                TransactionMetadata,
            },
            ReceiverTransactionProtocol,
        },
    };

    #[test]
    fn single_round_recipient() {
        let factories = CryptoFactories::default();
        let p = TestParams::new();
        let m = TransactionMetadata::new(MicroTari(125), 0);
        let script = TariScript::default();
        let amount = MicroTari(500);

        let features = OutputFeatures::default();
        let msg = SingleRoundSenderData {
            tx_id: 15u64.into(),
            amount,
            public_excess: PublicKey::from_secret_key(&p.spend_key), // any random key will do
            public_nonce: PublicKey::from_secret_key(&p.change_spend_key), // any random key will do
            metadata: m.clone(),
            message: "".to_string(),
            features,
            script,
            sender_offset_public_key: p.sender_offset_public_key,
            ephemeral_public_nonce: p.sender_ephemeral_public_nonce,
            covenant: Covenant::default(),
            minimum_value_promise: MicroTari::zero(),
        };
        let sender_info = TransactionSenderMessage::Single(Box::new(msg.clone()));
        let pubkey = PublicKey::from_secret_key(&p.spend_key);
        let receiver = ReceiverTransactionProtocol::new(sender_info, p.nonce.clone(), p.spend_key.clone(), &factories);
        assert!(receiver.is_finalized());
        let data = receiver.get_signed_data().unwrap();
        assert_eq!(data.tx_id.as_u64(), 15);
        assert_eq!(data.public_spend_key, pubkey);
        assert!(factories
            .commitment
            .open_value(&p.spend_key, 500, &data.output.commitment));
        data.output.verify_range_proof(&factories.range_proof).unwrap();
        let r_sum = &msg.public_nonce + &p.public_nonce;
        let excess = &msg.public_excess + &PublicKey::from_secret_key(&p.spend_key);
        let e = TransactionKernel::build_kernel_challenge_from_tx_meta(
            &TransactionKernelVersion::get_current_version(),
            &r_sum,
            &excess,
            &m,
        );
        let s = Signature::sign_raw(&p.spend_key, p.nonce, &e).unwrap();
        assert_eq!(data.partial_signature, s);
    }

    #[test]
    fn single_round_recipient_with_recovery() {
        let factories = CryptoFactories::default();
        let p = TestParams::new();
        // Rewind params
        let recovery_data = RecoveryData {
            encryption_key: PrivateKey::random(&mut OsRng),
        };
        let amount = MicroTari(500);
        let m = TransactionMetadata::new(MicroTari(125), 0);
        let script = TariScript::default();

        let features = OutputFeatures::default();
        let msg = SingleRoundSenderData {
            tx_id: 15u64.into(),
            amount,
            public_excess: PublicKey::from_secret_key(&p.spend_key), // any random key will do
            public_nonce: PublicKey::from_secret_key(&p.change_spend_key), // any random key will do
            metadata: m,
            message: "".to_string(),
            features,
            script,
            sender_offset_public_key: p.sender_offset_public_key,
            ephemeral_public_nonce: p.sender_ephemeral_public_nonce,
            covenant: Covenant::default(),
            minimum_value_promise: MicroTari::zero(),
        };
        let encrypted_data = EncryptedData::encrypt_data(
            &recovery_data.encryption_key,
            &factories.commitment.commit_value(&p.spend_key, amount.into()),
            amount,
            &p.spend_key,
        )
        .unwrap();
        let sender_info = TransactionSenderMessage::Single(Box::new(msg));
        let receiver = ReceiverTransactionProtocol::new_with_recoverable_output(
            sender_info,
            p.nonce.clone(),
            p.spend_key.clone(),
            &factories,
            &encrypted_data,
        );
        assert!(receiver.is_finalized());
        let data = receiver.get_signed_data().unwrap();

        let output = &data.output;
        let (committed_value, blinding_factor) = EncryptedData::decrypt_data(
            &recovery_data.encryption_key,
            &output.commitment,
            &output.encrypted_data,
        )
        .unwrap();
        assert_eq!(committed_value, amount);
        assert_eq!(blinding_factor, p.spend_key);
    }
}
