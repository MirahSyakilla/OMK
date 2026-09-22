use std::ffi::{c_void, CStr};

const NID_SHA256: i32 = 672;

type SignFn = unsafe extern "C" fn(i32, *const u8, u32, *mut u8, *mut u32, *mut c_void) -> i32;
type Sha256Fn = unsafe extern "C" fn(*const u8, usize, *mut u8) -> *mut u8;

pub(super) struct Identity {
    pub json: Vec<u8>,
    pub json_signature: Vec<u8>,
    rsa: *mut c_void,
    sign: SignFn,
    sha256: Sha256Fn,
}

unsafe impl Send for Identity {}
unsafe impl Sync for Identity {}

impl Identity {
    pub(super) fn sign(&self, message: &[u8]) -> Result<Vec<u8>, String> {
        let mut digest = [0u8; 32];
        unsafe { (self.sha256)(message.as_ptr(), message.len(), digest.as_mut_ptr()) };
        let mut signature = vec![0u8; 256];
        let mut length = signature.len() as u32;
        let ok = unsafe {
            (self.sign)(
                NID_SHA256,
                digest.as_ptr(),
                digest.len() as u32,
                signature.as_mut_ptr(),
                &mut length,
                self.rsa,
            )
        };
        if ok != 1 || length == 0 || length as usize > signature.len() {
            return Err("Soter RSA signature failed".to_string());
        }
        signature.truncate(length as usize);
        Ok(signature)
    }
}

pub(super) fn load() -> Result<Identity, String> {
    let lib = unsafe { libc::dlopen(c"libcrypto.so".as_ptr(), libc::RTLD_NOW) };
    if lib.is_null() {
        return Err("Soter cannot load libcrypto".to_string());
    }
    unsafe {
        let rsa_new: unsafe extern "C" fn() -> *mut c_void = sym(lib, c"RSA_new")?;
        let rsa_generate: unsafe extern "C" fn(*mut c_void, i32, *mut c_void, *mut c_void) -> i32 =
            sym(lib, c"RSA_generate_key_ex")?;
        let bn_new: unsafe extern "C" fn() -> *mut c_void = sym(lib, c"BN_new")?;
        let bn_set: unsafe extern "C" fn(*mut c_void, u64) -> i32 = sym(lib, c"BN_set_word")?;
        let bn_free: unsafe extern "C" fn(*mut c_void) = sym(lib, c"BN_free")?;
        let i2d: unsafe extern "C" fn(*const c_void, *mut *mut u8) -> i32 =
            sym(lib, c"i2d_RSA_PUBKEY")?;
        let sign: SignFn = sym(lib, c"RSA_sign")?;
        let sha256: Sha256Fn = sym(lib, c"SHA256")?;

        let rsa = rsa_new();
        let exponent = bn_new();
        if rsa.is_null() || exponent.is_null() || bn_set(exponent, 65537) != 1 {
            bn_free(exponent);
            return Err("Soter cannot allocate an RSA key".to_string());
        }
        if rsa_generate(rsa, 2048, exponent, std::ptr::null_mut()) != 1 {
            bn_free(exponent);
            return Err("Soter RSA key generation failed".to_string());
        }
        bn_free(exponent);

        let mut encoded: *mut u8 = std::ptr::null_mut();
        let encoded_len = i2d(rsa, &mut encoded);
        if encoded_len <= 0 || encoded.is_null() {
            return Err("Soter cannot encode the RSA public key".to_string());
        }
        let spki = std::slice::from_raw_parts(encoded, encoded_len as usize).to_vec();

        let cpu_id = cpu_id();
        let uid = libc::getuid();
        let json = format!(
            "{{\"pub_key\":\"{}\",\"counter\":1,\"cpu_id\":\"{cpu_id}\",\"uid\":{uid}}}",
            base64(&spki)
        )
        .into_bytes();
        let identity = Identity {
            json_signature: Vec::new(),
            json,
            rsa,
            sign,
            sha256,
        };
        let json_signature = identity.sign(&identity.json)?;
        Ok(Identity {
            json_signature,
            ..identity
        })
    }
}

fn cpu_id() -> String {
    let mut raw = [0u8; 8];
    let file = std::fs::File::open("/proc/sys/kernel/random/boot_id");
    if let Ok(mut file) = file {
        use std::io::Read;
        let _ = file.read(&mut raw);
    }
    raw.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut index = 0;
    while index + 3 <= bytes.len() {
        let block = u32::from_be_bytes([0, bytes[index], bytes[index + 1], bytes[index + 2]]);
        out.push(TABLE[((block >> 18) & 63) as usize] as char);
        out.push(TABLE[((block >> 12) & 63) as usize] as char);
        out.push(TABLE[((block >> 6) & 63) as usize] as char);
        out.push(TABLE[(block & 63) as usize] as char);
        index += 3;
    }
    if index < bytes.len() {
        let remain = bytes.len() - index;
        let mut block = [0u8; 3];
        block[..remain].copy_from_slice(&bytes[index..]);
        let value = u32::from_be_bytes([0, block[0], block[1], block[2]]);
        out.push(TABLE[((value >> 18) & 63) as usize] as char);
        out.push(TABLE[((value >> 12) & 63) as usize] as char);
        if remain == 2 {
            out.push(TABLE[((value >> 6) & 63) as usize] as char);
            out.push('=');
        } else {
            out.push('=');
            out.push('=');
        }
    }
    out
}

unsafe fn sym<T>(lib: *mut c_void, name: &CStr) -> Result<T, String> {
    let pointer = libc::dlsym(lib, name.as_ptr());
    if pointer.is_null() {
        return Err(format!(
            "Soter libcrypto is missing {}",
            name.to_string_lossy()
        ));
    }
    Ok(std::mem::transmute_copy(&pointer))
}
