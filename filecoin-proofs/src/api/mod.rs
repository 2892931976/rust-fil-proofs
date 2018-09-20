use libc;

use sector_base::api::SectorStore;
use std::ffi::CString;
use std::mem::forget;

mod internal;
pub mod util;

type SectorAccess = *const libc::c_char;
type StatusCode = u32;
type GetUnsealedRangeResultPtr = *mut u64;

/// This is also defined in api::internal, but we make it explicit here for API consumers.
/// How big, in bytes, is a SNARK proof?
pub const SNARK_BYTES: usize = 192;

/// Seals a sector and returns a status code indicating success or failure.
///
/// # Arguments
///
/// * `ss_ptr`              - pointer to a boxed SectorStore
/// * `unsealed`            - path of unsealed sector-file
/// * `sealed`              - path of sealed sector-file
/// * `prover_id_ptr`       - pointer to first cell in a 31-length array of u8
/// * `sector_id_ptr`       - pointer to first cell in a 31-length array of u8
/// * `result_ptr`          - pointer to first cell in a 64-length array of u8,
///                           mutated by seal in order to pass commitments back
///                           to caller (first 32 elements correspond to comm_r
///                           and second 32 to comm_d)
#[no_mangle]
pub unsafe extern "C" fn seal(
    ss_ptr: *mut Box<SectorStore>,
    unsealed_path: SectorAccess,
    sealed_path: SectorAccess,
    prover_id: &[u8; 31],
    sector_id: &[u8; 31],
    result_comm_r: &mut [u8; 32],
    result_comm_d: &mut [u8; 32],
    result_proof: &mut [u8; SNARK_BYTES],
) -> StatusCode {
    let sector_store = &mut **ss_ptr;

    let unsealed_path_buf = util::pbuf_from_c(unsealed_path);
    let sealed_path_buf = util::pbuf_from_c(sealed_path);

    let result = internal::seal(
        sector_store,
        &unsealed_path_buf,
        &sealed_path_buf,
        *prover_id,
        *sector_id,
    );

    match result {
        Ok((comm_r, comm_d, snark_proof)) => {
            // let caller manage this memory, preventing the need for calling back into
            // Rust code later to deallocate
            result_comm_r[..32].clone_from_slice(&comm_r[..32]);
            result_comm_d[..32].clone_from_slice(&comm_d[..32]);
            result_proof[..SNARK_BYTES].clone_from_slice(&snark_proof[..SNARK_BYTES]);

            0
        }
        Err(_) => {
            // TODO: Log FPS errors throughout.
            // TODO: make a decision about which status code to return using Err
            10
        }
    }
}

/// Verifies the output of seal and returns a status code indicating success or failure.
///
/// # Arguments
///
/// * `ss_ptr`             - pointer to a boxed SectorStore
/// * `comm_r_ptr`         - pointer to first cell in a 32-length array of u8 contaning the replica
///                          commitment
/// * `comm_d_ptr`         - pointer to first cell in a 32-length array of u8 containing the data
///                          commitment
/// * `prover_id_ptr`      - pointer to first cell in a 31-length array of u8
/// * `sector_id_ptr`      - pointer to first cell in a 31-length array of u8
/// * `proof_ptr`          - pointer to first cell in a SNARK_BYTES-length array of u8
#[no_mangle]
pub unsafe extern "C" fn verify_seal(
    ss_ptr: *mut Box<SectorStore>,
    comm_r: &[u8; 32],
    comm_d: &[u8; 32],
    prover_id: &[u8; 31],
    sector_id: &[u8; 31],
    proof: &[u8; SNARK_BYTES],
) -> StatusCode {
    let sector_store = &mut **ss_ptr;

    match internal::verify_seal(
        sector_store,
        *comm_r,
        *comm_d,
        *prover_id,
        *sector_id,
        proof,
    ) {
        Ok(true) => 0,
        Ok(false) => 20,
        Err(_) => 21,
    }
}

/// Gets bytes from a sealed sector-file and writes them, unsealed, to the output path and returns a
/// status code indicating success or failure.
///
/// It is possible that the get_unsealed_range operation writes a number of bytes to the output_path which is
/// less than num_bytes.
///
/// # Arguments
///
/// * `ss_ptr`       - pointer to a boxed SectorStore
/// * `sealed_path`  - path of sealed sector-file
/// * `output_path`  - path where sector file's unsealed bytes should be written
/// * `start_offset` - zero-based byte offset in original, unsealed sector-file
/// * `num_bytes`    - number of bytes to unseal and get (corresponds to contents of unsealed sector-file)
/// * `prover_id_ptr`- pointer to first cell in a 31-length array of u8
/// * `sector_id_ptr`- pointer to first cell in a 31-length array of u8
/// * `result_ptr`   - pointer to a u64, mutated by get_unsealed_range in order to communicate the number of
///                    bytes that were unsealed and written to the output_path
#[no_mangle]
pub unsafe extern "C" fn get_unsealed_range(
    ss_ptr: *mut Box<SectorStore>,
    sealed_path: SectorAccess,
    output_path: SectorAccess,
    start_offset: u64,
    num_bytes: u64,
    prover_id: &[u8; 31],
    sector_id: &[u8; 31],
    result_ptr: GetUnsealedRangeResultPtr,
) -> StatusCode {
    // How to read: &mut **ss_ptr throughout:
    // ss_ptr is a pointer to a Box
    // *ss_ptr is the Box.
    // **ss_ptr is the Box's content: a SectorStore.
    // &mut **ss_ptr is a mutable reference to the SectorStore.
    let sector_store = &mut **ss_ptr;
    let sealed_path_buf = util::pbuf_from_c(sealed_path);
    let output_path_buf = util::pbuf_from_c(output_path);

    match internal::get_unsealed_range(
        sector_store,
        &sealed_path_buf,
        &output_path_buf,
        *prover_id,
        *sector_id,
        start_offset,
        num_bytes,
    ) {
        Ok(num_bytes) => {
            result_ptr.write(num_bytes);
            0
        }
        Err(_) => 30,
    }
}

/// Gets an entire sealed sector-file and writes it, unsealed, to the output path and returns a
/// status code indicating success or failure.
///
/// # Arguments
///
/// * `ss_ptr`       - pointer to a boxed SectorStore
/// * `sealed_path`  - path of sealed sector-file
/// * `output_path`  - path where sector file's unsealed bytes should be written
/// * `prover_id_ptr`- pointer to first cell in a 31-length array of u8
/// * `sector_id_ptr`- pointer to first cell in a 31-length array of u8
#[no_mangle]
pub unsafe extern "C" fn get_unsealed(
    ss_ptr: *mut Box<SectorStore>,
    sealed_path: SectorAccess,
    output_path: SectorAccess,
    prover_id: &[u8; 31],
    sector_id: &[u8; 31],
) -> StatusCode {
    let sector_store = &mut **ss_ptr;

    let sealed_path_buf = util::pbuf_from_c(sealed_path);
    let output_path_buf = util::pbuf_from_c(output_path);
    let sector_bytes = sector_store.config().max_unsealed_bytes_per_sector();

    match internal::get_unsealed_range(
        sector_store,
        &sealed_path_buf,
        &output_path_buf,
        *prover_id,
        *sector_id,
        0,
        sector_bytes,
    ) {
        Ok(_) => 0,
        // TODO: this can differ, due to padding. Figure out how to best handle this.
        // if num_bytes == sector_bytes {
        //     0
        // } else {
        //     30
        // },
        Err(_) => 30,
    }
}

/// Returns a human-readable message for the provided status code.
///
/// Callers are responsible for freeing the returned string.
///
/// TODO: This thing needs to be reworked such that filecoin-proofs doesn't know about the
/// internals of various instances of SectorStore. This may be a matter of narrowing these codes to
/// something which communicates that the error was the caller's fault versus something unexpected
/// which happened on the receiver side (e.g. disk failure).
///
/// # Arguments
///
/// * `status_code` - a status code returned from an FPS operation, such as seal or verify_seal
#[no_mangle]
pub extern "C" fn status_to_string(status_code: StatusCode) -> *const libc::c_char {
    // TODO: Use an enum for each range of StatusCodes, so use throughout is comprehensible.
    // Also, we're comingling the status codes returned by sector-base and filecoin-proofs.
    // Can we tease this apart?
    let s = match status_code {
        0 => CString::new("success"),
        10 => CString::new("failed to seal"),
        20 => CString::new("invalid replica and/or data commitment"),
        21 => CString::new("unhandled verify_seal error"),
        30 => CString::new("failed to get unsealed range"),
        40 => CString::new("failed to write to unsealed sector"),
        41 => CString::new("failed to create unsealed sector"),
        50 => CString::new("failed to open file for truncating"),
        51 => CString::new("failed to set file length"),
        60 => CString::new("could not read unsealed sector file size"),
        70 => CString::new("could not create sector access parent directory"),
        71 => CString::new("could not create sector file"),
        72 => CString::new("could not stringify path buffer"),
        73 => CString::new("could not create C string"),
        80 => CString::new("could not create sealed sector-directory"),
        81 => CString::new("could not create sealed sector"),
        n => CString::new(format!("unknown status code {}", n)),
    }.unwrap();

    let p = s.as_ptr();

    forget(s);

    p
}

#[no_mangle]
pub extern "C" fn generatePost() {
    unimplemented!()
}

#[no_mangle]
pub extern "C" fn verifyPost() {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{thread_rng, Rng};

    use api::SNARK_BYTES;
    use sector_base::api::disk_backed_storage::{
        init_new_proof_test_sector_store, init_new_sector_store, init_new_test_sector_store,
        ConfiguredStore,
    };
    use sector_base::api::{
        new_sealed_sector_access, new_staging_sector_access, write_unsealed, SectorStore,
    };
    use std::ffi::CString;
    use std::fs::{create_dir_all, File};
    use std::io::Read;
    use tempfile;

    fn rust_str_to_c_str(s: &str) -> *const libc::c_char {
        CString::new(s).unwrap().into_raw()
    }

    /// simulates a call through the FFI to provision a SectorAccess
    fn create_sector_access(
        storage: *mut Box<SectorStore>,
        f: unsafe extern "C" fn(*mut Box<SectorStore>, *mut *const libc::c_char) -> StatusCode,
    ) -> SectorAccess {
        let result = &mut rust_str_to_c_str("");
        let _ = unsafe { f(storage, result) };
        *result
    }

    fn create_storage(cs: &ConfiguredStore) -> *mut Box<SectorStore> {
        let staging_path = tempfile::tempdir().unwrap().path().to_owned();
        let sealed_path = tempfile::tempdir().unwrap().path().to_owned();

        create_dir_all(&staging_path).expect("failed to create staging dir");
        create_dir_all(&sealed_path).expect("failed to create sealed dir");

        let s1 = rust_str_to_c_str(&staging_path.to_str().unwrap().to_owned());
        let s2 = rust_str_to_c_str(&sealed_path.to_str().unwrap().to_owned());

        match cs {
            ConfiguredStore::Live => unsafe { init_new_sector_store(s1, s2) },
            ConfiguredStore::Test => unsafe { init_new_test_sector_store(s1, s2) },
            ConfiguredStore::ProofTest => unsafe { init_new_proof_test_sector_store(s1, s2) },
        }
    }

    // TODO: create a way to run these super-slow-by-design tests manually.
    //    fn seal_verify_live() {
    //        seal_verify_aux(ConfiguredStore::Live);
    //    }
    //
    //    fn seal_unsealed_roundtrip_live() {
    //        seal_unsealed_roundtrip_aux(ConfiguredStore::Live);
    //    }
    //
    //    fn seal_unsealed_range_roundtrip_live() {
    //        seal_unsealed_range_roundtrip_aux(ConfiguredStore::Live);
    //    }

    #[test]
    #[ignore] // Slow test – run only when compiled for release.
    fn seal_verify_test() {
        seal_verify_aux(ConfiguredStore::Test);
    }

    #[test]
    #[ignore] // Slow test – run only when compiled for release.
    fn seal_verify_proof_test() {
        seal_verify_aux(ConfiguredStore::ProofTest);
    }

    #[test]
    #[ignore] // Slow test – run only when compiled for release.
    fn seal_unsealed_roundtrip_test() {
        seal_unsealed_roundtrip_aux(ConfiguredStore::Test);
    }

    #[test]
    #[ignore] // Slow test – run only when compiled for release.
    fn seal_unsealed_roundtrip_proof_test() {
        seal_unsealed_roundtrip_aux(ConfiguredStore::ProofTest);
    }

    #[test]
    #[ignore] // Slow test – run only when compiled for release.
    fn seal_unsealed_range_roundtrip_test() {
        seal_unsealed_range_roundtrip_aux(ConfiguredStore::Test);
    }

    #[test]
    #[ignore] // Slow test – run only when compiled for release.
    fn seal_unsealed_range_roundtrip_proof_test() {
        seal_unsealed_range_roundtrip_aux(ConfiguredStore::ProofTest);
    }

    fn storage_bytes(sector_store: &'static SectorStore) -> usize {
        sector_store.config().max_unsealed_bytes_per_sector() as usize
    }

    fn make_data_for_storage(sector_store: &'static SectorStore) -> Vec<u8> {
        let mut rng = thread_rng();
        let length = storage_bytes(sector_store) - 50; // Leave some room for padding.
        (0..length)
            .map(|_| rng.gen::<u8>() & 0b00111111) // Mask out two most significant bits so data is always Fr.
            .collect::<Vec<u8>>()
    }

    fn seal_verify_aux(cs: ConfiguredStore) {
        let storage = create_storage(&cs);

        let seal_input_path = create_sector_access(storage, new_staging_sector_access);
        let seal_output_path = create_sector_access(storage, new_sealed_sector_access);

        let result_comm_r: &mut [u8; 32] = &mut [0; 32];
        let result_comm_d: &mut [u8; 32] = &mut [0; 32];
        let result_proof: &mut [u8; 192] = &mut [0; 192];
        let prover_id = &[2; 31];
        let sector_id = &[0; 31];

        let contents = unsafe { make_data_for_storage(&**storage) };
        let result_ptr = &mut 0u64;

        assert_eq!(
            0,
            unsafe {
                write_unsealed(
                    storage,
                    seal_input_path,
                    &contents[0],
                    contents.len(),
                    result_ptr,
                )
            },
            "write_unsealed failed for {:?}",
            cs
        );

        let good_seal = unsafe {
            seal(
                storage,
                seal_input_path,
                seal_output_path,
                prover_id,
                sector_id,
                result_comm_r,
                result_comm_d,
                result_proof,
            )
        };

        assert_eq!(0, good_seal, "seal failed for {:?}", cs);

        let good_verify = unsafe {
            verify_seal(
                storage,
                result_comm_r,
                result_comm_d,
                prover_id,
                sector_id,
                result_proof,
            )
        };

        assert_eq!(0, good_verify, "verification failed for {:?}", cs);

        // FIXME: This test will not pass until we actually make use of the commtiments in ZigZag
        // that will be implemented in https://github.com/filecoin-project/rust-proofs/issues/145
        //        let bad_verify = unsafe {
        //            verify_seal(
        //                &result[32],
        //                &result[0],
        //                &prover_id[0],
        //                &challenge_seed[0],
        //                &result[64],
        //            )
        //        };
        // assert_eq!(20, bad_verify);
    }

    fn seal_unsealed_roundtrip_aux(cs: ConfiguredStore) {
        let storage = create_storage(&cs);

        let seal_input_path = create_sector_access(storage, new_staging_sector_access);
        let seal_output_path = create_sector_access(storage, new_sealed_sector_access);
        let get_unsealed_range_output_path =
            create_sector_access(storage, new_staging_sector_access);

        let result_comm_r: &mut [u8; 32] = &mut [0; 32];
        let result_comm_d: &mut [u8; 32] = &mut [0; 32];
        let result_proof: &mut [u8; SNARK_BYTES] = &mut [0; SNARK_BYTES];
        let prover_id = &[2; 31];
        let sector_id = &[0; 31];

        let contents = unsafe { make_data_for_storage(&**storage) };
        let result_ptr = &mut 0u64;

        assert_eq!(
            0,
            unsafe {
                write_unsealed(
                    storage,
                    seal_input_path,
                    &contents[0],
                    contents.len(),
                    result_ptr,
                )
            },
            "write_unsealed failed for {:?}",
            cs
        );

        let good_seal = unsafe {
            seal(
                storage,
                seal_input_path,
                seal_output_path,
                prover_id,
                sector_id,
                result_comm_r,
                result_comm_d,
                result_proof,
            )
        };

        assert_eq!(0, good_seal, "seal failed for {:?}", cs);

        let good_unsealed = unsafe {
            get_unsealed(
                storage,
                seal_output_path,
                get_unsealed_range_output_path,
                prover_id,
                sector_id,
            )
        };

        assert_eq!(0, good_unsealed, "get_unsealed failed for {:?}", cs);

        let mut file =
            File::open(unsafe { util::pbuf_from_c(get_unsealed_range_output_path) }).unwrap();
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).unwrap();

        assert_eq!(
            contents[..],
            buf[0..contents.len()],
            "original and unsealed contents differed for {:?}",
            cs
        );
    }

    fn seal_unsealed_range_roundtrip_aux(cs: ConfiguredStore) {
        let storage = create_storage(&cs);

        let seal_input_path = create_sector_access(storage, new_staging_sector_access);
        let seal_output_path = create_sector_access(storage, new_sealed_sector_access);
        let get_unsealed_range_output_path =
            create_sector_access(storage, new_staging_sector_access);

        let result_comm_r: &mut [u8; 32] = &mut [0; 32];
        let result_comm_d: &mut [u8; 32] = &mut [0; 32];
        let result_proof: &mut [u8; SNARK_BYTES] = &mut [0; SNARK_BYTES];
        let prover_id = &[2; 31];
        let sector_id = &[0; 31];

        let contents = unsafe { make_data_for_storage(&**storage) };
        let result_ptr = &mut 0u64;

        assert_eq!(
            0,
            unsafe {
                write_unsealed(
                    storage,
                    seal_input_path,
                    &contents[0],
                    contents.len(),
                    result_ptr,
                )
            },
            "write_unsealed failed for {:?}",
            cs
        );

        let good_seal = unsafe {
            seal(
                storage,
                seal_input_path,
                seal_output_path,
                prover_id,
                sector_id,
                result_comm_r,
                result_comm_d,
                result_proof,
            )
        };

        assert_eq!(0, good_seal, "seal failed for {:?}", cs);

        let offset = 5;
        let range_length = contents.len() as u64 - offset;
        let good_unsealed = unsafe {
            get_unsealed_range(
                storage,
                seal_output_path,
                get_unsealed_range_output_path,
                offset,
                range_length,
                prover_id,
                sector_id,
                result_ptr,
            )
        };

        assert_eq!(0, good_unsealed, "get_unsealed failed for {:?}", cs);
        assert_eq!(
            range_length, *result_ptr,
            "expected range length {}; got {} for {:?}",
            range_length, *result_ptr, cs
        );

        let mut file =
            File::open(unsafe { util::pbuf_from_c(get_unsealed_range_output_path) }).unwrap();
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).unwrap();

        assert_eq!(
            contents[(offset as usize)..],
            buf[0..(range_length as usize)],
            "original and unsealed_range contents differed for {:?}",
            cs
        );
    }
}
