use std::sync::OnceLock;

use super::{BAD_VALUE, DESCRIPTOR, MAX_REQUEST_BYTES, UNKNOWN_TRANSACTION};

const SIGNATURE: [u8; 256] = [0; 256];
const EXPORT_JSON: &str = concat!(
    "{\"pub_key\":\"",
    "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAw8gEMK6J6jBvJr1b9K8j",
    "o4jMHF5D4BoHYXTsRov+v+clqEwXntTeXrOcQeuQX9Fys5S3Jmbs6safW1vmbJps",
    "k8Qe7wbi9p1v9uh3JzmF3j2Mw+tXtGI9h/1Vm1n6T3GrQJQ+tvuQ+vN8n6kMYl64",
    "J7CuyYw6P5vl6Z4WlfhdY5oJc0Q9T6xVwK6bg3DOjFEq5k1DTXJZuzqjONyYCuuP",
    "v7TTuLT8yT0+9m+CF7i65DKQJE3Ak0dCj0Ar1sIH7yLPlvWv85ExKYOvCXLdB6t8",
    "eWg0/eeoPHDLLv11Oyq9JR0gDk0iHT5SWG2FHKY5xIb3C2we8O7CVOaPwIDAQAB",
    "\",\"counter\":0,\"cpu_id\":\"0000000000000000\",\"uid\":0}"
);

fn export_blob() -> &'static [u8] {
    static BLOB: OnceLock<Vec<u8>> = OnceLock::new();
    BLOB.get_or_init(|| {
        let mut bytes = Vec::with_capacity(4 + EXPORT_JSON.len() + SIGNATURE.len());
        bytes.extend_from_slice(&(EXPORT_JSON.len() as u32).to_le_bytes());
        bytes.extend_from_slice(EXPORT_JSON.as_bytes());
        bytes.extend_from_slice(&SIGNATURE);
        bytes
    })
}

fn utf16le_contains(bytes: &[u8], ascii: &[u8]) -> bool {
    let width = ascii.len().saturating_mul(2);
    if width == 0 || bytes.len() < width {
        return false;
    }
    bytes.windows(width).any(|token| {
        ascii
            .iter()
            .enumerate()
            .all(|(index, byte)| token[index * 2] == *byte && token[index * 2 + 1] == 0)
    })
}

pub(super) fn valid_request(code: u32, bytes: &[u8]) -> bool {
    if !(1..=13).contains(&code) || bytes.len() > MAX_REQUEST_BYTES {
        return false;
    }
    utf16le_contains(bytes, DESCRIPTOR.to_bytes())
}

pub(super) trait Writer {
    fn int32(&mut self, value: i32) -> Result<(), i32>;
    fn int64(&mut self, value: i64) -> Result<(), i32>;
    fn bytes(&mut self, value: &[u8]) -> Result<(), i32>;
    fn string(&mut self, value: &str) -> Result<(), i32>;
}

pub(super) fn write_reply(code: u32, output: &mut impl Writer) -> Result<(), i32> {
    if !(1..=13).contains(&code) {
        return Err(UNKNOWN_TRANSACTION);
    }
    output.int32(0)?;
    match code {
        1 | 4 | 5 | 7 => output.int32(0),
        3 | 8 | 12 => output.int32(1),
        2 | 6 | 10 | 11 => {
            let bytes = match code {
                2 | 6 => export_blob(),
                10 => &SIGNATURE,
                11 => return Err(BAD_VALUE),
                _ => return Err(BAD_VALUE),
            };
            output.int32(1)?;
            output.int32(0)?;
            output.bytes(bytes)?;
            output.int32(bytes.len() as i32)
        }
        9 => {
            output.int32(1)?;
            output.int64(1)?;
            output.int32(0)
        }
        13 => {
            output.int32(1)?;
            output.int32(0)?;
            output.string("optical")
        }
        _ => Err(UNKNOWN_TRANSACTION),
    }
}
