use bellman::{Circuit, ConstraintSystem, SynthesisError};
use sapling_crypto::circuit::num;
use sapling_crypto::jubjub::JubjubEngine;

use crate::circuit::constraint;
use crate::circuit::porc;
use crate::circuit::sloth;

/// This is an instance of the `HVH-PoSt` circuit.
pub struct HvhPost<'a, E: JubjubEngine> {
    /// Paramters for the engine.
    pub params: &'a E::Params,

    // VDF
    pub vdf_key: Option<E::Fr>,
    pub vdf_ys: Vec<Option<E::Fr>>,
    pub vdf_xs: Vec<Option<E::Fr>>,
    pub vdf_sloth_rounds: usize,

    // PoRCs
    pub challenged_leafs_vec: Vec<Vec<Option<E::Fr>>>,
    pub commitments_vec: Vec<Vec<Option<E::Fr>>>,
    pub paths_vec: Vec<Vec<Vec<Option<(E::Fr, bool)>>>>,
}

impl<'a, E: JubjubEngine> Circuit<E> for HvhPost<'a, E> {
    fn synthesize<CS: ConstraintSystem<E>>(self, cs: &mut CS) -> Result<(), SynthesisError> {
        hvh_post(
            cs,
            self.params,
            self.vdf_key,
            &self.vdf_ys,
            &self.vdf_xs,
            self.vdf_sloth_rounds,
            &self.challenged_leafs_vec,
            &self.commitments_vec,
            &self.paths_vec,
        )
    }
}

pub fn hvh_post<E: JubjubEngine, CS: ConstraintSystem<E>>(
    cs: &mut CS,
    params: &E::Params,
    vdf_key: Option<E::Fr>,
    vdf_ys: &[Option<E::Fr>],
    vdf_xs: &[Option<E::Fr>],
    vdf_sloth_rounds: usize,
    challenged_leafs_vec: &[Vec<Option<E::Fr>>],
    commitments_vec: &[Vec<Option<E::Fr>>],
    paths_vec: &[Vec<Vec<Option<(E::Fr, bool)>>>],
) -> Result<(), SynthesisError> {
    // VDF Output Verification
    assert_eq!(vdf_xs.len(), vdf_ys.len());

    let vdf_key = num::AllocatedNum::alloc(cs.namespace(|| "vdf_key"), || {
        vdf_key.ok_or_else(|| SynthesisError::AssignmentMissing)
    })?;

    for (i, (y, x)) in vdf_ys.iter().zip(vdf_xs.iter()).enumerate() {
        let mut cs = cs.namespace(|| format!("vdf_verification_round_{}", i));

        let decoded = sloth::decode(
            cs.namespace(|| "sloth_decode"),
            &vdf_key,
            *y,
            vdf_sloth_rounds,
        )?;

        let x_alloc = num::AllocatedNum::alloc(cs.namespace(|| "x"), || {
            x.ok_or_else(|| SynthesisError::AssignmentMissing)
        })?;

        constraint::equal(&mut cs, || "equality", &x_alloc, &decoded);

        // TODO: is this the right thing to inputize?
        decoded.inputize(cs.namespace(|| "vdf_result"))?;
    }

    // PoRC Verification
    assert_eq!(challenged_leafs_vec.len(), commitments_vec.len());
    assert_eq!(paths_vec.len(), commitments_vec.len());

    for (i, (challenged_leafs, (commitments, paths))) in challenged_leafs_vec
        .iter()
        .zip(commitments_vec.iter().zip(paths_vec.iter()))
        .enumerate()
    {
        let mut cs = cs.namespace(|| format!("porc_verification_round_{}", i));
        porc::porc(&mut cs, params, challenged_leafs, commitments, paths)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use pairing::bls12_381::*;
    use pairing::Field;
    use rand::{Rng, SeedableRng, XorShiftRng};
    use sapling_crypto::jubjub::JubjubBls12;

    use crate::circuit::test::*;
    use crate::drgraph::{new_seed, BucketGraph, Graph};
    use crate::fr32::fr_into_bytes;
    use crate::hasher::pedersen::*;
    use crate::hvh_post;
    use crate::proof::ProofScheme;
    use crate::vdf_sloth;

    #[test]
    fn test_hvh_post_circuit_with_bls12_381() {
        let params = &JubjubBls12::new();
        let rng = &mut XorShiftRng::from_seed([0x3dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654]);

        let lambda = 32;

        let sp = hvh_post::SetupParams::<PedersenDomain, vdf_sloth::Sloth> {
            challenge_count: 10,
            sector_size: 1024 * lambda,
            post_epochs: 3,
            setup_params_vdf: vdf_sloth::SetupParams {
                key: rng.gen(),
                rounds: 1,
            },
            lambda,
            sectors_count: 2,
        };

        let pub_params = hvh_post::HvhPost::<PedersenHasher, vdf_sloth::Sloth>::setup(&sp).unwrap();

        let data0: Vec<u8> = (0..1024)
            .flat_map(|_| fr_into_bytes::<Bls12>(&rng.gen()))
            .collect();
        let data1: Vec<u8> = (0..1024)
            .flat_map(|_| fr_into_bytes::<Bls12>(&rng.gen()))
            .collect();

        let graph0 = BucketGraph::<PedersenHasher>::new(1024, 5, 0, new_seed());
        let tree0 = graph0.merkle_tree(data0.as_slice(), lambda).unwrap();
        let graph1 = BucketGraph::<PedersenHasher>::new(1024, 5, 0, new_seed());
        let tree1 = graph1.merkle_tree(data1.as_slice(), lambda).unwrap();

        let pub_inputs = hvh_post::PublicInputs {
            challenges: vec![rng.gen(), rng.gen()],
            commitments: vec![tree0.root(), tree1.root()],
        };

        let replicas = [&data0[..], &data1[..]];
        let trees = [&tree0, &tree1];
        let priv_inputs = hvh_post::PrivateInputs::new(&replicas[..], &trees[..]);

        let proof = hvh_post::HvhPost::<PedersenHasher, vdf_sloth::Sloth>::prove(
            &pub_params,
            &pub_inputs,
            &priv_inputs,
        )
        .unwrap();

        assert!(
            hvh_post::HvhPost::<PedersenHasher, vdf_sloth::Sloth>::verify(
                &pub_params,
                &pub_inputs,
                &proof
            )
            .unwrap()
        );

        // actual circuit test

        let vdf_ys = proof
            .ys
            .iter()
            .map(|y| Some(y.clone().into()))
            .collect::<Vec<_>>();
        let vdf_xs = proof
            .proofs_porep
            .iter()
            .take(vdf_ys.len())
            .map(|p| Some(hvh_post::extract_vdf_input::<PedersenHasher>(p).into()))
            .collect();

        let mut paths_vec = Vec::new();
        let mut challenged_leafs_vec = Vec::new();
        let mut commitments_vec = Vec::new();

        for proof_porep in &proof.proofs_porep {
            // -- paths
            paths_vec.push(
                proof_porep
                    .paths()
                    .iter()
                    .map(|p| {
                        p.iter()
                            .map(|v| Some((v.0.into(), v.1)))
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>(),
            );

            // -- challenged leafs
            challenged_leafs_vec.push(
                proof_porep
                    .leafs()
                    .iter()
                    .map(|l| Some((**l).into()))
                    .collect::<Vec<_>>(),
            );

            // -- commitments
            commitments_vec.push(
                proof_porep
                    .commitments()
                    .iter()
                    .map(|c| Some((**c).into()))
                    .collect::<Vec<_>>(),
            );
        }

        let mut cs = TestConstraintSystem::<Bls12>::new();

        let instance = HvhPost {
            params,
            vdf_key: Some(pub_params.pub_params_vdf.key.into()),
            vdf_xs,
            vdf_ys,
            vdf_sloth_rounds: pub_params.pub_params_vdf.rounds,
            challenged_leafs_vec,
            paths_vec,
            commitments_vec,
        };

        instance
            .synthesize(&mut cs)
            .expect("failed to synthesize circuit");

        assert!(cs.is_satisfied(), "constraints not satisfied");

        assert_eq!(cs.num_inputs(), 69, "wrong number of inputs");
        assert_eq!(cs.num_constraints(), 304140, "wrong number of constraints");
        assert_eq!(cs.get_input(0, "ONE"), Fr::one());
    }
}
