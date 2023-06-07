//! # Eigen Trust
//!
//! A library for managing trust in a distributed network with zero-knowledge
//! features.
//!
//! ## Main characteristics:
//! **Self-policing** - the shared ethics of the user population is defined and
//! enforced by the peers themselves and not by some central authority.
//!
//! **Minimal** - computation, infrastructure, storage, and message complexity
//! are reduced to a minimum.
//!
//! **Incorruptible** - Reputation should be obtained by consistent good
//! behavior through several transactions. This is enforced for all users, so no
//! one can cheat the system and obtain a higher reputation. It is also
//! resistant to malicious collectives.
//!
//! ## Implementation
//! The library is implemented according to the original [Eigen Trust paper](http://ilpubs.stanford.edu:8090/562/1/2002-56.pdf).
//! It is developed under the Ethereum Foundation grant.

// Rustc
#![warn(trivial_casts)]
// #![deny(
// 	absolute_paths_not_starting_with_crate, deprecated, future_incompatible, missing_docs,
// 	nonstandard_style, unreachable_code, unreachable_patterns
// )]
#![forbid(unsafe_code)]
// Clippy
// #![allow(clippy::tabs_in_doc_comments)]
#![deny(
// 	// Complexity
// 	clippy::unnecessary_cast,
// 	// clippy::needless_question_mark,
// 	// Pedantic
// 	clippy::cast_lossless,
// 	clippy::cast_possible_wrap,
// 	// Perf
	clippy::redundant_clone,
// 	// Restriction
// 	clippy::panic,
// 	// Style
// 	// clippy::let_and_return,
// 	// clippy::needless_borrow
)]

pub mod att_station;
pub mod attestation;
pub mod error;
pub mod eth;
pub mod utils;

use att_station::{AttestationCreatedFilter, AttestationStation};
use attestation::{att_data_from_signed_att, Attestation, AttestationPayload};
use eigen_trust_circuit::{
	dynamic_sets::ecdsa_native::{ECDSAPublicKey, EigenTrustSet, SignedAttestation},
	halo2::halo2curves::bn256::Fr as Scalar,
};
use error::EigenError;
use eth::{address_from_public_key, ecdsa_secret_from_mnemonic, scalar_from_address};
use ethers::{
	abi::{Address, RawLog},
	contract::EthEvent,
	prelude::EthDisplay,
	providers::Middleware,
	signers::{LocalWallet, Signer},
	types::{Filter, H256},
};
use ethers::{
	middleware::SignerMiddleware,
	providers::{Http, Provider},
	signers::{coins_bip39::English, MnemonicBuilder},
};
use secp256k1::{ecdsa::RecoverableSignature, Message, SecretKey, SECP256K1};
use serde::{Deserialize, Serialize};
use std::{
	collections::{HashMap, HashSet},
	sync::Arc,
};

/// Max amount of participants
const MAX_NEIGHBOURS: usize = 2;
/// Number of iterations to run the eigen trust algorithm
const NUM_ITERATIONS: usize = 10;
/// Initial score for each participant before the algorithms is run
const INITIAL_SCORE: u128 = 1000;

#[derive(Serialize, Deserialize, Debug, EthDisplay, Clone)]
pub struct ClientConfig {
	pub as_address: String,
	pub domain: String,
	pub mnemonic: String,
	pub node_url: String,
	pub verifier_address: String,
}

/// Signer middleware type alias
pub type SignerMiddlewareArc = Arc<SignerMiddleware<Provider<Http>, LocalWallet>>;

/// Client
pub struct Client {
	client: SignerMiddlewareArc,
	config: ClientConfig,
}

impl Client {
	/// Create a new client
	pub fn new(config: ClientConfig) -> Self {
		let client = setup_client(&config.mnemonic, &config.node_url);
		Self { client, config }
	}

	/// Submit an attestation to the attestation station
	pub async fn attest(&self, attestation: Attestation) -> Result<(), EigenError> {
		let ctx = SECP256K1;
		let secret_keys: Vec<SecretKey> =
			ecdsa_secret_from_mnemonic(&self.config.mnemonic, 1).unwrap();

		// Get AttestationFr
		let attestation_fr = attestation.to_attestation_fr().unwrap();

		// Format for signature
		let att_hash = attestation_fr.hash();

		// Sign attestation
		let signature: RecoverableSignature = ctx.sign_ecdsa_recoverable(
			&Message::from_slice(att_hash.to_bytes().as_slice()).unwrap(),
			&secret_keys[0],
		);

		let signed_attestation = SignedAttestation::new(attestation_fr, signature);

		let as_address_res = self.config.as_address.parse::<Address>();
		let as_address = as_address_res.map_err(|_| EigenError::ParseError)?;
		let as_contract = AttestationStation::new(as_address, self.client.clone());

		// Verify signature is recoverable
		let recovered_pubkey = signed_attestation.recover_public_key().unwrap();
		let recovered_address = address_from_public_key(&recovered_pubkey).unwrap();
		assert!(recovered_address == self.client.address());

		let tx_call =
			as_contract.attest(vec![att_data_from_signed_att(&signed_attestation).unwrap()]);
		let tx_res = tx_call.send();
		let tx = tx_res.await.map_err(|_| EigenError::TransactionError)?;
		let res = tx.await.map_err(|_| EigenError::TransactionError)?;

		if let Some(receipt) = res {
			println!("Transaction status: {:?}", receipt.status);
		}

		Ok(())
	}

	/// Calculate scores
	pub async fn calculate_scores(&mut self) -> Result<(), EigenError> {
		// Get attestations
		let attestations = self.get_attestations().await?;

		// Get participants
		let mut participants_set = HashSet::<Address>::new();

		for (signed_att, att) in attestations.iter() {
			// Add attested
			participants_set.insert(att.about);

			// Add signer
			let signer_pub_key = signed_att.recover_public_key().unwrap();
			let signer_address = address_from_public_key(&signer_pub_key).unwrap();
			participants_set.insert(signer_address);
		}

		// Create EigenTrustSet
		let mut eigen_trust_set =
			EigenTrustSet::<MAX_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>::new();

		// Add members to set
		let participants: Vec<Scalar> =
			participants_set.iter().map(|address| scalar_from_address(address).unwrap()).collect();

		for participant in participants {
			eigen_trust_set.add_member(participant);
		}

		// Update members opinions
		// This could be done in the same step as getting the participant set
		// Create attestation map
		let mut attestation_map: HashMap<ECDSAPublicKey, Vec<SignedAttestation>> = HashMap::new();

		for (signed_att, _) in attestations.iter() {
			// Get signer ECDSA public key
			let signer_pub_key = signed_att.recover_public_key().unwrap();

			// Put attestation in new vector
			let att_vec = vec![signed_att.clone()];

			// If signer exist, append vector, if not, just add
			if attestation_map.contains_key(&signer_pub_key) {
				let mut attestation_vec = attestation_map.get(&signer_pub_key).unwrap().clone();
				attestation_vec.extend(att_vec);
				attestation_map.insert(signer_pub_key, attestation_vec);
			} else {
				attestation_map.insert(signer_pub_key, att_vec);
			}
		}

		// Update opinions
		for (signer_pub_key, att_vec) in attestation_map.iter() {
			eigen_trust_set.update_op(signer_pub_key.clone(), att_vec.clone());
		}

		// Calculate scores
		let scores = eigen_trust_set.converge();

		println!("Scores: {:?}", scores);

		Ok(())
	}

	/// Get the attestations from the contract
	pub async fn get_attestations(
		&self,
	) -> Result<Vec<(SignedAttestation, Attestation)>, EigenError> {
		let filter = Filter::new()
			.address(self.config.as_address.parse::<Address>().unwrap())
			.event("AttestationCreated(address,address,bytes32,bytes)")
			.topic1(Vec::<H256>::new())
			.topic2(Vec::<H256>::new())
			.from_block(0);
		let logs = &self.client.get_logs(&filter).await.unwrap();
		let mut att_tuple: Vec<(SignedAttestation, Attestation)> = Vec::new();

		println!("Indexed attestations: {}", logs.iter().len());

		for log in logs.iter() {
			let raw_log = RawLog::from((log.topics.clone(), log.data.to_vec()));
			let att_created = AttestationCreatedFilter::decode_log(&raw_log).unwrap();
			let att_data =
				AttestationPayload::from_bytes(att_created.val.to_vec()).expect("Failed to decode");

			let att = Attestation::new(
				att_created.about,
				att_created.key.into(),
				att_data.get_value(),
				Some(att_data.get_message().into()),
			);

			let att_fr = att.to_attestation_fr().unwrap();

			let signature = att_data.get_signature();

			let signed_att = SignedAttestation::new(att_fr, signature);

			att_tuple.push((signed_att, att));
		}

		Ok(att_tuple)
	}

	/// Verifies last generated proof
	pub async fn verify(&self) -> Result<(), EigenError> {
		// TODO: Verify proof
		Ok(())
	}
}

/// Setup Client middleware
fn setup_client(mnemonic_phrase: &str, node_url: &str) -> SignerMiddlewareArc {
	let provider = Provider::<Http>::try_from(node_url).unwrap();
	let wallet = MnemonicBuilder::<English>::default().phrase(mnemonic_phrase).build().unwrap();
	let client = SignerMiddleware::new(provider, wallet.with_chain_id(31337u64));

	Arc::new(client)
}

#[cfg(test)]
mod lib_tests {
	use crate::{
		attestation::Attestation,
		eth::{deploy_as, deploy_verifier},
		Client, ClientConfig,
	};
	use eigen_trust_circuit::utils::read_bytes_data;
	use ethers::abi::Address;
	use ethers::{types::U256, utils::Anvil};

	#[tokio::test]
	async fn test_attest() {
		let anvil = Anvil::new().spawn();
		let node_url = anvil.endpoint();
		let mnemonic = "test test test test test test test test test test test junk".to_string();

		let as_address = deploy_as(&mnemonic, &node_url).await.unwrap();
		let verifier_address =
			deploy_verifier(&mnemonic, &node_url, read_bytes_data("et_verifier")).await.unwrap();

		let config = ClientConfig {
			as_address: format!("{:?}", as_address),
			domain: "0x0000000000000000000000000000000000000000".to_string(),
			mnemonic: mnemonic.clone(),
			node_url,
			verifier_address: format!("{:?}", verifier_address),
		};

		let attestation = Attestation::new(Address::default(), U256::default(), 1, None);

		assert!(Client::new(config).attest(attestation).await.is_ok());

		drop(anvil);
	}
}
