use super::{BAD_VALUE, DESCRIPTOR, MAX_REQUEST_BYTES, UNKNOWN_TRANSACTION};

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

pub(super) struct ReplyMaterial<'a> {
    pub export_json: &'a [u8],
    pub export_signature: &'a [u8],
    pub request_signature: &'a [u8],
}

fn export_blob(json: &[u8], signature: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(4 + json.len() + signature.len());
    bytes.extend_from_slice(&(json.len() as u32).to_le_bytes());
    bytes.extend_from_slice(json);
    bytes.extend_from_slice(signature);
    bytes
}

pub(super) fn write_reply(
    code: u32,
    output: &mut impl Writer,
    material: ReplyMaterial<'_>,
) -> Result<(), i32> {
    if !(1..=13).contains(&code) {
        return Err(UNKNOWN_TRANSACTION);
    }
    output.int32(0)?;
    match code {
        1 | 4 | 5 | 7 => output.int32(0),
        3 | 8 | 12 => output.int32(1),
        2 | 6 | 10 | 11 => {
            let owned = match code {
                2 | 6 => export_blob(material.export_json, material.export_signature),
                10 => material.request_signature.to_vec(),
                11 => return Err(BAD_VALUE),
                _ => return Err(BAD_VALUE),
            };
            let bytes = owned.as_slice();
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
