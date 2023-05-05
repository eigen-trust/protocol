use std::collections::HashMap;

use crate::eddsa::native::{PublicKey, Signature};
use halo2::{
	arithmetic::Field,
	halo2curves::{bn256::Fr, ff::PrimeField},
};

/// Opinion info of peer
#[derive(Debug, Clone)]
pub struct Opinion<const NUM_NEIGHBOURS: usize> {
	/// Signature of opinion
	pub sig: Signature,
	/// Hash of opinion message
	pub message_hash: Fr,
	/// Array of real opinions
	pub scores: Vec<(PublicKey, Fr)>,
}

impl<const NUM_NEIGHBOURS: usize> Opinion<NUM_NEIGHBOURS> {
	/// Constructs the instance of `Opinion`
	pub fn new(sig: Signature, message_hash: Fr, scores: Vec<(PublicKey, Fr)>) -> Self {
		Self { sig, message_hash, scores }
	}
}

impl<const NUM_NEIGHBOURS: usize> Default for Opinion<NUM_NEIGHBOURS> {
	fn default() -> Self {
		let sig = Signature::new(Fr::zero(), Fr::zero(), Fr::zero());
		let message_hash = Fr::zero();
		let scores = vec![(PublicKey::default(), Fr::zero()); NUM_NEIGHBOURS];
		Self { sig, message_hash, scores }
	}
}

/// Dynamic set for EigenTrust
pub struct EigenTrustSet<
	const NUM_NEIGHBOURS: usize,
	const NUM_ITERATIONS: usize,
	const INITIAL_SCORE: u128,
> {
	set: Vec<(PublicKey, Fr)>,
	ops: HashMap<PublicKey, Opinion<NUM_NEIGHBOURS>>,
}

impl<const NUM_NEIGHBOURS: usize, const NUM_ITERATIONS: usize, const INITIAL_SCORE: u128>
	EigenTrustSet<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>
{
	/// Constructs new instance
	pub fn new() -> Self {
		Self {
			set: vec![(PublicKey::default(), Fr::zero()); NUM_NEIGHBOURS],
			ops: HashMap::new(),
		}
	}

	/// Add new set member and initial score
	pub fn add_member(&mut self, pk: PublicKey) {
		let pos = self.set.iter().position(|&(x, _)| x == pk);
		// Make sure not already in the set
		assert!(pos.is_none());

		let first_available = self.set.iter().position(|&(x, _)| x == PublicKey::default());
		let index = first_available.unwrap();

		// Give the initial score.
		let initial_score = Fr::from_u128(INITIAL_SCORE);
		self.set[index] = (pk, initial_score);
	}

	/// Remove the member and its opinion
	pub fn remove_member(&mut self, pk: PublicKey) {
		let pos = self.set.iter().position(|&(x, _)| x == pk);
		// Make sure already in the set
		assert!(pos.is_some());

		let index = pos.unwrap();
		self.set[index] = (PublicKey::default(), Fr::zero());

		self.ops.remove(&pk);
	}

	/// Update the opinion of the member
	pub fn update_op(&mut self, from: PublicKey, op: Opinion<NUM_NEIGHBOURS>) {
		let pos_from = self.set.iter().position(|&(x, _)| x == from);
		assert!(pos_from.is_some());

		self.ops.insert(from, op);
	}

	/// Compute the EigenTrust score
	pub fn converge(&self) -> Vec<Fr> {
		let mut filtered_ops: HashMap<PublicKey, Opinion<NUM_NEIGHBOURS>> = self.filter_peers_ops();

		// Normalize the opinion scores
		for i in 0..NUM_NEIGHBOURS {
			let (pk, _) = self.set[i];
			if pk == PublicKey::default() {
				continue;
			}
			let ops_i = filtered_ops.get_mut(&pk).unwrap();

			let op_score_sum = ops_i.scores.iter().fold(Fr::zero(), |acc, &(_, score)| acc + score);
			let inverted_sum = op_score_sum.invert().unwrap_or(Fr::zero());

			for j in 0..NUM_NEIGHBOURS {
				let (_, op_score) = ops_i.scores[j].clone();
				ops_i.scores[j].1 = op_score * inverted_sum;
			}
		}

		// There should be at least 2 valid peers(valid opinions) for calculation
		let valid_peers = self.set.iter().filter(|(pk, _)| pk != &PublicKey::default()).count();
		assert!(valid_peers >= 2, "Insufficient peers for calculation!");

		// By this point we should use filtered_opinions
		let mut s: Vec<Fr> = self.set.iter().map(|(_, score)| score.clone()).collect();
		for _ in 0..NUM_ITERATIONS {
			let mut sop = Vec::new();
			for i in 0..NUM_NEIGHBOURS {
				let pk_i = self.set[i].0.clone();
				let n_score = s[i];
				if pk_i == PublicKey::default() {
					sop.push(vec![Fr::zero(); NUM_NEIGHBOURS]);
					continue;
				}

				let op_i = filtered_ops.get(&pk_i).unwrap();

				let mut sop_i = Vec::new();
				for j in 0..NUM_NEIGHBOURS {
					let score = op_i.scores[j].1;
					let res = score * n_score;
					sop_i.push(res);
				}
				sop.push(sop_i);
			}

			for i in 0..NUM_NEIGHBOURS {
				let mut new_score = Fr::zero();
				for j in 0..NUM_NEIGHBOURS {
					new_score += sop[j][i];
				}
				s[i] = new_score;
			}
		}

		println!("new s: {:#?}", s.iter().map(|p| p));

		// Apply scaling for converting large final scores to small values (easy to read)
		let scale_factor = Fr::from_u128(10).pow(&[NUM_ITERATIONS as u64, 0, 0, 0]);
		println!("scaled new s: {:#?}", s.iter().map(|p| p * scale_factor));

		// Assert the score sum for checking the possible reputation leak
		let sum_initial = self.set.iter().fold(Fr::zero(), |acc, &(_, score)| acc + score);
		let sum_final = s.iter().fold(Fr::zero(), |acc, &score| acc + score);
		assert!(sum_initial == sum_final);
		println!("sum before: {:?}, sum after: {:?}", sum_initial, sum_final);

		s
	}

	fn filter_peers_ops(&self) -> HashMap<PublicKey, Opinion<NUM_NEIGHBOURS>> {
		let mut filtered_ops: HashMap<PublicKey, Opinion<NUM_NEIGHBOURS>> = HashMap::new();

		// Distribute the scores to valid peers
		for i in 0..NUM_NEIGHBOURS {
			let (pk_i, _) = self.set[i].clone();
			if pk_i == PublicKey::default() {
				continue;
			}

			let mut ops_i = self.ops.get(&pk_i).unwrap_or(&Opinion::default()).clone();

			// Update the opinion array - pairs of (key, score)
			//
			// Example 1:
			// 	set => [p1, null, p3]
			//	Peer1 opinion
			// 		[(p1, 10), (p6, 10),  (p3, 10)]
			//   => [(p1, 0), (null, 0), (p3, 10)]
			//
			// Example 2:
			// 	set => [p1, p2, null]
			//	Peer1 opinion
			// 		[(p1, 0), (p3, 10), (null, 10)]
			//   => [(p1, 0), (p2, 0),  (p3, 0)]
			for j in 0..NUM_NEIGHBOURS {
				let (set_pk_j, _) = self.set[j];
				let (op_pk_j, _) = ops_i.scores[j].clone();

				let is_diff_pk_j = set_pk_j != op_pk_j;
				let is_pk_j_null = set_pk_j == PublicKey::default();
				let is_pk_i = set_pk_j == pk_i;

				// Conditions for nullifying the score
				// 1. set_pk_j != op_pk_j
				// 2. set_pk_j == 0 (null or default key)
				// 3. set_pk_j == pk_i
				if is_diff_pk_j || is_pk_j_null || is_pk_i {
					ops_i.scores[j].1 = Fr::zero();
				}

				// Condition for correcting the pk
				// 1. set_pk_j != op_pk_j
				if is_diff_pk_j {
					ops_i.scores[j].0 = set_pk_j;
				}
			}

			// Distribute the scores
			//
			// Example 1:
			// 	set => [p1, p2, p3]
			//	Peer1 opinion
			// 		[(p1, 0), (p2, 0), (p3, 10)]
			//   => [(p1, 0), (p2, 0), (p3, 10)]
			//
			// Example 2:
			// 	set => [p1, p2, p3]
			//	Peer1 opinion
			//      [(p1, 0), (p2, 0), (p3, 0)]
			//   => [(p1, 0), (p2, 1), (p3, 1)]
			let op_score_sum = ops_i.scores.iter().fold(Fr::zero(), |acc, &(_, score)| acc + score);
			if op_score_sum == Fr::zero() {
				for j in 0..NUM_NEIGHBOURS {
					let (pk_j, _) = ops_i.scores[j].clone();

					let is_diff_pk = pk_j != pk_i;
					let is_not_null = pk_j != PublicKey::default();

					// Conditions for distributing the score
					// 1. pk_j != pk_i
					// 2. pk_j != PublicKey::default()
					if is_diff_pk && is_not_null {
						ops_i.scores[j] = (pk_j, Fr::from(1));
					}
				}
			}

			filtered_ops.insert(pk_i, ops_i);
		}

		filtered_ops
	}
}

#[cfg(test)]
mod test {
	use super::{EigenTrustSet, Opinion};
	use crate::{
		calculate_message_hash,
		eddsa::native::{sign, PublicKey, SecretKey},
	};

	use halo2::halo2curves::{bn256::Fr, ff::PrimeField};
	use rand::{thread_rng, Rng};

	const NUM_NEIGHBOURS: usize = 123;
	const NUM_ITERATIONS: usize = 10;
	const INITIAL_SCORE: u128 = 2437;

	fn sign_opinion<
		const NUM_NEIGHBOURS: usize,
		const NUM_ITERATIONS: usize,
		const INITIAL_SCORE: u128,
	>(
		sk: &SecretKey, pk: &PublicKey, pks: &[PublicKey; NUM_NEIGHBOURS],
		scores: &[Fr; NUM_NEIGHBOURS],
	) -> Opinion<NUM_NEIGHBOURS> {
		let (_, message_hashes) =
			calculate_message_hash::<NUM_NEIGHBOURS, 1>(pks.to_vec(), vec![scores.to_vec()]);
		let sig = sign(sk, pk, message_hashes[0]);

		// let scores = pks.zip(*scores);
		let mut op_scores = vec![];
		for i in 0..NUM_NEIGHBOURS {
			op_scores.push((pks[i], scores[i]));
		}
		let op = Opinion::new(sig, message_hashes[0], op_scores.to_vec());
		op
	}

	#[test]
	#[should_panic]
	fn test_add_member_in_initial_set() {
		let mut set = EigenTrustSet::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>::new();

		let rng = &mut thread_rng();

		let sk1 = SecretKey::random(rng);
		let pk1 = sk1.public();

		set.add_member(pk1);

		// Re-adding the member should panic
		set.add_member(pk1);
	}

	#[test]
	#[should_panic]
	fn test_one_member_converge() {
		let mut set = EigenTrustSet::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>::new();

		let rng = &mut thread_rng();

		let sk1 = SecretKey::random(rng);
		let pk1 = sk1.public();

		set.add_member(pk1);

		set.converge();
	}

	#[test]
	fn test_add_two_members_without_opinions() {
		let mut set = EigenTrustSet::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>::new();

		let rng = &mut thread_rng();

		let sk1 = SecretKey::random(rng);
		let sk2 = SecretKey::random(rng);

		let pk1 = sk1.public();
		let pk2 = sk2.public();

		set.add_member(pk1);
		set.add_member(pk2);

		set.converge();
	}

	#[test]
	fn test_add_two_members_with_one_opinion() {
		let mut set = EigenTrustSet::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>::new();

		let rng = &mut thread_rng();

		let sk1 = SecretKey::random(rng);
		let sk2 = SecretKey::random(rng);

		let pk1 = sk1.public();
		let pk2 = sk2.public();

		set.add_member(pk1);
		set.add_member(pk2);

		// Peer1(pk1) signs the opinion
		let mut pks = [PublicKey::default(); NUM_NEIGHBOURS];
		pks[0] = pk1;
		pks[1] = pk2;

		let mut scores = [Fr::zero(); NUM_NEIGHBOURS];
		scores[1] = Fr::from_u128(INITIAL_SCORE);

		let op1 = sign_opinion::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>(
			&sk1, &pk1, &pks, &scores,
		);

		set.update_op(pk1, op1);

		set.converge();
	}

	#[test]
	fn test_add_two_members_with_opinions() {
		let mut set = EigenTrustSet::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>::new();

		let rng = &mut thread_rng();

		let sk1 = SecretKey::random(rng);
		let sk2 = SecretKey::random(rng);

		let pk1 = sk1.public();
		let pk2 = sk2.public();

		set.add_member(pk1);
		set.add_member(pk2);

		// Peer1(pk1) signs the opinion
		let mut pks = [PublicKey::default(); NUM_NEIGHBOURS];
		pks[0] = pk1;
		pks[1] = pk2;

		let mut scores = [Fr::zero(); NUM_NEIGHBOURS];
		scores[1] = Fr::from_u128(INITIAL_SCORE);

		let op1 = sign_opinion::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>(
			&sk1, &pk1, &pks, &scores,
		);

		set.update_op(pk1, op1);

		// Peer2(pk2) signs the opinion
		let mut pks = [PublicKey::default(); NUM_NEIGHBOURS];
		pks[0] = pk1;
		pks[1] = pk2;

		let mut scores = [Fr::zero(); NUM_NEIGHBOURS];
		scores[1] = Fr::from_u128(INITIAL_SCORE);

		let op2 = sign_opinion::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>(
			&sk2, &pk2, &pks, &scores,
		);

		set.update_op(pk2, op2);

		set.converge();
	}

	#[test]
	fn test_add_three_members_with_opinions() {
		let mut set = EigenTrustSet::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>::new();

		let rng = &mut thread_rng();

		let sk1 = SecretKey::random(rng);
		let sk2 = SecretKey::random(rng);
		let sk3 = SecretKey::random(rng);

		let pk1 = sk1.public();
		let pk2 = sk2.public();
		let pk3 = sk3.public();

		set.add_member(pk1);
		set.add_member(pk2);
		set.add_member(pk3);

		// Peer1(pk1) signs the opinion
		let mut pks = [PublicKey::default(); NUM_NEIGHBOURS];
		pks[0] = pk1;
		pks[1] = pk2;
		pks[2] = pk3;

		let mut scores = [Fr::zero(); NUM_NEIGHBOURS];
		scores[1] = Fr::from_u128(300);
		scores[2] = Fr::from_u128(700);

		let op1 = sign_opinion::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>(
			&sk1, &pk1, &pks, &scores,
		);

		set.update_op(pk1, op1);

		// Peer2(pk2) signs the opinion
		let mut pks = [PublicKey::default(); NUM_NEIGHBOURS];
		pks[0] = pk1;
		pks[1] = pk2;
		pks[2] = pk3;

		let mut scores = [Fr::zero(); NUM_NEIGHBOURS];
		scores[0] = Fr::from_u128(600);
		scores[2] = Fr::from_u128(400);

		let op2 = sign_opinion::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>(
			&sk2, &pk2, &pks, &scores,
		);

		set.update_op(pk2, op2);

		// Peer3(pk3) signs the opinion
		let mut pks = [PublicKey::default(); NUM_NEIGHBOURS];
		pks[0] = pk1;
		pks[1] = pk2;
		pks[2] = pk3;

		let mut scores = [Fr::zero(); NUM_NEIGHBOURS];
		scores[0] = Fr::from_u128(600);
		scores[1] = Fr::from_u128(400);

		let op3 = sign_opinion::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>(
			&sk3, &pk3, &pks, &scores,
		);

		set.update_op(pk3, op3);

		set.converge();
	}

	#[test]
	fn test_add_three_members_with_two_opinions() {
		let mut set = EigenTrustSet::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>::new();

		let rng = &mut thread_rng();

		let sk1 = SecretKey::random(rng);
		let sk2 = SecretKey::random(rng);
		let sk3 = SecretKey::random(rng);

		let pk1 = sk1.public();
		let pk2 = sk2.public();
		let pk3 = sk3.public();

		set.add_member(pk1);
		set.add_member(pk2);
		set.add_member(pk3);

		// Peer1(pk1) signs the opinion
		let mut pks = [PublicKey::default(); NUM_NEIGHBOURS];
		pks[0] = pk1;
		pks[1] = pk2;
		pks[2] = pk3;

		let mut scores = [Fr::zero(); NUM_NEIGHBOURS];
		scores[1] = Fr::from_u128(300);
		scores[2] = Fr::from_u128(700);

		let op1 = sign_opinion::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>(
			&sk1, &pk1, &pks, &scores,
		);

		set.update_op(pk1, op1);

		// Peer2(pk2) signs the opinion
		let mut pks = [PublicKey::default(); NUM_NEIGHBOURS];
		pks[0] = pk1;
		pks[1] = pk2;
		pks[2] = pk3;

		let mut scores = [Fr::zero(); NUM_NEIGHBOURS];
		scores[0] = Fr::from_u128(600);
		scores[2] = Fr::from_u128(400);

		let op2 = sign_opinion::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>(
			&sk2, &pk2, &pks, &scores,
		);

		set.update_op(pk2, op2);

		set.converge();
	}

	#[test]
	fn test_add_3_members_with_3_ops_quit_1_member() {
		let mut set = EigenTrustSet::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>::new();

		let rng = &mut thread_rng();

		let sk1 = SecretKey::random(rng);
		let sk2 = SecretKey::random(rng);
		let sk3 = SecretKey::random(rng);

		let pk1 = sk1.public();
		let pk2 = sk2.public();
		let pk3 = sk3.public();

		set.add_member(pk1);
		set.add_member(pk2);
		set.add_member(pk3);

		// Peer1(pk1) signs the opinion
		// Peer2(pk2) signs the opinion
		let mut pks = [PublicKey::default(); NUM_NEIGHBOURS];
		pks[0] = pk1;
		pks[1] = pk2;
		pks[2] = pk3;

		let mut scores = [Fr::zero(); NUM_NEIGHBOURS];
		scores[1] = Fr::from_u128(300);
		scores[2] = Fr::from_u128(700);

		let op1 = sign_opinion::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>(
			&sk1, &pk1, &pks, &scores,
		);

		set.update_op(pk1, op1);

		// Peer2(pk2) signs the opinion
		let mut pks = [PublicKey::default(); NUM_NEIGHBOURS];
		pks[0] = pk1;
		pks[1] = pk2;
		pks[2] = pk3;

		let mut scores = [Fr::zero(); NUM_NEIGHBOURS];
		scores[0] = Fr::from_u128(600);
		scores[2] = Fr::from_u128(400);

		let op2 = sign_opinion::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>(
			&sk2, &pk2, &pks, &scores,
		);

		set.update_op(pk2, op2);

		// Peer3(pk3) signs the opinion
		let mut pks = [PublicKey::default(); NUM_NEIGHBOURS];
		pks[0] = pk1;
		pks[1] = pk2;
		pks[2] = pk3;

		let mut scores = [Fr::zero(); NUM_NEIGHBOURS];
		scores[0] = Fr::from_u128(600);
		scores[1] = Fr::from_u128(400);

		let op3 = sign_opinion::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>(
			&sk3, &pk3, &pks, &scores,
		);

		set.update_op(pk3, op3);

		set.converge();

		// Peer2 quits
		set.remove_member(pk2);

		set.converge();
	}

	#[test]
	fn test_add_3_members_with_2_ops_quit_1_member_1_op() {
		let mut set = EigenTrustSet::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>::new();

		let rng = &mut thread_rng();

		let sk1 = SecretKey::random(rng);
		let sk2 = SecretKey::random(rng);
		let sk3 = SecretKey::random(rng);

		let pk1 = sk1.public();
		let pk2 = sk2.public();
		let pk3 = sk3.public();

		set.add_member(pk1);
		set.add_member(pk2);
		set.add_member(pk3);

		// Peer1(pk1) signs the opinion
		let mut pks = [PublicKey::default(); NUM_NEIGHBOURS];
		pks[0] = pk1;
		pks[1] = pk2;
		pks[2] = pk3;

		let mut scores = [Fr::zero(); NUM_NEIGHBOURS];
		scores[1] = Fr::from_u128(300);
		scores[2] = Fr::from_u128(700);

		let op1 = sign_opinion::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>(
			&sk1, &pk1, &pks, &scores,
		);

		set.update_op(pk1, op1);

		// Peer2(pk2) signs the opinion
		let mut pks = [PublicKey::default(); NUM_NEIGHBOURS];
		pks[0] = pk1;
		pks[1] = pk2;
		pks[2] = pk3;

		let mut scores = [Fr::zero(); NUM_NEIGHBOURS];
		scores[0] = Fr::from_u128(600);
		scores[2] = Fr::from_u128(400);

		let op2 = sign_opinion::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>(
			&sk2, &pk2, &pks, &scores,
		);

		set.update_op(pk2, op2);

		set.converge();

		// // Peer1 quits
		// set.remove_member(pk1);

		// set.converge();
	}

	#[test]
	fn test_filter_peers_ops() {
		//	Filter the peers with following opinions:
		//			1	2	3	4	 5
		//		-----------------------
		//		1	10	10	.	.	10
		//		2	.	.	30	.	.
		//		3	10	.	.	.	.
		//		4	.	.	.	.	.
		//		5	.	.	.	.	.

		let rng = &mut thread_rng();

		let sk1 = SecretKey::random(rng);
		let sk2 = SecretKey::random(rng);
		let sk3 = SecretKey::random(rng);
		let sk8 = SecretKey::random(rng);

		let pk1 = sk1.public();
		let pk2 = sk2.public();
		let pk3 = sk3.public();
		let pk8 = sk8.public();

		// Peer1(pk1) signs the opinion
		let mut pks = [PublicKey::default(); NUM_NEIGHBOURS];
		pks[0] = pk1;
		pks[1] = pk2;
		pks[2] = pk3;

		let mut scores = [Fr::zero(); NUM_NEIGHBOURS];
		scores[0] = Fr::from_u128(10);
		scores[1] = Fr::from_u128(10);

		let op1 = sign_opinion::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>(
			&sk1, &pk1, &pks, &scores,
		);

		// Peer2(pk2) signs the opinion
		let mut pks = [PublicKey::default(); NUM_NEIGHBOURS];
		pks[0] = pk1;
		pks[1] = pk2;
		pks[2] = pk3;

		let mut scores = [Fr::zero(); NUM_NEIGHBOURS];
		scores[2] = Fr::from_u128(30);

		let op2 = sign_opinion::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>(
			&sk2, &pk2, &pks, &scores,
		);

		// Peer3(pk3) signs the opinion
		let mut pks = [PublicKey::default(); NUM_NEIGHBOURS];
		pks[0] = pk1;
		pks[1] = pk2;
		pks[2] = pk3;

		let mut scores = [Fr::zero(); NUM_NEIGHBOURS];
		scores[0] = Fr::from_u128(10);

		let op3 = sign_opinion::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>(
			&sk3, &pk3, &pks, &scores,
		);

		// Setup EigenTrustSet
		let mut eigen_trust_set =
			EigenTrustSet::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>::new();

		eigen_trust_set.add_member(pk1);
		eigen_trust_set.add_member(pk2);
		eigen_trust_set.add_member(pk3);

		eigen_trust_set.update_op(pk1, op1);
		eigen_trust_set.update_op(pk2, op2);
		eigen_trust_set.update_op(pk3, op3);

		let filtered_ops = eigen_trust_set.filter_peers_ops();

		let final_peers_cnt =
			eigen_trust_set.set.iter().filter(|&&(pk, _)| pk != PublicKey::default()).count();
		let final_ops_cnt = filtered_ops.keys().count();
		assert!(final_peers_cnt == final_ops_cnt);
	}

	// NUM_NEIGHBOURS - 2^16;
	// NUM_ITERATIONS - 20;
	// INITIAL_SCORE - 1000000;

	fn eigen_trust_set_testing_helper<
		const NUM_NEIGHBOURS: usize,
		const NUM_ITERATIONS: usize,
		const INITIAL_SCORE: u128,
	>(
		real_neighbors: usize,
	) {
		let mut set = EigenTrustSet::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>::new();

		let rng = &mut thread_rng();

		let sks = [(); NUM_NEIGHBOURS].map(|__| SecretKey::random(rng));
		let pks = sks.clone().map(|s| s.public());

		// Add the publicKey to the set
		let _: Vec<_> = pks.iter().map(|pk| set.add_member(*pk)).collect();

		// Update the opinions
		for i in 0..real_neighbors {
			let scores = [(); NUM_NEIGHBOURS].map(|_| {
				let score = rng.gen::<u8>() as u128;
				Fr::from_u128(score)
			});

			let op_i = sign_opinion::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>(
				&sks[i], &pks[i], &pks, &scores,
			);

			set.update_op(pks[i], op_i);
		}

		set.converge();
	}

	#[test]
	fn test_scaling_1() {
		const NUM_NEIGHBOURS: usize = 2_usize.pow(16);
		const NUM_ITERATIONS: usize = 20;
		const INITIAL_SCORE: u128 = 1000000;

		let real_neighbors = 2;

		eigen_trust_set_testing_helper::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>(
			real_neighbors,
		)
	}

	#[test]
	fn test_scaling_2() {
		const NUM_NEIGHBOURS: usize = 2_usize.pow(16);
		const NUM_ITERATIONS: usize = 20;
		const INITIAL_SCORE: u128 = 1000000;

		let real_neighbors = 256;

		eigen_trust_set_testing_helper::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>(
			real_neighbors,
		)
	}

	#[test]
	fn test_scaling_3() {
		const NUM_NEIGHBOURS: usize = 2_usize.pow(16);
		const NUM_ITERATIONS: usize = 20;
		const INITIAL_SCORE: u128 = 1000000;

		let real_neighbors = 8172;

		eigen_trust_set_testing_helper::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>(
			real_neighbors,
		)
	}

	#[test]
	fn test_scaling_4() {
		const NUM_NEIGHBOURS: usize = 2_usize.pow(16);
		const NUM_ITERATIONS: usize = 20;
		const INITIAL_SCORE: u128 = 1000000;

		let real_neighbors = 32768;

		eigen_trust_set_testing_helper::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>(
			real_neighbors,
		)
	}

	#[test]
	fn test_scaling_5() {
		const NUM_NEIGHBOURS: usize = 2_usize.pow(16);
		const NUM_ITERATIONS: usize = 20;
		const INITIAL_SCORE: u128 = 1000000;

		let real_neighbors = 65000;

		eigen_trust_set_testing_helper::<NUM_NEIGHBOURS, NUM_ITERATIONS, INITIAL_SCORE>(
			real_neighbors,
		)
	}
}
