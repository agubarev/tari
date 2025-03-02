//  Copyright 2021, The Tari Project
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

use std::{
    any::Any,
    fmt::{Display, Formatter},
    io,
    iter::FromIterator,
};

use borsh::{BorshDeserialize, BorshSerialize};
use digest::Digest;
use integer_encoding::VarIntWriter;
use tari_crypto::{hash::blake2::Blake256, hashing::DomainSeparation};

use super::{
    decoder::{CovenantDecodeError, CovenantReadExt},
    encoder::CovenentWriteExt,
    BaseLayerCovenantsDomain,
    COVENANTS_FIELD_HASHER_LABEL,
};
use crate::{
    covenants::{byte_codes, error::CovenantError},
    transactions::transaction_components::{TransactionInput, TransactionOutput},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
#[repr(u8)]
pub enum OutputField {
    Commitment = byte_codes::FIELD_COMMITMENT,
    Script = byte_codes::FIELD_SCRIPT,
    SenderOffsetPublicKey = byte_codes::FIELD_SENDER_OFFSET_PUBLIC_KEY,
    Covenant = byte_codes::FIELD_COVENANT,
    Features = byte_codes::FIELD_FEATURES,
    FeaturesOutputType = byte_codes::FIELD_FEATURES_OUTPUT_TYPE,
    FeaturesMaturity = byte_codes::FIELD_FEATURES_MATURITY,
    FeaturesSideChainFeatures = byte_codes::FIELD_FEATURES_SIDE_CHAIN_FEATURES,
}

impl OutputField {
    pub fn from_byte(byte: u8) -> Result<Self, CovenantDecodeError> {
        use byte_codes::*;
        #[allow(clippy::enum_glob_use)]
        use OutputField::*;
        match byte {
            FIELD_COMMITMENT => Ok(Commitment),
            FIELD_SCRIPT => Ok(Script),
            FIELD_SENDER_OFFSET_PUBLIC_KEY => Ok(SenderOffsetPublicKey),
            FIELD_COVENANT => Ok(Covenant),
            FIELD_FEATURES => Ok(Features),
            FIELD_FEATURES_OUTPUT_TYPE => Ok(FeaturesOutputType),
            FIELD_FEATURES_MATURITY => Ok(FeaturesMaturity),
            FIELD_FEATURES_SIDE_CHAIN_FEATURES => Ok(FeaturesSideChainFeatures),

            _ => Err(CovenantDecodeError::UnknownByteCode { code: byte }),
        }
    }

    pub fn as_byte(self) -> u8 {
        self as u8
    }

    pub(super) fn get_field_value_ref<T: 'static + std::fmt::Debug>(self, output: &TransactionOutput) -> Option<&T> {
        #[allow(clippy::enum_glob_use)]
        use OutputField::*;
        let val = match self {
            Commitment => &output.commitment as &dyn Any,
            Script => &output.script as &dyn Any,
            SenderOffsetPublicKey => &output.sender_offset_public_key as &dyn Any,
            Covenant => &output.covenant as &dyn Any,
            Features => &output.features as &dyn Any,
            FeaturesOutputType => &output.features.output_type as &dyn Any,
            FeaturesMaturity => &output.features.maturity as &dyn Any,
            FeaturesSideChainFeatures => &output.features.sidechain_feature as &dyn Any,
        };
        val.downcast_ref::<T>()
    }

    pub fn get_field_value_bytes(self, output: &TransactionOutput) -> Vec<u8> {
        #[allow(clippy::enum_glob_use)]
        use OutputField::*;

        let mut writer = Vec::new();
        match self {
            Commitment => BorshSerialize::serialize(&output.commitment, &mut writer),
            Script => BorshSerialize::serialize(&output.script, &mut writer),
            SenderOffsetPublicKey => BorshSerialize::serialize(&output.sender_offset_public_key, &mut writer),
            Covenant => BorshSerialize::serialize(&output.covenant, &mut writer),
            Features => BorshSerialize::serialize(&output.features, &mut writer),
            FeaturesOutputType => BorshSerialize::serialize(&output.features.output_type, &mut writer),
            FeaturesMaturity => BorshSerialize::serialize(&output.features.maturity, &mut writer),
            FeaturesSideChainFeatures => BorshSerialize::serialize(&output.features.sidechain_feature, &mut writer),
        }
        .unwrap();
        writer
    }

    pub fn is_eq_input(self, input: &TransactionInput, output: &TransactionOutput) -> bool {
        #[allow(clippy::enum_glob_use)]
        use OutputField::*;
        match self {
            Commitment => input
                .commitment()
                .map(|commitment| *commitment == output.commitment)
                .unwrap_or(false),
            Script => input.script().map(|script| *script == output.script).unwrap_or(false),
            SenderOffsetPublicKey => input
                .sender_offset_public_key()
                .map(|sender_offset_public_key| *sender_offset_public_key == output.sender_offset_public_key)
                .unwrap_or(false),
            Covenant => input
                .covenant()
                .map(|covenant| *covenant == output.covenant)
                .unwrap_or(false),
            Features => input
                .features()
                .map(|features| *features == output.features)
                .unwrap_or(false),
            FeaturesOutputType => input
                .features()
                .map(|features| features.output_type == output.features.output_type)
                .unwrap_or(false),
            FeaturesMaturity => input
                .features()
                .map(|features| features.maturity == output.features.maturity)
                .unwrap_or(false),
            FeaturesSideChainFeatures => input
                .features()
                .map(|features| features.sidechain_feature == output.features.sidechain_feature)
                .unwrap_or(false),
        }
    }

    pub fn is_eq<T: PartialEq + std::fmt::Debug + 'static>(
        self,
        output: &TransactionOutput,
        val: &T,
    ) -> Result<bool, CovenantError> {
        #[allow(clippy::enum_glob_use)]
        use OutputField::*;
        match self {
            FeaturesSideChainFeatures => {
                match self.get_field_value_ref::<Option<Box<T>>>(output) {
                    Some(Some(field_val)) => Ok(**field_val == *val),
                    Some(None) => Ok(false),
                    None => {
                        // We need to check this, if T is of type output, then we need to check for a boxed<T>
                        // otherwise we need to check for a T, so we check both cases.
                        match self.get_field_value_ref::<Option<T>>(output) {
                            Some(Some(field_val)) => Ok(field_val == val),
                            Some(None) => Ok(false),
                            None => Err(CovenantError::InvalidArgument {
                                filter: "is_eq",
                                details: format!("Invalid type for field {}", self),
                            }),
                        }
                    },
                }
            },
            _ => match self.get_field_value_ref::<T>(output) {
                Some(field_val) => Ok(field_val == val),
                None => Err(CovenantError::InvalidArgument {
                    filter: "is_eq",
                    details: format!("Invalid type for field {}", self),
                }),
            },
        }
    }

    //---------------------------------- Macro helpers --------------------------------------------//
    #[allow(dead_code)]
    pub fn commitment() -> Self {
        OutputField::Commitment
    }

    #[allow(dead_code)]
    pub fn script() -> Self {
        OutputField::Script
    }

    #[allow(dead_code)]
    pub fn sender_offset_public_key() -> Self {
        OutputField::SenderOffsetPublicKey
    }

    #[allow(dead_code)]
    pub fn covenant() -> Self {
        OutputField::Covenant
    }

    #[allow(dead_code)]
    pub fn features() -> Self {
        OutputField::Features
    }

    #[allow(dead_code)]
    pub fn features_output_type() -> Self {
        OutputField::FeaturesOutputType
    }

    #[allow(dead_code)]
    pub fn features_maturity() -> Self {
        OutputField::FeaturesMaturity
    }

    #[allow(dead_code)]
    #[allow(dead_code)]
    pub fn features_sidechain_feature() -> Self {
        OutputField::FeaturesSideChainFeatures
    }
}

impl Display for OutputField {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        #[allow(clippy::enum_glob_use)]
        use OutputField::*;
        match self {
            Commitment => write!(f, "field::commitment"),
            SenderOffsetPublicKey => write!(f, "field::sender_offset_public_key"),
            Script => write!(f, "field::script"),
            Covenant => write!(f, "field::covenant"),
            Features => write!(f, "field::features"),
            FeaturesOutputType => write!(f, "field::features_flags"),
            FeaturesSideChainFeatures => write!(f, "field::features_sidechain_feature"),
            FeaturesMaturity => write!(f, "field::features_maturity"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, BorshSerialize, BorshDeserialize)]
pub struct OutputFields {
    fields: Vec<OutputField>,
}

impl OutputFields {
    /// The number of unique fields available. This always matches the number of variants in `OutputField`.
    pub const NUM_FIELDS: usize = 10;

    pub fn new() -> Self {
        Default::default()
    }

    pub fn push(&mut self, field: OutputField) {
        self.fields.push(field);
    }

    pub fn read_from<R: io::Read>(reader: &mut R) -> Result<Self, CovenantDecodeError> {
        // Each field is a byte
        let buf = reader.read_variable_length_bytes(Self::NUM_FIELDS)?;
        buf.iter().map(|byte| OutputField::from_byte(*byte)).collect()
    }

    pub fn write_to<W: io::Write>(&self, writer: &mut W) -> Result<usize, io::Error> {
        let len = self.fields.len();
        if len > Self::NUM_FIELDS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "tried to write more than maximum number of fields",
            ));
        }
        let mut written = writer.write_varint(len)?;
        for byte in self.iter().map(|f| f.as_byte()) {
            written += writer.write_u8_fixed(byte)?;
        }
        Ok(written)
    }

    pub fn iter(&self) -> impl Iterator<Item = &OutputField> + '_ {
        self.fields.iter()
    }

    pub fn len(&self) -> usize {
        self.fields.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    pub fn construct_challenge_from(&self, output: &TransactionOutput) -> Blake256 {
        let mut challenge = Blake256::new();
        BaseLayerCovenantsDomain::add_domain_separation_tag(&mut challenge, COVENANTS_FIELD_HASHER_LABEL);
        for field in &self.fields {
            challenge.update(field.get_field_value_bytes(output).as_slice());
        }
        challenge
    }

    pub fn fields(&self) -> &[OutputField] {
        &self.fields
    }
}

impl From<Vec<OutputField>> for OutputFields {
    fn from(fields: Vec<OutputField>) -> Self {
        OutputFields { fields }
    }
}
impl FromIterator<OutputField> for OutputFields {
    fn from_iter<T: IntoIterator<Item = OutputField>>(iter: T) -> Self {
        Self {
            fields: iter.into_iter().collect(),
        }
    }
}

#[cfg(test)]
mod test {

    use tari_common_types::types::{Commitment, PublicKey};
    use tari_script::script;

    use super::*;
    use crate::{
        covenant,
        covenants::test::{create_input, create_outputs, make_sample_sidechain_feature},
        transactions::{
            test_helpers::UtxoTestParams,
            transaction_components::{OutputFeatures, OutputType, SpentOutput},
        },
    };

    mod output_field {
        use super::*;

        mod is_eq {

            use super::*;

            #[test]
            fn it_returns_true_if_eq() {
                let side_chain_features = make_sample_sidechain_feature();
                let output = create_outputs(1, UtxoTestParams {
                    features: OutputFeatures {
                        sidechain_feature: Some(side_chain_features),
                        ..Default::default()
                    },
                    script: script![Drop Nop],
                    ..Default::default()
                })
                .remove(0);

                assert!(OutputField::Commitment.is_eq(&output, &output.commitment).unwrap());
                assert!(OutputField::Features.is_eq(&output, &output.features).unwrap());
                assert!(OutputField::Script.is_eq(&output, &output.script).unwrap());
                assert!(OutputField::Covenant.is_eq(&output, &output.covenant).unwrap());
                assert!(OutputField::FeaturesMaturity
                    .is_eq(&output, &output.features.maturity)
                    .unwrap());
                assert!(OutputField::FeaturesOutputType
                    .is_eq(&output, &output.features.output_type)
                    .unwrap());
                assert!(OutputField::FeaturesSideChainFeatures
                    .is_eq(&output, output.features.sidechain_feature.as_ref().unwrap())
                    .unwrap());
                assert!(OutputField::SenderOffsetPublicKey
                    .is_eq(&output, &output.sender_offset_public_key)
                    .unwrap());
            }

            #[test]
            fn it_returns_false_if_not_eq() {
                let side_chain_features = make_sample_sidechain_feature();
                let output = create_outputs(1, UtxoTestParams {
                    features: OutputFeatures {
                        sidechain_feature: Some(side_chain_features),
                        ..Default::default()
                    },
                    script: script![Drop Nop],
                    ..Default::default()
                })
                .remove(0);

                assert!(!OutputField::Commitment.is_eq(&output, &Commitment::default()).unwrap());
                assert!(!OutputField::Features
                    .is_eq(&output, &OutputFeatures::default())
                    .unwrap());
                assert!(!OutputField::Script.is_eq(&output, &script![Nop Drop]).unwrap());
                assert!(!OutputField::Covenant
                    .is_eq(&output, &covenant!(and(identity(), identity())))
                    .unwrap());
                assert!(!OutputField::FeaturesMaturity.is_eq(&output, &123u64).unwrap());
                assert!(!OutputField::FeaturesOutputType
                    .is_eq(&output, &OutputType::Coinbase)
                    .unwrap());
                assert!(!OutputField::SenderOffsetPublicKey
                    .is_eq(&output, &PublicKey::default())
                    .unwrap());
            }
        }

        mod is_eq_input {
            use super::*;

            #[test]
            fn it_returns_true_if_eq_input() {
                let output = create_outputs(1, UtxoTestParams {
                    features: OutputFeatures {
                        maturity: 42,
                        ..Default::default()
                    },
                    script: script![Drop Nop],
                    ..Default::default()
                })
                .remove(0);
                let mut input = create_input();
                if let SpentOutput::OutputData {
                    features,
                    commitment,
                    script,
                    sender_offset_public_key,
                    covenant,
                    ..
                } = &mut input.spent_output
                {
                    *features = output.features.clone();
                    *commitment = output.commitment.clone();
                    *script = output.script.clone();
                    *sender_offset_public_key = output.sender_offset_public_key.clone();
                    *covenant = output.covenant.clone();
                }

                assert!(OutputField::Commitment.is_eq_input(&input, &output));
                assert!(OutputField::Features.is_eq_input(&input, &output));
                assert!(OutputField::Script.is_eq_input(&input, &output));
                assert!(OutputField::Covenant.is_eq_input(&input, &output));
                assert!(OutputField::FeaturesMaturity.is_eq_input(&input, &output));
                assert!(OutputField::FeaturesOutputType.is_eq_input(&input, &output));
                assert!(OutputField::FeaturesSideChainFeatures.is_eq_input(&input, &output));
                assert!(OutputField::SenderOffsetPublicKey.is_eq_input(&input, &output));
            }
        }

        #[test]
        fn display() {
            let output_fields = [
                OutputField::Commitment,
                OutputField::Features,
                OutputField::FeaturesOutputType,
                OutputField::FeaturesSideChainFeatures,
                OutputField::FeaturesMaturity,
                OutputField::SenderOffsetPublicKey,
                OutputField::Script,
                OutputField::Covenant,
            ];
            output_fields.iter().for_each(|f| {
                assert!(f.to_string().starts_with("field::"));
            })
        }
    }

    mod output_fields {
        use super::*;

        mod construct_challenge_from {
            use blake2::Digest;
            use tari_crypto::hashing::DomainSeparation;

            use super::*;

            #[test]
            fn it_constructs_challenge_using_consensus_encoding() {
                let features = OutputFeatures {
                    maturity: 42,
                    output_type: OutputType::Coinbase,
                    ..Default::default()
                };
                let output = create_outputs(1, UtxoTestParams {
                    features,
                    script: script![Drop Nop],
                    ..Default::default()
                })
                .remove(0);

                let mut fields = OutputFields::new();
                fields.push(OutputField::Features);
                fields.push(OutputField::Commitment);
                fields.push(OutputField::Script);
                let hash = fields.construct_challenge_from(&output).finalize();
                let hash = hash.to_vec();

                let mut hasher = Blake256::new();
                BaseLayerCovenantsDomain::add_domain_separation_tag(&mut hasher, COVENANTS_FIELD_HASHER_LABEL);
                let expected_hash = hasher
                    .chain(output.features.try_to_vec().unwrap())
                    .chain(output.commitment.try_to_vec().unwrap())
                    .chain(output.script.try_to_vec().unwrap())
                    .finalize()
                    .to_vec();
                assert_eq!(hash, expected_hash);
            }
        }

        mod get_field_value_ref {
            use super::*;

            #[test]
            fn it_retrieves_the_value_as_ref() {
                let features = OutputFeatures {
                    maturity: 42,
                    ..Default::default()
                };
                let output = create_outputs(1, UtxoTestParams {
                    features: features.clone(),
                    ..Default::default()
                })
                .pop()
                .unwrap();
                let r = OutputField::Features.get_field_value_ref::<OutputFeatures>(&output);
                assert_eq!(*r.unwrap(), features);
            }
        }
    }
}
