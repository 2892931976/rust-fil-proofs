use libc;
use std::fs::{create_dir_all, metadata, File, OpenOptions};
use std::io::BufWriter;
use std::path::Path;

use api::util;
use api::{SectorConfig, SectorManager, SectorStore, StatusCode};
use storage_proofs::io::fr32::write_padded;

pub const REAL_SECTOR_SIZE: u64 = 128;
pub const FAST_SECTOR_SIZE: u64 = 1024;
pub const SLOW_SECTOR_SIZE: u64 = 1 << 30;

pub const FAST_DELAY_SECONDS: u32 = 10;
pub const SLOW_DELAY_SECONDS: u32 = 4 * 60 * 60;

/// Initializes and returns a boxed SectorStore instance suitable for exercising the proofs code
/// to its fullest capacity.
///
/// # Arguments
///
/// * `staging_dir_path` - path to the staging directory
/// * `sealed_dir_path`  - path to the sealed directory
#[no_mangle]
pub unsafe extern "C" fn init_new_proof_test_sector_store(
    staging_dir_path: *const libc::c_char,
    sealed_dir_path: *const libc::c_char,
) -> *mut Box<SectorStore> {
    let boxed = Box::new(new_sector_store(
        &ConfiguredStore::ProofTest,
        util::c_str_to_rust_str(sealed_dir_path),
        util::c_str_to_rust_str(staging_dir_path),
    ));
    util::raw_ptr(boxed)
}

/// Initializes and returns a boxed SectorStore instance which is very similar to the Alpha-release
/// SectorStore that Filecoin node-users will rely upon - but with manageably-small delays for seal
/// and unseal.
///
/// # Arguments
///
/// * `staging_dir_path` - path to the staging directory
/// * `sealed_dir_path`  - path to the sealed directory
#[no_mangle]
pub unsafe extern "C" fn init_new_test_sector_store(
    staging_dir_path: *const libc::c_char,
    sealed_dir_path: *const libc::c_char,
) -> *mut Box<SectorStore> {
    let boxed = Box::new(new_sector_store(
        &ConfiguredStore::Test,
        util::c_str_to_rust_str(sealed_dir_path),
        util::c_str_to_rust_str(staging_dir_path),
    ));
    util::raw_ptr(boxed)
}

/// Initializes and returns a boxed SectorStore instance which Alpha Filecoin node-users will rely
/// upon. Some operations are substantially delayed; sealing an unsealed sector using this could
/// take several hours.
///
/// # Arguments
///
/// * `staging_dir_path` - path to the staging directory
/// * `sealed_dir_path`  - path to the sealed directory
#[no_mangle]
pub unsafe extern "C" fn init_new_sector_store(
    staging_dir_path: *const libc::c_char,
    sealed_dir_path: *const libc::c_char,
) -> *mut Box<SectorStore> {
    let boxed = Box::new(new_sector_store(
        &ConfiguredStore::Live,
        util::c_str_to_rust_str(sealed_dir_path),
        util::c_str_to_rust_str(staging_dir_path),
    ));

    util::raw_ptr(boxed)
}

pub struct DiskManager {
    staging_path: String,
    sealed_path: String,
}

impl SectorManager for DiskManager {
    fn new_sealed_sector_access(&self) -> Result<String, StatusCode> {
        self.new_sector_access(Path::new(&self.sealed_path))
    }

    fn new_staging_sector_access(&self) -> Result<String, StatusCode> {
        self.new_sector_access(Path::new(&self.staging_path))
    }

    fn num_unsealed_bytes(&self, access: String) -> Result<u64, StatusCode> {
        metadata(access).map(|m| m.len()).map_err(|_| 60)
    }

    fn truncate_unsealed(&self, access: String, size: u64) -> Result<(), StatusCode> {
        OpenOptions::new()
            .write(true)
            .open(access)
            .map_err(|_| 50)
            .and_then(|file| file.set_len(size).map_err(|_| 51).map(|_| ()))
    }

    fn write_unsealed(&self, access: String, data: &[u8]) -> Result<u64, StatusCode> {
        OpenOptions::new()
            .read(true)
            .append(true)
            .open(access)
            .map_err(|_| 40)
            .and_then(|file| {
                let mut buf = BufWriter::new(file);

                write_padded(data, &mut buf)
                    .map_err(|_| 41)
                    .map(|n| n as u64)
            })
    }
}

impl DiskManager {
    fn new_sector_access(&self, root: &Path) -> Result<String, StatusCode> {
        let pbuf = root.join(util::rand_alpha_string(32));

        create_dir_all(root)
            .map_err(|_| 70)
            .and_then(|_| File::create(&pbuf).map(|_| 0).map_err(|_| 71))
            .and_then(|_| {
                pbuf.to_str()
                    .map_or_else(|| Err(72), |str_ref| Ok(str_ref.to_owned()))
            })
    }
}

pub struct RealConfig {
    sector_bytes: u64,
}
pub struct FakeConfig {
    sector_bytes: u64,
    delay_seconds: u32,
}

#[derive(Debug)]
pub enum ConfiguredStore {
    Live,
    Test,
    ProofTest,
}

pub struct ConcreteSectorStore {
    config: Box<SectorConfig>,
    manager: Box<SectorManager>,
}

impl SectorStore for ConcreteSectorStore {
    fn config(&self) -> &SectorConfig {
        self.config.as_ref()
    }
    fn manager(&self) -> &SectorManager {
        self.manager.as_ref()
    }
}

pub fn new_real_sector_store(sealed_path: String, staging_path: String) -> ConcreteSectorStore {
    ConcreteSectorStore {
        config: Box::new(RealConfig {
            sector_bytes: REAL_SECTOR_SIZE,
        }),
        manager: Box::new(DiskManager {
            sealed_path,
            staging_path,
        }),
    }
}

pub fn new_sector_store(
    cs: &ConfiguredStore,
    sealed_path: String,
    staging_path: String,
) -> ConcreteSectorStore {
    match *cs {
        ConfiguredStore::Live => new_slow_fake_sector_store(sealed_path, staging_path),
        ConfiguredStore::Test => new_fast_fake_sector_store(sealed_path, staging_path),
        ConfiguredStore::ProofTest => new_real_sector_store(sealed_path, staging_path),
    }
}

pub fn new_slow_fake_sector_store(
    sealed_path: String,
    staging_path: String,
) -> ConcreteSectorStore {
    new_fake_sector_store(
        sealed_path,
        staging_path,
        SLOW_SECTOR_SIZE,
        SLOW_DELAY_SECONDS,
    )
}

pub fn new_fast_fake_sector_store(
    sealed_path: String,
    staging_path: String,
) -> ConcreteSectorStore {
    new_fake_sector_store(
        sealed_path,
        staging_path,
        FAST_SECTOR_SIZE,
        FAST_DELAY_SECONDS,
    )
}

fn new_fake_sector_store(
    sealed_path: String,
    staging_path: String,
    sector_bytes: u64,
    delay_seconds: u32,
) -> ConcreteSectorStore {
    ConcreteSectorStore {
        config: Box::new(FakeConfig {
            sector_bytes,
            delay_seconds,
        }),
        manager: Box::new(DiskManager {
            sealed_path,
            staging_path,
        }),
    }
}

impl SectorConfig for RealConfig {
    fn is_fake(&self) -> bool {
        false
    }

    fn simulate_delay_seconds(&self) -> Option<u32> {
        None
    }

    fn max_unsealed_bytes_per_sector(&self) -> u64 {
        self.sector_bytes
    }
}

impl SectorConfig for FakeConfig {
    fn is_fake(&self) -> bool {
        true
    }

    fn simulate_delay_seconds(&self) -> Option<u32> {
        Some(self.delay_seconds)
    }

    fn max_unsealed_bytes_per_sector(&self) -> u64 {
        self.sector_bytes
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{create_dir_all, File};
    use std::io::Read;
    use tempfile;

    use super::*;

    use api::disk_backed_storage::init_new_proof_test_sector_store;
    use api::util;
    use api::{new_staging_sector_access, num_unsealed_bytes, truncate_unsealed, write_unsealed};

    fn create_storage() -> *mut Box<SectorStore> {
        let staging_path = tempfile::tempdir().unwrap().path().to_owned();
        let sealed_path = tempfile::tempdir().unwrap().path().to_owned();

        create_dir_all(&staging_path).expect("failed to create staging dir");
        create_dir_all(&sealed_path).expect("failed to create sealed dir");

        let s1 = util::rust_str_to_c_str(&staging_path.to_str().unwrap().to_owned());
        let s2 = util::rust_str_to_c_str(&sealed_path.to_str().unwrap().to_owned());

        unsafe { init_new_proof_test_sector_store(s1, s2) }
    }

    fn read_all_bytes(access: *const libc::c_char) -> Vec<u8> {
        let pbuf = unsafe { util::pbuf_from_c(access) };
        let mut file = File::open(pbuf).unwrap();
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).unwrap();

        buf
    }

    #[test]
    fn unsealed_sector_write_and_truncate() {
        let storage = create_storage();

        let access = unsafe {
            let result = &mut util::rust_str_to_c_str("");
            new_staging_sector_access(storage, result);
            *result
        };

        let contents = &[2u8; 500];
        let write_result_ptr = &mut 0u64;

        assert_eq!(0, unsafe {
            write_unsealed(
                storage,
                access,
                &contents[0],
                contents.len(),
                write_result_ptr,
            )
        });

        // buffer the file's bytes into memory after writing bytes
        let buf = read_all_bytes(access);

        // ensure that we reported the correct number of written bytes
        assert_eq!(contents.len(), *write_result_ptr as usize);

        // ensure the file we wrote to contains the expected bytes
        assert_eq!(contents[0..31], buf[0..31]);
        assert_eq!(0u8, buf[31]);

        // read the file into memory again - this time after we truncate
        let buf = read_all_bytes(access);

        // ensure the file we wrote to contains the expected bytes
        assert_eq!(503, buf.len());

        assert_eq!(0, unsafe { truncate_unsealed(storage, access, 1) });

        // read the file into memory again - this time after we truncate
        let buf = read_all_bytes(access);

        // ensure the file we wrote to contains the expected bytes
        assert_eq!(1, buf.len());
        assert_eq!(contents[0..1], buf[0..]);

        let num_bytes_result_ptr = &mut 0u64;

        assert_eq!(0, unsafe {
            num_unsealed_bytes(storage, access, num_bytes_result_ptr)
        });

        // ensure that our byte-counting function works
        assert_eq!(buf.len(), *num_bytes_result_ptr as usize);
    }
}
