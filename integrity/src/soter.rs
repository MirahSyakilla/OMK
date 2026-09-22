//! Simulated Tencent Soter replies. Not a hardware key or a payment fix.
//!
//! Reply values follow ajfkdk/D-soter (Apache-2.0), commit
//! 6148e02ea5977cb95b5a162a405fc915e39c01db, module/jni/dsoter.cpp.
//! The ioctl hook is installed by the integrity Zygisk module.

use std::{
    ffi::{c_char, c_void, CStr},
    mem::{align_of, size_of},
    panic::{catch_unwind, AssertUnwindSafe},
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering},
        OnceLock,
    },
};

mod wire;

pub(crate) const PACKAGE: &str = "com.tencent.soter.soterserver";
const DESCRIPTOR: &CStr = c"com.tencent.soter.soterserver.ISoterService";
const MAX_READ_BYTES: usize = 1024 * 1024;
const MAX_REQUEST_BYTES: usize = MAX_READ_BYTES;
const BINDER_WRITE_READ: i32 = 0xc030_6201u32 as i32;
const BR_TRANSACTION: u32 = 0x8040_7202;
const BR_TRANSACTION_SEC_CTX: u32 = 0x8048_7202;
const BINDER_TYPE_BINDER: u32 = 0x7362_2a85;
const BAD_VALUE: i32 = -libc::EINVAL;
const UNKNOWN_TRANSACTION: i32 = -libc::EBADMSG;

#[repr(C)]
#[derive(Clone, Copy)]
struct Transaction {
    target: u64,
    cookie: u64,
    code: u32,
    flags: u32,
    sender_pid: i32,
    sender_euid: u32,
    data_size: u64,
    offsets_size: u64,
    buffer: u64,
    offsets: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct WriteRead {
    write_size: u64,
    write_consumed: u64,
    write_buffer: u64,
    read_size: u64,
    read_consumed: u64,
    read_buffer: u64,
}

#[derive(Clone, Copy)]
struct Target {
    ptr: u64,
    cookie: u64,
}

type Ioctl = unsafe extern "C" fn(i32, i32, *mut c_void) -> i32;
type OnCreate = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type OnDestroy = unsafe extern "C" fn(*mut c_void);
type OnTransact = unsafe extern "C" fn(*mut c_void, u32, *const c_void, *mut c_void) -> i32;
type ClassDefine =
    unsafe extern "C" fn(*const c_char, OnCreate, OnDestroy, OnTransact) -> *mut c_void;
type DisableInterfaceHeader = unsafe extern "C" fn(*mut c_void);
type BinderNew = unsafe extern "C" fn(*const c_void, *mut c_void) -> *mut c_void;
type BinderRef = unsafe extern "C" fn(*mut c_void);
type ParcelCreate = unsafe extern "C" fn() -> *mut c_void;
type ParcelDelete = unsafe extern "C" fn(*mut c_void);
type WriteBinder = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32;
type ParcelSize = unsafe extern "C" fn(*const c_void) -> i32;
type ViewPlatform = unsafe extern "C" fn(*const c_void) -> *const c_void;
type PlatformData = unsafe extern "C" fn(*const c_void) -> *const u8;
type PlatformSize = unsafe extern "C" fn(*const c_void) -> usize;
type WriteInt32 = unsafe extern "C" fn(*mut c_void, i32) -> i32;
type WriteInt64 = unsafe extern "C" fn(*mut c_void, i64) -> i32;
type WriteBytes = unsafe extern "C" fn(*mut c_void, *const i8, i32) -> i32;
type WriteString = unsafe extern "C" fn(*mut c_void, *const c_char, i32) -> i32;

#[repr(C)]
struct LegacyParcel {
    binder: *const c_void,
    parcel: *const c_void,
    owns_parcel: u8,
}

struct NativeApi {
    parcel_size: ParcelSize,
    view_platform: Option<ViewPlatform>,
    legacy_layout: bool,
    platform_data: PlatformData,
    platform_size: PlatformSize,
    platform_objects: PlatformSize,
    write_i32: WriteInt32,
    write_i64: WriteInt64,
    write_bytes: WriteBytes,
    write_string: WriteString,
}

struct NativeStub {
    api: NativeApi,
    binder: usize,
    target: Target,
}

static NATIVE: OnceLock<Result<NativeStub, String>> = OnceLock::new();
static ORIGINAL_IOCTL: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static ACTIVE: AtomicBool = AtomicBool::new(false);
static MATCHED_CODES: AtomicU32 = AtomicU32::new(0);
static REPLIED_CODES: AtomicU32 = AtomicU32::new(0);
static FAILED_CODES: AtomicU32 = AtomicU32::new(0);

unsafe extern "C" {
    fn __android_log_write(prio: i32, tag: *const c_char, text: *const c_char) -> i32;
}

fn log(message: &str) {
    let Ok(message) = std::ffi::CString::new(message) else {
        return;
    };
    unsafe {
        __android_log_write(4, c"OMK-Integrity".as_ptr(), message.as_ptr());
    }
}

fn log_code_once(codes: &AtomicU32, code: u32, event: &str) {
    if (1..=13).contains(&code) && codes.fetch_or(1 << code, Ordering::Relaxed) & (1 << code) == 0 {
        log(&format!("{event}: code={code}"));
    }
}

fn is_soter_process(name: Option<&str>, dir: Option<&str>) -> bool {
    if name != Some(PACKAGE) {
        return false;
    }
    let Some(dir) = dir.filter(|dir| !dir.is_empty()) else {
        return true;
    };
    let suffix = format!("/{PACKAGE}");
    let Some(parent) = dir.strip_suffix(&suffix) else {
        return false;
    };
    parent == "/data/data"
        || parent.starts_with("/data/user/")
        || parent.starts_with("/data/user_de/")
}

/// # Safety
/// `name` and `dir` are NUL-terminated process strings. Either may be null.
#[no_mangle]
pub unsafe extern "C" fn omk_soter_is_target(name: *const c_char, dir: *const c_char) -> i32 {
    let name = if name.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(name) }.to_string_lossy())
    };
    let dir = if dir.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(dir) }.to_string_lossy())
    };
    i32::from(is_soter_process(
        name.as_deref(),
        dir.as_deref().filter(|dir| !dir.is_empty()),
    ))
}

/// # Safety
/// `original` must be the process `ioctl` Zygisk saved, or null.
#[no_mangle]
pub unsafe extern "C" fn omk_soter_set_ioctl(original: *mut c_void) {
    ORIGINAL_IOCTL.store(original, Ordering::Release);
}

/// Builds the simulated binder after the app UID is assigned.
#[no_mangle]
pub extern "C" fn omk_soter_activate() {
    if size_of::<usize>() != 8 {
        log("Soter requires a 64-bit process");
        return;
    }
    let ndk = match library(c"libbinder_ndk.so") {
        Ok(handle) => handle,
        Err(error) => {
            log(&error);
            return;
        }
    };
    let binder = match library(c"libbinder.so") {
        Ok(handle) => handle,
        Err(error) => {
            log(&error);
            return;
        }
    };
    match NATIVE.get_or_init(|| load_native(ndk, binder)) {
        Ok(_) => {
            ACTIVE.store(true, Ordering::Release);
            log("Soter hook active; replies are simulated");
        }
        Err(error) => log(&format!("Soter native handler unavailable: {error}")),
    }
}

/// # Safety
/// Called as the `ioctl` PLT replacement. `argument` is the libc ioctl argument.
#[no_mangle]
pub unsafe extern "C" fn omk_soter_ioctl(fd: i32, request: i32, argument: *mut c_void) -> i32 {
    let original = ORIGINAL_IOCTL.load(Ordering::Acquire);
    if original.is_null() {
        unsafe { *libc::__errno() = libc::ENOSYS };
        return -1;
    }
    let original: Ioctl = unsafe { std::mem::transmute(original) };
    let result = unsafe { original(fd, request, argument) };
    let saved_errno = unsafe { *libc::__errno() };
    if result >= 0
        && request == BINDER_WRITE_READ
        && !argument.is_null()
        && ACTIVE.load(Ordering::Acquire)
    {
        let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
            inspect_read(argument.cast());
        }));
    }
    unsafe { *libc::__errno() = saved_errno };
    result
}

unsafe fn inspect_read(argument: *const WriteRead) {
    let Some(Ok(native)) = NATIVE.get() else {
        return;
    };
    let read = unsafe { ptr::read_unaligned(argument) };
    if read.read_consumed == 0
        || read.read_consumed > read.read_size
        || read.read_consumed > MAX_READ_BYTES as u64
        || !valid_pointer_range(read.read_buffer, read.read_consumed)
    {
        return;
    }
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(read.read_buffer as *mut u8, read.read_consumed as usize)
    };
    if !valid_read_commands(bytes) {
        return;
    }
    visit_transactions(bytes, |transaction| {
        if !candidate(transaction) {
            return;
        }
        let data = unsafe {
            std::slice::from_raw_parts(
                transaction.buffer as *const u8,
                transaction.data_size as usize,
            )
        };
        if retarget(transaction, data, native.target) {
            log_code_once(
                &MATCHED_CODES,
                transaction.code,
                "Soter request intercepted",
            );
        }
    });
}

fn valid_pointer_range(pointer: u64, size: u64) -> bool {
    pointer != 0
        && size <= isize::MAX as u64
        && pointer
            .checked_add(size)
            .is_some_and(|end| end <= usize::MAX as u64)
}

fn candidate(transaction: &Transaction) -> bool {
    // Code 11 is getDeviceId. The published simulator id is a known marker, so
    // the real service answers that call.
    (1..=10).contains(&transaction.code) || transaction.code == 12 || transaction.code == 13
        && transaction.target != 0
        && transaction.data_size <= MAX_REQUEST_BYTES as u64
        && valid_pointer_range(transaction.buffer, transaction.data_size)
}

fn retarget(transaction: &mut Transaction, data: &[u8], target: Target) -> bool {
    if candidate(transaction)
        && transaction.data_size as usize == data.len()
        && wire::valid_request(transaction.code, data)
    {
        transaction.target = target.ptr;
        transaction.cookie = target.cookie;
        return true;
    }
    false
}

fn valid_read_commands(bytes: &[u8]) -> bool {
    let mut position = 0usize;
    while position < bytes.len() {
        let Some(header) = bytes.get(position..position + 4) else {
            return false;
        };
        let command = u32::from_ne_bytes(header.try_into().expect("four-byte command"));
        let size = ((command >> 16) & 0x3fff) as usize;
        position += 4;
        if bytes.len() - position < size {
            return false;
        }
        position += size;
    }
    true
}

fn visit_transactions(bytes: &mut [u8], mut visit: impl FnMut(&mut Transaction)) {
    let mut position = 0usize;
    while position + 4 <= bytes.len() {
        let command = u32::from_ne_bytes(bytes[position..position + 4].try_into().unwrap());
        position += 4;
        let size = ((command >> 16) & 0x3fff) as usize;
        let Some(payload) = bytes.get_mut(position..position + size) else {
            return;
        };
        if matches!(command, BR_TRANSACTION | BR_TRANSACTION_SEC_CTX)
            && payload.len() >= size_of::<Transaction>()
        {
            let mut transaction =
                unsafe { ptr::read_unaligned(payload.as_ptr().cast::<Transaction>()) };
            visit(&mut transaction);
            unsafe {
                ptr::write_unaligned(payload.as_mut_ptr().cast::<Transaction>(), transaction);
            }
        }
        position += size;
    }
}

fn symbol<T: Copy>(library: usize, name: &CStr) -> Result<T, String> {
    let symbol = unsafe { libc::dlsym(library as *mut c_void, name.as_ptr()) };
    if symbol.is_null() || size_of::<T>() != size_of::<*mut c_void>() {
        return Err(format!(
            "Soter native API unavailable: {}",
            name.to_string_lossy()
        ));
    }
    Ok(unsafe { std::mem::transmute_copy(&symbol) })
}

fn library(name: &CStr) -> Result<usize, String> {
    let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        let error = unsafe { libc::dlerror() };
        let detail = if error.is_null() {
            "unknown linker error".into()
        } else {
            unsafe { CStr::from_ptr(error) }.to_string_lossy()
        };
        return Err(format!(
            "Soter cannot load {}: {detail}",
            name.to_string_lossy()
        ));
    }
    Ok(handle as usize)
}

fn load_native(ndk: usize, binder: usize) -> Result<NativeStub, String> {
    let view_platform = symbol(ndk, c"_Z26AParcel_viewPlatformParcelPK7AParcel").ok();
    let device_api_address =
        unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"android_get_device_api_level".as_ptr()) };
    if device_api_address.is_null() {
        return Err("Soter cannot identify the Android API level".to_string());
    }
    let device_api: unsafe extern "C" fn() -> i32 =
        unsafe { std::mem::transmute(device_api_address) };
    let sdk = unsafe { device_api() };
    if sdk < 31 || (view_platform.is_none() && !matches!(sdk, 31..=33)) {
        return Err("Soter cannot access this Android version's native Parcel".to_string());
    }
    let api = NativeApi {
        parcel_size: symbol(ndk, c"AParcel_getDataSize")?,
        view_platform,
        legacy_layout: matches!(sdk, 31..=33),
        platform_data: symbol(binder, c"_ZNK7android6Parcel4dataEv")?,
        platform_size: symbol(binder, c"_ZNK7android6Parcel8dataSizeEv")?,
        platform_objects: symbol(binder, c"_ZNK7android6Parcel12objectsCountEv")?,
        write_i32: symbol(ndk, c"AParcel_writeInt32")?,
        write_i64: symbol(ndk, c"AParcel_writeInt64")?,
        write_bytes: symbol(ndk, c"AParcel_writeByteArray")?,
        write_string: symbol(ndk, c"AParcel_writeString")?,
    };
    let define: ClassDefine = symbol(ndk, c"AIBinder_Class_define")?;
    let new: BinderNew = symbol(ndk, c"AIBinder_new")?;
    let dec_strong: BinderRef = symbol(ndk, c"AIBinder_decStrong")?;
    let create: ParcelCreate = symbol(ndk, c"AParcel_create")?;
    let delete: ParcelDelete = symbol(ndk, c"AParcel_delete")?;
    let write_binder: WriteBinder = symbol(ndk, c"AParcel_writeStrongBinder")?;
    let class = unsafe { define(DESCRIPTOR.as_ptr(), on_create, on_destroy, on_transact) };
    if class.is_null() {
        return Err("Soter cannot define native Binder class".to_string());
    }
    if sdk >= 33 {
        let disable_header: DisableInterfaceHeader =
            symbol(ndk, c"AIBinder_Class_disableInterfaceTokenHeader")?;
        unsafe { disable_header(class) };
    }
    let stub = unsafe { new(class, ptr::null_mut()) };
    if stub.is_null() {
        return Err("Soter cannot allocate native Binder".to_string());
    }
    let carrier = unsafe { create() };
    if carrier.is_null() {
        unsafe { dec_strong(stub) };
        return Err("Soter cannot allocate Binder carrier".to_string());
    }
    let target = (|| {
        let status = unsafe { write_binder(carrier, stub) };
        if status != 0 {
            return Err(format!("Soter Binder carrier write failed: {status}"));
        }
        let bytes = unsafe { api.bytes(carrier, ptr::null(), true, Some(1))? };
        parse_carrier(bytes).ok_or_else(|| "Soter native Binder carrier is unsupported".to_string())
    })();
    unsafe { delete(carrier) };
    match target {
        Ok(target) => Ok(NativeStub {
            api,
            binder: stub as usize,
            target,
        }),
        Err(error) => {
            unsafe { dec_strong(stub) };
            Err(error)
        }
    }
}

fn parse_carrier(bytes: &[u8]) -> Option<Target> {
    if bytes.len() != 28 || u32::from_ne_bytes(bytes[0..4].try_into().ok()?) != BINDER_TYPE_BINDER {
        return None;
    }
    let ptr = u64::from_ne_bytes(bytes[8..16].try_into().ok()?);
    let cookie = u64::from_ne_bytes(bytes[16..24].try_into().ok()?);
    (ptr != 0 && cookie != 0).then_some(Target { ptr, cookie })
}

impl NativeApi {
    unsafe fn bytes<'a>(
        &self,
        parcel: *const c_void,
        binder: *const c_void,
        owns: bool,
        objects: Option<usize>,
    ) -> Result<&'a [u8], String> {
        if parcel.is_null() {
            return Err("Soter received a null Parcel".to_string());
        }
        let platform = if let Some(view) = self.view_platform {
            unsafe { view(parcel) }
        } else if self.legacy_layout {
            let legacy = unsafe { &*parcel.cast::<LegacyParcel>() };
            if legacy.binder != binder || legacy.owns_parcel != u8::from(owns) {
                return Err("Soter received an incompatible legacy Parcel".to_string());
            }
            legacy.parcel
        } else {
            ptr::null()
        };
        if platform.is_null() || !(platform as usize).is_multiple_of(align_of::<usize>()) {
            return Err("Soter received an invalid platform Parcel".to_string());
        }
        let size = unsafe { (self.platform_size)(platform) };
        if size > MAX_REQUEST_BYTES
            || unsafe { (self.parcel_size)(parcel) } != size as i32
            || objects
                .is_some_and(|expected| unsafe { (self.platform_objects)(platform) != expected })
        {
            return Err("Soter received an unsupported Parcel size or object count".to_string());
        }
        if size == 0 {
            return Ok(&[]);
        }
        let data = unsafe { (self.platform_data)(platform) };
        if !valid_pointer_range(data as u64, size as u64) {
            return Err("Soter received an invalid Parcel buffer".to_string());
        }
        Ok(unsafe { std::slice::from_raw_parts(data, size) })
    }
}

unsafe extern "C" fn on_create(data: *mut c_void) -> *mut c_void {
    data
}
unsafe extern "C" fn on_destroy(_: *mut c_void) {}

unsafe extern "C" fn on_transact(
    binder: *mut c_void,
    code: u32,
    input: *const c_void,
    output: *mut c_void,
) -> i32 {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let Some(Ok(native)) = NATIVE.get() else {
            return UNKNOWN_TRANSACTION;
        };
        if binder as usize != native.binder || !(1..=13).contains(&code) {
            return UNKNOWN_TRANSACTION;
        }
        let Ok(bytes) = (unsafe { native.api.bytes(input, binder, false, None) }) else {
            return BAD_VALUE;
        };
        if !wire::valid_request(code, bytes) {
            return BAD_VALUE;
        }
        if output.is_null() {
            return 0;
        }
        let Ok(reply) = (unsafe { native.api.bytes(output, binder, false, Some(0)) }) else {
            return BAD_VALUE;
        };
        if !reply.is_empty() {
            return BAD_VALUE;
        }
        wire::write_reply(
            code,
            &mut NativeWriter {
                api: &native.api,
                output,
            },
        )
        .map_or_else(|status| status, |()| 0)
    }))
    .unwrap_or(-libc::EFAULT);
    if result == 0 {
        log_code_once(&REPLIED_CODES, code, "Soter simulated reply delivered");
    } else {
        log_code_once(
            &FAILED_CODES,
            code,
            &format!("Soter reply failed (status={result})"),
        );
    }
    result
}

struct NativeWriter<'a> {
    api: &'a NativeApi,
    output: *mut c_void,
}

impl wire::Writer for NativeWriter<'_> {
    fn int32(&mut self, value: i32) -> Result<(), i32> {
        status(unsafe { (self.api.write_i32)(self.output, value) })
    }
    fn int64(&mut self, value: i64) -> Result<(), i32> {
        status(unsafe { (self.api.write_i64)(self.output, value) })
    }
    fn bytes(&mut self, value: &[u8]) -> Result<(), i32> {
        status(unsafe {
            (self.api.write_bytes)(self.output, value.as_ptr().cast(), value.len() as i32)
        })
    }
    fn string(&mut self, value: &str) -> Result<(), i32> {
        status(unsafe {
            (self.api.write_string)(self.output, value.as_ptr().cast(), value.len() as i32)
        })
    }
}

fn status(value: i32) -> Result<(), i32> {
    if value == 0 {
        Ok(())
    } else {
        Err(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mem(Vec<i32>);

    impl wire::Writer for Mem {
        fn int32(&mut self, value: i32) -> Result<(), i32> {
            self.0.push(value);
            Ok(())
        }
        fn int64(&mut self, value: i64) -> Result<(), i32> {
            self.0.push(value as i32);
            self.0.push((value >> 32) as i32);
            Ok(())
        }
        fn bytes(&mut self, value: &[u8]) -> Result<(), i32> {
            self.0.push(value.len() as i32);
            Ok(())
        }
        fn string(&mut self, value: &str) -> Result<(), i32> {
            self.0.push(value.len() as i32);
            Ok(())
        }
    }

    #[test]
    fn process_match_is_exact_name_or_data_dir_suffix() {
        assert!(is_soter_process(Some(PACKAGE), None));
        assert!(is_soter_process(
            Some(PACKAGE),
            Some("/data/user/0/com.tencent.soter.soterserver")
        ));
        assert!(is_soter_process(
            Some(PACKAGE),
            Some("/data/data/com.tencent.soter.soterserver")
        ));
        assert!(!is_soter_process(
            Some("com.tencent.soter.soterserver:remote"),
            Some("/data/data/com.tencent.soter.soterserver")
        ));
        assert!(!is_soter_process(
            Some(PACKAGE),
            Some("/data/local/tmp/com.tencent.soter.soterserver")
        ));
    }

    #[test]
    fn descriptor_must_be_utf16_inside_the_request() {
        let ascii = DESCRIPTOR.to_bytes();
        let mut utf16 = Vec::new();
        for byte in ascii {
            utf16.push(*byte);
            utf16.push(0);
        }
        assert!(wire::valid_request(1, &utf16));
        assert!(!wire::valid_request(1, ascii));
        assert!(!wire::valid_request(14, &utf16));
    }

    #[test]
    fn every_supported_code_writes_a_reply() {
        for code in 1..=13 {
            let mut output = Mem(Vec::new());
            let result = wire::write_reply(code, &mut output);
            if code == 11 {
                assert!(result.is_err());
            } else {
                result.unwrap();
                assert_eq!(output.0.first().copied(), Some(0));
            }
        }
    }
}
