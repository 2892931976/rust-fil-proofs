use api::sector_builder::errors::SectorBuilderErr;
use api::sector_builder::SectorBuilder;
use api::{API_POREP_PROOF_BYTES, API_POST_PROOF_BYTES};
use failure::Error;
use ffi_toolkit::c_str_to_rust_str;
use libc;
use sector_base::api::errors::SectorManagerErr;
use std::ffi::CString;
use std::mem;
use std::ptr;

// TODO: libfilecoin_proofs.h and libsector_base.h will likely be consumed by
// the same program, so these names need to be unique. Alternatively, figure
// out a way to share this enum across crates in a way that won't cause
// cbindgen to fail.
#[repr(C)]
#[derive(PartialEq, Debug)]
pub enum FCPResponseStatus {
    // Don't use FCPSuccess, since that complicates description of 'successful' verification.
    FCPNoError = 0,
    FCPUnclassifiedError = 1,
    FCPCallerError = 2,
    FCPReceiverError = 3,
}

///////////////////////////////////////////////////////////////////////////////
/// SealResponse
////////////////

#[repr(C)]
#[derive(Destroy)]
pub struct SealResponse {
    pub status_code: FCPResponseStatus,
    pub error_msg: *const libc::c_char,
    pub comm_d: [u8; 32],
    pub comm_r: [u8; 32],
    pub comm_r_star: [u8; 32],
    pub proof: [u8; API_POREP_PROOF_BYTES],
}

impl Default for SealResponse {
    fn default() -> SealResponse {
        SealResponse {
            status_code: FCPResponseStatus::FCPNoError,
            error_msg: ptr::null(),
            comm_d: [0; 32],
            comm_r: [0; 32],
            comm_r_star: [0; 32],
            proof: [0; API_POREP_PROOF_BYTES],
        }
    }
}

impl Drop for SealResponse {
    fn drop(&mut self) {
        unsafe {
            drop(c_str_to_rust_str(self.error_msg));
        };
    }
}

///////////////////////////////////////////////////////////////////////////////
/// VerifySealResponse
//////////////////////

#[repr(C)]
#[derive(Destroy)]
pub struct VerifySealResponse {
    pub status_code: FCPResponseStatus,
    pub error_msg: *const libc::c_char,
    pub is_valid: bool,
}

impl Default for VerifySealResponse {
    fn default() -> VerifySealResponse {
        VerifySealResponse {
            status_code: FCPResponseStatus::FCPNoError,
            error_msg: ptr::null(),
            is_valid: false,
        }
    }
}

impl Drop for VerifySealResponse {
    fn drop(&mut self) {
        unsafe {
            drop(c_str_to_rust_str(self.error_msg));
        };
    }
}

///////////////////////////////////////////////////////////////////////////////
/// GetUnsealedRangeResponse
////////////////////////////

#[repr(C)]
#[derive(Destroy)]
pub struct GetUnsealedRangeResponse {
    pub status_code: FCPResponseStatus,
    pub error_msg: *const libc::c_char,
    pub num_bytes_written: u64,
}

impl Default for GetUnsealedRangeResponse {
    fn default() -> GetUnsealedRangeResponse {
        GetUnsealedRangeResponse {
            status_code: FCPResponseStatus::FCPNoError,
            error_msg: ptr::null(),
            num_bytes_written: 0,
        }
    }
}

impl Drop for GetUnsealedRangeResponse {
    fn drop(&mut self) {
        unsafe {
            drop(c_str_to_rust_str(self.error_msg));
        };
    }
}

///////////////////////////////////////////////////////////////////////////////
/// GetUnsealedResponse
///////////////////////

#[repr(C)]
#[derive(Destroy)]
pub struct GetUnsealedResponse {
    pub status_code: FCPResponseStatus,
    pub error_msg: *const libc::c_char,
}

impl Default for GetUnsealedResponse {
    fn default() -> GetUnsealedResponse {
        GetUnsealedResponse {
            status_code: FCPResponseStatus::FCPNoError,
            error_msg: ptr::null(),
        }
    }
}

impl Drop for GetUnsealedResponse {
    fn drop(&mut self) {
        unsafe {
            drop(c_str_to_rust_str(self.error_msg));
        };
    }
}

///////////////////////////////////////////////////////////////////////////////
/// GeneratePoSTResult
//////////////////////

#[repr(C)]
#[derive(Destroy)]
pub struct GeneratePoSTResponse {
    pub status_code: FCPResponseStatus,
    pub error_msg: *const libc::c_char,
    pub faults_len: libc::size_t,
    pub faults_ptr: *const u64,
    pub proof: [u8; API_POST_PROOF_BYTES],
}

impl Default for GeneratePoSTResponse {
    fn default() -> GeneratePoSTResponse {
        GeneratePoSTResponse {
            status_code: FCPResponseStatus::FCPNoError,
            error_msg: ptr::null(),
            faults_len: 0,
            faults_ptr: ptr::null(),
            proof: [0; API_POST_PROOF_BYTES],
        }
    }
}

impl Drop for GeneratePoSTResponse {
    fn drop(&mut self) {
        unsafe {
            drop(Vec::from_raw_parts(
                self.faults_ptr as *mut u8,
                self.faults_len,
                self.faults_len,
            ));

            drop(c_str_to_rust_str(self.error_msg));
        };
    }
}

///////////////////////////////////////////////////////////////////////////////
/// VerifyPoSTResult
////////////////////

#[repr(C)]
#[derive(Destroy)]
pub struct VerifyPoSTResponse {
    pub status_code: FCPResponseStatus,
    pub error_msg: *const libc::c_char,
    pub is_valid: bool,
}

impl Default for VerifyPoSTResponse {
    fn default() -> VerifyPoSTResponse {
        VerifyPoSTResponse {
            status_code: FCPResponseStatus::FCPNoError,
            error_msg: ptr::null(),
            is_valid: false,
        }
    }
}

impl Drop for VerifyPoSTResponse {
    fn drop(&mut self) {
        unsafe {
            drop(c_str_to_rust_str(self.error_msg));
        };
    }
}

// err_code_and_msg accepts an Error struct and produces a tuple of response
// status code and a pointer to a C string, both of which can be used to set
// fields in a response struct to be returned from an FFI call.
pub fn err_code_and_msg(err: &Error) -> (FCPResponseStatus, *const libc::c_char) {
    use api::responses::FCPResponseStatus::*;

    let msg = CString::new(format!("{}", err)).unwrap();
    let ptr = msg.as_ptr();
    mem::forget(msg);

    match err.downcast_ref() {
        Some(SectorBuilderErr::OverflowError { .. }) => return (FCPCallerError, ptr),
        Some(SectorBuilderErr::IncompleteWriteError { .. }) => return (FCPReceiverError, ptr),
        Some(SectorBuilderErr::Unrecoverable(_)) => return (FCPReceiverError, ptr),
        None => (),
    }

    match err.downcast_ref() {
        Some(SectorManagerErr::UnclassifiedError(_)) => return (FCPUnclassifiedError, ptr),
        Some(SectorManagerErr::CallerError(_)) => return (FCPCallerError, ptr),
        Some(SectorManagerErr::ReceiverError(_)) => return (FCPReceiverError, ptr),
        None => (),
    }

    (FCPUnclassifiedError, ptr)
}

///////////////////////////////////////////////////////////////////////////////
/// InitSectorBuilderResponse
/////////////////////////////

#[repr(C)]
#[derive(Destroy)]
pub struct InitSectorBuilderResponse {
    pub status_code: FCPResponseStatus,
    pub error_msg: *const libc::c_char,
    pub sector_builder: *mut SectorBuilder,
}

impl Default for InitSectorBuilderResponse {
    fn default() -> InitSectorBuilderResponse {
        InitSectorBuilderResponse {
            status_code: FCPResponseStatus::FCPNoError,
            error_msg: ptr::null(),
            sector_builder: ptr::null_mut(),
        }
    }
}

impl Drop for InitSectorBuilderResponse {
    fn drop(&mut self) {
        unsafe {
            drop(c_str_to_rust_str(self.error_msg));
        };
    }
}

///////////////////////////////////////////////////////////////////////////////
/// AddPieceResponse
////////////////////

#[repr(C)]
#[derive(Destroy)]
pub struct AddPieceResponse {
    pub status_code: FCPResponseStatus,
    pub error_msg: *const libc::c_char,
    pub sector_id: u64,
}

impl Default for AddPieceResponse {
    fn default() -> AddPieceResponse {
        AddPieceResponse {
            status_code: FCPResponseStatus::FCPNoError,
            error_msg: ptr::null(),
            sector_id: 0,
        }
    }
}

impl Drop for AddPieceResponse {
    fn drop(&mut self) {
        unsafe {
            drop(c_str_to_rust_str(self.error_msg));
        };
    }
}

///////////////////////////////////////////////////////////////////////////////
/// GetMaxStagedBytesPerSector
//////////////////////////////

#[repr(C)]
#[derive(Destroy)]
pub struct GetMaxStagedBytesPerSector {
    pub status_code: FCPResponseStatus,
    pub error_msg: *const libc::c_char,
    pub max_staged_bytes_per_sector: u64,
}

impl Default for GetMaxStagedBytesPerSector {
    fn default() -> GetMaxStagedBytesPerSector {
        GetMaxStagedBytesPerSector {
            status_code: FCPResponseStatus::FCPNoError,
            error_msg: ptr::null(),
            max_staged_bytes_per_sector: 0,
        }
    }
}

impl Drop for GetMaxStagedBytesPerSector {
    fn drop(&mut self) {
        unsafe {
            drop(c_str_to_rust_str(self.error_msg));
        };
    }
}

///////////////////////////////////////////////////////////////////////////////
/// GetSealedSectorMetadataResponse
///////////////////////////////////

#[repr(C)]
#[derive(Destroy)]
pub struct FindSealedSectorMetadataResponse {
    pub status_code: FCPResponseStatus,
    pub error_msg: *const libc::c_char,

    pub metadata_exists: bool,

    pub comm_d: [u8; 32],
    pub comm_r: [u8; 32],
    pub comm_r_star: [u8; 32],
    pub sector_access: *const libc::c_char,
    pub sector_id: u64,
    pub snark_proof: [u8; API_POREP_PROOF_BYTES],
    // TODO: Are pieces needed? Will the proofs-related stuff suffice?
}

impl Default for FindSealedSectorMetadataResponse {
    fn default() -> FindSealedSectorMetadataResponse {
        FindSealedSectorMetadataResponse {
            status_code: FCPResponseStatus::FCPNoError,
            error_msg: ptr::null(),

            metadata_exists: false,

            comm_d: Default::default(),
            comm_r: Default::default(),
            comm_r_star: Default::default(),
            sector_access: ptr::null(),
            sector_id: 0,
            snark_proof: [0; 384],
        }
    }
}

impl Drop for FindSealedSectorMetadataResponse {
    fn drop(&mut self) {
        unsafe {
            drop(c_str_to_rust_str(self.error_msg));
            drop(c_str_to_rust_str(self.sector_access));
        };
    }
}
