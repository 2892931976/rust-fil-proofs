use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::PathBuf;
use std::{thread, time};

use bellman::groth16;
use pairing::bls12_381::Bls12;
use pairing::Engine;
use sapling_crypto::jubjub::JubjubBls12;

use storage_proofs::circuit::zigzag::ZigZagCompound;
use storage_proofs::compound_proof::{self, CompoundProof};
use storage_proofs::drgporep::{self, DrgParams};
use storage_proofs::drgraph::new_seed;
use storage_proofs::error::Result;
use storage_proofs::fr32::{bytes_into_fr, fr_into_bytes, Fr32Ary};
use storage_proofs::hasher::pedersen::PedersenHash;
use storage_proofs::io::fr32::{write_unpadded, FR_PADDED_BITS, FR_UNPADDED_BITS};
use storage_proofs::layered_drgporep::{self, simplify_tau};
use storage_proofs::parameter_cache::{
    parameter_cache_path, read_cached_params, write_params_to_cache,
};
use storage_proofs::porep::{replica_id, PoRep, ProverAux, Tau};
use storage_proofs::proof::ProofScheme;
use storage_proofs::zigzag_drgporep::ZigZagDrgPoRep;
use storage_proofs::zigzag_graph::ZigZagBucketGraph;

use sector_base::api::SectorStore;

type Commitment = [u8; 32];

/// FrSafe is an array of the largest whole number of bytes guaranteed not to overflow the field.
type FrSafe = [u8; 31];

/// How big, in bytes, is the SNARK proof exposed by the API?
pub const SNARK_BYTES: usize = 192;
type SnarkProof = [u8; SNARK_BYTES];

/// How big should a fake sector be when faking proofs?
const FAKE_SECTOR_BYTES: usize = 128;

/// We need a distinguished place to cache 'the' parameters corresponding to the SetupParams
/// currently being used. These are only easily generated at replication time but need to be
/// accessed at verification time too.
const DUMMY_PARAMETER_CACHE_FILE: &str = "API-dummy-parameters";
/// If we try to read the cache while another process (like a test on CI…) is writing it,
/// things will go badly.

fn dummy_parameter_cache_path(sector_size: usize) -> PathBuf {
    parameter_cache_path(&format!("{}[{}]", DUMMY_PARAMETER_CACHE_FILE, sector_size))
}

lazy_static! {
    pub static ref ENGINE_PARAMS: JubjubBls12 = JubjubBls12::new();
}

pub const LAMBDA: usize = 32;
const DEGREE: usize = 1; // TODO: 10;
const EXPANSION_DEGREE: usize = 2; // TODO: 10
const SLOTH_ITER: usize = 1;
const LAYERS: usize = 2; // TODO: 6;

fn setup_params(sector_bytes: usize) -> layered_drgporep::SetupParams {
    assert!(
        sector_bytes % LAMBDA == 0,
        "sector_bytes ({}) must be a multiple of lambda {}",
        sector_bytes,
        LAMBDA
    );
    let nodes = sector_bytes / LAMBDA;
    layered_drgporep::SetupParams {
        drg_porep_setup_params: drgporep::SetupParams {
            lambda: LAMBDA,
            drg: DrgParams {
                nodes,
                degree: DEGREE,
                expansion_degree: EXPANSION_DEGREE,
                seed: new_seed(),
            },
            sloth_iter: SLOTH_ITER,
        },
        layers: LAYERS,
    }
}

fn public_params(sector_bytes: usize) -> layered_drgporep::PublicParams<ZigZagBucketGraph> {
    ZigZagDrgPoRep::setup(&setup_params(sector_bytes)).unwrap()
}

fn commitment_from_fr<E: Engine>(fr: E::Fr) -> Commitment {
    let mut commitment = [0; 32];
    for (i, b) in fr_into_bytes::<E>(&fr).iter().enumerate() {
        commitment[i] = *b;
    }
    commitment
}

fn pad_safe_fr(unpadded: FrSafe) -> Fr32Ary {
    let mut res = [0; 32];
    res[0..31].copy_from_slice(&unpadded);
    res
}

/// Validate sector_store configuration and calculates derived configuration.
///
/// # Return Values
/// * - `fake` is true when faking.
/// * - `delay_seconds` is None if no delay.
/// * - `sector_bytes` is the size (in bytes) of sector which should be stored on disk.
/// * - `proof_sector_bytes` is the size of the sector which will be proved when faking.
pub fn get_config(sector_store: &'static SectorStore) -> (bool, Option<u32>, usize, usize) {
    let fake = sector_store.config().is_fake();
    let delay_seconds = sector_store.config().simulate_delay_seconds();
    let delayed = delay_seconds.is_some();
    let sector_bytes = sector_store.config().max_unsealed_bytes_per_sector() as usize;
    let proof_sector_bytes = if fake {
        FAKE_SECTOR_BYTES
    } else {
        sector_bytes
    };

    // It doesn't make sense to set a delay when not faking. The current implementations of SectorStore
    // never do, but if that were to change, it would be a mistake.
    let valid = if fake { true } else { !delayed };
    assert!(valid, "delay is only valid when faking");

    (fake, delay_seconds, sector_bytes, proof_sector_bytes)
}

pub fn seal(
    sector_store: &'static SectorStore,
    in_path: &PathBuf,
    out_path: &PathBuf,
    prover_id_in: FrSafe,
    sector_id_in: FrSafe,
) -> Result<(Commitment, Commitment, SnarkProof)> {
    let (fake, delay_seconds, sector_bytes, proof_sector_bytes) = get_config(sector_store);

    if let Some(delay) = delay_seconds {
        delay_seal(delay);
    };

    let f_in = File::open(in_path)?;

    // Read all the provided data, even if we will prove less of it because we are faking.
    let mut data = Vec::with_capacity(sector_bytes);
    f_in.take(sector_bytes as u64).read_to_end(&mut data)?;

    // Zero-pad the data to the requested size.
    for _ in data.len()..sector_bytes {
        data.push(0);
    }

    // Copy all the data.
    let data_copy = data.clone();

    // Zero-pad the prover_id to 32 bytes (and therefore Fr32).
    let prover_id = pad_safe_fr(prover_id_in);
    // Zero-pad the sector_id to 32 bytes (and therefore Fr32).
    let sector_id = pad_safe_fr(sector_id_in);
    let replica_id = replica_id(prover_id, sector_id);

    let compound_setup_params = compound_proof::SetupParams {
        // The proof might use a different number of bytes than we read and copied, if we are faking.
        vanilla_params: &setup_params(proof_sector_bytes),
        engine_params: &(*ENGINE_PARAMS),
    };

    let compound_public_params = ZigZagCompound::setup(&compound_setup_params)?;

    let (tau, aux) = perform_replication(
        &out_path,
        &compound_public_params.vanilla_params,
        &replica_id,
        &mut data,
        fake,
        proof_sector_bytes,
    )?;

    let replica_id_fr = bytes_into_fr::<Bls12>(&replica_id)?;

    let public_tau = simplify_tau(&tau);
    // This is the commitment to the original data.
    let comm_d = public_tau.comm_d;
    // This is the commitment to the last layer's replica.
    let comm_r = public_tau.comm_r;

    let challenges = derive_challenges(
        fr_into_bytes::<Bls12>(&comm_r.0).as_slice(),
        fr_into_bytes::<Bls12>(&comm_d.0).as_slice(),
    );
    let public_inputs = layered_drgporep::PublicInputs {
        replica_id: replica_id_fr,
        challenges,
        tau: Some(public_tau),
    };

    let private_inputs = layered_drgporep::PrivateInputs {
        replica: &data_copy[0..proof_sector_bytes],
        aux,
        tau,
    };

    let proof = ZigZagCompound::prove(&compound_public_params, &public_inputs, &private_inputs)?;

    let mut buf = Vec::with_capacity(SNARK_BYTES);

    proof.circuit_proof.write(&mut buf)?;

    let mut proof_bytes = [0; SNARK_BYTES];
    proof_bytes.copy_from_slice(&buf);

    write_params_to_cache(
        proof.groth_params.clone(),
        &dummy_parameter_cache_path(proof_sector_bytes),
    )?;

    // Verification is cheap when parameters are cached,
    // and it is never correct to return a proof which does not verify.
    verify_seal(
        sector_store,
        commitment_from_fr::<Bls12>(comm_r.0),
        commitment_from_fr::<Bls12>(comm_d.0),
        prover_id_in,
        sector_id_in,
        &proof_bytes,
    ).expect("post-seal verification sanity check failed");

    Ok((
        commitment_from_fr::<Bls12>(public_tau.comm_r.0),
        commitment_from_fr::<Bls12>(public_tau.comm_d.0),
        proof_bytes,
    ))
}

fn delay_seal(seconds: u32) {
    let delay = time::Duration::from_secs(u64::from(seconds));
    thread::sleep(delay);
}

fn delay_get_unsealed_range(base_seconds: u32) {
    let delay = time::Duration::from_secs(u64::from(base_seconds / 2));
    thread::sleep(delay);
}

fn perform_replication(
    out_path: &PathBuf,
    public_params: &<ZigZagDrgPoRep as ProofScheme>::PublicParams,
    replica_id: &[u8],
    data: &mut [u8],
    fake: bool,
    proof_sector_bytes: usize,
) -> Result<(Vec<Tau>, Vec<ProverAux>)> {
    if fake {
        // When faking replication, we write the original data to disk, before replication.
        write_data(out_path, data)?;

        assert!(
            data.len() >= FAKE_SECTOR_BYTES,
            "data length ({}) is less than FAKE_SECTOR_BYTES ({}) when faking replication",
            data.len(),
            FAKE_SECTOR_BYTES
        );
        let (tau, aux) = ZigZagDrgPoRep::replicate(
            public_params,
            &replica_id,
            &mut data[0..proof_sector_bytes],
        )?;
        Ok((tau, aux))
    } else {
        // When not faking replication, we write the replicated data to disk, after replication.
        let (tau, aux) = ZigZagDrgPoRep::replicate(public_params, &replica_id, data)?;

        write_data(out_path, data)?;
        Ok((tau, aux))
    }
}

fn write_data(out_path: &PathBuf, data: &[u8]) -> Result<()> {
    // Write replicated data to out_path.
    let f_out = File::create(out_path)?;
    let mut buf_writer = BufWriter::new(f_out);
    buf_writer.write_all(&data)?;
    Ok(())
}

pub fn get_unsealed_range(
    sector_store: &'static SectorStore,
    sealed_path: &PathBuf,
    output_path: &PathBuf,
    prover_id_in: FrSafe,
    sector_id_in: FrSafe,
    offset: u64,
    num_bytes: u64,
) -> Result<(u64)> {
    let (fake, delay_seconds, sector_bytes, _proof_sector_bytes) = get_config(sector_store);
    if let Some(delay) = delay_seconds {
        delay_get_unsealed_range(delay);
    }

    let prover_id = pad_safe_fr(prover_id_in);
    let sector_id = pad_safe_fr(sector_id_in);
    let replica_id = replica_id(prover_id, sector_id);

    let f_in = File::open(sealed_path)?;
    let mut data = Vec::new();
    f_in.take(sector_bytes as u64).read_to_end(&mut data)?;

    let f_out = File::create(output_path)?;
    let mut buf_writer = BufWriter::new(f_out);

    let unsealed = if fake {
        data
    } else {
        ZigZagDrgPoRep::extract_all(&public_params(sector_bytes), &replica_id, &data)?
    };

    let written = write_unpadded(
        &unsealed,
        &mut buf_writer,
        offset as usize,
        num_bytes as usize,
    )?;

    Ok(written as u64)
}

pub fn verify_seal(
    sector_store: &'static SectorStore,
    comm_r: Commitment,
    comm_d: Commitment,
    prover_id_in: FrSafe,
    sector_id_in: FrSafe,
    proof_vec: &[u8],
) -> Result<bool> {
    let (_fake, _delay_seconds, _sector_bytes, proof_sector_bytes) = get_config(sector_store);

    let challenges = derive_challenges(&comm_r, &comm_d);
    let prover_id = pad_safe_fr(prover_id_in);
    let sector_id = pad_safe_fr(sector_id_in);
    let replica_id = replica_id(prover_id, sector_id);
    let replica_id_fr = bytes_into_fr::<Bls12>(&replica_id)?;

    let comm_r = PedersenHash(bytes_into_fr::<Bls12>(&comm_r)?);
    let comm_d = PedersenHash(bytes_into_fr::<Bls12>(&comm_d)?);

    let public_inputs = layered_drgporep::PublicInputs {
        replica_id: replica_id_fr, // FIXME: Change prover_id field name to replica_id everywhere.
        challenges,
        tau: Some(Tau { comm_r, comm_d }),
    };

    let proof = groth16::Proof::read(proof_vec)?;
    let groth_params = read_cached_params(&dummy_parameter_cache_path(proof_sector_bytes))?;

    let proof = compound_proof::Proof {
        circuit_proof: proof,
        groth_params,
    };

    ZigZagCompound::verify(&public_params(proof_sector_bytes), &public_inputs, proof)
}

fn derive_challenges(_comm_r: &[u8], _comm_d: &[u8]) -> Vec<usize> {
    // TODO: actually derive challenge(s).
    vec![1]
}
