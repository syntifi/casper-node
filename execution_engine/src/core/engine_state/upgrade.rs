//! Support for applying upgrades on the execution engine.
use std::{cell::RefCell, collections::BTreeMap, fmt, rc::Rc};

use num_rational::Ratio;
use thiserror::Error;

use casper_hashing::Digest;
use casper_types::{
    bytesrepr,
    system::{
        auction, handle_payment, mint, standard_payment, AUCTION, HANDLE_PAYMENT, MINT,
        STANDARD_PAYMENT,
    },
    Contract, ContractHash, EntryPoints, EraId, Key, ProtocolVersion, StoredValue,
};

use crate::{
    core::{engine_state::execution_effect::ExecutionEffect, tracking_copy::TrackingCopy},
    shared::newtypes::CorrelationId,
    storage::global_state::StateProvider,
};

/// Represents a successfuly executed upgrade.
#[derive(Debug, Clone)]
pub struct UpgradeSuccess {
    /// New state root hash generated after effects were applied.
    pub post_state_hash: Digest,
    /// Effects of executing an upgrade request.
    pub execution_effect: ExecutionEffect,
}

impl fmt::Display for UpgradeSuccess {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(
            f,
            "Success: {} {:?}",
            self.post_state_hash, self.execution_effect
        )
    }
}

/// Represents the configuration of a protocol upgrade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeConfig {
    pre_state_hash: Digest,
    current_protocol_version: ProtocolVersion,
    new_protocol_version: ProtocolVersion,
    activation_point: Option<EraId>,
    new_validator_slots: Option<u32>,
    new_auction_delay: Option<u64>,
    new_locked_funds_period_millis: Option<u64>,
    new_round_seigniorage_rate: Option<Ratio<u64>>,
    new_unbonding_delay: Option<u64>,
    global_state_update: BTreeMap<Key, StoredValue>,
}

impl UpgradeConfig {
    /// Create new upgrade config.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pre_state_hash: Digest,
        current_protocol_version: ProtocolVersion,
        new_protocol_version: ProtocolVersion,
        activation_point: Option<EraId>,
        new_validator_slots: Option<u32>,
        new_auction_delay: Option<u64>,
        new_locked_funds_period_millis: Option<u64>,
        new_round_seigniorage_rate: Option<Ratio<u64>>,
        new_unbonding_delay: Option<u64>,
        global_state_update: BTreeMap<Key, StoredValue>,
    ) -> Self {
        UpgradeConfig {
            pre_state_hash,
            current_protocol_version,
            new_protocol_version,
            activation_point,
            new_validator_slots,
            new_auction_delay,
            new_locked_funds_period_millis,
            new_round_seigniorage_rate,
            new_unbonding_delay,
            global_state_update,
        }
    }

    /// Returns the current state root state hash
    pub fn pre_state_hash(&self) -> Digest {
        self.pre_state_hash
    }

    /// Returns current protocol version of this upgrade.
    pub fn current_protocol_version(&self) -> ProtocolVersion {
        self.current_protocol_version
    }

    /// Returns new protocol version of this upgrade.
    pub fn new_protocol_version(&self) -> ProtocolVersion {
        self.new_protocol_version
    }

    /// Returns activation point in eras.
    pub fn activation_point(&self) -> Option<EraId> {
        self.activation_point
    }

    /// Returns new validator slots if specified.
    pub fn new_validator_slots(&self) -> Option<u32> {
        self.new_validator_slots
    }

    /// Returns new auction delay if specified.
    pub fn new_auction_delay(&self) -> Option<u64> {
        self.new_auction_delay
    }

    /// Returns new locked funds period if specified.
    pub fn new_locked_funds_period_millis(&self) -> Option<u64> {
        self.new_locked_funds_period_millis
    }

    /// Returns new round seigniorage rate if specified.
    pub fn new_round_seigniorage_rate(&self) -> Option<Ratio<u64>> {
        self.new_round_seigniorage_rate
    }

    /// Returns new unbonding delay if specified.
    pub fn new_unbonding_delay(&self) -> Option<u64> {
        self.new_unbonding_delay
    }

    /// Returns new map of emergency global state updates.
    pub fn global_state_update(&self) -> &BTreeMap<Key, StoredValue> {
        &self.global_state_update
    }

    /// Sets new pre state hash.
    pub fn with_pre_state_hash(&mut self, pre_state_hash: Digest) {
        self.pre_state_hash = pre_state_hash;
    }
}

/// Represents outcomes of a failed protocol upgrade.
#[derive(Clone, Error, Debug)]
pub enum ProtocolUpgradeError {
    /// Error validating a protocol upgrade config.
    #[error("Invalid upgrade config")]
    InvalidUpgradeConfig,
    /// Unable to retrieve a system contract.
    #[error("Unable to retrieve system contract: {0}")]
    UnableToRetrieveSystemContract(String),
    /// Unable to retrieve a system contract package.
    #[error("Unable to retrieve system contract package: {0}")]
    UnableToRetrieveSystemContractPackage(String),
    /// Unable to disable previous version of a system contract.
    #[error("Failed to disable previous version of system contract: {0}")]
    FailedToDisablePreviousVersion(String),
    /// (De)serialization error.
    #[error("{0}")]
    Bytesrepr(bytesrepr::Error),
    /// Failed to create system contract registry.
    #[error("Failed to insert system contract registry")]
    FailedToCreateSystemRegistry,
}

impl From<bytesrepr::Error> for ProtocolUpgradeError {
    fn from(error: bytesrepr::Error) -> Self {
        ProtocolUpgradeError::Bytesrepr(error)
    }
}

/// The system upgrader deals with conducting an actual protocol upgrade.
pub(crate) struct SystemUpgrader<S>
where
    S: StateProvider,
{
    new_protocol_version: ProtocolVersion,
    tracking_copy: Rc<RefCell<TrackingCopy<<S as StateProvider>::Reader>>>,
}

impl<S> SystemUpgrader<S>
where
    S: StateProvider,
{
    /// Creates new system upgrader instance.
    pub(crate) fn new(
        new_protocol_version: ProtocolVersion,
        tracking_copy: Rc<RefCell<TrackingCopy<<S as StateProvider>::Reader>>>,
    ) -> Self {
        SystemUpgrader {
            new_protocol_version,
            tracking_copy,
        }
    }

    /// Bump major version for system contracts.
    pub(crate) fn upgrade_system_contracts_major_version(
        &self,
        correlation_id: CorrelationId,
        mint_hash: &ContractHash,
        auction_hash: &ContractHash,
        handle_payment_hash: &ContractHash,
        standard_payment_hash: &ContractHash,
    ) -> Result<(), ProtocolUpgradeError> {
        self.store_contract(correlation_id, *mint_hash, MINT, mint::mint_entry_points())?;
        self.store_contract(
            correlation_id,
            *auction_hash,
            AUCTION,
            auction::auction_entry_points(),
        )?;
        self.store_contract(
            correlation_id,
            *handle_payment_hash,
            HANDLE_PAYMENT,
            handle_payment::handle_payment_entry_points(),
        )?;
        self.store_contract(
            correlation_id,
            *standard_payment_hash,
            STANDARD_PAYMENT,
            standard_payment::standard_payment_entry_points(),
        )?;

        Ok(())
    }

    /// Store new system contract.
    fn store_contract(
        &self,
        correlation_id: CorrelationId,
        contract_hash: ContractHash,
        contract_name: &str,
        entry_points: EntryPoints,
    ) -> Result<(), ProtocolUpgradeError> {
        let contract_key = Key::Hash(contract_hash.value());

        let mut contract = if let StoredValue::Contract(contract) = self
            .tracking_copy
            .borrow_mut()
            .read(correlation_id, &contract_key)
            .map_err(|_| {
                ProtocolUpgradeError::UnableToRetrieveSystemContract(contract_name.to_string())
            })?
            .ok_or_else(|| {
                ProtocolUpgradeError::UnableToRetrieveSystemContract(contract_name.to_string())
            })? {
            contract
        } else {
            return Err(ProtocolUpgradeError::UnableToRetrieveSystemContract(
                contract_name.to_string(),
            ));
        };

        let contract_package_key = Key::Hash(contract.contract_package_hash().value());

        let mut contract_package = if let StoredValue::ContractPackage(contract_package) = self
            .tracking_copy
            .borrow_mut()
            .read(correlation_id, &contract_package_key)
            .map_err(|_| {
                ProtocolUpgradeError::UnableToRetrieveSystemContractPackage(
                    contract_name.to_string(),
                )
            })?
            .ok_or_else(|| {
                ProtocolUpgradeError::UnableToRetrieveSystemContractPackage(
                    contract_name.to_string(),
                )
            })? {
            contract_package
        } else {
            return Err(ProtocolUpgradeError::UnableToRetrieveSystemContractPackage(
                contract_name.to_string(),
            ));
        };

        contract_package
            .disable_contract_version(contract_hash)
            .map_err(|_| {
                ProtocolUpgradeError::FailedToDisablePreviousVersion(contract_name.to_string())
            })?;
        contract.set_protocol_version(self.new_protocol_version);

        let new_contract = Contract::new(
            contract.contract_package_hash(),
            contract.contract_wasm_hash(),
            contract.named_keys().clone(),
            entry_points,
            self.new_protocol_version,
        );
        self.tracking_copy
            .borrow_mut()
            .write(contract_hash.into(), StoredValue::Contract(new_contract));

        contract_package
            .insert_contract_version(self.new_protocol_version.value().major, contract_hash);

        self.tracking_copy.borrow_mut().write(
            contract_package_key,
            StoredValue::ContractPackage(contract_package),
        );

        Ok(())
    }
}
