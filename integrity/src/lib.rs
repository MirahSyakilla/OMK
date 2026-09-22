mod payload;
mod soter;

use payload::{load_from_disk, IntegrityPayload};

unsafe extern "C" {
    fn omk_zygisk_module_entry(table: *mut core::ffi::c_void, env: *mut core::ffi::c_void);
    fn omk_zygisk_companion_entry(client: i32);
}

/// Forwards Zygisk's module entry into the C++ implementation.
///
/// # Safety
/// `table` and `env` must be the pointers Zygisk passes to `zygisk_module_entry`.
#[no_mangle]
pub unsafe extern "C" fn zygisk_module_entry(
    table: *mut core::ffi::c_void,
    env: *mut core::ffi::c_void,
) {
    omk_zygisk_module_entry(table, env);
}

#[no_mangle]
pub extern "C" fn zygisk_companion_entry(client: i32) {
    unsafe { omk_zygisk_companion_entry(client) };
}

unsafe fn write_all(fd: i32, buf: &[u8]) -> bool {
    let mut written = 0usize;
    while written < buf.len() {
        let n = libc::write(fd, buf.as_ptr().add(written).cast(), buf.len() - written);
        if n <= 0 {
            return false;
        }
        written += n as usize;
    }
    true
}

unsafe fn read_all(fd: i32, buf: &mut [u8]) -> bool {
    let mut read = 0usize;
    while read < buf.len() {
        let n = libc::read(fd, buf.as_mut_ptr().add(read).cast(), buf.len() - read);
        if n <= 0 {
            return false;
        }
        read += n as usize;
    }
    true
}

fn payload_bytes(payload: &IntegrityPayload) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(
            (payload as *const IntegrityPayload).cast::<u8>(),
            std::mem::size_of::<IntegrityPayload>(),
        )
    }
}

#[no_mangle]
pub extern "C" fn omk_integrity_companion(fd: i32) -> i32 {
    let payload = load_from_disk();
    if unsafe { write_all(fd, payload_bytes(&payload)) } {
        0
    } else {
        -1
    }
}

/// Reads one payload from `fd` into `out`.
///
/// # Safety
/// `out` must be non-null and point to a writable `IntegrityPayload`.
#[no_mangle]
pub unsafe extern "C" fn omk_integrity_recv(fd: i32, out: *mut IntegrityPayload) -> i32 {
    if out.is_null() {
        return -1;
    }
    let buf =
        std::slice::from_raw_parts_mut(out.cast::<u8>(), std::mem::size_of::<IntegrityPayload>());
    if read_all(fd, buf) {
        0
    } else {
        -1
    }
}
