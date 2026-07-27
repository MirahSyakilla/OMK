use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::c_void;
use std::mem::size_of;
use std::ops::{Deref, DerefMut};
use std::os::raw::c_int;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use log::{debug, warn};
use nix::unistd::Pid;

use super::binder::{
    _ioc_dir, _ioc_nr, _ioc_size, binder_node_debug_info, binder_ptr_cookie,
    binder_transaction_data, binder_transaction_data_secctx, binder_transaction_data_sg,
    binder_version, binder_write_read, format_target, log_write_transaction,
    preview_transaction_parcel, BC_ACQUIRE_DONE_CMD, BC_FREE_BUFFER_NR, BC_REPLY_NR,
    BC_REPLY_SG_NR, BC_TRANSACTION_NR, BC_TRANSACTION_SG_NR, BINDER_GET_NODE_DEBUG_INFO,
    BINDER_VERSION, BINDER_WRITE_READ, BR_ACQUIRE_NR, BR_DEAD_REPLY_NR, BR_FAILED_REPLY_NR,
    BR_FROZEN_REPLY_NR, BR_ONEWAY_SPAM_SUSPECT_NR, BR_REPLY_NR, BR_TRANSACTION_COMPLETE_CMD,
    BR_TRANSACTION_NR, BR_TRANSACTION_PENDING_FROZEN_NR, TF_ONE_WAY,
};
#[cfg(test)]
use super::binder::{BC_FREE_BUFFER_CMD, BC_REPLY_CMD, BR_NOOP_CMD, BR_TRANSACTION_CMD};
use super::rewrite::{
    abort_bc_reply, cancel_operation_publication_acquire_pending, clear_binder_fd_thread_state,
    clear_outbound_reply_buffers, commit_bc_reply, finish_operation_publication_probe,
    handle_bc_reply, handle_br_transaction, lookup_synthetic_target,
    mark_operation_publication_acquire_committed, mark_operation_publication_acquire_pending,
    mark_operation_publication_completed, next_operation_publication_probe_deadline,
    operation_publication_acquire_is_pending, operation_publication_pending_acquire,
    push_pending_frame, retire_binder_connection_publications,
    retire_synthetic_operation_retirement, take_operation_publication_probe,
    OperationPublicationProbe,
};
#[cfg(test)]
use super::rewrite::{
    bind_operation_publication_connection, finish_local_operation_publication,
    register_operation_publication_for_test,
};
use super::{
    BinderFdToken, BinderStateKey, OLD_CLOSE, OLD_DUP, OLD_DUP2, OLD_DUP3, OLD_FCNTL,
    OLD_FDSAN_CLOSE, OLD_IOCTL,
};
use crate::hook::binder::{LocalBinderTarget, NativeBinderRetirement};

struct PendingTransactionCompletion {
    is_reply: bool,
    expects_reply: bool,
    operation_target: Option<NativeBinderRetirement>,
}

#[derive(Clone, Copy)]
struct PreparedBcReply {
    frame_id: Option<u64>,
    data_ptr: usize,
    transaction: binder_transaction_data,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SyncTransactionState {
    PendingCompletion,
    AwaitingReply,
}

thread_local! {
    static PENDING_TRANSACTION_COMPLETIONS: RefCell<HashMap<BinderStateKey, VecDeque<PendingTransactionCompletion>>> = RefCell::default();
    static SYNC_TRANSACTIONS: RefCell<HashMap<BinderStateKey, Vec<SyncTransactionState>>> = RefCell::default();
    static PREPARED_BC_REPLIES: RefCell<HashMap<BinderStateKey, VecDeque<PreparedBcReply>>> = RefCell::default();
    static OBSERVED_BINDER_FD_TOKENS: RefCell<HashMap<c_int, BinderFdToken>> = RefCell::default();
    static PENDING_IOCTL_COPYBACKS: RefCell<HashMap<BinderStateKey, PendingIoctlCopyback>> = RefCell::default();
}

struct PendingIoctlCopyback {
    arg: usize,
    write_buffer: libc::c_ulong,
    read_buffer: libc::c_ulong,
    write_size: usize,
    read_size: usize,
    read: PendingReadCopyback,
    output: binder_write_read,
    read_effects: PendingReadEffects,
    freed_inbound_shadows: Vec<(usize, usize)>,
    ret: c_int,
    errno: c_int,
}

enum PendingReadCopyback {
    None,
    Unread { address: usize, len: usize },
    Processed { address: usize, data: Vec<u8> },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InboundTransactionShadowState {
    Staged,
    Live,
    KernelFreedPendingAck,
}

struct InboundTransactionShadow {
    payload: TransactionPayloadShadow,
    state: InboundTransactionShadowState,
}

struct PendingReadEffects {
    connection: BinderStateKey,
    staged_inbound_shadows: Vec<usize>,
    operation_acquires: Vec<NativeBinderRetirement>,
}

static INBOUND_TRANSACTION_SHADOWS: LazyLock<
    Mutex<HashMap<(BinderStateKey, usize), InboundTransactionShadow>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn copy_from_process(address: usize, destination: &mut [u8]) -> bool {
    crate::sys::read_process_exact(Pid::this(), address, destination).is_ok()
}

fn copy_to_process(address: usize, source: &[u8]) -> bool {
    crate::sys::write_process_exact(Pid::this(), address, source).is_ok()
}

fn zeroed_buffer(size: usize) -> Result<Vec<u8>, c_int> {
    let mut buffer = Vec::new();
    buffer.try_reserve_exact(size).map_err(|_| libc::ENOMEM)?;
    buffer.resize(size, 0);
    Ok(buffer)
}

fn copy_process_buffer(address: usize, size: usize) -> Result<Vec<u8>, c_int> {
    let mut buffer = zeroed_buffer(size)?;
    copy_from_process(address, &mut buffer)
        .then_some(buffer)
        .ok_or(libc::EFAULT)
}

fn copy_process_c_string(address: usize) -> Option<String> {
    if address == 0 {
        return None;
    }
    let mut bytes = Vec::new();
    while bytes.len() < 4096 {
        let current = address.checked_add(bytes.len())?;
        let chunk_len = (4096 - bytes.len()).min(256).min(4096 - (current & 4095));
        let mut chunk = [0; 256];
        if !copy_from_process(current, &mut chunk[..chunk_len]) {
            return None;
        }
        if let Some(end) = chunk[..chunk_len].iter().position(|byte| *byte == 0) {
            bytes.extend_from_slice(&chunk[..end]);
            return Some(String::from_utf8_lossy(&bytes).into_owned());
        }
        bytes.extend_from_slice(&chunk[..chunk_len]);
    }
    None
}

fn copy_process_value(value: *const binder_write_read) -> Option<binder_write_read> {
    if value.is_null() {
        return None;
    }
    let mut copy: binder_write_read = unsafe { std::mem::zeroed() };
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(
            (&mut copy as *mut binder_write_read).cast::<u8>(),
            size_of::<binder_write_read>(),
        )
    };
    copy_from_process(value as usize, bytes).then_some(copy)
}

fn write_process_value<T: Copy>(destination: *mut T, value: &T) -> bool {
    let bytes =
        unsafe { std::slice::from_raw_parts((value as *const T).cast::<u8>(), size_of::<T>()) };
    copy_to_process(destination as usize, bytes)
}

fn retry_pending_ioctl_copyback(
    fd: c_int,
    connection: BinderStateKey,
    arg: *mut c_void,
) -> Option<c_int> {
    let mut pending = PENDING_IOCTL_COPYBACKS.with(|slot| slot.borrow_mut().remove(&connection))?;
    if pending.arg != arg as usize {
        PENDING_IOCTL_COPYBACKS.with(|slot| {
            slot.borrow_mut().insert(connection, pending);
        });
        unsafe { *libc::__errno() = libc::EBUSY };
        return Some(-1);
    }

    let current = copy_process_value(arg.cast::<binder_write_read>());
    let identity_matches = current.is_some_and(|current| {
        current.write_buffer == pending.write_buffer
            && current.read_buffer == pending.read_buffer
            && current.write_size == pending.write_size
            && current.read_size == pending.read_size
    });
    if !identity_matches {
        PENDING_IOCTL_COPYBACKS.with(|slot| {
            slot.borrow_mut().insert(connection, pending);
        });
        unsafe { *libc::__errno() = libc::EFAULT };
        return Some(-1);
    }

    let (read, read_ok, effects) =
        match std::mem::replace(&mut pending.read, PendingReadCopyback::None) {
            PendingReadCopyback::None => (PendingReadCopyback::None, true, None),
            PendingReadCopyback::Processed { address, data } => {
                let copied = copy_to_process(address, &data);
                (
                    PendingReadCopyback::Processed { address, data },
                    copied,
                    None,
                )
            }
            PendingReadCopyback::Unread { address, len } => match copy_process_buffer(address, len)
            {
                Ok(mut data) => {
                    let effects = unsafe { parse_read_buffer(fd, &mut data) };
                    let copied = copy_to_process(address, &data);
                    (
                        PendingReadCopyback::Processed { address, data },
                        copied,
                        Some(effects),
                    )
                }
                Err(_) => (PendingReadCopyback::Unread { address, len }, false, None),
            },
        };
    pending.read = read;
    if let Some(effects) = effects {
        pending.read_effects = effects;
    }
    let counters_ok =
        read_ok && write_process_value(arg.cast::<binder_write_read>(), &pending.output);
    if !counters_ok {
        PENDING_IOCTL_COPYBACKS.with(|slot| {
            slot.borrow_mut().insert(connection, pending);
        });
        unsafe { *libc::__errno() = libc::EFAULT };
        return Some(-1);
    }

    pending.read_effects.commit();
    unsafe { *libc::__errno() = pending.errno };
    Some(pending.ret)
}

struct TransactionPayloadShadow {
    data: Vec<u8>,
    offsets: Vec<usize>,
    original_buffer: libc::c_ulong,
    original_offsets: libc::c_ulong,
}

impl TransactionPayloadShadow {
    unsafe fn read(tr: &binder_transaction_data) -> Option<Self> {
        let original_buffer = tr.data.ptr.buffer;
        let original_offsets = tr.data.ptr.offsets;
        let data = copy_process_buffer(original_buffer as usize, tr.data_size).ok()?;
        if !tr.offsets_size.is_multiple_of(size_of::<usize>()) {
            return None;
        }
        let mut offsets = Vec::new();
        let count = tr.offsets_size / size_of::<usize>();
        offsets.try_reserve_exact(count).ok()?;
        offsets.resize(count, 0);
        let offset_bytes =
            std::slice::from_raw_parts_mut(offsets.as_mut_ptr().cast::<u8>(), tr.offsets_size);
        if !copy_from_process(original_offsets as usize, offset_bytes) {
            return None;
        }
        Some(Self {
            data,
            offsets,
            original_buffer,
            original_offsets,
        })
    }

    unsafe fn install(&mut self, tr: &mut binder_transaction_data) {
        if tr.data_size != 0 {
            tr.data.ptr.buffer = self.data.as_mut_ptr() as libc::c_ulong;
        }
        if tr.offsets_size != 0 {
            tr.data.ptr.offsets = self.offsets.as_mut_ptr() as libc::c_ulong;
        }
    }

    unsafe fn restore(&self, tr: &mut binder_transaction_data) {
        if tr.data_size != 0 && tr.data.ptr.buffer == self.data.as_ptr() as libc::c_ulong {
            tr.data.ptr.buffer = self.original_buffer;
        }
        if tr.offsets_size != 0 && tr.data.ptr.offsets == self.offsets.as_ptr() as libc::c_ulong {
            tr.data.ptr.offsets = self.original_offsets;
        }
    }

    fn data_ptr(&self) -> usize {
        self.data.as_ptr() as usize
    }
}

fn retain_inbound_transaction_shadow(
    connection: BinderStateKey,
    shadow: TransactionPayloadShadow,
) -> usize {
    debug_assert!(!shadow.data.is_empty());
    let shadow_buffer = shadow.data_ptr();
    let key = (connection, shadow_buffer);
    let previous = INBOUND_TRANSACTION_SHADOWS
        .lock()
        .expect("inbound transaction shadow map poisoned")
        .insert(
            key,
            InboundTransactionShadow {
                payload: shadow,
                state: InboundTransactionShadowState::Staged,
            },
        );
    debug_assert!(previous.is_none());
    shadow_buffer
}

fn publish_inbound_transaction_shadows(connection: BinderStateKey, shadows: &[usize]) {
    let mut entries = INBOUND_TRANSACTION_SHADOWS
        .lock()
        .expect("inbound transaction shadow map poisoned");
    for shadow_buffer in shadows {
        if let Some(shadow) = entries.get_mut(&(connection, *shadow_buffer)) {
            if shadow.state == InboundTransactionShadowState::Staged {
                shadow.state = InboundTransactionShadowState::Live;
            }
        }
    }
}

fn inbound_transaction_original_buffer(
    connection: BinderStateKey,
    shadow_buffer: usize,
) -> Option<libc::c_ulong> {
    INBOUND_TRANSACTION_SHADOWS
        .lock()
        .expect("inbound transaction shadow map poisoned")
        .get(&(connection, shadow_buffer))
        .filter(|shadow| shadow.state == InboundTransactionShadowState::Live)
        .map(|shadow| shadow.payload.original_buffer)
}

#[cfg(test)]
fn clear_inbound_transaction_shadows(connection: BinderStateKey) {
    INBOUND_TRANSACTION_SHADOWS
        .lock()
        .expect("inbound transaction shadow map poisoned")
        .retain(|(shadow_connection, _), _| *shadow_connection != connection);
}

impl PendingReadEffects {
    fn new(connection: BinderStateKey) -> Self {
        Self {
            connection,
            staged_inbound_shadows: Vec::new(),
            operation_acquires: Vec::new(),
        }
    }

    fn commit(&mut self) {
        publish_inbound_transaction_shadows(self.connection, &self.staged_inbound_shadows);
        self.staged_inbound_shadows.clear();
        self.operation_acquires.clear();
    }
}

impl Drop for PendingReadEffects {
    fn drop(&mut self) {
        {
            let mut entries = INBOUND_TRANSACTION_SHADOWS
                .lock()
                .expect("inbound transaction shadow map poisoned");
            for shadow_buffer in self.staged_inbound_shadows.drain(..) {
                let key = (self.connection, shadow_buffer);
                if entries
                    .get(&key)
                    .is_some_and(|shadow| shadow.state == InboundTransactionShadowState::Staged)
                {
                    entries.remove(&key);
                }
            }
        }

        let canceled_acquire = !self.operation_acquires.is_empty();
        for retirement in self.operation_acquires.drain(..) {
            cancel_operation_publication_acquire_pending(retirement);
        }
        if canceled_acquire && binder_connection_cleanup_ready(self.connection) {
            retire_binder_connection_publications(self.connection);
        }
    }
}

impl Drop for PendingIoctlCopyback {
    fn drop(&mut self) {
        complete_inbound_free_buffers(
            self.read_effects.connection,
            &self.freed_inbound_shadows,
            self.output.write_consumed,
        );
    }
}

#[derive(Default)]
struct BinderFdLifecycleState {
    generation: u64,
    in_flight: usize,
    retired: bool,
    protocol_error: bool,
}

struct BinderFdLifecycle {
    connection: BinderStateKey,
    state: Mutex<BinderFdLifecycleState>,
    // Closing a Binder dup flushes and wakes every looper, so keep one pin per connection.
    pinned_fd: OnceLock<c_int>,
}

impl Drop for BinderFdLifecycle {
    fn drop(&mut self) {
        let error = unsafe { *libc::__errno() };
        if let Some(&fd) = self.pinned_fd.get() {
            unsafe {
                libc::syscall(libc::SYS_close, fd);
            }
        }
        unsafe {
            *libc::__errno() = error;
        }
    }
}

#[derive(Default)]
struct BinderFdRegistry {
    by_fd: HashMap<c_int, Arc<BinderFdLifecycle>>,
    by_connection: HashMap<BinderStateKey, Arc<BinderFdLifecycle>>,
}

static NEXT_BINDER_CONNECTION: AtomicU64 = AtomicU64::new(1);
static BINDER_FD_REGISTRY: LazyLock<Mutex<BinderFdRegistry>> =
    LazyLock::new(|| Mutex::new(BinderFdRegistry::default()));
static OPERATION_PUBLICATION_WAKE: LazyLock<(Mutex<()>, Condvar)> =
    LazyLock::new(|| (Mutex::new(()), Condvar::new()));
static OPERATION_PUBLICATION_WORKER: OnceLock<Result<(), String>> = OnceLock::new();

pub(super) fn wake_operation_publication_worker() {
    let (lock, wake) = &*OPERATION_PUBLICATION_WAKE;
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    wake.notify_one();
}

pub(super) fn start_operation_publication_worker() -> Result<(), String> {
    match OPERATION_PUBLICATION_WORKER.get_or_init(|| {
        std::thread::Builder::new()
            .name("omk-binder-publication".to_string())
            .spawn(operation_publication_worker)
            .map(|_| ())
            .map_err(|error| format!("failed to start operation publication worker: {error}"))
    }) {
        Ok(()) => Ok(()),
        Err(error) => Err(error.clone()),
    }
}

fn operation_publication_worker() {
    loop {
        wait_for_operation_publication_deadline();

        let old_ioctl = OLD_IOCTL.load(Ordering::Acquire);
        if old_ioctl.is_null() {
            warn!("event=synthetic operation publication worker stopped because original ioctl is unavailable");
            return;
        }
        let old_ioctl_fn: unsafe extern "C" fn(c_int, c_int, *mut c_void) -> c_int =
            unsafe { std::mem::transmute(old_ioctl) };
        unsafe { flush_native_binder_lifecycle(old_ioctl_fn) };
    }
}

pub(super) fn wait_for_operation_publication_deadline() {
    let (lock, wake) = &*OPERATION_PUBLICATION_WAKE;
    let mut guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    loop {
        match next_operation_publication_probe_deadline() {
            None => {
                guard = wake
                    .wait(guard)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            Some(deadline) => {
                let now = Instant::now();
                if deadline <= now {
                    return;
                }
                let (next_guard, _) = wake
                    .wait_timeout(guard, deadline.saturating_duration_since(now))
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                guard = next_guard;
            }
        }
    }
}

struct SignalMaskGuard {
    previous: libc::sigset_t,
    active: bool,
}

impl SignalMaskGuard {
    fn block() -> Self {
        unsafe {
            let mut blocked = std::mem::zeroed();
            let mut previous = std::mem::zeroed();
            let active = libc::sigfillset(&mut blocked) == 0
                && libc::sigdelset(&mut blocked, libc::SIGKILL) == 0
                && libc::sigdelset(&mut blocked, libc::SIGSTOP) == 0
                && libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut previous) == 0;
            Self { previous, active }
        }
    }
}

impl Drop for SignalMaskGuard {
    fn drop(&mut self) {
        if self.active {
            unsafe {
                libc::pthread_sigmask(libc::SIG_SETMASK, &self.previous, std::ptr::null_mut());
            }
        }
    }
}

struct BinderFdRegistryGuard {
    registry: MutexGuard<'static, BinderFdRegistry>,
    _signals: SignalMaskGuard,
}

impl BinderFdRegistryGuard {
    fn unlock(self) -> SignalMaskGuard {
        let Self { registry, _signals } = self;
        drop(registry);
        _signals
    }
}

impl Deref for BinderFdRegistryGuard {
    type Target = BinderFdRegistry;

    fn deref(&self) -> &Self::Target {
        &self.registry
    }
}

impl DerefMut for BinderFdRegistryGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.registry
    }
}

fn binder_fd_registry() -> BinderFdRegistryGuard {
    let signals = SignalMaskGuard::block();
    let registry = BINDER_FD_REGISTRY
        .lock()
        .expect("binder fd registry poisoned");
    BinderFdRegistryGuard {
        registry,
        _signals: signals,
    }
}

fn ensure_binder_fd_lifecycle(
    registry: &mut BinderFdRegistry,
    fd: c_int,
) -> Arc<BinderFdLifecycle> {
    if let Some(lifecycle) = registry.by_fd.get(&fd) {
        return lifecycle.clone();
    }
    let lifecycle = Arc::new(BinderFdLifecycle {
        connection: NEXT_BINDER_CONNECTION.fetch_add(1, Ordering::Relaxed),
        state: Mutex::new(BinderFdLifecycleState::default()),
        pinned_fd: OnceLock::new(),
    });
    registry
        .by_connection
        .insert(lifecycle.connection, lifecycle.clone());
    registry.by_fd.insert(fd, lifecycle.clone());
    lifecycle
}

#[cfg(test)]
fn binder_fd_lifecycle(fd: c_int) -> Arc<BinderFdLifecycle> {
    let mut registry = binder_fd_registry();
    ensure_binder_fd_lifecycle(&mut registry, fd)
}

#[cfg(test)]
fn existing_binder_fd_lifecycle(fd: c_int) -> Option<Arc<BinderFdLifecycle>> {
    binder_fd_registry().by_fd.get(&fd).cloned()
}

fn binder_fd_token(fd: c_int) -> BinderFdToken {
    let mut registry = binder_fd_registry();
    let lifecycle = ensure_binder_fd_lifecycle(&mut registry, fd);
    let generation = lifecycle
        .state
        .lock()
        .expect("binder fd lifecycle poisoned")
        .generation;
    BinderFdToken {
        fd,
        generation,
        connection: lifecycle.connection,
    }
}

#[cfg(test)]
fn binder_fd_token_is_current(token: BinderFdToken) -> bool {
    existing_binder_fd_lifecycle(token.fd).is_some_and(|lifecycle| {
        lifecycle.connection == token.connection
            && lifecycle
                .state
                .lock()
                .expect("binder fd lifecycle poisoned")
                .generation
                == token.generation
    })
}

fn observed_binder_fd_token(fd: c_int) -> BinderFdToken {
    OBSERVED_BINDER_FD_TOKENS
        .with(|observed| observed.borrow().get(&fd).copied())
        .unwrap_or_else(|| binder_fd_token(fd))
}

fn binder_state_key(fd: c_int) -> BinderStateKey {
    observed_binder_fd_token(fd).connection
}

fn reset_current_thread_binder_state(connection: BinderStateKey) {
    abort_prepared_bc_replies_for_connection(connection);
    clear_binder_fd_thread_state(connection);
    PENDING_IOCTL_COPYBACKS.with(|pending| {
        pending.borrow_mut().remove(&connection);
    });
    SYNC_TRANSACTIONS.with(|transactions| {
        transactions.borrow_mut().remove(&connection);
    });
    let completions = PENDING_TRANSACTION_COMPLETIONS
        .with(|pending| pending.borrow_mut().remove(&connection))
        .unwrap_or_default();
    for target in completions
        .into_iter()
        .filter_map(|completion| completion.operation_target)
    {
        retire_synthetic_operation_retirement(target);
    }
    debug!("event=binder cleared stale thread state for connection={connection}");
}

fn forget_current_thread_binder_fd(fd: c_int, retired: Option<BinderStateKey>) {
    if let Some(connection) = retired {
        reset_current_thread_binder_state(connection);
        if binder_connection_cleanup_ready(connection) {
            retire_binder_connection_publications(connection);
        }
    }
    OBSERVED_BINDER_FD_TOKENS.with(|observed| {
        observed.borrow_mut().remove(&fd);
    });
}

fn binder_connection_cleanup_ready(connection: BinderStateKey) -> bool {
    let registry = binder_fd_registry();
    let Some(lifecycle) = registry.by_connection.get(&connection) else {
        return true;
    };
    let state = lifecycle
        .state
        .lock()
        .expect("binder fd lifecycle poisoned");
    state.retired && state.in_flight == 0
}

fn cleanup_retired_current_thread_connections() {
    let mut connections = HashSet::new();
    PENDING_TRANSACTION_COMPLETIONS.with(|state| {
        connections.extend(state.borrow().keys().copied());
    });
    SYNC_TRANSACTIONS.with(|state| {
        connections.extend(state.borrow().keys().copied());
    });
    PREPARED_BC_REPLIES.with(|state| {
        connections.extend(state.borrow().keys().copied());
    });
    PENDING_IOCTL_COPYBACKS.with(|state| {
        connections.extend(state.borrow().keys().copied());
    });
    OBSERVED_BINDER_FD_TOKENS.with(|state| {
        connections.extend(state.borrow().values().map(|token| token.connection));
    });

    for connection in connections
        .into_iter()
        .filter(|connection| binder_connection_cleanup_ready(*connection))
    {
        reset_current_thread_binder_state(connection);
        OBSERVED_BINDER_FD_TOKENS.with(|observed| {
            observed
                .borrow_mut()
                .retain(|_, token| token.connection != connection);
        });
    }
}

fn synchronize_binder_fd_generation(fd: c_int) -> Result<BinderFdToken, BinderFdToken> {
    let token = binder_fd_token(fd);
    let previous = OBSERVED_BINDER_FD_TOKENS.with(|observed| observed.borrow().get(&fd).copied());
    if let Some(previous) = previous.filter(|previous| *previous != token) {
        if binder_connection_is_retired(previous) {
            reset_current_thread_binder_state(previous.connection);
        }
        OBSERVED_BINDER_FD_TOKENS.with(|observed| {
            observed.borrow_mut().insert(fd, token);
        });
        return Err(previous);
    }
    if previous.is_none() {
        OBSERVED_BINDER_FD_TOKENS.with(|observed| {
            observed.borrow_mut().insert(fd, token);
        });
    }
    Ok(token)
}

fn binder_connection_is_retired(token: BinderFdToken) -> bool {
    let registry = binder_fd_registry();
    let Some(lifecycle) = registry.by_connection.get(&token.connection) else {
        return true;
    };
    let state = lifecycle
        .state
        .lock()
        .expect("binder fd lifecycle poisoned");
    state.generation != token.generation || state.retired && state.in_flight == 0
}

fn retire_unaliased_lifecycle(
    registry: &mut BinderFdRegistry,
    lifecycle: &Arc<BinderFdLifecycle>,
) -> bool {
    if registry
        .by_fd
        .values()
        .any(|candidate| Arc::ptr_eq(candidate, lifecycle))
    {
        return false;
    }
    let mut state = lifecycle
        .state
        .lock()
        .expect("binder fd lifecycle poisoned");
    state.retired = true;
    state.generation = state.generation.wrapping_add(1);
    if state.in_flight == 0 {
        registry.by_connection.remove(&lifecycle.connection);
    }
    true
}

fn invalidate_binder_fd_token(token: BinderFdToken) -> Option<BinderStateKey> {
    let mut registry = binder_fd_registry();
    let lifecycle = registry.by_fd.get(&token.fd).cloned()?;
    let generation = lifecycle
        .state
        .lock()
        .expect("binder fd lifecycle poisoned")
        .generation;
    if lifecycle.connection != token.connection || generation != token.generation {
        return None;
    }
    registry.by_fd.remove(&token.fd);
    retire_unaliased_lifecycle(&mut registry, &lifecycle).then_some(lifecycle.connection)
}

#[cfg(test)]
fn invalidate_binder_fd(fd: c_int) {
    let _ = invalidate_binder_fd_token(binder_fd_token(fd));
}

unsafe fn close_with_binder_fd_lifecycle<F>(fd: c_int, close: F) -> c_int
where
    F: FnOnce() -> c_int,
{
    if fd < 0 {
        return close();
    }
    let mut registry = binder_fd_registry();
    let lifecycle = registry.by_fd.get(&fd).cloned();
    let result = close();
    let error = *libc::__errno();
    let retired = lifecycle.and_then(|lifecycle| {
        registry.by_fd.remove(&fd);
        retire_unaliased_lifecycle(&mut registry, &lifecycle).then_some(lifecycle.connection)
    });
    let signals = registry.unlock();
    forget_current_thread_binder_fd(fd, retired);
    drop(signals);
    *libc::__errno() = error;
    result
}

unsafe fn duplicate_binder_fd_with_lifecycle<F>(
    old_fd: c_int,
    new_fd: Option<c_int>,
    duplicate: F,
) -> c_int
where
    F: FnOnce() -> c_int,
{
    unsafe fn is_binder_driver_fd(fd: c_int) -> bool {
        let saved_errno = *libc::__errno();
        let mut stat: libc::stat = std::mem::zeroed();
        let is_character_device =
            libc::fstat(fd, &mut stat) == 0 && stat.st_mode & libc::S_IFMT == libc::S_IFCHR;
        let mut version = binder_version::default();
        let is_binder = is_character_device
            && libc::syscall(
                libc::SYS_ioctl,
                fd,
                BINDER_VERSION as libc::c_ulong,
                &mut version,
            ) == 0;
        *libc::__errno() = saved_errno;
        is_binder
    }

    if new_fd == Some(old_fd) {
        return duplicate();
    }
    let mut registry = binder_fd_registry();
    let source = registry.by_fd.get(&old_fd).cloned().or_else(|| {
        is_binder_driver_fd(old_fd).then(|| ensure_binder_fd_lifecycle(&mut registry, old_fd))
    });
    let result = duplicate();
    let error = *libc::__errno();
    let destination = new_fd.unwrap_or(result);
    let retired = (result >= 0)
        .then(|| {
            let previous = registry.by_fd.remove(&destination);
            if let Some(source) = source {
                registry.by_fd.insert(destination, source);
            }
            previous.and_then(|previous| {
                retire_unaliased_lifecycle(&mut registry, &previous).then_some(previous.connection)
            })
        })
        .flatten();
    let signals = registry.unlock();
    if result >= 0 {
        forget_current_thread_binder_fd(destination, retired);
    }
    drop(signals);
    *libc::__errno() = error;
    result
}

struct BinderIoctlGuard {
    lifecycle: Arc<BinderFdLifecycle>,
    pinned_fd: c_int,
}

impl BinderIoctlGuard {
    fn begin(token: BinderFdToken) -> Result<Self, c_int> {
        let registry = binder_fd_registry();
        let lifecycle = registry.by_fd.get(&token.fd).ok_or(libc::EBADF)?.clone();
        if lifecycle.connection != token.connection {
            return Err(libc::ESTALE);
        }
        let mut state = lifecycle
            .state
            .lock()
            .expect("binder fd lifecycle poisoned");
        if state.retired || state.generation != token.generation {
            return Err(libc::ESTALE);
        }
        if state.protocol_error {
            return Err(libc::EPROTO);
        }
        let pinned_fd = if let Some(&pinned_fd) = lifecycle.pinned_fd.get() {
            pinned_fd
        } else {
            let raw_pin = unsafe {
                libc::syscall(libc::SYS_fcntl, token.fd, libc::F_DUPFD_CLOEXEC, 0) as c_int
            };
            #[cfg(not(test))]
            if raw_pin < 0 {
                return Err(unsafe { *libc::__errno() });
            }
            // Mock-ioctl tests use synthetic fd numbers; real-fd tests still exercise the pin.
            if raw_pin < 0 {
                token.fd
            } else {
                lifecycle
                    .pinned_fd
                    .set(raw_pin)
                    .expect("binder fd pin initialized once under registry lock");
                raw_pin
            }
        };
        state.in_flight += 1;
        drop(state);
        drop(registry);
        Ok(Self {
            lifecycle,
            pinned_fd,
        })
    }

    fn fd(&self) -> c_int {
        self.pinned_fd
    }
}

impl Drop for BinderIoctlGuard {
    fn drop(&mut self) {
        let error = unsafe { *libc::__errno() };
        let mut registry = binder_fd_registry();
        let mut state = self
            .lifecycle
            .state
            .lock()
            .expect("binder fd lifecycle poisoned");
        state.in_flight = state.in_flight.saturating_sub(1);
        let retired = (state.retired && state.in_flight == 0).then_some(self.lifecycle.connection);
        if retired.is_some() {
            registry.by_connection.remove(&self.lifecycle.connection);
        }
        drop(state);
        drop(registry);
        if let Some(connection) = retired {
            retire_binder_connection_publications(connection);
        }
        unsafe {
            *libc::__errno() = error;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BinderIoctlCall {
    Called(c_int),
    Stale,
    Retired,
}

unsafe fn call_binder_ioctl(
    token: BinderFdToken,
    old_ioctl_fn: unsafe extern "C" fn(c_int, c_int, *mut c_void) -> c_int,
    request: c_int,
    arg: *mut c_void,
) -> BinderIoctlCall {
    let Ok(guard) = BinderIoctlGuard::begin(token) else {
        return BinderIoctlCall::Stale;
    };
    BinderIoctlCall::Called(old_ioctl_fn(guard.fd(), request, arg))
}

unsafe fn call_binder_connection_ioctl(
    token: BinderFdToken,
    old_ioctl_fn: unsafe extern "C" fn(c_int, c_int, *mut c_void) -> c_int,
    request: c_int,
    arg: *mut c_void,
) -> BinderIoctlCall {
    let (fd, lifecycle, generation) = {
        let registry = binder_fd_registry();
        let Some(lifecycle) = registry.by_connection.get(&token.connection).cloned() else {
            return BinderIoctlCall::Retired;
        };
        let fd = registry
            .by_fd
            .iter()
            .find_map(|(fd, candidate)| Arc::ptr_eq(candidate, &lifecycle).then_some(*fd));
        let Some(fd) = fd else {
            return BinderIoctlCall::Stale;
        };
        let generation = lifecycle
            .state
            .lock()
            .expect("binder fd lifecycle poisoned")
            .generation;
        (fd, lifecycle, generation)
    };
    let current = BinderFdToken {
        fd,
        generation,
        connection: lifecycle.connection,
    };
    if generation != token.generation {
        return BinderIoctlCall::Retired;
    }
    call_binder_ioctl(current, old_ioctl_fn, request, arg)
}

pub(super) unsafe fn new_close(fd: c_int) -> c_int {
    let mut old_close = OLD_CLOSE.load(Ordering::Relaxed);
    if old_close.is_null() {
        extern "C" {
            fn close(fd: c_int) -> c_int;
        }
        old_close = close as *mut c_void;
    }
    let close: unsafe extern "C" fn(c_int) -> c_int = std::mem::transmute(old_close);
    close_with_binder_fd_lifecycle(fd, || close(fd))
}

pub(super) unsafe fn new_fdsan_close(fd: c_int, tag: u64) -> c_int {
    let old_close = OLD_FDSAN_CLOSE.load(Ordering::Relaxed);
    if old_close.is_null() {
        return new_close(fd);
    }
    let close: unsafe extern "C" fn(c_int, u64) -> c_int = std::mem::transmute(old_close);
    close_with_binder_fd_lifecycle(fd, || close(fd, tag))
}

pub(super) unsafe fn new_dup(fd: c_int) -> c_int {
    let mut old_dup = OLD_DUP.load(Ordering::Relaxed);
    if old_dup.is_null() {
        extern "C" {
            fn dup(fd: c_int) -> c_int;
        }
        old_dup = dup as *mut c_void;
    }
    let dup: unsafe extern "C" fn(c_int) -> c_int = std::mem::transmute(old_dup);
    duplicate_binder_fd_with_lifecycle(fd, None, || dup(fd))
}

pub(super) unsafe fn new_dup2(old_fd: c_int, new_fd: c_int) -> c_int {
    let mut old_dup2 = OLD_DUP2.load(Ordering::Relaxed);
    if old_dup2.is_null() {
        extern "C" {
            fn dup2(old_fd: c_int, new_fd: c_int) -> c_int;
        }
        old_dup2 = dup2 as *mut c_void;
    }
    let dup2: unsafe extern "C" fn(c_int, c_int) -> c_int = std::mem::transmute(old_dup2);
    duplicate_binder_fd_with_lifecycle(old_fd, Some(new_fd), || dup2(old_fd, new_fd))
}

pub(super) unsafe fn new_dup3(old_fd: c_int, new_fd: c_int, flags: c_int) -> c_int {
    let mut old_dup3 = OLD_DUP3.load(Ordering::Relaxed);
    if old_dup3.is_null() {
        extern "C" {
            fn dup3(old_fd: c_int, new_fd: c_int, flags: c_int) -> c_int;
        }
        old_dup3 = dup3 as *mut c_void;
    }
    let dup3: unsafe extern "C" fn(c_int, c_int, c_int) -> c_int = std::mem::transmute(old_dup3);
    duplicate_binder_fd_with_lifecycle(old_fd, Some(new_fd), || dup3(old_fd, new_fd, flags))
}

pub(super) unsafe fn new_fcntl(fd: c_int, command: c_int, arg: libc::c_ulong) -> c_int {
    let mut old_fcntl = OLD_FCNTL.load(Ordering::Relaxed);
    if old_fcntl.is_null() {
        extern "C" {
            fn fcntl(fd: c_int, command: c_int, arg: libc::c_ulong) -> c_int;
        }
        old_fcntl = fcntl as *mut c_void;
    }
    let fcntl: unsafe extern "C" fn(c_int, c_int, libc::c_ulong) -> c_int =
        std::mem::transmute(old_fcntl);
    if matches!(command, libc::F_DUPFD | libc::F_DUPFD_CLOEXEC) {
        duplicate_binder_fd_with_lifecycle(fd, None, || fcntl(fd, command, arg))
    } else {
        fcntl(fd, command, arg)
    }
}

pub(super) unsafe fn new_ioctl(fd: c_int, request: c_int, arg: *mut c_void) -> c_int {
    let mut old_ioctl_ptr = OLD_IOCTL.load(Ordering::Relaxed);
    if old_ioctl_ptr.is_null() {
        extern "C" {
            fn ioctl(fd: c_int, request: c_int, arg: *mut c_void) -> c_int;
        }
        old_ioctl_ptr = ioctl as *mut c_void;
    }

    let old_ioctl_fn: unsafe extern "C" fn(c_int, c_int, *mut c_void) -> c_int =
        std::mem::transmute(old_ioctl_ptr);

    if request as u32 != BINDER_WRITE_READ {
        return old_ioctl_fn(fd, request, arg);
    }

    let binder_token = match synchronize_binder_fd_generation(fd) {
        Ok(token) => token,
        Err(_) => {
            cleanup_retired_current_thread_connections();
            *libc::__errno() = libc::EBADF;
            return -1;
        }
    };
    cleanup_retired_current_thread_connections();
    let binder_ioctl_guard = match BinderIoctlGuard::begin(binder_token) {
        Ok(guard) => guard,
        Err(error) => {
            forget_current_thread_binder_fd(fd, None);
            *libc::__errno() = error;
            return -1;
        }
    };
    let connection = binder_state_key(fd);
    if let Some(result) = retry_pending_ioctl_copyback(fd, connection, arg) {
        return result;
    }

    let Some(mut bwr) = copy_process_value(arg.cast::<binder_write_read>()) else {
        return old_ioctl_fn(binder_ioctl_guard.fd(), request, arg);
    };
    let fail_before_ioctl = |error| {
        *libc::__errno() = error;
        -1
    };
    let original_write_buffer = bwr.write_buffer;
    let original_read_buffer = bwr.read_buffer;
    let original_write_size = bwr.write_size;
    let original_read_size = bwr.read_size;
    let input_write_consumed = bwr.write_consumed;
    let input_read_consumed = bwr.read_consumed;
    let Some(write_remaining) = original_write_size.checked_sub(input_write_consumed) else {
        return fail_before_ioctl(libc::EINVAL);
    };
    let Some(read_remaining) = original_read_size.checked_sub(input_read_consumed) else {
        return fail_before_ioctl(libc::EINVAL);
    };
    let mut host_write = Vec::new();
    if write_remaining > 0 {
        let Some(write_address) =
            (original_write_buffer as usize).checked_add(input_write_consumed)
        else {
            return fail_before_ioctl(libc::EFAULT);
        };
        let Some(_) = (original_write_buffer as usize).checked_add(original_write_size) else {
            return fail_before_ioctl(libc::EFAULT);
        };
        host_write = match copy_process_buffer(write_address, write_remaining) {
            Ok(write) => write,
            Err(error) => return fail_before_ioctl(error),
        };
    }
    if read_remaining > 0 {
        let Some(_) = (original_read_buffer as usize).checked_add(input_read_consumed) else {
            return fail_before_ioctl(libc::EFAULT);
        };
        let Some(_) = (original_read_buffer as usize).checked_add(original_read_size) else {
            return fail_before_ioctl(libc::EFAULT);
        };
    }
    if !write_process_value(arg.cast::<binder_write_read>(), &bwr) {
        return fail_before_ioctl(libc::EFAULT);
    }

    flush_native_binder_lifecycle(old_ioctl_fn);

    let mut completion_commands = Vec::new();
    let mut freed_inbound_shadows = Vec::new();
    if write_remaining > 0 {
        freed_inbound_shadows = rewrite_inbound_free_buffers(connection, &mut host_write);
        for (end, _) in &mut freed_inbound_shadows {
            *end += input_write_consumed;
        }
        if write_buffer_is_safe_to_intercept(&host_write) {
            completion_commands = parse_write_buffer(fd, &mut host_write);
            for (end, _, _, _) in &mut completion_commands {
                *end += input_write_consumed;
            }
        } else {
            warn!(
                "event=binder skipped unsafe write command stream fd={} remaining={} consumed={}",
                fd, write_remaining, input_write_consumed
            );
        }
        bwr.write_buffer = host_write.as_mut_ptr() as libc::c_ulong;
    }
    bwr.write_size = write_remaining;
    bwr.write_consumed = 0;

    let ret = old_ioctl_fn(
        binder_ioctl_guard.fd(),
        request,
        (&mut bwr as *mut binder_write_read).cast(),
    );
    let ioctl_errno = *libc::__errno();
    let ioctl_error = (ret < 0).then_some(ioctl_errno);
    let driver_write_consumed = bwr.write_consumed;
    let driver_read_consumed = bwr.read_consumed;
    let write_consumption_valid = driver_write_consumed <= write_remaining;
    // binder_ioctl_write_read() resets read_consumed when the write phase fails,
    // even when userspace supplied an accumulated non-zero value.
    let read_consumption_reset =
        ioctl_error.is_some() && write_remaining > 0 && driver_read_consumed == 0;
    let read_consumption_valid = read_consumption_reset
        || (input_read_consumed..=original_read_size).contains(&driver_read_consumed);
    if !write_consumption_valid || !read_consumption_valid {
        if !write_consumption_valid {
            warn!(
                "event=binder driver reported invalid write consumption fd={} consumed={} remaining={}",
                fd, driver_write_consumed, write_remaining
            );
        }
        if !read_consumption_valid {
            warn!(
                "event=binder driver reported invalid read consumption fd={} previous={} consumed={} size={}",
                fd, input_read_consumed, driver_read_consumed, original_read_size
            );
        }
        binder_ioctl_guard
            .lifecycle
            .state
            .lock()
            .expect("binder fd lifecycle poisoned")
            .protocol_error = true;
        *libc::__errno() = libc::EPROTO;
        return -1;
    }
    bwr.write_size = original_write_size;
    bwr.read_size = original_read_size;
    bwr.write_consumed = input_write_consumed + driver_write_consumed;
    bwr.read_consumed = driver_read_consumed;
    bwr.write_buffer = original_write_buffer;
    bwr.read_buffer = original_read_buffer;

    for &(_, reply_data, expects_reply, acquire_target) in completion_commands
        .iter()
        .take_while(|(end, _, _, _)| *end <= bwr.write_consumed)
    {
        if let Some(target) = acquire_target {
            complete_operation_acquire(target);
            continue;
        }
        let is_reply = reply_data.is_some();
        let operation_target =
            reply_data.and_then(|data_ptr| complete_prepared_bc_reply(fd, data_ptr));
        if let Some(target) = operation_target {
            complete_operation_publication(target, fd);
        }
        record_transaction_completion(fd, is_reply, expects_reply, operation_target);
    }
    if bwr.write_size > 0 && bwr.write_consumed == bwr.write_size {
        abort_prepared_bc_replies(fd);
        clear_outbound_reply_buffers(binder_state_key(fd));
    }

    if let Some(error) = ioctl_error {
        if !matches!(error, libc::EINTR | libc::EAGAIN) {
            abort_prepared_bc_replies(fd);
            clear_outbound_reply_buffers(binder_state_key(fd));
        }
        if error == libc::EBADF {
            let retired = invalidate_binder_fd_token(binder_token);
            forget_current_thread_binder_fd(fd, retired);
        }
    }

    mark_inbound_free_buffers_consumed(connection, &freed_inbound_shadows, bwr.write_consumed);

    let mut pending_read = PendingReadCopyback::None;
    let mut read_effects = PendingReadEffects::new(connection);
    let mut read_copy_back_ok = true;
    if driver_read_consumed > input_read_consumed {
        let read_len = driver_read_consumed - input_read_consumed;
        let read_address = (original_read_buffer as usize)
            .checked_add(input_read_consumed)
            .expect("read address was validated before ioctl");
        match copy_process_buffer(read_address, read_len) {
            Ok(mut read) => {
                read_effects = parse_read_buffer(fd, &mut read);
                if !copy_to_process(read_address, &read) {
                    warn!(
                        "event=binder failed to copy processed read buffer after ioctl fd={} previous={} consumed={}",
                        fd, input_read_consumed, bwr.read_consumed
                    );
                    read_copy_back_ok = false;
                }
                pending_read = PendingReadCopyback::Processed {
                    address: read_address,
                    data: read,
                };
            }
            Err(_) => {
                warn!(
                    "event=binder failed to read driver output after ioctl fd={} previous={} consumed={}",
                    fd, input_read_consumed, bwr.read_consumed
                );
                pending_read = PendingReadCopyback::Unread {
                    address: read_address,
                    len: read_len,
                };
                read_copy_back_ok = false;
            }
        }
    }
    if ret >= 0 && bwr.read_size > 0 {
        flush_native_binder_lifecycle(old_ioctl_fn);
    }
    let mut visible_bwr = bwr;
    if !read_copy_back_ok {
        visible_bwr.read_consumed = input_read_consumed;
    }
    let counters_copy_back_ok = write_process_value(arg.cast::<binder_write_read>(), &visible_bwr);
    if !counters_copy_back_ok {
        warn!(
            "event=binder failed to copy consumed counters after ioctl fd={}",
            fd
        );
    }
    if !read_copy_back_ok || !counters_copy_back_ok {
        PENDING_IOCTL_COPYBACKS.with(|slot| {
            slot.borrow_mut().insert(
                connection,
                PendingIoctlCopyback {
                    arg: arg as usize,
                    write_buffer: original_write_buffer,
                    read_buffer: original_read_buffer,
                    write_size: original_write_size,
                    read_size: original_read_size,
                    read: pending_read,
                    output: bwr,
                    read_effects,
                    freed_inbound_shadows,
                    ret,
                    errno: ioctl_errno,
                },
            );
        });
        *libc::__errno() = libc::EFAULT;
        return -1;
    }
    read_effects.commit();
    complete_inbound_free_buffers(connection, &freed_inbound_shadows, bwr.write_consumed);
    *libc::__errno() = ioctl_errno;
    ret
}

fn prepared_bc_reply(fd: c_int, reply_index: usize) -> Option<PreparedBcReply> {
    let connection = binder_state_key(fd);
    PREPARED_BC_REPLIES.with(|prepared| {
        prepared
            .borrow()
            .get(&connection)
            .and_then(|replies| replies.get(reply_index))
            .copied()
    })
}

fn remember_prepared_bc_reply(fd: c_int, prepared_reply: PreparedBcReply) {
    let connection = binder_state_key(fd);
    PREPARED_BC_REPLIES.with(|prepared| {
        prepared
            .borrow_mut()
            .entry(connection)
            .or_default()
            .push_back(prepared_reply);
    });
}

fn take_prepared_bc_reply(fd: c_int) -> Option<PreparedBcReply> {
    let connection = binder_state_key(fd);
    PREPARED_BC_REPLIES.with(|prepared| {
        let mut prepared = prepared.borrow_mut();
        let replies = prepared.get_mut(&connection)?;
        let reply = replies.pop_front();
        if replies.is_empty() {
            prepared.remove(&connection);
        }
        reply
    })
}

fn complete_prepared_bc_reply(
    fd: c_int,
    observed_data_ptr: usize,
) -> Option<NativeBinderRetirement> {
    let connection = binder_state_key(fd);
    let Some(prepared) = take_prepared_bc_reply(fd) else {
        return commit_bc_reply(connection, None, observed_data_ptr);
    };
    let data_ptr = prepared.data_ptr;
    if data_ptr != observed_data_ptr {
        warn!(
            "event=reply consumed prepared BC_REPLY with changed data pointer fd={} prepared=0x{:x} observed=0x{:x}",
            fd, data_ptr, observed_data_ptr
        );
    }
    commit_bc_reply(connection, prepared.frame_id, data_ptr)
}

fn abort_prepared_bc_replies(fd: c_int) {
    abort_prepared_bc_replies_for_connection(binder_state_key(fd));
}

fn abort_prepared_bc_replies_for_connection(connection: BinderStateKey) {
    let prepared = PREPARED_BC_REPLIES.with(|prepared| prepared.borrow_mut().remove(&connection));
    if let Some(prepared) = prepared {
        for reply in prepared {
            abort_bc_reply(connection, reply.frame_id, reply.data_ptr);
        }
    }
}

unsafe fn write_buffer_is_safe_to_intercept(write: &[u8]) -> bool {
    let mut offset = 0usize;
    while offset < write.len() {
        if write.len() - offset < size_of::<u32>() {
            return false;
        }
        let cmd = std::ptr::read_unaligned(write.as_ptr().add(offset) as *const u32);
        offset += size_of::<u32>();
        let cmd_size = _ioc_size(cmd);
        if cmd_size > write.len() - offset {
            return false;
        }
        let command_end = offset + cmd_size;
        let cmd_nr = _ioc_nr(cmd);
        if _ioc_dir(cmd) == 1 && matches!(cmd_nr, BC_REPLY_NR | BC_REPLY_SG_NR) {
            let expected_size = if cmd_nr == BC_REPLY_SG_NR {
                size_of::<binder_transaction_data_sg>()
            } else {
                size_of::<binder_transaction_data>()
            };
            if cmd_size != expected_size {
                return false;
            }
            let tr = std::ptr::read_unaligned(
                write.as_ptr().add(offset) as *const binder_transaction_data
            );
            if TransactionPayloadShadow::read(&tr).is_none() {
                return false;
            }
        }
        offset = command_end;
    }
    true
}

unsafe fn rewrite_inbound_free_buffers(
    connection: BinderStateKey,
    write: &mut [u8],
) -> Vec<(usize, usize)> {
    let mut offset = 0usize;
    let mut rewritten = Vec::new();
    while write.len().saturating_sub(offset) >= size_of::<u32>() {
        let cmd = std::ptr::read_unaligned(write.as_ptr().add(offset) as *const u32);
        offset += size_of::<u32>();
        let cmd_size = _ioc_size(cmd);
        if cmd_size > write.len().saturating_sub(offset) {
            break;
        }
        let command_end = offset + cmd_size;
        if _ioc_dir(cmd) == 1
            && _ioc_nr(cmd) == BC_FREE_BUFFER_NR
            && cmd_size == size_of::<libc::c_ulong>()
        {
            let payload = write.as_mut_ptr().add(offset) as *mut libc::c_ulong;
            let shadow_buffer = std::ptr::read_unaligned(payload) as usize;
            if let Some(original_buffer) =
                inbound_transaction_original_buffer(connection, shadow_buffer)
            {
                std::ptr::write_unaligned(payload, original_buffer);
                rewritten.push((command_end, shadow_buffer));
            }
        }
        offset = command_end;
    }
    rewritten
}

fn mark_inbound_free_buffers_consumed(
    connection: BinderStateKey,
    rewritten: &[(usize, usize)],
    write_consumed: usize,
) {
    let mut entries = INBOUND_TRANSACTION_SHADOWS
        .lock()
        .expect("inbound transaction shadow map poisoned");
    for &(_, shadow_buffer) in rewritten
        .iter()
        .take_while(|(end, _)| *end <= write_consumed)
    {
        if let Some(shadow) = entries.get_mut(&(connection, shadow_buffer)) {
            if shadow.state == InboundTransactionShadowState::Live {
                shadow.state = InboundTransactionShadowState::KernelFreedPendingAck;
            }
        }
    }
}

fn complete_inbound_free_buffers(
    connection: BinderStateKey,
    rewritten: &[(usize, usize)],
    write_consumed: usize,
) {
    let mut entries = INBOUND_TRANSACTION_SHADOWS
        .lock()
        .expect("inbound transaction shadow map poisoned");
    for &(_, shadow_buffer) in rewritten
        .iter()
        .take_while(|(end, _)| *end <= write_consumed)
    {
        if entries
            .get(&(connection, shadow_buffer))
            .is_some_and(|shadow| {
                shadow.state == InboundTransactionShadowState::KernelFreedPendingAck
            })
        {
            entries.remove(&(connection, shadow_buffer));
        }
    }
}

unsafe fn parse_write_buffer(
    fd: c_int,
    write: &mut [u8],
) -> Vec<(usize, Option<usize>, bool, Option<NativeBinderRetirement>)> {
    let base = write.as_mut_ptr();
    let total_size = write.len();
    let mut offset = 0usize;
    let mut completion_commands = Vec::new();
    let mut reply_count = 0;
    while offset < total_size {
        let command_start = base.add(offset);
        if total_size.saturating_sub(offset) < size_of::<u32>() {
            warn!(
                "truncated binder write command header: remaining={}",
                total_size.saturating_sub(offset)
            );
            break;
        }

        let cmd = std::ptr::read_unaligned(command_start as *const u32);
        offset += size_of::<u32>();
        let payload = base.add(offset);

        let cmd_size = _ioc_size(cmd);
        if cmd_size > total_size.saturating_sub(offset) {
            warn!(
                "truncated binder write command payload: nr={} size={} remaining={}",
                _ioc_nr(cmd),
                cmd_size,
                total_size.saturating_sub(offset)
            );
            break;
        }

        let cmd_nr = _ioc_nr(cmd);
        let is_write = _ioc_dir(cmd) == 1;

        if is_write {
            match cmd_nr {
                BC_TRANSACTION_NR | BC_REPLY_NR | BC_TRANSACTION_SG_NR | BC_REPLY_SG_NR => {
                    let is_sg = matches!(cmd_nr, BC_TRANSACTION_SG_NR | BC_REPLY_SG_NR);
                    let is_reply = matches!(cmd_nr, BC_REPLY_NR | BC_REPLY_SG_NR);
                    let expected_size = if is_sg {
                        size_of::<binder_transaction_data_sg>()
                    } else {
                        size_of::<binder_transaction_data>()
                    };
                    if cmd_size == expected_size {
                        let tr_ptr = payload as *mut binder_transaction_data;
                        let mut tr = std::ptr::read_unaligned(tr_ptr);
                        let prepared = is_reply
                            .then(|| prepared_bc_reply(fd, reply_count))
                            .flatten();
                        if let Some(prepared) = prepared {
                            tr = prepared.transaction;
                        }
                        let mut shadow = None;
                        let inspectable = if let Some(mut payload) =
                            TransactionPayloadShadow::read(&tr)
                        {
                            payload.install(&mut tr);
                            shadow = Some(payload);
                            true
                        } else {
                            warn!(
                                "event=binder skipped unsafe {} parcel fd={} data_size={} offsets_size={}",
                                if is_reply { "reply" } else { "transaction" },
                                fd,
                                tr.data_size,
                                tr.offsets_size
                            );
                            false
                        };
                        let label = match cmd_nr {
                            BC_TRANSACTION_NR => "BC_TRANSACTION",
                            BC_REPLY_NR => "BC_REPLY",
                            BC_TRANSACTION_SG_NR => "BC_TRANSACTION_SG",
                            BC_REPLY_SG_NR => "BC_REPLY_SG",
                            _ => unreachable!(),
                        };
                        let frame_id = if is_reply && prepared.is_none() && inspectable {
                            Some(handle_bc_reply(binder_state_key(fd), &mut tr))
                        } else {
                            None
                        };
                        if inspectable {
                            log_write_transaction(label, &tr);
                        }
                        if let Some(shadow) = shadow.as_ref() {
                            shadow.restore(&mut tr);
                        }
                        if let Some(frame_id) = frame_id {
                            remember_prepared_bc_reply(
                                fd,
                                PreparedBcReply {
                                    frame_id,
                                    data_ptr: tr.data.ptr.buffer as usize,
                                    transaction: tr,
                                },
                            );
                        }
                        std::ptr::write_unaligned(tr_ptr, tr);
                        completion_commands.push((
                            offset + cmd_size,
                            is_reply.then_some(tr.data.ptr.buffer as usize),
                            !is_reply && (tr.flags & TF_ONE_WAY) == 0,
                            None,
                        ));
                    } else if !is_sg {
                        warn!(
                            "unexpected binder write command payload size {} for nr={}",
                            cmd_size, cmd_nr
                        );
                    } else {
                        warn!(
                            "unexpected binder write SG payload size {} for nr={}",
                            cmd_size, cmd_nr
                        );
                    }
                    if cmd_size != expected_size {
                        completion_commands.push((
                            offset + cmd_size,
                            is_reply.then_some(0),
                            false,
                            None,
                        ));
                    }
                    if is_reply {
                        reply_count += 1;
                    }
                }
                _ => {}
            }
            if cmd == BC_ACQUIRE_DONE_CMD && cmd_size == size_of::<binder_ptr_cookie>() {
                let ptr_cookie = std::ptr::read_unaligned(payload as *const binder_ptr_cookie);
                let target = LocalBinderTarget {
                    ptr: ptr_cookie.ptr,
                    cookie: ptr_cookie.cookie,
                };
                completion_commands.push((
                    offset + cmd_size,
                    None,
                    false,
                    operation_publication_pending_acquire(target, binder_state_key(fd)),
                ));
            }
        }

        offset += cmd_size;
    }

    completion_commands
}

unsafe fn parse_read_buffer(fd: c_int, read: &mut [u8]) -> PendingReadEffects {
    let base = read.as_mut_ptr();
    let total_size = read.len();
    let mut offset = 0usize;
    let connection = binder_state_key(fd);
    let mut effects = PendingReadEffects::new(connection);
    while offset < total_size {
        let command_start = base.add(offset);
        if total_size.saturating_sub(offset) < size_of::<u32>() {
            warn!(
                "truncated binder read command header: remaining={}",
                total_size.saturating_sub(offset)
            );
            break;
        }

        let cmd = std::ptr::read_unaligned(command_start as *const u32);
        offset += size_of::<u32>();
        let payload = base.add(offset);

        let cmd_size = _ioc_size(cmd);
        if cmd_size > total_size.saturating_sub(offset) {
            warn!(
                "truncated binder read command payload: nr={} size={} remaining={}",
                _ioc_nr(cmd),
                cmd_size,
                total_size.saturating_sub(offset)
            );
            break;
        }

        let cmd_nr = _ioc_nr(cmd);
        let is_read = _ioc_dir(cmd) == 2;
        let terminal_reply = matches!(
            cmd_nr,
            BR_DEAD_REPLY_NR | BR_FAILED_REPLY_NR | BR_FROZEN_REPLY_NR
        );

        if cmd == BR_TRANSACTION_COMPLETE_CMD {
            complete_transaction_submission(fd);
        } else if terminal_reply
            || matches!(
                cmd_nr,
                BR_ONEWAY_SPAM_SUSPECT_NR | BR_TRANSACTION_PENDING_FROZEN_NR
            )
        {
            complete_failed_transaction_submission(fd, cmd_nr);
        } else if is_read {
            match cmd_nr {
                BR_TRANSACTION_NR => {
                    let transaction = if cmd_size == size_of::<binder_transaction_data_secctx>() {
                        let tr = std::ptr::read_unaligned(
                            payload as *const binder_transaction_data_secctx,
                        );
                        let caller_sid = if tr.secctx == 0 {
                            Some(None)
                        } else {
                            copy_process_c_string(tr.secctx as usize).map(Some)
                        };
                        caller_sid.map(|caller_sid| {
                            (tr.transaction_data, caller_sid, "BR_TRANSACTION_SEC_CTX")
                        })
                    } else if cmd_size == size_of::<binder_transaction_data>() {
                        Some((
                            std::ptr::read_unaligned(payload as *const binder_transaction_data),
                            None,
                            "BR_TRANSACTION",
                        ))
                    } else {
                        warn!("unexpected BR_TRANSACTION-like payload size {}", cmd_size);
                        None
                    };
                    if let Some((mut tr, caller_sid, label)) = transaction {
                        if let Some(mut shadow) = TransactionPayloadShadow::read(&tr) {
                            shadow.install(&mut tr);
                            let handled =
                                handle_incoming_transaction(fd, &mut tr, caller_sid, label);
                            if handled {
                                if tr.data_size != 0
                                    && tr.data.ptr.buffer as usize == shadow.data_ptr()
                                {
                                    effects.staged_inbound_shadows.push(
                                        retain_inbound_transaction_shadow(connection, shadow),
                                    );
                                } else {
                                    shadow.restore(&mut tr);
                                }
                                std::ptr::write_unaligned(
                                    payload as *mut binder_transaction_data,
                                    tr,
                                );
                            } else {
                                shadow.restore(&mut tr);
                            }
                        } else {
                            warn!(
                                "event=binder skipped unsafe incoming transaction parcel fd={} data_size={} offsets_size={}",
                                fd, tr.data_size, tr.offsets_size
                            );
                        }
                    }
                }
                BR_ACQUIRE_NR => {
                    if cmd_size == size_of::<binder_ptr_cookie>() {
                        let ptr_cookie =
                            std::ptr::read_unaligned(payload as *const binder_ptr_cookie);
                        let target = LocalBinderTarget {
                            ptr: ptr_cookie.ptr,
                            cookie: ptr_cookie.cookie,
                        };
                        if let Some(retirement) = observe_operation_acquire(fd, target) {
                            effects.operation_acquires.push(retirement);
                        }
                    } else {
                        warn!(
                            "unexpected binder ref command payload size {} for nr={}",
                            cmd_size, cmd_nr
                        );
                    }
                }
                BR_REPLY_NR => {
                    complete_sync_transaction(fd, SyncTransactionState::AwaitingReply);
                    if cmd_size == size_of::<binder_transaction_data>() {
                        let mut tr =
                            std::ptr::read_unaligned(payload as *const binder_transaction_data);
                        if let Some(mut shadow) = TransactionPayloadShadow::read(&tr) {
                            shadow.install(&mut tr);
                            debug!(
                                ">>> BR_REPLY | target: {}, code: 0x{:x}, sender_euid: {}, sender_pid: {}, flags: 0x{:x}{}, parcel_size: {}, offsets_size: {}, parcel: {}",
                                format_target(&tr),
                                tr.code,
                                tr.sender_euid,
                                tr.sender_pid,
                                tr.flags,
                                if (tr.flags & TF_ONE_WAY) != 0 { ", oneway" } else { "" },
                                tr.data_size,
                                tr.offsets_size,
                                preview_transaction_parcel(&tr),
                            );
                        } else {
                            warn!(
                                "event=binder skipped unsafe BR_REPLY parcel fd={} data_size={} offsets_size={}",
                                fd, tr.data_size, tr.offsets_size
                            );
                        }
                    } else {
                        warn!("unexpected BR_REPLY payload size {}", cmd_size);
                    }
                }
                _ => {}
            }
        }
        offset += cmd_size;
    }
    effects
}

unsafe fn handle_incoming_transaction(
    fd: c_int,
    tr: &mut binder_transaction_data,
    caller_sid: Option<String>,
    label: &str,
) -> bool {
    let target = LocalBinderTarget {
        ptr: tr.target.ptr,
        cookie: tr.cookie,
    };
    if lookup_synthetic_target(target).is_some() {
        if (tr.flags & TF_ONE_WAY) == 0 {
            push_pending_frame(binder_state_key(fd));
        }
        return false;
    }

    handle_br_transaction(binder_state_key(fd), tr, caller_sid, label)
}

#[cfg(test)]
fn push_unaligned<T: Copy>(out: &mut Vec<u8>, value: &T) {
    let start = out.len();
    out.resize(start + size_of::<T>(), 0);
    unsafe {
        std::ptr::write_unaligned(out.as_mut_ptr().add(start) as *mut T, *value);
    }
}

fn record_transaction_completion(
    fd: c_int,
    is_reply: bool,
    expects_reply: bool,
    operation_target: Option<NativeBinderRetirement>,
) {
    let connection = binder_state_key(fd);
    let pending_count = PENDING_TRANSACTION_COMPLETIONS.with(|pending| {
        let mut pending = pending.borrow_mut();
        let queue = pending.entry(connection).or_default();
        queue.push_back(PendingTransactionCompletion {
            is_reply,
            expects_reply,
            operation_target,
        });
        queue.len()
    });
    if expects_reply {
        SYNC_TRANSACTIONS.with(|transactions| {
            transactions
                .borrow_mut()
                .entry(connection)
                .or_default()
                .push(SyncTransactionState::PendingCompletion);
        });
    }
    debug!(
        "event=synthetic registered BR_TRANSACTION_COMPLETE for fd={} thread={:?} reply={} expects_reply={} pending={}",
        fd,
        std::thread::current().id(),
        is_reply,
        expects_reply,
        pending_count
    );
}

fn observe_operation_acquire(
    fd: c_int,
    target: LocalBinderTarget,
) -> Option<NativeBinderRetirement> {
    let retirement = mark_operation_publication_acquire_pending(target, binder_state_key(fd));
    if retirement.is_some() {
        debug!(
            "event=synthetic observed BR_ACQUIRE for operation target ptr=0x{:x} cookie=0x{:x}",
            target.ptr, target.cookie
        );
    }
    retirement
}

fn complete_operation_acquire(retirement: NativeBinderRetirement) {
    mark_operation_publication_acquire_committed(retirement);
}

fn complete_operation_publication(retirement: NativeBinderRetirement, binder_fd: c_int) {
    mark_operation_publication_completed(retirement, observed_binder_fd_token(binder_fd));
}

unsafe fn flush_native_binder_lifecycle(
    old_ioctl_fn: unsafe extern "C" fn(c_int, c_int, *mut c_void) -> c_int,
) {
    while let Some(probe) = take_operation_publication_probe(Instant::now()) {
        let node_exists = operation_binder_node_exists(old_ioctl_fn, probe);
        if let Some(retirement) =
            finish_operation_publication_probe(probe, node_exists, Instant::now())
        {
            debug!(
                "event=synthetic operation publication has no driver references; dropping ptr=0x{:x} cookie=0x{:x}",
                retirement.target.ptr, retirement.target.cookie
            );
            crate::hook::rewrite::drop_synthetic_operation_retirement(retirement);
        }
    }
}

unsafe fn operation_binder_node_exists(
    old_ioctl_fn: unsafe extern "C" fn(c_int, c_int, *mut c_void) -> c_int,
    probe: OperationPublicationProbe,
) -> Result<bool, c_int> {
    let target = probe.target;
    let mut info = binder_node_debug_info {
        // Binder returns the first node whose ptr is strictly greater than the cursor.
        ptr: target.ptr.checked_sub(1).ok_or(libc::EINVAL)?,
        ..Default::default()
    };
    let ret = match call_binder_connection_ioctl(
        probe.binder,
        old_ioctl_fn,
        BINDER_GET_NODE_DEBUG_INFO as c_int,
        &mut info as *mut binder_node_debug_info as *mut c_void,
    ) {
        BinderIoctlCall::Called(ret) => ret,
        BinderIoctlCall::Stale => {
            debug!(
                "event=synthetic operation publication belongs to a retired Binder fd generation; retaining until acquire ownership is resolved ptr=0x{:x} cookie=0x{:x} fd={} generation={}",
                target.ptr, target.cookie, probe.binder.fd, probe.binder.generation
            );
            return Err(libc::ESTALE);
        }
        BinderIoctlCall::Retired => {
            if operation_publication_acquire_is_pending(probe) {
                return Err(libc::ESTALE);
            }
            return Ok(false);
        }
    };
    if ret < 0 {
        let error = std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO);
        if error == libc::EBADF {
            debug!(
                "event=synthetic operation node query fd no longer references its Binder connection; retaining because process-wide acquire work may still be in flight fd={} ptr=0x{:x} cookie=0x{:x} errno={}",
                probe.binder.fd, target.ptr, target.cookie, error
            );
            return Err(error);
        }
        debug!(
            "event=synthetic operation node query failed fd={} ptr=0x{:x} cookie=0x{:x} errno={}",
            probe.binder.fd, target.ptr, target.cookie, error
        );
        return Err(error);
    }
    if info.ptr == 0 || info.ptr > target.ptr {
        return Ok(false);
    }
    if info.ptr == target.ptr {
        if info.cookie == target.cookie {
            return Ok(true);
        }
        warn!(
            "event=synthetic operation node identity changed fd={} expected_ptr=0x{:x} expected_cookie=0x{:x} actual_cookie=0x{:x}; treating target as gone",
            probe.binder.fd, target.ptr, target.cookie, info.cookie
        );
        return Ok(false);
    }
    warn!(
        "event=synthetic operation node query returned unexpected identity fd={} expected_ptr=0x{:x} expected_cookie=0x{:x} actual_ptr=0x{:x} actual_cookie=0x{:x}",
        probe.binder.fd, target.ptr, target.cookie, info.ptr, info.cookie
    );
    Err(libc::EPROTO)
}

fn complete_transaction_submission(fd: c_int) -> Option<()> {
    let connection = binder_state_key(fd);
    let (completion, remaining) = PENDING_TRANSACTION_COMPLETIONS.with(|pending| {
        let mut pending = pending.borrow_mut();
        let queue = pending.get_mut(&connection)?;
        let completion = queue.pop_front()?;
        let remaining = queue.len();
        if queue.is_empty() {
            pending.remove(&connection);
        }
        Some((completion, remaining))
    })?;

    if completion.expects_reply && !mark_sync_transaction_completed(fd) {
        warn!(
            "event=synthetic synchronous transaction completion had no pending transaction fd={} thread={:?}",
            fd,
            std::thread::current().id()
        );
    }

    debug!(
        "event=synthetic consumed BR_TRANSACTION_COMPLETE for fd={} thread={:?} remaining={}",
        fd,
        std::thread::current().id(),
        remaining
    );
    Some(())
}

fn mark_sync_transaction_completed(fd: c_int) -> bool {
    let connection = binder_state_key(fd);
    SYNC_TRANSACTIONS.with(|transactions| {
        let mut transactions = transactions.borrow_mut();
        let Some(stack) = transactions.get_mut(&connection) else {
            return false;
        };
        let Some(state) = stack.last_mut() else {
            return false;
        };
        if *state != SyncTransactionState::PendingCompletion {
            return false;
        }
        *state = SyncTransactionState::AwaitingReply;
        true
    })
}

fn complete_sync_transaction(fd: c_int, expected: SyncTransactionState) -> bool {
    let connection = binder_state_key(fd);
    SYNC_TRANSACTIONS.with(|transactions| {
        let mut transactions = transactions.borrow_mut();
        let Some(stack) = transactions.get_mut(&connection) else {
            return false;
        };
        if stack.last() != Some(&expected) {
            return false;
        }
        stack.pop();
        if stack.is_empty() {
            transactions.remove(&connection);
        }
        true
    })
}

fn complete_failed_transaction_submission(fd: c_int, cmd_nr: u32) {
    let connection = binder_state_key(fd);
    let terminal_reply = matches!(
        cmd_nr,
        BR_DEAD_REPLY_NR | BR_FAILED_REPLY_NR | BR_FROZEN_REPLY_NR
    );
    if terminal_reply {
        let failed_reply = PENDING_TRANSACTION_COMPLETIONS.with(|pending| {
            let mut pending = pending.borrow_mut();
            let queue = pending.get_mut(&connection)?;
            if queue.front().is_none_or(|completion| !completion.is_reply) {
                return None;
            }
            let completion = queue.pop_front()?;
            if queue.is_empty() {
                pending.remove(&connection);
            }
            Some(completion)
        });
        if let Some(completion) = failed_reply {
            if let Some(target) = completion.operation_target {
                retire_synthetic_operation_retirement(target);
                debug!(
                    "event=synthetic failed reply retired operation backend and retained publication tombstone fd={} ptr=0x{:x} cookie=0x{:x}",
                    fd, target.target.ptr, target.target.cookie
                );
            }
            debug!(
                "event=synthetic consumed terminal result for failed synthetic reply fd={} thread={:?}",
                fd,
                std::thread::current().id()
            );
            return;
        }
    }

    let front = PENDING_TRANSACTION_COMPLETIONS.with(|pending| {
        pending
            .borrow()
            .get(&connection)
            .and_then(|queue| queue.front())
            .map(|completion| (completion.is_reply, completion.expects_reply))
    });
    let immediate_failure = match front {
        Some((false, false)) => true,
        Some((false, true)) => {
            complete_sync_transaction(fd, SyncTransactionState::PendingCompletion)
        }
        Some((true, _)) | None => false,
    };

    if terminal_reply
        && !immediate_failure
        && complete_sync_transaction(fd, SyncTransactionState::AwaitingReply)
    {
        debug!(
            "event=synthetic consumed terminal result after completed synchronous transaction fd={} thread={:?}",
            fd,
            std::thread::current().id()
        );
        return;
    }

    if !immediate_failure && matches!(front, Some((false, true))) {
        warn!(
            "event=synthetic terminal result found a synchronous completion marker without matching transaction state fd={} thread={:?}",
            fd,
            std::thread::current().id()
        );
    }

    let removed = PENDING_TRANSACTION_COMPLETIONS.with(|pending| {
        let mut pending = pending.borrow_mut();
        let queue = pending.get_mut(&connection)?;
        if queue.front().is_none_or(|completion| completion.is_reply) {
            return None;
        }
        queue.pop_front();
        if queue.is_empty() {
            pending.remove(&connection);
        }
        Some(())
    });
    if removed.is_some() {
        debug!(
            "event=synthetic consumed terminal result for failed outgoing transaction fd={} thread={:?}",
            fd,
            std::thread::current().id()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::rewrite::{
        pending_reply_frame_claims_for_test, pending_reply_frame_count_for_test,
        reset_pending_reply_frames_for_test,
    };
    use std::sync::atomic::{AtomicI32, AtomicUsize};
    use std::sync::Barrier;
    use std::time::Duration;

    static CAPTURED_REPLY_DATA: Mutex<Option<Vec<u8>>> = Mutex::new(None);
    static HOST_IOCTL_CALLS: AtomicUsize = AtomicUsize::new(0);
    static INTERLEAVED_REPLY_IOCTL_CALLS: AtomicUsize = AtomicUsize::new(0);
    static EFAULT_IOCTL_CALLS: AtomicUsize = AtomicUsize::new(0);
    static COPYBACK_IOCTL_CALLS: AtomicUsize = AtomicUsize::new(0);
    static BOUNDARY_IOCTL_CALLS: AtomicUsize = AtomicUsize::new(0);
    static BOUNDARY_IOCTL_MODE: AtomicUsize = AtomicUsize::new(0);
    static POST_IOCTL_INVALIDATE_FD: AtomicI32 = AtomicI32::new(-1);
    static POST_IOCTL_PROTECT_ADDRESS: AtomicUsize = AtomicUsize::new(0);
    static POST_IOCTL_PROTECT_LENGTH: AtomicUsize = AtomicUsize::new(0);
    static NODE_QUERY_RESULTS: Mutex<VecDeque<Result<binder_node_debug_info, c_int>>> =
        Mutex::new(VecDeque::new());
    static SYNTHETIC_REPLY_TEST_LOCK: Mutex<()> = Mutex::new(());
    static PINNED_IOCTL_ENTERED: LazyLock<Barrier> = LazyLock::new(|| Barrier::new(2));
    static PINNED_IOCTL_RELEASE: LazyLock<Barrier> = LazyLock::new(|| Barrier::new(2));

    fn drain_transaction_completions(fd: c_int) {
        while complete_transaction_submission(fd).is_some() {}
        abort_prepared_bc_replies(fd);
        let connection = binder_state_key(fd);
        clear_outbound_reply_buffers(connection);
        SYNC_TRANSACTIONS.with(|transactions| {
            transactions.borrow_mut().remove(&connection);
        });
    }

    fn reset_binder_fd_for_test(fd: c_int) {
        let retired = invalidate_binder_fd_token(binder_fd_token(fd));
        forget_current_thread_binder_fd(fd, retired);
    }

    unsafe extern "C" fn capture_reply_ioctl(
        _fd: c_int,
        request: c_int,
        arg: *mut c_void,
    ) -> c_int {
        if request != BINDER_WRITE_READ as c_int || arg.is_null() {
            return -1;
        }

        let bwr = &mut *(arg as *mut binder_write_read);
        let write = if bwr.write_size == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(bwr.write_buffer as *const u8, bwr.write_size)
        };
        let mut offset = 0usize;
        let mut captured = None;
        while offset + size_of::<u32>() <= write.len() {
            let cmd = std::ptr::read_unaligned(write.as_ptr().add(offset) as *const u32);
            offset += size_of::<u32>();
            match cmd {
                BC_FREE_BUFFER_CMD => {
                    offset = offset.saturating_add(size_of::<libc::c_ulong>());
                }
                BC_REPLY_CMD => {
                    if offset + size_of::<binder_transaction_data>() > write.len() {
                        return -1;
                    }
                    let tr = std::ptr::read_unaligned(
                        write.as_ptr().add(offset) as *const binder_transaction_data
                    );
                    offset += size_of::<binder_transaction_data>();
                    captured = Some(
                        std::slice::from_raw_parts(tr.data.ptr.buffer as *const u8, tr.data_size)
                            .to_vec(),
                    );
                }
                _ => return -1,
            }
        }

        if captured.is_some() {
            *CAPTURED_REPLY_DATA
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = captured;
        }
        bwr.write_consumed = bwr.write_size;
        0
    }

    unsafe extern "C" fn fail_reply_ioctl(_fd: c_int, _request: c_int, arg: *mut c_void) -> c_int {
        let bwr = &mut *(arg as *mut binder_write_read);
        bwr.write_consumed = 0;
        *libc::__errno() = libc::EIO;
        -1
    }

    unsafe extern "C" fn efault_ioctl(_fd: c_int, _request: c_int, _arg: *mut c_void) -> c_int {
        EFAULT_IOCTL_CALLS.fetch_add(1, Ordering::SeqCst);
        *libc::__errno() = libc::EFAULT;
        -1
    }

    unsafe extern "C" fn node_query_ioctl(_fd: c_int, request: c_int, arg: *mut c_void) -> c_int {
        assert_eq!(request, BINDER_GET_NODE_DEBUG_INFO as c_int);
        assert_eq!((*(arg as *const binder_node_debug_info)).ptr, 0x2f);
        let result = NODE_QUERY_RESULTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
            .expect("node query result should be queued");
        match result {
            Ok(info) => {
                std::ptr::write(arg as *mut binder_node_debug_info, info);
                0
            }
            Err(error) => {
                *libc::__errno() = error;
                -1
            }
        }
    }

    unsafe extern "C" fn blocking_ioctl(_fd: c_int, _request: c_int, _arg: *mut c_void) -> c_int {
        PINNED_IOCTL_ENTERED.wait();
        PINNED_IOCTL_RELEASE.wait();
        0
    }

    unsafe extern "C" fn invalidate_after_consuming_ioctl(
        _fd: c_int,
        request: c_int,
        arg: *mut c_void,
    ) -> c_int {
        assert_eq!(request, BINDER_WRITE_READ as c_int);
        let bwr = &mut *(arg as *mut binder_write_read);
        bwr.write_consumed = bwr.write_size;
        assert!(bwr.read_size >= size_of::<u32>());
        std::ptr::write_unaligned(bwr.read_buffer as *mut u32, BR_NOOP_CMD);
        bwr.read_consumed = size_of::<u32>();
        invalidate_binder_fd(POST_IOCTL_INVALIDATE_FD.load(Ordering::SeqCst));
        0
    }

    unsafe extern "C" fn partial_read_ioctl(_fd: c_int, request: c_int, arg: *mut c_void) -> c_int {
        assert_eq!(request, BINDER_WRITE_READ as c_int);
        let bwr = &mut *(arg as *mut binder_write_read);
        assert_eq!(bwr.read_consumed, size_of::<u32>());
        assert!(bwr.read_size >= 3 * size_of::<u32>());
        std::ptr::write_unaligned(
            (bwr.read_buffer as *mut u8).add(bwr.read_consumed) as *mut u32,
            BR_NOOP_CMD,
        );
        bwr.read_consumed += size_of::<u32>();
        0
    }

    unsafe extern "C" fn prefix_only_read_ioctl(
        _fd: c_int,
        request: c_int,
        arg: *mut c_void,
    ) -> c_int {
        assert_eq!(request, BINDER_WRITE_READ as c_int);
        let bwr = &mut *(arg as *mut binder_write_read);
        assert!(bwr.read_consumed > 0);
        0
    }

    unsafe extern "C" fn protect_copyback_ioctl(
        _fd: c_int,
        request: c_int,
        arg: *mut c_void,
    ) -> c_int {
        assert_eq!(request, BINDER_WRITE_READ as c_int);
        COPYBACK_IOCTL_CALLS.fetch_add(1, Ordering::SeqCst);
        let bwr = &mut *(arg as *mut binder_write_read);
        bwr.write_consumed = bwr.write_size;
        if bwr.read_consumed < bwr.read_size {
            std::ptr::write_unaligned(
                (bwr.read_buffer as *mut u8).add(bwr.read_consumed) as *mut u32,
                BR_TRANSACTION_COMPLETE_CMD,
            );
            bwr.read_consumed += size_of::<u32>();
        }
        assert_eq!(
            libc::mprotect(
                POST_IOCTL_PROTECT_ADDRESS.load(Ordering::SeqCst) as *mut c_void,
                POST_IOCTL_PROTECT_LENGTH.load(Ordering::SeqCst),
                libc::PROT_NONE,
            ),
            0
        );
        0
    }

    unsafe extern "C" fn invalid_consumption_ioctl(
        _fd: c_int,
        request: c_int,
        arg: *mut c_void,
    ) -> c_int {
        assert_eq!(request, BINDER_WRITE_READ as c_int);
        BOUNDARY_IOCTL_CALLS.fetch_add(1, Ordering::SeqCst);
        let bwr = &mut *(arg as *mut binder_write_read);
        match BOUNDARY_IOCTL_MODE.load(Ordering::SeqCst) {
            0 => bwr.write_consumed = bwr.write_size + 1,
            1 => bwr.read_consumed = bwr.read_size + 1,
            _ => bwr.read_consumed -= 1,
        }
        0
    }

    unsafe extern "C" fn retry_host_reply_ioctl(
        _fd: c_int,
        request: c_int,
        arg: *mut c_void,
    ) -> c_int {
        assert_eq!(request, BINDER_WRITE_READ as c_int);
        let bwr = &mut *(arg as *mut binder_write_read);
        match HOST_IOCTL_CALLS.fetch_add(1, Ordering::SeqCst) {
            0 => {
                bwr.write_consumed = 0;
                0
            }
            1 => {
                bwr.write_consumed = size_of::<u32>() + size_of::<binder_transaction_data>();
                *libc::__errno() = libc::EIO;
                -1
            }
            _ => {
                bwr.write_consumed = bwr.write_size;
                0
            }
        }
    }

    unsafe extern "C" fn partial_eintr_host_write_ioctl(
        _fd: c_int,
        request: c_int,
        arg: *mut c_void,
    ) -> c_int {
        assert_eq!(request, BINDER_WRITE_READ as c_int);
        let bwr = &mut *(arg as *mut binder_write_read);
        let command_size = size_of::<u32>() + size_of::<binder_transaction_data>();
        if HOST_IOCTL_CALLS.fetch_add(1, Ordering::SeqCst) == 0 {
            assert_eq!(bwr.write_consumed, 0);
            bwr.write_consumed = command_size;
            *libc::__errno() = libc::EINTR;
            -1
        } else {
            assert_eq!(bwr.write_consumed, 0);
            assert_eq!(bwr.write_size, command_size);
            bwr.write_consumed = bwr.write_size;
            0
        }
    }

    unsafe extern "C" fn write_error_resets_accumulated_read_ioctl(
        _fd: c_int,
        request: c_int,
        arg: *mut c_void,
    ) -> c_int {
        assert_eq!(request, BINDER_WRITE_READ as c_int);
        let bwr = &mut *(arg as *mut binder_write_read);
        assert!(bwr.write_size > 0);
        assert!(bwr.read_consumed > 0);
        bwr.write_consumed = 0;
        bwr.read_consumed = 0;
        *libc::__errno() = libc::EINTR;
        -1
    }

    unsafe extern "C" fn interleaved_host_reply_ioctl(
        _fd: c_int,
        request: c_int,
        arg: *mut c_void,
    ) -> c_int {
        assert_eq!(request, BINDER_WRITE_READ as c_int);
        let bwr = &mut *(arg as *mut binder_write_read);
        match INTERLEAVED_REPLY_IOCTL_CALLS.fetch_add(1, Ordering::SeqCst) {
            0 => {
                assert!(bwr.write_size > 0);
                assert!(bwr.read_size >= size_of::<u32>() + size_of::<binder_transaction_data>());
                bwr.write_consumed = 0;
                std::ptr::write_unaligned(bwr.read_buffer as *mut u32, BR_TRANSACTION_CMD);
                std::ptr::write_unaligned(
                    (bwr.read_buffer as *mut u8).add(size_of::<u32>())
                        as *mut binder_transaction_data,
                    std::mem::zeroed(),
                );
                bwr.read_consumed = size_of::<u32>() + size_of::<binder_transaction_data>();
            }
            1 => {
                bwr.write_consumed = bwr.write_size;
                assert_eq!(
                    bwr.read_consumed,
                    size_of::<u32>() + size_of::<binder_transaction_data>()
                );
            }
            call => panic!("unexpected interleaved reply ioctl call {call}"),
        }
        0
    }

    #[test]
    fn write_parser_tracks_transaction_and_reply_completions() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _route_guard = crate::tracker::state_test_guard();
        reset_pending_reply_frames_for_test(0, 0);
        let tr: binder_transaction_data = unsafe { std::mem::zeroed() };
        let mut write = Vec::new();
        let bc_transaction_cmd = BC_REPLY_CMD - BC_REPLY_NR + BC_TRANSACTION_NR;
        push_unaligned(&mut write, &bc_transaction_cmd);
        push_unaligned(&mut write, &tr);
        push_unaligned(&mut write, &BC_REPLY_CMD);
        push_unaligned(&mut write, &tr);
        push_unaligned(&mut write, &BC_ACQUIRE_DONE_CMD);
        let acquire_target = LocalBinderTarget {
            ptr: 0x1234,
            cookie: 0x5678,
        };
        let acquire_retirement = register_operation_publication_for_test(acquire_target);
        let connection = binder_state_key(20);
        bind_operation_publication_connection(acquire_retirement, connection);
        assert_eq!(
            mark_operation_publication_acquire_pending(acquire_target, connection),
            Some(acquire_retirement)
        );
        push_unaligned(
            &mut write,
            &binder_ptr_cookie {
                ptr: acquire_target.ptr,
                cookie: acquire_target.cookie,
            },
        );

        let completions = unsafe { parse_write_buffer(20, &mut write) };
        assert_eq!(completions.len(), 3);
        assert_eq!(completions[0].1, None);
        assert!(completions[0].2);
        assert_eq!(completions[0].3, None);
        assert_eq!(completions[1].1, Some(0));
        assert!(!completions[1].2);
        assert_eq!(completions[1].3, None);
        assert_eq!(completions[2].3, Some(acquire_retirement));
        abort_prepared_bc_replies(20);
        cancel_operation_publication_acquire_pending(acquire_retirement);
        finish_local_operation_publication(acquire_retirement);
    }

    #[test]
    fn reset_discards_pending_staged_shadow_but_preserves_live_shadow() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let connection = binder_state_key(20);
        clear_inbound_transaction_shadows(connection);
        let original = [1u8, 2, 3, 4];
        let mut tr: binder_transaction_data = unsafe { std::mem::zeroed() };
        tr.data_size = original.len();
        tr.data.ptr.buffer = original.as_ptr() as libc::c_ulong;
        let shadow = unsafe { TransactionPayloadShadow::read(&tr) }
            .expect("readable transaction should create a shadow");
        let live_buffer = retain_inbound_transaction_shadow(connection, shadow);

        assert_eq!(
            inbound_transaction_original_buffer(connection, live_buffer),
            None
        );
        publish_inbound_transaction_shadows(connection, &[live_buffer]);
        assert_eq!(
            inbound_transaction_original_buffer(connection, live_buffer),
            Some(original.as_ptr() as libc::c_ulong)
        );

        let staged = unsafe { TransactionPayloadShadow::read(&tr) }
            .expect("readable transaction should create a shadow");
        let staged_buffer = retain_inbound_transaction_shadow(connection, staged);
        let mut read_effects = PendingReadEffects::new(connection);
        read_effects.staged_inbound_shadows.push(staged_buffer);
        PENDING_IOCTL_COPYBACKS.with(|pending| {
            pending.borrow_mut().insert(
                connection,
                PendingIoctlCopyback {
                    arg: 0,
                    write_buffer: 0,
                    read_buffer: 0,
                    write_size: 0,
                    read_size: 0,
                    read: PendingReadCopyback::None,
                    output: unsafe { std::mem::zeroed() },
                    read_effects,
                    freed_inbound_shadows: Vec::new(),
                    ret: 0,
                    errno: 0,
                },
            );
        });

        reset_current_thread_binder_state(connection);
        assert_eq!(
            inbound_transaction_original_buffer(connection, live_buffer),
            Some(original.as_ptr() as libc::c_ulong)
        );
        assert!(!INBOUND_TRANSACTION_SHADOWS
            .lock()
            .expect("inbound transaction shadow map poisoned")
            .contains_key(&(connection, staged_buffer)));
        clear_inbound_transaction_shadows(connection);
    }

    #[test]
    fn undelivered_operation_acquire_is_canceled_but_delivered_acquire_is_retained() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _route_guard = crate::tracker::state_test_guard();
        let fd = 98;
        let connection = binder_state_key(fd);
        let target = LocalBinderTarget {
            ptr: 0x1234,
            cookie: 0x5678,
        };
        let retirement = register_operation_publication_for_test(target);
        bind_operation_publication_connection(retirement, connection);

        let mut effects = PendingReadEffects::new(connection);
        effects
            .operation_acquires
            .push(observe_operation_acquire(fd, target).unwrap());
        drop(effects);
        assert_eq!(
            mark_operation_publication_acquire_pending(target, connection),
            Some(retirement)
        );
        cancel_operation_publication_acquire_pending(retirement);

        let mut effects = PendingReadEffects::new(connection);
        effects
            .operation_acquires
            .push(observe_operation_acquire(fd, target).unwrap());
        effects.commit();
        assert_eq!(
            mark_operation_publication_acquire_pending(target, connection),
            None
        );

        cancel_operation_publication_acquire_pending(retirement);
        finish_local_operation_publication(retirement);
        reset_binder_fd_for_test(fd);
    }

    #[test]
    fn inbound_shadow_free_is_translated_and_released_only_after_consumption() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let connection = binder_state_key(20);
        clear_inbound_transaction_shadows(connection);
        let original = [1u8, 2, 3, 4];
        let mut tr: binder_transaction_data = unsafe { std::mem::zeroed() };
        tr.data_size = original.len();
        tr.data.ptr.buffer = original.as_ptr() as libc::c_ulong;
        let shadow = unsafe { TransactionPayloadShadow::read(&tr) }
            .expect("readable transaction should create a shadow");
        let shadow_buffer = retain_inbound_transaction_shadow(connection, shadow);
        publish_inbound_transaction_shadows(connection, &[shadow_buffer]);

        let mut write = Vec::new();
        push_unaligned(&mut write, &BC_FREE_BUFFER_CMD);
        push_unaligned(&mut write, &shadow_buffer);
        push_unaligned(&mut write, &BC_REPLY_CMD);
        assert!(!unsafe { write_buffer_is_safe_to_intercept(&write) });

        let rewritten = unsafe { rewrite_inbound_free_buffers(connection, &mut write) };
        let command_end = size_of::<u32>() + size_of::<libc::c_ulong>();
        assert_eq!(rewritten, vec![(command_end, shadow_buffer)]);
        assert_eq!(
            unsafe {
                std::ptr::read_unaligned(write.as_ptr().add(size_of::<u32>()) as *const usize)
            },
            original.as_ptr() as usize
        );

        complete_inbound_free_buffers(connection, &rewritten, command_end - 1);
        assert_eq!(
            inbound_transaction_original_buffer(connection, shadow_buffer),
            Some(original.as_ptr() as libc::c_ulong)
        );
        mark_inbound_free_buffers_consumed(connection, &rewritten, command_end);
        complete_inbound_free_buffers(connection, &rewritten, command_end);
        assert_eq!(
            inbound_transaction_original_buffer(connection, shadow_buffer),
            None
        );
    }

    #[test]
    fn binder_fd_generation_reset_discards_stale_thread_state() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fd = 90;
        reset_binder_fd_for_test(fd);
        drain_transaction_completions(fd);

        let original = synchronize_binder_fd_generation(fd).unwrap();
        let connection = original.connection;
        record_transaction_completion(fd, false, true, None);
        PREPARED_BC_REPLIES.with(|prepared| {
            prepared
                .borrow_mut()
                .entry(connection)
                .or_default()
                .push_back(PreparedBcReply {
                    frame_id: None,
                    data_ptr: 0,
                    transaction: unsafe { std::mem::zeroed() },
                });
        });
        reset_pending_reply_frames_for_test(connection, 1);

        invalidate_binder_fd(fd);
        assert_eq!(synchronize_binder_fd_generation(fd), Err(original));
        let replacement = synchronize_binder_fd_generation(fd).unwrap();

        assert_ne!(replacement.connection, original.connection);
        assert!(!binder_fd_token_is_current(original));
        assert!(binder_fd_token_is_current(replacement));
        assert!(PENDING_TRANSACTION_COMPLETIONS
            .with(|pending| { !pending.borrow().contains_key(&connection) }));
        assert!(SYNC_TRANSACTIONS
            .with(|transactions| { !transactions.borrow().contains_key(&connection) }));
        assert!(
            PREPARED_BC_REPLIES.with(|prepared| { !prepared.borrow().contains_key(&connection) })
        );
        assert_eq!(pending_reply_frame_count_for_test(connection), 0);

        reset_binder_fd_for_test(fd);
    }

    #[test]
    fn invalid_binder_write_read_input_returns_efault_without_crashing() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        EFAULT_IOCTL_CALLS.store(0, Ordering::SeqCst);
        let previous = OLD_IOCTL.swap(efault_ioctl as *mut c_void, Ordering::SeqCst);

        assert_eq!(
            unsafe {
                new_ioctl(
                    91,
                    BINDER_WRITE_READ as c_int,
                    std::ptr::dangling_mut::<c_void>(),
                )
            },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EFAULT)
        );

        let mut bwr = binder_write_read {
            write_size: size_of::<u32>(),
            write_consumed: 0,
            write_buffer: 1,
            read_size: 0,
            read_consumed: 0,
            read_buffer: 0,
        };
        assert_eq!(
            unsafe {
                new_ioctl(
                    91,
                    BINDER_WRITE_READ as c_int,
                    (&mut bwr as *mut binder_write_read).cast(),
                )
            },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EFAULT)
        );
        assert_eq!(EFAULT_IOCTL_CALLS.load(Ordering::SeqCst), 1);

        bwr.write_size = 0;
        bwr.write_buffer = 0;
        bwr.read_size = size_of::<u32>();
        bwr.read_buffer = 1;
        assert_eq!(
            unsafe {
                new_ioctl(
                    91,
                    BINDER_WRITE_READ as c_int,
                    (&mut bwr as *mut binder_write_read).cast(),
                )
            },
            -1
        );
        assert_eq!(EFAULT_IOCTL_CALLS.load(Ordering::SeqCst), 2);

        OLD_IOCTL.store(previous, Ordering::SeqCst);
        reset_binder_fd_for_test(91);
        unsafe { *libc::__errno() = 0 };
    }

    #[test]
    fn read_shadow_only_copies_newly_consumed_bytes() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fd = 96;
        reset_binder_fd_for_test(fd);
        let mut read = [0x5au8; 3 * size_of::<u32>()];
        let mut bwr = binder_write_read {
            write_size: 0,
            write_consumed: 0,
            write_buffer: 0,
            read_size: read.len(),
            read_consumed: size_of::<u32>(),
            read_buffer: read.as_mut_ptr() as libc::c_ulong,
        };
        let previous = OLD_IOCTL.swap(capture_reply_ioctl as *mut c_void, Ordering::SeqCst);

        assert_eq!(
            unsafe {
                new_ioctl(
                    fd,
                    BINDER_WRITE_READ as c_int,
                    (&mut bwr as *mut binder_write_read).cast(),
                )
            },
            0
        );
        assert_eq!(read, [0x5a; 3 * size_of::<u32>()]);

        OLD_IOCTL.store(partial_read_ioctl as *mut c_void, Ordering::SeqCst);
        assert_eq!(
            unsafe {
                new_ioctl(
                    fd,
                    BINDER_WRITE_READ as c_int,
                    (&mut bwr as *mut binder_write_read).cast(),
                )
            },
            0
        );
        assert_eq!(bwr.read_consumed, 2 * size_of::<u32>());
        assert_eq!(&read[..size_of::<u32>()], &[0x5a; size_of::<u32>()]);
        assert_eq!(
            &read[size_of::<u32>()..2 * size_of::<u32>()],
            &BR_NOOP_CMD.to_ne_bytes()
        );
        assert_eq!(&read[2 * size_of::<u32>()..], &[0x5a; size_of::<u32>()]);

        read[..size_of::<u32>()].fill(0x33);
        OLD_IOCTL.store(prefix_only_read_ioctl as *mut c_void, Ordering::SeqCst);
        assert_eq!(
            unsafe {
                new_ioctl(
                    fd,
                    BINDER_WRITE_READ as c_int,
                    (&mut bwr as *mut binder_write_read).cast(),
                )
            },
            0
        );
        assert_eq!(bwr.read_consumed, 2 * size_of::<u32>());
        assert_eq!(&read[..size_of::<u32>()], &[0x33; size_of::<u32>()]);
        assert_eq!(&read[2 * size_of::<u32>()..], &[0x5a; size_of::<u32>()]);

        OLD_IOCTL.store(previous, Ordering::SeqCst);
        reset_binder_fd_for_test(fd);
    }

    #[test]
    fn post_ioctl_copyback_failures_return_efault_without_replaying_ioctl() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fd = 97;
        reset_binder_fd_for_test(fd);
        let connection = binder_state_key(fd);
        let pending_completions = || {
            PENDING_TRANSACTION_COMPLETIONS
                .with(|pending| pending.borrow().get(&connection).map_or(0, VecDeque::len))
        };
        record_transaction_completion(fd, false, false, None);
        assert_eq!(pending_completions(), 1);
        COPYBACK_IOCTL_CALLS.store(0, Ordering::SeqCst);
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize };
        let map_page = || unsafe {
            let page = libc::mmap(
                std::ptr::null_mut(),
                page_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            assert_ne!(page, libc::MAP_FAILED);
            page
        };
        let read_page = map_page();
        unsafe { std::ptr::write_bytes(read_page.cast::<u8>(), 0x5a, page_size) };
        let mut bwr = binder_write_read {
            write_size: 0,
            write_consumed: 0,
            write_buffer: 0,
            read_size: size_of::<u32>(),
            read_consumed: 0,
            read_buffer: read_page as libc::c_ulong,
        };
        POST_IOCTL_PROTECT_ADDRESS.store(read_page as usize, Ordering::SeqCst);
        POST_IOCTL_PROTECT_LENGTH.store(page_size, Ordering::SeqCst);
        let previous = OLD_IOCTL.swap(protect_copyback_ioctl as *mut c_void, Ordering::SeqCst);

        assert_eq!(
            unsafe {
                new_ioctl(
                    fd,
                    BINDER_WRITE_READ as c_int,
                    (&mut bwr as *mut binder_write_read).cast(),
                )
            },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EFAULT)
        );
        assert_eq!(bwr.read_consumed, 0);
        assert_eq!(pending_completions(), 1);
        assert_eq!(
            unsafe { libc::mprotect(read_page, page_size, libc::PROT_READ | libc::PROT_WRITE,) },
            0
        );
        assert_eq!(
            unsafe {
                new_ioctl(
                    fd,
                    BINDER_WRITE_READ as c_int,
                    (&mut bwr as *mut binder_write_read).cast(),
                )
            },
            0
        );
        assert_eq!(COPYBACK_IOCTL_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(bwr.read_consumed, size_of::<u32>());
        assert_eq!(pending_completions(), 0);
        assert_eq!(
            unsafe { std::slice::from_raw_parts(read_page.cast::<u8>(), size_of::<u32>()) },
            &BR_TRANSACTION_COMPLETE_CMD.to_ne_bytes()
        );

        let bwr_page = map_page();
        let bwr_ptr = bwr_page.cast::<binder_write_read>();
        unsafe {
            std::ptr::write(
                bwr_ptr,
                binder_write_read {
                    write_size: 0,
                    write_consumed: 0,
                    write_buffer: 0,
                    read_size: 0,
                    read_consumed: 0,
                    read_buffer: 0,
                },
            )
        };
        POST_IOCTL_PROTECT_ADDRESS.store(bwr_page as usize, Ordering::SeqCst);
        assert_eq!(
            unsafe { new_ioctl(fd, BINDER_WRITE_READ as c_int, bwr_ptr.cast()) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EFAULT)
        );
        assert_eq!(COPYBACK_IOCTL_CALLS.load(Ordering::SeqCst), 2);

        assert_eq!(
            unsafe { libc::mprotect(bwr_page, page_size, libc::PROT_READ | libc::PROT_WRITE,) },
            0
        );
        assert_eq!(
            unsafe { new_ioctl(fd, BINDER_WRITE_READ as c_int, bwr_ptr.cast()) },
            0
        );
        assert_eq!(COPYBACK_IOCTL_CALLS.load(Ordering::SeqCst), 2);
        OLD_IOCTL.store(previous, Ordering::SeqCst);
        unsafe {
            libc::munmap(read_page, page_size);
            libc::munmap(bwr_page, page_size);
        }
        reset_binder_fd_for_test(fd);
        unsafe { *libc::__errno() = 0 };
    }

    #[test]
    fn invalid_driver_consumption_poison_connection_without_replaying_ioctl() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = OLD_IOCTL.swap(invalid_consumption_ioctl as *mut c_void, Ordering::SeqCst);
        let mut write = BR_NOOP_CMD.to_ne_bytes();
        let mut read = [0u8; 2 * size_of::<u32>()];

        let assert_poisoned = |fd, bwr: &mut binder_write_read, mode| {
            reset_binder_fd_for_test(fd);
            BOUNDARY_IOCTL_CALLS.store(0, Ordering::SeqCst);
            BOUNDARY_IOCTL_MODE.store(mode, Ordering::SeqCst);
            for _ in 0..2 {
                assert_eq!(
                    unsafe {
                        new_ioctl(
                            fd,
                            BINDER_WRITE_READ as c_int,
                            (bwr as *mut binder_write_read).cast(),
                        )
                    },
                    -1
                );
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::EPROTO)
                );
            }
            assert_eq!(BOUNDARY_IOCTL_CALLS.load(Ordering::SeqCst), 1);
            assert!(PENDING_IOCTL_COPYBACKS
                .with(|pending| !pending.borrow().contains_key(&binder_state_key(fd))));
            reset_binder_fd_for_test(fd);
        };

        assert_poisoned(
            99,
            &mut binder_write_read {
                write_size: write.len(),
                write_consumed: 0,
                write_buffer: write.as_mut_ptr() as libc::c_ulong,
                read_size: 0,
                read_consumed: 0,
                read_buffer: 0,
            },
            0,
        );
        assert_poisoned(
            100,
            &mut binder_write_read {
                write_size: 0,
                write_consumed: 0,
                write_buffer: 0,
                read_size: read.len(),
                read_consumed: 0,
                read_buffer: read.as_mut_ptr() as libc::c_ulong,
            },
            1,
        );
        assert_poisoned(
            101,
            &mut binder_write_read {
                write_size: 0,
                write_consumed: 0,
                write_buffer: 0,
                read_size: read.len(),
                read_consumed: size_of::<u32>(),
                read_buffer: read.as_mut_ptr() as libc::c_ulong,
            },
            2,
        );

        OLD_IOCTL.store(previous, Ordering::SeqCst);
        unsafe { *libc::__errno() = 0 };
    }

    #[test]
    fn unsafe_reply_parcels_and_partial_commands_are_not_claimed() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fd = 93;
        let connection = binder_state_key(fd);
        let previous = OLD_IOCTL.swap(efault_ioctl as *mut c_void, Ordering::SeqCst);
        let mut tr: binder_transaction_data = unsafe { std::mem::zeroed() };
        tr.data_size = 1;
        tr.data.ptr.buffer = 1;
        let mut write = Vec::new();
        push_unaligned(&mut write, &BC_REPLY_CMD);
        push_unaligned(&mut write, &tr);
        let mut bwr = binder_write_read {
            write_size: write.len(),
            write_consumed: 0,
            write_buffer: write.as_mut_ptr() as libc::c_ulong,
            read_size: 0,
            read_consumed: 0,
            read_buffer: 0,
        };

        reset_pending_reply_frames_for_test(connection, 1);
        assert_eq!(
            unsafe {
                new_ioctl(
                    fd,
                    BINDER_WRITE_READ as c_int,
                    (&mut bwr as *mut binder_write_read).cast(),
                )
            },
            -1
        );
        assert_eq!(pending_reply_frame_claims_for_test(connection), vec![false]);

        tr.data_size = 0;
        tr.data.ptr.buffer = 0;
        write.clear();
        push_unaligned(&mut write, &BC_REPLY_CMD);
        push_unaligned(&mut write, &tr);
        bwr.write_size = write.len();
        bwr.write_consumed = size_of::<u32>() + 1;
        bwr.write_buffer = write.as_mut_ptr() as libc::c_ulong;
        assert_eq!(
            unsafe {
                new_ioctl(
                    fd,
                    BINDER_WRITE_READ as c_int,
                    (&mut bwr as *mut binder_write_read).cast(),
                )
            },
            -1
        );
        assert_eq!(pending_reply_frame_claims_for_test(connection), vec![false]);

        OLD_IOCTL.store(previous, Ordering::SeqCst);
        reset_pending_reply_frames_for_test(connection, 0);
        reset_binder_fd_for_test(fd);
        unsafe { *libc::__errno() = 0 };
    }

    #[test]
    fn reused_fd_fails_closed_once_then_accepts_the_new_generation() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fd = 89;
        reset_binder_fd_for_test(fd);

        let original = synchronize_binder_fd_generation(fd).unwrap();
        invalidate_binder_fd(fd);
        assert_eq!(synchronize_binder_fd_generation(fd), Err(original));
        let replacement = synchronize_binder_fd_generation(fd).unwrap();

        assert_ne!(replacement.connection, original.connection);
        assert!(!binder_fd_token_is_current(original));
        assert!(binder_fd_token_is_current(replacement));

        reset_binder_fd_for_test(fd);
    }

    #[test]
    fn duplicated_binder_fds_share_connection_state_until_the_last_close() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let original_fd = 94;
        let alias_fd = 95;
        reset_binder_fd_for_test(original_fd);
        reset_binder_fd_for_test(alias_fd);

        let original = binder_fd_token(original_fd);
        assert_eq!(
            unsafe { duplicate_binder_fd_with_lifecycle(original_fd, None, || alias_fd) },
            alias_fd
        );
        let alias = binder_fd_token(alias_fd);
        assert_eq!(alias.connection, original.connection);
        assert_eq!(alias.generation, original.generation);

        record_transaction_completion(original_fd, false, false, None);
        assert_eq!(complete_transaction_submission(alias_fd), Some(()));
        assert_eq!(
            unsafe { close_with_binder_fd_lifecycle(original_fd, || 0) },
            0
        );
        assert!(binder_fd_token_is_current(alias));
        assert_eq!(unsafe { close_with_binder_fd_lifecycle(alias_fd, || 0) }, 0);
        assert!(!binder_fd_token_is_current(alias));

        reset_binder_fd_for_test(original_fd);
        reset_binder_fd_for_test(alias_fd);
    }

    #[test]
    fn binder_fd_duplicated_before_first_io_shares_its_lifecycle() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fd = unsafe { libc::open(c"/dev/binder".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        assert!(fd >= 0, "test requires the Android Binder driver");
        assert!(existing_binder_fd_lifecycle(fd).is_none());

        let alias = unsafe { duplicate_binder_fd_with_lifecycle(fd, None, || libc::dup(fd)) };
        assert!(alias >= 0);
        let source = existing_binder_fd_lifecycle(fd).expect("source should be registered");
        let destination =
            existing_binder_fd_lifecycle(alias).expect("destination should inherit source");
        assert!(Arc::ptr_eq(&source, &destination));

        assert_eq!(
            unsafe { close_with_binder_fd_lifecycle(alias, || libc::close(alias)) },
            0
        );
        assert_eq!(
            unsafe { close_with_binder_fd_lifecycle(fd, || libc::close(fd)) },
            0
        );
    }

    #[test]
    fn reused_fd_never_submits_an_ambiguous_write() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fd = 88;
        reset_binder_fd_for_test(fd);

        synchronize_binder_fd_generation(fd).unwrap();
        invalidate_binder_fd(fd);
        let mut write = Vec::new();
        push_unaligned(&mut write, &BC_FREE_BUFFER_CMD);
        push_unaligned(&mut write, &0usize);
        let mut bwr = binder_write_read {
            write_size: write.len(),
            write_consumed: 0,
            write_buffer: write.as_mut_ptr() as libc::c_ulong,
            read_size: 0,
            read_consumed: 0,
            read_buffer: 0,
        };
        let previous = OLD_IOCTL.swap(capture_reply_ioctl as *mut c_void, Ordering::SeqCst);

        assert_eq!(
            unsafe {
                new_ioctl(
                    fd,
                    BINDER_WRITE_READ as c_int,
                    &mut bwr as *mut binder_write_read as *mut c_void,
                )
            },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EBADF)
        );
        assert_eq!(bwr.write_consumed, 0);

        bwr.write_size = 0;
        bwr.write_consumed = 0;
        assert_eq!(
            unsafe {
                new_ioctl(
                    fd,
                    BINDER_WRITE_READ as c_int,
                    &mut bwr as *mut binder_write_read as *mut c_void,
                )
            },
            0
        );

        OLD_IOCTL.store(previous, Ordering::SeqCst);
        reset_binder_fd_for_test(fd);
    }

    #[test]
    fn binder_fd_close_and_replacement_serialize_with_ioctl() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fd = 91;
        reset_binder_fd_for_test(fd);
        let original = binder_fd_token(fd);
        assert!(binder_fd_token_is_current(original));
        let lifecycle = binder_fd_lifecycle(fd);
        let ioctl = lifecycle
            .state
            .lock()
            .expect("binder fd lifecycle poisoned");
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();

        let close_thread = std::thread::spawn(move || {
            let result = unsafe {
                close_with_binder_fd_lifecycle(fd, || {
                    started_tx.send(()).unwrap();
                    0
                })
            };
            done_tx.send(result).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(done_rx.try_recv().is_err());

        drop(ioctl);
        assert_eq!(done_rx.recv().unwrap(), 0);
        close_thread.join().unwrap();
        assert!(!binder_fd_token_is_current(original));

        let after_close = binder_fd_token(fd);
        unsafe {
            *libc::__errno() = libc::EIO;
        }
        assert_eq!(
            unsafe {
                duplicate_binder_fd_with_lifecycle(fd + 1, Some(fd), || {
                    *libc::__errno() = libc::EPERM;
                    -1
                })
            },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        );
        assert!(binder_fd_token_is_current(after_close));
        assert_eq!(
            unsafe { duplicate_binder_fd_with_lifecycle(fd + 1, Some(fd), || fd) },
            fd
        );
        assert!(!binder_fd_token_is_current(after_close));

        reset_binder_fd_for_test(fd);
        reset_binder_fd_for_test(fd + 1);
    }

    #[test]
    fn blocking_binder_read_does_not_hold_the_fd_lifecycle_lock() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut pipe = [-1; 2];
        assert_eq!(
            unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) },
            0
        );
        let fd = pipe[0];
        reset_binder_fd_for_test(fd);
        let token = binder_fd_token(fd);
        let lifecycle = existing_binder_fd_lifecycle(fd).unwrap();
        let ioctl_thread = std::thread::spawn(move || unsafe {
            call_binder_ioctl(
                token,
                blocking_ioctl,
                BINDER_WRITE_READ as c_int,
                std::ptr::null_mut(),
            )
        });
        PINNED_IOCTL_ENTERED.wait();
        let pinned_fd = *lifecycle.pinned_fd.get().unwrap();

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let close_thread = std::thread::spawn(move || {
            let result = unsafe { close_with_binder_fd_lifecycle(fd, || 7) };
            done_tx.send(result).unwrap();
        });
        let close_result = done_rx.recv_timeout(Duration::from_secs(1));
        assert!(unsafe { libc::fcntl(pinned_fd, libc::F_GETFD) } >= 0);
        PINNED_IOCTL_RELEASE.wait();

        assert_eq!(ioctl_thread.join().unwrap(), BinderIoctlCall::Called(0));
        close_thread.join().unwrap();
        assert_eq!(close_result.unwrap(), 7);
        drop(lifecycle);
        assert_eq!(unsafe { libc::fcntl(pinned_fd, libc::F_GETFD) }, -1);
        unsafe {
            libc::syscall(libc::SYS_close, pipe[0]);
            libc::syscall(libc::SYS_close, pipe[1]);
        }
        reset_binder_fd_for_test(fd);
    }

    #[test]
    fn binder_ioctl_reuses_one_pin_until_connection_retires() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut pipe = [-1; 2];
        assert_eq!(
            unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) },
            0
        );
        let fd = pipe[0];
        reset_binder_fd_for_test(fd);
        let token = binder_fd_token(fd);

        let first = BinderIoctlGuard::begin(token).unwrap();
        let pinned_fd = first.fd();
        assert_ne!(pinned_fd, fd);
        drop(first);
        assert!(unsafe { libc::fcntl(pinned_fd, libc::F_GETFD) } >= 0);

        let second = BinderIoctlGuard::begin(token).unwrap();
        assert_eq!(second.fd(), pinned_fd);
        drop(second);

        assert_eq!(
            unsafe { close_with_binder_fd_lifecycle(fd, || libc::close(fd)) },
            0
        );
        assert_eq!(unsafe { libc::fcntl(pinned_fd, libc::F_GETFD) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EBADF)
        );
        unsafe {
            libc::close(pipe[1]);
        }
    }

    #[test]
    fn completed_ioctl_keeps_its_result_after_fd_reuse() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fd = 92;
        reset_binder_fd_for_test(fd);
        POST_IOCTL_INVALIDATE_FD.store(fd, Ordering::SeqCst);

        let mut write = Vec::new();
        push_unaligned(&mut write, &BC_FREE_BUFFER_CMD);
        push_unaligned(&mut write, &0usize);
        let mut read = [0u8; size_of::<u32>()];
        let mut bwr = binder_write_read {
            write_size: write.len(),
            write_consumed: 0,
            write_buffer: write.as_mut_ptr() as libc::c_ulong,
            read_size: read.len(),
            read_consumed: 0,
            read_buffer: read.as_mut_ptr() as libc::c_ulong,
        };
        let previous = OLD_IOCTL.swap(
            invalidate_after_consuming_ioctl as *mut c_void,
            Ordering::SeqCst,
        );

        assert_eq!(
            unsafe {
                new_ioctl(
                    fd,
                    BINDER_WRITE_READ as c_int,
                    &mut bwr as *mut binder_write_read as *mut c_void,
                )
            },
            0
        );
        assert_eq!(bwr.write_consumed, bwr.write_size);
        assert_eq!(bwr.read_consumed, size_of::<u32>());
        assert_eq!(read, BR_NOOP_CMD.to_ne_bytes());

        OLD_IOCTL.store(previous, Ordering::SeqCst);
        reset_binder_fd_for_test(fd);
    }

    #[test]
    fn operation_node_query_requires_an_exact_node() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let target = LocalBinderTarget {
            ptr: 0x30,
            cookie: 0x40,
        };
        reset_binder_fd_for_test(19);
        let binder = synchronize_binder_fd_generation(19).unwrap();
        let probe = OperationPublicationProbe {
            target,
            binder,
            generation: 1,
            not_before: Instant::now(),
            query_failures: 0,
        };
        let info = |ptr, cookie, has_strong_ref, has_weak_ref| binder_node_debug_info {
            ptr,
            cookie,
            has_strong_ref,
            has_weak_ref,
        };
        let queue = |results| {
            *NODE_QUERY_RESULTS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = results;
        };

        queue(VecDeque::from([Ok(info(0x30, 0x40, 1, 0))]));
        assert_eq!(
            unsafe { operation_binder_node_exists(node_query_ioctl, probe) },
            Ok(true)
        );

        queue(VecDeque::from([Ok(info(0x30, 0x40, 0, 0))]));
        assert_eq!(
            unsafe { operation_binder_node_exists(node_query_ioctl, probe) },
            Ok(true)
        );

        queue(VecDeque::from([Ok(info(0x30, 0x40, 0, 1))]));
        assert_eq!(
            unsafe { operation_binder_node_exists(node_query_ioctl, probe) },
            Ok(true)
        );

        queue(VecDeque::from([Ok(info(0x50, 2, 0, 0))]));
        assert_eq!(
            unsafe { operation_binder_node_exists(node_query_ioctl, probe) },
            Ok(false)
        );

        queue(VecDeque::from([Ok(info(0x30, 0x41, 1, 0))]));
        assert_eq!(
            unsafe { operation_binder_node_exists(node_query_ioctl, probe) },
            Ok(false)
        );

        queue(VecDeque::from([Err(libc::EIO)]));
        assert_eq!(
            unsafe { operation_binder_node_exists(node_query_ioctl, probe) },
            Err(libc::EIO)
        );

        queue(VecDeque::from([Err(libc::EBADF)]));
        assert_eq!(
            unsafe { operation_binder_node_exists(node_query_ioctl, probe) },
            Err(libc::EBADF)
        );

        queue(VecDeque::from([Err(libc::ENOTTY)]));
        assert_eq!(
            unsafe { operation_binder_node_exists(node_query_ioctl, probe) },
            Err(libc::ENOTTY)
        );

        invalidate_binder_fd(19);
        queue(VecDeque::from([Ok(info(0x30, 0x40, 1, 0))]));
        assert_eq!(
            unsafe { operation_binder_node_exists(node_query_ioctl, probe) },
            Ok(false)
        );
        assert_eq!(
            NODE_QUERY_RESULTS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len(),
            1,
            "retired publication must not query a reused Binder fd"
        );
        NODE_QUERY_RESULTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        reset_binder_fd_for_test(19);
    }

    #[test]
    fn fatal_zero_progress_host_reply_aborts_its_prepared_frame() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fd = 24;
        let connection = binder_state_key(fd);
        drain_transaction_completions(fd);
        reset_pending_reply_frames_for_test(connection, 2);
        *CAPTURED_REPLY_DATA
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;

        let mut tr: binder_transaction_data = unsafe { std::mem::zeroed() };
        tr.data.ptr.buffer = 0x1111;
        let mut write = Vec::new();
        push_unaligned(&mut write, &BC_REPLY_CMD);
        push_unaligned(&mut write, &tr);
        let mut bwr = binder_write_read {
            write_size: write.len(),
            write_consumed: 0,
            write_buffer: write.as_mut_ptr() as libc::c_ulong,
            read_size: 0,
            read_consumed: 0,
            read_buffer: 0,
        };
        let previous = OLD_IOCTL.swap(fail_reply_ioctl as *mut c_void, Ordering::SeqCst);

        assert_eq!(
            unsafe {
                new_ioctl(
                    fd,
                    BINDER_WRITE_READ as c_int,
                    &mut bwr as *mut binder_write_read as *mut c_void,
                )
            },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EIO)
        );
        assert_eq!(pending_reply_frame_claims_for_test(connection), vec![false]);
        PREPARED_BC_REPLIES.with(|prepared| assert!(!prepared.borrow().contains_key(&connection)));

        tr.data.ptr.buffer = 0x2222;
        write.clear();
        push_unaligned(&mut write, &BC_REPLY_CMD);
        push_unaligned(&mut write, &tr);
        bwr.write_size = write.len();
        bwr.write_consumed = 0;
        bwr.write_buffer = write.as_mut_ptr() as libc::c_ulong;
        OLD_IOCTL.store(capture_reply_ioctl as *mut c_void, Ordering::SeqCst);
        assert_eq!(
            unsafe {
                new_ioctl(
                    fd,
                    BINDER_WRITE_READ as c_int,
                    &mut bwr as *mut binder_write_read as *mut c_void,
                )
            },
            0
        );
        assert_eq!(pending_reply_frame_count_for_test(connection), 0);
        PREPARED_BC_REPLIES.with(|prepared| assert!(!prepared.borrow().contains_key(&connection)));

        OLD_IOCTL.store(previous, Ordering::SeqCst);
        reset_pending_reply_frames_for_test(connection, 0);
        drain_transaction_completions(fd);
        CAPTURED_REPLY_DATA
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        unsafe { *libc::__errno() = 0 };
    }

    #[test]
    fn fatal_partial_host_reply_commits_prefix_and_aborts_suffix() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fd = 6;
        let connection = binder_state_key(fd);
        drain_transaction_completions(fd);
        reset_pending_reply_frames_for_test(connection, 3);
        HOST_IOCTL_CALLS.store(0, Ordering::SeqCst);

        let tr: binder_transaction_data = unsafe { std::mem::zeroed() };
        let mut write = Vec::new();
        push_unaligned(&mut write, &BC_REPLY_CMD);
        push_unaligned(&mut write, &tr);
        push_unaligned(&mut write, &BC_REPLY_CMD);
        push_unaligned(&mut write, &tr);
        let mut bwr = binder_write_read {
            write_size: write.len(),
            write_consumed: 0,
            write_buffer: write.as_mut_ptr() as libc::c_ulong,
            read_size: 0,
            read_consumed: 0,
            read_buffer: 0,
        };
        let previous = OLD_IOCTL.swap(retry_host_reply_ioctl as *mut c_void, Ordering::SeqCst);

        assert_eq!(
            unsafe {
                new_ioctl(
                    fd,
                    BINDER_WRITE_READ as c_int,
                    &mut bwr as *mut binder_write_read as *mut c_void,
                )
            },
            0
        );
        assert_eq!(pending_reply_frame_count_for_test(connection), 3);

        assert_eq!(
            unsafe {
                new_ioctl(
                    fd,
                    BINDER_WRITE_READ as c_int,
                    &mut bwr as *mut binder_write_read as *mut c_void,
                )
            },
            -1
        );
        assert_eq!(pending_reply_frame_count_for_test(connection), 1);
        PREPARED_BC_REPLIES.with(|prepared| assert!(!prepared.borrow().contains_key(&connection)));

        write.clear();
        push_unaligned(&mut write, &BC_REPLY_CMD);
        push_unaligned(&mut write, &tr);
        bwr.write_buffer = write.as_mut_ptr() as libc::c_ulong;
        bwr.write_size = write.len();
        bwr.write_consumed = 0;

        assert_eq!(
            unsafe {
                new_ioctl(
                    fd,
                    BINDER_WRITE_READ as c_int,
                    &mut bwr as *mut binder_write_read as *mut c_void,
                )
            },
            0
        );
        assert_eq!(pending_reply_frame_count_for_test(connection), 0);

        OLD_IOCTL.store(previous, Ordering::SeqCst);
        reset_pending_reply_frames_for_test(connection, 0);
        drain_transaction_completions(fd);
        unsafe { *libc::__errno() = 0 };
    }

    #[test]
    fn fatal_partial_write_retains_unconsumed_operation_acquire() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _route_guard = crate::tracker::state_test_guard();
        let fd = 32;
        let target = LocalBinderTarget {
            ptr: 0x1234,
            cookie: 0x5678,
        };
        let retirement = register_operation_publication_for_test(target);
        let connection = binder_state_key(fd);
        bind_operation_publication_connection(retirement, connection);
        assert_eq!(
            mark_operation_publication_acquire_pending(target, connection),
            Some(retirement)
        );

        let mut transaction: binder_transaction_data = unsafe { std::mem::zeroed() };
        transaction.flags = TF_ONE_WAY;
        let transaction_command = BC_REPLY_CMD - BC_REPLY_NR + BC_TRANSACTION_NR;
        let mut write = Vec::new();
        push_unaligned(&mut write, &transaction_command);
        push_unaligned(&mut write, &transaction);
        push_unaligned(&mut write, &BC_ACQUIRE_DONE_CMD);
        push_unaligned(
            &mut write,
            &binder_ptr_cookie {
                ptr: target.ptr,
                cookie: target.cookie,
            },
        );
        let mut bwr = binder_write_read {
            write_size: write.len(),
            write_consumed: 0,
            write_buffer: write.as_mut_ptr() as libc::c_ulong,
            read_size: 0,
            read_consumed: 0,
            read_buffer: 0,
        };
        HOST_IOCTL_CALLS.store(1, Ordering::SeqCst);
        let previous = OLD_IOCTL.swap(retry_host_reply_ioctl as *mut c_void, Ordering::SeqCst);

        assert_eq!(
            unsafe {
                new_ioctl(
                    fd,
                    BINDER_WRITE_READ as c_int,
                    (&mut bwr as *mut binder_write_read).cast(),
                )
            },
            -1
        );
        assert_eq!(
            bwr.write_consumed,
            size_of::<u32>() + size_of::<binder_transaction_data>()
        );
        assert_eq!(
            mark_operation_publication_acquire_pending(target, connection),
            None
        );

        cancel_operation_publication_acquire_pending(retirement);
        finish_local_operation_publication(retirement);
        drain_transaction_completions(fd);
        OLD_IOCTL.store(previous, Ordering::SeqCst);
        reset_binder_fd_for_test(fd);
        unsafe { *libc::__errno() = 0 };
    }

    #[test]
    fn partial_eintr_retry_registers_each_host_transaction_once() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fd = 25;
        drain_transaction_completions(fd);
        HOST_IOCTL_CALLS.store(0, Ordering::SeqCst);

        let mut tr: binder_transaction_data = unsafe { std::mem::zeroed() };
        tr.flags = TF_ONE_WAY;
        let command = BC_REPLY_CMD - BC_REPLY_NR + BC_TRANSACTION_NR;
        let mut write = Vec::new();
        push_unaligned(&mut write, &command);
        push_unaligned(&mut write, &tr);
        push_unaligned(&mut write, &command);
        push_unaligned(&mut write, &tr);
        let mut bwr = binder_write_read {
            write_size: write.len(),
            write_consumed: 0,
            write_buffer: write.as_mut_ptr() as libc::c_ulong,
            read_size: 0,
            read_consumed: 0,
            read_buffer: 0,
        };
        let previous = OLD_IOCTL.swap(
            partial_eintr_host_write_ioctl as *mut c_void,
            Ordering::SeqCst,
        );

        assert_eq!(
            unsafe {
                new_ioctl(
                    fd,
                    BINDER_WRITE_READ as c_int,
                    &mut bwr as *mut binder_write_read as *mut c_void,
                )
            },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EINTR)
        );
        assert_eq!(
            unsafe {
                new_ioctl(
                    fd,
                    BINDER_WRITE_READ as c_int,
                    &mut bwr as *mut binder_write_read as *mut c_void,
                )
            },
            0
        );
        assert_eq!(complete_transaction_submission(fd), Some(()));
        assert_eq!(complete_transaction_submission(fd), Some(()));
        assert_eq!(complete_transaction_submission(fd), None);

        OLD_IOCTL.store(previous, Ordering::SeqCst);
        drain_transaction_completions(fd);
        unsafe { *libc::__errno() = 0 };
    }

    #[test]
    fn write_error_preserves_kernel_read_consumed_reset() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fd = 26;
        let mut write = [0u8; size_of::<u32>()];
        let mut read = [0x5au8; 2 * size_of::<u32>()];
        let mut bwr = binder_write_read {
            write_size: write.len(),
            write_consumed: 0,
            write_buffer: write.as_mut_ptr() as libc::c_ulong,
            read_size: read.len(),
            read_consumed: size_of::<u32>(),
            read_buffer: read.as_mut_ptr() as libc::c_ulong,
        };
        let previous = OLD_IOCTL.swap(
            write_error_resets_accumulated_read_ioctl as *mut c_void,
            Ordering::SeqCst,
        );

        assert_eq!(
            unsafe {
                new_ioctl(
                    fd,
                    BINDER_WRITE_READ as c_int,
                    (&mut bwr as *mut binder_write_read).cast(),
                )
            },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EINTR)
        );
        assert_eq!(bwr.read_consumed, 0);
        assert_eq!(read, [0x5a; 2 * size_of::<u32>()]);

        OLD_IOCTL.store(previous, Ordering::SeqCst);
        reset_binder_fd_for_test(fd);
        unsafe { *libc::__errno() = 0 };
    }

    #[test]
    fn zero_progress_host_reply_keeps_its_frame_across_nested_transaction() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fd = 21;
        let connection = binder_state_key(fd);
        drain_transaction_completions(fd);
        reset_pending_reply_frames_for_test(connection, 1);
        INTERLEAVED_REPLY_IOCTL_CALLS.store(0, Ordering::SeqCst);

        let tr: binder_transaction_data = unsafe { std::mem::zeroed() };
        let mut write = Vec::new();
        push_unaligned(&mut write, &BC_REPLY_CMD);
        push_unaligned(&mut write, &tr);
        let mut read = vec![0u8; size_of::<u32>() + size_of::<binder_transaction_data>()];
        let mut bwr = binder_write_read {
            write_size: write.len(),
            write_consumed: 0,
            write_buffer: write.as_mut_ptr() as libc::c_ulong,
            read_size: read.len(),
            read_consumed: 0,
            read_buffer: read.as_mut_ptr() as libc::c_ulong,
        };
        let previous = OLD_IOCTL.swap(
            interleaved_host_reply_ioctl as *mut c_void,
            Ordering::SeqCst,
        );

        assert_eq!(
            unsafe {
                new_ioctl(
                    fd,
                    BINDER_WRITE_READ as c_int,
                    &mut bwr as *mut binder_write_read as *mut c_void,
                )
            },
            0
        );
        assert_eq!(bwr.write_consumed, 0);
        assert_eq!(
            pending_reply_frame_claims_for_test(connection),
            vec![true, false]
        );

        assert_eq!(
            unsafe {
                new_ioctl(
                    fd,
                    BINDER_WRITE_READ as c_int,
                    &mut bwr as *mut binder_write_read as *mut c_void,
                )
            },
            0
        );
        assert_eq!(bwr.write_consumed, bwr.write_size);
        assert_eq!(pending_reply_frame_claims_for_test(connection), vec![false]);

        OLD_IOCTL.store(previous, Ordering::SeqCst);
        reset_pending_reply_frames_for_test(connection, 0);
        drain_transaction_completions(fd);
    }

    #[test]
    fn terminal_results_preserve_nested_sync_completion_order() {
        let _guard = SYNTHETIC_REPLY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fd = 9;
        drain_transaction_completions(fd);

        record_transaction_completion(fd, false, true, None);
        assert_eq!(complete_transaction_submission(fd), Some(()));
        record_transaction_completion(fd, false, true, None);
        complete_failed_transaction_submission(fd, BR_DEAD_REPLY_NR);
        assert_eq!(complete_transaction_submission(fd), None);
        complete_failed_transaction_submission(fd, BR_FROZEN_REPLY_NR);
        assert!(!complete_sync_transaction(
            fd,
            SyncTransactionState::AwaitingReply
        ));

        record_transaction_completion(fd, false, true, None);
        assert_eq!(complete_transaction_submission(fd), Some(()));
        assert!(complete_sync_transaction(
            fd,
            SyncTransactionState::AwaitingReply
        ));
        record_transaction_completion(fd, false, true, None);
        complete_failed_transaction_submission(fd, BR_FAILED_REPLY_NR);
        assert_eq!(complete_transaction_submission(fd), None);

        record_transaction_completion(fd, false, true, None);
        assert_eq!(complete_transaction_submission(fd), Some(()));
        record_transaction_completion(fd, false, false, None);
        complete_failed_transaction_submission(fd, BR_DEAD_REPLY_NR);
        assert_eq!(complete_transaction_submission(fd), None);
        assert!(complete_sync_transaction(
            fd,
            SyncTransactionState::AwaitingReply
        ));
        drain_transaction_completions(fd);
    }
}
