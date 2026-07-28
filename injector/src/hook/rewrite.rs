use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, LazyLock, Mutex, OnceLock,
};
use std::time::{Duration, Instant};

use log::{debug, info, warn};
use rsbinder::{ExceptionCode, Status, StatusCode};
use serde::{Deserialize, Serialize};

use super::binder::{
    binder_transaction_data, create_native_operation_binder, create_native_security_level_binder,
    describe_transaction_objects, format_target, parse_local_binder_target_from_parcel_bytes,
    LocalBinderTarget, NativeBinder, NativeBinderRetirement,
};
use super::{BinderFdToken, BinderStateKey};
use crate::android::system::keystore2::Domain::Domain;
use crate::android::system::keystore2::KeyDescriptor::KeyDescriptor;
use crate::android::system::keystore2::KeyEntryResponse::KeyEntryResponse;
use crate::android::system::keystore2::ResponseCode::ResponseCode;
use crate::config;
use crate::filter::{self, FilterReason, PackageResolution};
use crate::forward::{self, BypassGuard};
use crate::identify::{
    self, AidlMetadataMethod, AuthorizationMethod, OperationMethod, SecurityLevelMethod,
    ServiceMethod,
};
use crate::ipc;
use crate::parcel::{
    self, ParsedAuthorizationRequest, ParsedMaintenanceRequest, ParsedOperationRequest,
    ParsedSecurityLevelRequest, ParsedServiceRequest,
};
use crate::route::{AospOperationBinder, CallerIdentity, RouteTarget};
use crate::top::qwq2333::ohmykeymint::CallerInfo::CallerInfo;
use crate::tracker::{self, SecurityLevelTargetInfo};

struct PendingAuthorizationCall {
    request: ParsedAuthorizationRequest,
    method: AuthorizationMethod,
    caller: CallerIdentity,
    mirror_update: Option<MirrorUpdateReservation>,
}

struct PendingMaintenanceCall {
    request: ParsedMaintenanceRequest,
    caller: CallerIdentity,
    route: RouteTarget,
    mirror_update: Option<MirrorUpdateReservation>,
}

struct PendingServiceCall {
    request: ParsedServiceRequest,
    caller: CallerIdentity,
    packages: Vec<String>,
    route: RouteTarget,
}

struct PendingSecurityLevelCall {
    request: ParsedSecurityLevelRequest,
    caller: CallerIdentity,
    packages: Vec<String>,
    route: RouteTarget,
    security_level: crate::android::hardware::security::keymint::SecurityLevel::SecurityLevel,
}

struct PendingOperationCall {
    request: ParsedOperationRequest,
    caller: CallerIdentity,
    target: LocalBinderTarget,
}

enum PrecomputedServiceReply {
    UpdateSubcomponentSuccess,
    GrantSuccess(KeyDescriptor),
    UngrantSuccess,
    DeleteKeySuccess,
    Error(Status),
}

enum PrecomputedMaintenanceReply {
    Success,
    Error(Status),
}

enum OmkServicePrecompute {
    Reply(PrecomputedServiceReply),
    PreserveSystem,
}

enum OmkMigratePrecompute {
    Reply(PrecomputedMaintenanceReply),
    PreserveSystem,
}

enum PendingCall {
    Authorization(PendingAuthorizationCall),
    PrecomputedAuthorization(PendingAuthorizationCall, Status),
    Maintenance(PendingMaintenanceCall),
    PrecomputedMaintenance(PendingMaintenanceCall, PrecomputedMaintenanceReply),
    Service(PendingServiceCall),
    PrecomputedService(PendingServiceCall, PrecomputedServiceReply),
    SecurityLevel(PendingSecurityLevelCall),
    PrecomputedSecurityLevel(PendingSecurityLevelCall, Box<Option<OutboundReply>>),
    Operation(PendingOperationCall),
}

struct PendingReplyFrame {
    id: u64,
    connection: BinderStateKey,
    pending: Option<PendingCall>,
    claimed: bool,
}

impl PendingCall {
    fn reply_log_context(&self) -> (&'static str, String, u32, i32) {
        match self {
            Self::Authorization(call) | Self::PrecomputedAuthorization(call, _) => (
                "authorization",
                format!("{:?}", call.method),
                call.caller.uid,
                call.caller.pid,
            ),
            Self::Maintenance(call) | Self::PrecomputedMaintenance(call, _) => (
                "maintenance",
                format!("{:?}", call.request.method()),
                call.caller.uid,
                call.caller.pid,
            ),
            Self::Service(call) | Self::PrecomputedService(call, _) => (
                "service",
                format!("{:?}", call.request.method()),
                call.caller.uid,
                call.caller.pid,
            ),
            Self::SecurityLevel(call) | Self::PrecomputedSecurityLevel(call, _) => (
                "security-level",
                format!("{:?}", call.request.method()),
                call.caller.uid,
                call.caller.pid,
            ),
            Self::Operation(call) => (
                "operation",
                format!("{:?}", call.request.method()),
                call.caller.uid,
                call.caller.pid,
            ),
        }
    }
}

thread_local! {
    static PENDING_REPLY_QUEUE: RefCell<Vec<PendingReplyFrame>> = RefCell::default();
    static OUTBOUND_REPLY_BUFFERS: RefCell<Vec<(BinderStateKey, parcel::OwnedReply)>> = RefCell::default();
}

#[derive(Clone)]
struct OperationTargetInfo {
    route: RouteTarget,
    aad_allowed: bool,
    backend: Option<AospOperationBinder>,
    finalized: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OperationPublication {
    generation: u64,
    acquire_pending: bool,
    acquire_owned: bool,
    connection: Option<BinderStateKey>,
    binder: Option<BinderFdToken>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct OperationPublicationProbe {
    pub target: LocalBinderTarget,
    pub binder: BinderFdToken,
    pub generation: u64,
    pub not_before: Instant,
    pub query_failures: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SyntheticTargetKind {
    SecurityLevel,
    Operation,
}

pub(super) enum SyntheticReply {
    Parcel(Box<parcel::OwnedReply>),
    Status(i32),
    NoReply,
}

type OutboundReply = parcel::OwnedReply;

#[derive(Clone, Debug)]
struct SyntheticTargetInfo {
    kind: SyntheticTargetKind,
    caller: Option<CallerIdentity>,
    native_generation: Option<u64>,
}

static OPERATION_TARGETS: LazyLock<Mutex<HashMap<LocalBinderTarget, OperationTargetInfo>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static SYNTHETIC_SECURITY_LEVEL_TARGETS: LazyLock<
    Mutex<
        HashMap<
            crate::android::hardware::security::keymint::SecurityLevel::SecurityLevel,
            LocalBinderTarget,
        >,
    >,
> = LazyLock::new(|| Mutex::new(HashMap::new()));
static SYNTHETIC_TARGETS: LazyLock<Mutex<HashMap<LocalBinderTarget, SyntheticTargetInfo>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static NATIVE_BINDERS: LazyLock<Mutex<HashMap<LocalBinderTarget, Arc<NativeBinder>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static OPERATION_PUBLICATIONS: LazyLock<Mutex<HashMap<LocalBinderTarget, OperationPublication>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static OPERATION_PUBLICATION_PROBES: LazyLock<Mutex<VecDeque<OperationPublicationProbe>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));
static KEYSTORE2_AIDL_METADATA: OnceLock<Result<Keystore2AidlMetadata, String>> = OnceLock::new();
static AUTHORIZATION_MIRROR_STATE_DIRTY: AtomicBool = AtomicBool::new(false);
static MAINTENANCE_MIRROR_STATE_DIRTY: AtomicBool = AtomicBool::new(false);
static MIRROR_RECOVERY_STATE: LazyLock<Mutex<MirrorRecoveryState>> =
    LazyLock::new(|| Mutex::new(MirrorRecoveryState::default()));
static MIRROR_RECOVERY_CONTROL: LazyLock<Mutex<MirrorRecoveryControl>> =
    LazyLock::new(|| Mutex::new(MirrorRecoveryControl::default()));
static MIRROR_RECOVERY_WORKER: OnceLock<Result<std::sync::mpsc::SyncSender<()>, String>> =
    OnceLock::new();
static NEXT_PENDING_REPLY_FRAME_ID: AtomicU64 = AtomicU64::new(1);
#[cfg(test)]
static NEXT_TEST_TARGET_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_OPERATION_PUBLICATION_GENERATION: AtomicU64 = AtomicU64::new(1);

const OPERATION_PUBLICATION_PROBE_GRACE: Duration = Duration::from_millis(250);
const OPERATION_PUBLICATION_REPROBE_DELAY: Duration = Duration::from_secs(1);
const OPERATION_PUBLICATION_MAX_QUERY_BACKOFF_SHIFT: u8 = 5;
const MAX_PENDING_OPERATION_PUBLICATIONS: usize = 64;
const MAX_PENDING_MIRROR_REPLAYS: usize = 64;
const MIRROR_RECOVERY_RETRY_DELAY: Duration = Duration::from_secs(1);
const MIRROR_RECOVERY_LEDGER_PATH: &str = "/data/misc/keystore/omk/injector-mirror-recovery.toml";
const KERNEL_BOOT_ID_PATH: &str = "/proc/sys/kernel/random/boot_id";
const MIRROR_RECOVERY_LEDGER_VERSION: u32 = 1;
const MAX_MIRROR_RECOVERY_LEDGER_BYTES: u64 = 1024 * 1024;
const KEYSTORE2_HAL_NAME: &str = "android.system.keystore2";
const KEYSTORE2_SERVICE_INTERFACE: &str = "IKeystoreService";
const KEYSTORE2_SERVICE_INSTANCE: &str = "default";

fn queue_operation_publication_probe(probe: OperationPublicationProbe) {
    OPERATION_PUBLICATION_PROBES
        .lock()
        .expect("operation publication probe queue poisoned")
        .push_back(probe);
    super::intercept::wake_operation_publication_worker();
}

pub(super) fn next_operation_publication_probe_deadline() -> Option<Instant> {
    OPERATION_PUBLICATION_PROBES
        .lock()
        .expect("operation publication probe queue poisoned")
        .iter()
        .map(|probe| probe.not_before)
        .min()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Keystore2AidlMetadata {
    version: i32,
    hash: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MirrorStateKind {
    Authorization,
    Maintenance,
}

#[derive(Clone)]
enum MirrorReplayEvent {
    Authorization {
        request: ParsedAuthorizationRequest,
        caller: CallerIdentity,
    },
    Maintenance {
        request: ParsedMaintenanceRequest,
        caller: CallerIdentity,
    },
}

#[derive(Clone)]
struct SequencedMirrorReplay {
    sequence: u64,
    kind: MirrorStateKind,
    event: Option<MirrorReplayEvent>,
    retry_not_before: Option<Instant>,
}

#[derive(Default)]
struct MirrorRecoveryLane {
    pending: VecDeque<SequencedMirrorReplay>,
    draining: bool,
    poisoned: bool,
}

#[derive(Default)]
struct MirrorRecoveryControl {
    path: Option<PathBuf>,
    boot_id: Option<String>,
    poisoned: bool,
}

struct MirrorRecoveryState {
    authorization: MirrorRecoveryLane,
    maintenance: MirrorRecoveryLane,
    next_sequence: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MirrorRecoveryLedger {
    version: u32,
    boot_id: String,
    authorization: PersistedMirrorLane,
    maintenance: PersistedMirrorLane,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedMirrorLane {
    poisoned: bool,
    awaiting_system: bool,
    pending: Vec<PersistedSequencedMirrorReplay>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedSequencedMirrorReplay {
    sequence: u64,
    event: PersistedMirrorReplayEvent,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "domain")]
enum PersistedMirrorReplayEvent {
    Authorization {
        caller: PersistedCallerIdentity,
        request: PersistedAuthorizationRequest,
    },
    Maintenance {
        caller: PersistedCallerIdentity,
        request: PersistedMaintenanceRequest,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedCallerIdentity {
    uid: u32,
    pid: i32,
    sid: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "method")]
enum PersistedAuthorizationRequest {
    DeviceUnlocked {
        user_id: i32,
    },
    DeviceLocked {
        user_id: i32,
        unlocking_sids: Vec<i64>,
        weak_unlock_enabled: bool,
    },
    UserStorageLocked {
        user_id: i32,
    },
    WeakUnlockMethodsExpired {
        user_id: i32,
    },
    NonLskfUnlockMethodsExpired {
        user_id: i32,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "method")]
enum PersistedMaintenanceRequest {
    OnUserAdded { user_id: i32 },
    OnUserRemoved { user_id: i32 },
    OnUserLskfRemoved { user_id: i32 },
    ClearNamespace { domain: i32, nspace: i64 },
    EarlyBootEnded,
    DeleteAllKeys,
}

struct MirrorUpdateReservation {
    kind: MirrorStateKind,
    sequence: u64,
    active: bool,
}

impl Default for MirrorRecoveryState {
    fn default() -> Self {
        Self {
            authorization: MirrorRecoveryLane::default(),
            maintenance: MirrorRecoveryLane::default(),
            next_sequence: 1,
        }
    }
}

impl MirrorRecoveryState {
    fn lane(&self, kind: MirrorStateKind) -> &MirrorRecoveryLane {
        match kind {
            MirrorStateKind::Authorization => &self.authorization,
            MirrorStateKind::Maintenance => &self.maintenance,
        }
    }

    fn lane_mut(&mut self, kind: MirrorStateKind) -> &mut MirrorRecoveryLane {
        match kind {
            MirrorStateKind::Authorization => &mut self.authorization,
            MirrorStateKind::Maintenance => &mut self.maintenance,
        }
    }

    fn pending_len(&self) -> usize {
        self.authorization.pending.len() + self.maintenance.pending.len()
    }

    fn next_pending(&self) -> Option<&SequencedMirrorReplay> {
        self.authorization
            .pending
            .front()
            .into_iter()
            .chain(self.maintenance.pending.front())
            .min_by_key(|pending| pending.sequence)
    }

    fn next_worker_wait(&self, now: Instant) -> Option<Duration> {
        if self.authorization.poisoned
            || self.maintenance.poisoned
            || self.authorization.draining
            || self.maintenance.draining
        {
            return None;
        }
        let pending = self.next_pending()?;
        pending.event.as_ref()?;
        Some(
            pending
                .retry_not_before
                .map(|deadline| deadline.saturating_duration_since(now))
                .unwrap_or(Duration::ZERO),
        )
    }
}

impl MirrorStateKind {
    fn dirty(self) -> &'static AtomicBool {
        match self {
            Self::Authorization => &AUTHORIZATION_MIRROR_STATE_DIRTY,
            Self::Maintenance => &MAINTENANCE_MIRROR_STATE_DIRTY,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Authorization => "authorization",
            Self::Maintenance => "maintenance",
        }
    }
}

impl MirrorReplayEvent {
    fn kind(&self) -> MirrorStateKind {
        match self {
            Self::Authorization { .. } => MirrorStateKind::Authorization,
            Self::Maintenance { .. } => MirrorStateKind::Maintenance,
        }
    }

    fn method(&self) -> String {
        match self {
            Self::Authorization { request, .. } => format!("{:?}", request.method()),
            Self::Maintenance { request, .. } => format!("{:?}", request.method()),
        }
    }

    fn caller(&self) -> &CallerIdentity {
        match self {
            Self::Authorization { caller, .. } | Self::Maintenance { caller, .. } => caller,
        }
    }

    fn execute(&self) -> anyhow::Result<()> {
        match self {
            Self::Authorization { request, caller } => {
                execute_authorization_mirror(request, caller)
            }
            Self::Maintenance { request, caller } => execute_maintenance_mirror(request, caller),
        }
    }

    fn persisted(&self) -> Option<PersistedMirrorReplayEvent> {
        match self {
            Self::Authorization { request, caller } => {
                let request = match request {
                    ParsedAuthorizationRequest::OnDeviceUnlocked {
                        user_id,
                        password: None,
                    } => PersistedAuthorizationRequest::DeviceUnlocked { user_id: *user_id },
                    ParsedAuthorizationRequest::OnDeviceLocked {
                        user_id,
                        unlocking_sids,
                        weak_unlock_enabled,
                    } => PersistedAuthorizationRequest::DeviceLocked {
                        user_id: *user_id,
                        unlocking_sids: unlocking_sids.clone(),
                        weak_unlock_enabled: *weak_unlock_enabled,
                    },
                    ParsedAuthorizationRequest::OnUserStorageLocked { user_id } => {
                        PersistedAuthorizationRequest::UserStorageLocked { user_id: *user_id }
                    }
                    ParsedAuthorizationRequest::OnWeakUnlockMethodsExpired { user_id } => {
                        PersistedAuthorizationRequest::WeakUnlockMethodsExpired {
                            user_id: *user_id,
                        }
                    }
                    ParsedAuthorizationRequest::OnNonLskfUnlockMethodsExpired { user_id } => {
                        PersistedAuthorizationRequest::NonLskfUnlockMethodsExpired {
                            user_id: *user_id,
                        }
                    }
                    _ => return None,
                };
                Some(PersistedMirrorReplayEvent::Authorization {
                    caller: caller.into(),
                    request,
                })
            }
            Self::Maintenance { request, caller } => {
                let request = match request {
                    ParsedMaintenanceRequest::OnUserAdded { user_id } => {
                        PersistedMaintenanceRequest::OnUserAdded { user_id: *user_id }
                    }
                    ParsedMaintenanceRequest::OnUserRemoved { user_id } => {
                        PersistedMaintenanceRequest::OnUserRemoved { user_id: *user_id }
                    }
                    ParsedMaintenanceRequest::OnUserLskfRemoved { user_id } => {
                        PersistedMaintenanceRequest::OnUserLskfRemoved { user_id: *user_id }
                    }
                    ParsedMaintenanceRequest::ClearNamespace { domain, nspace } => {
                        PersistedMaintenanceRequest::ClearNamespace {
                            domain: domain.0,
                            nspace: *nspace,
                        }
                    }
                    ParsedMaintenanceRequest::EarlyBootEnded => {
                        PersistedMaintenanceRequest::EarlyBootEnded
                    }
                    ParsedMaintenanceRequest::DeleteAllKeys => {
                        PersistedMaintenanceRequest::DeleteAllKeys
                    }
                    _ => return None,
                };
                Some(PersistedMirrorReplayEvent::Maintenance {
                    caller: caller.into(),
                    request,
                })
            }
        }
    }
}

impl From<&CallerIdentity> for PersistedCallerIdentity {
    fn from(caller: &CallerIdentity) -> Self {
        Self {
            uid: caller.uid,
            pid: caller.pid,
            sid: caller.sid.clone(),
        }
    }
}

impl From<PersistedCallerIdentity> for CallerIdentity {
    fn from(caller: PersistedCallerIdentity) -> Self {
        CallerIdentity::new(caller.uid, caller.pid).with_sid(caller.sid)
    }
}

impl PersistedMirrorReplayEvent {
    fn valid_after_reboot(&self) -> bool {
        matches!(
            self,
            Self::Maintenance {
                request: PersistedMaintenanceRequest::OnUserAdded { .. }
                    | PersistedMaintenanceRequest::OnUserRemoved { .. }
                    | PersistedMaintenanceRequest::OnUserLskfRemoved { .. }
                    | PersistedMaintenanceRequest::ClearNamespace { .. }
                    | PersistedMaintenanceRequest::DeleteAllKeys,
                ..
            }
        )
    }

    fn into_runtime(self) -> anyhow::Result<MirrorReplayEvent> {
        Ok(match self {
            Self::Authorization { caller, request } => {
                let request = match request {
                    PersistedAuthorizationRequest::DeviceUnlocked { user_id } => {
                        ParsedAuthorizationRequest::OnDeviceUnlocked {
                            user_id,
                            password: None,
                        }
                    }
                    PersistedAuthorizationRequest::DeviceLocked {
                        user_id,
                        unlocking_sids,
                        weak_unlock_enabled,
                    } => ParsedAuthorizationRequest::OnDeviceLocked {
                        user_id,
                        unlocking_sids,
                        weak_unlock_enabled,
                    },
                    PersistedAuthorizationRequest::UserStorageLocked { user_id } => {
                        ParsedAuthorizationRequest::OnUserStorageLocked { user_id }
                    }
                    PersistedAuthorizationRequest::WeakUnlockMethodsExpired { user_id } => {
                        ParsedAuthorizationRequest::OnWeakUnlockMethodsExpired { user_id }
                    }
                    PersistedAuthorizationRequest::NonLskfUnlockMethodsExpired { user_id } => {
                        ParsedAuthorizationRequest::OnNonLskfUnlockMethodsExpired { user_id }
                    }
                };
                MirrorReplayEvent::Authorization {
                    request,
                    caller: caller.into(),
                }
            }
            Self::Maintenance { caller, request } => {
                let request = match request {
                    PersistedMaintenanceRequest::OnUserAdded { user_id } => {
                        ParsedMaintenanceRequest::OnUserAdded { user_id }
                    }
                    PersistedMaintenanceRequest::OnUserRemoved { user_id } => {
                        ParsedMaintenanceRequest::OnUserRemoved { user_id }
                    }
                    PersistedMaintenanceRequest::OnUserLskfRemoved { user_id } => {
                        ParsedMaintenanceRequest::OnUserLskfRemoved { user_id }
                    }
                    PersistedMaintenanceRequest::ClearNamespace { domain, nspace } => {
                        let domain = Domain(domain);
                        if !matches!(domain, Domain::APP | Domain::SELINUX) {
                            anyhow::bail!(
                                "mirror recovery ledger contains invalid clearNamespace domain {}",
                                domain.0
                            );
                        }
                        ParsedMaintenanceRequest::ClearNamespace { domain, nspace }
                    }
                    PersistedMaintenanceRequest::EarlyBootEnded => {
                        ParsedMaintenanceRequest::EarlyBootEnded
                    }
                    PersistedMaintenanceRequest::DeleteAllKeys => {
                        ParsedMaintenanceRequest::DeleteAllKeys
                    }
                };
                MirrorReplayEvent::Maintenance {
                    request,
                    caller: caller.into(),
                }
            }
        })
    }
}

fn persisted_mirror_lane(lane: &MirrorRecoveryLane) -> PersistedMirrorLane {
    let mut poisoned = lane.poisoned;
    let mut pending = Vec::new();
    for replay in &lane.pending {
        let Some(event) = replay.event.as_ref() else {
            continue;
        };
        let Some(event) = event.persisted() else {
            poisoned = true;
            continue;
        };
        pending.push(PersistedSequencedMirrorReplay {
            sequence: replay.sequence,
            event,
        });
    }
    PersistedMirrorLane {
        poisoned,
        awaiting_system: lane.pending.iter().any(|replay| replay.event.is_none()),
        pending,
    }
}

fn mirror_recovery_ledger_from_state(
    state: &MirrorRecoveryState,
    boot_id: &str,
) -> MirrorRecoveryLedger {
    MirrorRecoveryLedger {
        version: MIRROR_RECOVERY_LEDGER_VERSION,
        boot_id: boot_id.to_string(),
        authorization: persisted_mirror_lane(&state.authorization),
        maintenance: persisted_mirror_lane(&state.maintenance),
    }
}

fn restore_mirror_lane(
    kind: MirrorStateKind,
    persisted: PersistedMirrorLane,
    seen_sequences: &mut HashSet<u64>,
    max_sequence: &mut u64,
) -> anyhow::Result<MirrorRecoveryLane> {
    if persisted.pending.len() > MAX_PENDING_MIRROR_REPLAYS {
        anyhow::bail!(
            "mirror recovery ledger contains too many {} events",
            kind.label()
        );
    }
    if persisted.poisoned || persisted.awaiting_system {
        return Ok(MirrorRecoveryLane {
            poisoned: true,
            ..MirrorRecoveryLane::default()
        });
    }

    let mut lane = MirrorRecoveryLane::default();
    let mut previous_sequence = 0;
    for replay in persisted.pending {
        if replay.sequence == 0
            || replay.sequence <= previous_sequence
            || !seen_sequences.insert(replay.sequence)
        {
            anyhow::bail!("mirror recovery ledger contains invalid replay sequence");
        }
        previous_sequence = replay.sequence;
        *max_sequence = (*max_sequence).max(replay.sequence);
        let event = replay.event.into_runtime()?;
        if event.kind() != kind {
            anyhow::bail!("mirror recovery ledger stores an event in the wrong lane");
        }
        lane.pending.push_back(SequencedMirrorReplay {
            sequence: replay.sequence,
            kind,
            event: Some(event),
            retry_not_before: None,
        });
    }
    Ok(lane)
}

fn current_mirror_boot_id() -> anyhow::Result<String> {
    let boot_id =
        crate::utils::read_limited_string(Path::new(KERNEL_BOOT_ID_PATH), 128, "kernel boot ID")?
            .ok_or_else(|| anyhow::anyhow!("kernel boot ID is missing"))?;
    let boot_id = boot_id.trim();
    if boot_id.is_empty() {
        anyhow::bail!("kernel boot ID is empty");
    }
    Ok(boot_id.to_string())
}

fn load_mirror_recovery_state(
    path: &Path,
    current_boot_id: &str,
) -> anyhow::Result<MirrorRecoveryState> {
    let Some(contents) = crate::utils::read_limited_string(
        path,
        MAX_MIRROR_RECOVERY_LEDGER_BYTES,
        "mirror recovery ledger",
    )?
    else {
        return Ok(MirrorRecoveryState::default());
    };
    let mut ledger: MirrorRecoveryLedger = toml::from_str(&contents)
        .map_err(|error| anyhow::anyhow!("failed to parse mirror recovery ledger: {error}"))?;
    if ledger.version != MIRROR_RECOVERY_LEDGER_VERSION {
        anyhow::bail!(
            "unsupported mirror recovery ledger version {}",
            ledger.version
        );
    }
    if ledger.boot_id.trim().is_empty() {
        anyhow::bail!("mirror recovery ledger contains an empty boot ID");
    }
    if ledger.boot_id != current_boot_id {
        ledger.authorization.poisoned = false;
        ledger.authorization.awaiting_system = false;
        ledger.authorization.pending.clear();
        ledger
            .maintenance
            .pending
            .retain(|replay| replay.event.valid_after_reboot());
        info!("event=mirror discarded boot-scoped recovery state from a previous boot");
    }
    let mut seen_sequences = HashSet::new();
    let mut max_sequence = 0;
    let authorization = restore_mirror_lane(
        MirrorStateKind::Authorization,
        ledger.authorization,
        &mut seen_sequences,
        &mut max_sequence,
    )?;
    let maintenance = restore_mirror_lane(
        MirrorStateKind::Maintenance,
        ledger.maintenance,
        &mut seen_sequences,
        &mut max_sequence,
    )?;
    let next_sequence = max_sequence
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("mirror recovery sequence exhausted"))?
        .max(1);
    Ok(MirrorRecoveryState {
        authorization,
        maintenance,
        next_sequence,
    })
}

fn persist_mirror_recovery_state(state: &mut MirrorRecoveryState) -> anyhow::Result<()> {
    let mut control = MIRROR_RECOVERY_CONTROL
        .lock()
        .expect("mirror recovery control poisoned");
    let result = if control.poisoned {
        Err(anyhow::anyhow!(
            "mirror recovery persistence is fail-closed after an earlier error"
        ))
    } else if let Some(path) = control.path.as_deref() {
        (|| {
            let boot_id = control
                .boot_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("mirror recovery boot ID is unavailable"))?;
            let contents =
                toml::to_string_pretty(&mirror_recovery_ledger_from_state(state, boot_id))
                    .map_err(|error| {
                        anyhow::anyhow!("failed to serialize mirror recovery ledger: {error}")
                    })?;
            crate::utils::write_private_atomic(
                path,
                contents.as_bytes(),
                MAX_MIRROR_RECOVERY_LEDGER_BYTES,
                "mirror recovery ledger",
            )
        })()
    } else {
        Ok(())
    };
    if result.is_err() {
        control.poisoned = true;
        poison_mirror_state(state, None);
    }
    result
}

fn init_mirror_recovery_at(path: &Path) -> anyhow::Result<()> {
    let boot_id = current_mirror_boot_id()?;
    let state = load_mirror_recovery_state(path, &boot_id)?;
    let authorization_pending = state.authorization.pending.len();
    let maintenance_pending = state.maintenance.pending.len();
    let authorization_poisoned = state.authorization.poisoned;
    let maintenance_poisoned = state.maintenance.poisoned;
    let mut current = MIRROR_RECOVERY_STATE
        .lock()
        .expect("mirror recovery state poisoned");
    let mut control = MIRROR_RECOVERY_CONTROL
        .lock()
        .expect("mirror recovery control poisoned");
    *current = state;
    control.path = Some(path.to_path_buf());
    control.boot_id = Some(boot_id);
    control.poisoned = false;
    refresh_mirror_dirty_flags(&current);
    info!(
        "event=mirror loaded recovery ledger path={} authorization_pending={} maintenance_pending={} authorization_poisoned={} maintenance_poisoned={}",
        path.display(),
        authorization_pending,
        maintenance_pending,
        authorization_poisoned,
        maintenance_poisoned
    );
    Ok(())
}

pub(super) fn init_mirror_recovery() {
    let path = Path::new(MIRROR_RECOVERY_LEDGER_PATH);
    if let Err(error) = init_mirror_recovery_at(path) {
        let mut state = MIRROR_RECOVERY_STATE
            .lock()
            .expect("mirror recovery state poisoned");
        let mut control = MIRROR_RECOVERY_CONTROL
            .lock()
            .expect("mirror recovery control poisoned");
        *state = MirrorRecoveryState::default();
        poison_mirror_state(&mut state, None);
        control.path = Some(path.to_path_buf());
        control.boot_id = None;
        control.poisoned = true;
        log::error!(
            "event=mirror failed to load recovery ledger path={}: {:#}; OMK routes are fail-closed",
            path.display(),
            error
        );
    }
}

fn refresh_mirror_dirty_flags(state: &MirrorRecoveryState) {
    let authorization_dirty =
        state.authorization.poisoned || !state.authorization.pending.is_empty();
    let maintenance_dirty = state.maintenance.poisoned || !state.maintenance.pending.is_empty();
    AUTHORIZATION_MIRROR_STATE_DIRTY.store(authorization_dirty, Ordering::SeqCst);
    MAINTENANCE_MIRROR_STATE_DIRTY.store(maintenance_dirty, Ordering::SeqCst);
}

#[cfg(test)]
fn replayable_authorization_event(call: &PendingAuthorizationCall) -> Option<MirrorReplayEvent> {
    let event = MirrorReplayEvent::Authorization {
        request: call.request.clone(),
        caller: call.caller.clone(),
    };
    event.persisted().is_some().then_some(event)
}

fn authorization_requires_mirror(request: &ParsedAuthorizationRequest) -> bool {
    !matches!(
        request,
        ParsedAuthorizationRequest::GetAuthTokensForCredStore { .. }
            | ParsedAuthorizationRequest::GetLastAuthTime { .. }
    )
}

fn maintenance_requires_mirror(request: &ParsedMaintenanceRequest) -> bool {
    !matches!(
        request,
        ParsedMaintenanceRequest::GetState { .. }
            | ParsedMaintenanceRequest::OnDeviceOffBody
            | ParsedMaintenanceRequest::MigrateKeyNamespace { .. }
            | ParsedMaintenanceRequest::GetAppUidsAffectedBySid { .. }
    )
}

fn record_mirror_failure(
    kind: MirrorStateKind,
    method: impl std::fmt::Debug,
    caller: &CallerIdentity,
    replay: Option<MirrorReplayEvent>,
) -> anyhow::Result<()> {
    let mut state = MIRROR_RECOVERY_STATE
        .lock()
        .expect("mirror recovery state poisoned");
    mark_mirror_state_dirty(kind, &method, caller);
    let update = record_mirror_failure_locked(&mut state, kind, method, replay);
    let persisted = persist_mirror_recovery_state(&mut state);
    drop(state);
    wake_mirror_recovery_worker();
    update.and(persisted)
}

fn record_mirror_failure_locked(
    state: &mut MirrorRecoveryState,
    kind: MirrorStateKind,
    method: impl std::fmt::Debug,
    replay: Option<MirrorReplayEvent>,
) -> anyhow::Result<()> {
    if state.lane(kind).poisoned {
        anyhow::bail!("{} mirror recovery is already poisoned", kind.label());
    }
    let Some(event) = replay else {
        poison_mirror_state(state, Some(kind));
        warn!(
            "event=mirror {} {:?} cannot be replayed safely; OMK routes are fail-closed until process restart",
            kind.label(), method
        );
        return Ok(());
    };
    debug_assert_eq!(event.kind(), kind);
    if state.lane(kind).pending.len() >= MAX_PENDING_MIRROR_REPLAYS {
        poison_mirror_state(state, Some(kind));
        warn!(
            "event=mirror {} recovery queue reached {} events; OMK routes are fail-closed until process restart",
            kind.label(), MAX_PENDING_MIRROR_REPLAYS
        );
        anyhow::bail!("{} mirror recovery queue is full", kind.label());
    }
    let sequence = state.next_sequence;
    state.next_sequence = state.next_sequence.wrapping_add(1).max(1);
    state
        .lane_mut(kind)
        .pending
        .push_back(SequencedMirrorReplay {
            sequence,
            kind,
            event: Some(event),
            retry_not_before: None,
        });
    refresh_mirror_dirty_flags(state);
    warn!(
        "event=mirror queued replay sequence={} domain={} method={:?} pending={}",
        sequence,
        kind.label(),
        method,
        state.lane(kind).pending.len()
    );
    Ok(())
}

fn ensure_mirror_state_recovered() -> anyhow::Result<()> {
    let state = MIRROR_RECOVERY_STATE
        .lock()
        .expect("mirror recovery state poisoned");
    if state.authorization.poisoned || state.maintenance.poisoned {
        anyhow::bail!("OMK mirror recovery is poisoned by a non-replayable event");
    }
    if state.authorization.draining || state.maintenance.draining {
        anyhow::bail!("OMK mirror recovery is busy");
    }
    if state.pending_len() == 0 {
        refresh_mirror_dirty_flags(&state);
        return Ok(());
    }
    drop(state);
    wake_mirror_recovery_worker();
    anyhow::bail!("OMK mirror recovery is pending")
}

#[cfg(test)]
fn ensure_mirror_state_recovered_with(
    mut execute: impl FnMut(&MirrorReplayEvent) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    recover_mirror_state_with(&mut execute)
}

fn recover_mirror_state_with(
    execute: &mut impl FnMut(&MirrorReplayEvent) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    loop {
        let pending = {
            let mut state = MIRROR_RECOVERY_STATE
                .lock()
                .expect("mirror recovery state poisoned");
            refresh_mirror_dirty_flags(&state);
            if state.authorization.poisoned || state.maintenance.poisoned {
                anyhow::bail!("OMK mirror recovery is poisoned by a non-replayable event");
            }
            if state.authorization.draining || state.maintenance.draining {
                anyhow::bail!("OMK mirror recovery is busy");
            }
            let Some(pending) = state.next_pending().cloned() else {
                return Ok(());
            };
            if pending.event.is_none() {
                anyhow::bail!(
                    "{} mirror update is awaiting its System reply",
                    pending.kind.label()
                );
            }
            if pending
                .retry_not_before
                .is_some_and(|deadline| deadline > Instant::now())
            {
                anyhow::bail!("{} mirror retry is delayed", pending.kind.label());
            }
            let lane = state.lane_mut(pending.kind);
            lane.draining = true;
            pending
        };

        let event = pending
            .event
            .as_ref()
            .expect("ready mirror replay lost its event");
        let result = execute(event);
        let mut state = MIRROR_RECOVERY_STATE
            .lock()
            .expect("mirror recovery state poisoned");
        if state
            .lane(pending.kind)
            .pending
            .front()
            .map(|front| front.sequence)
            != Some(pending.sequence)
        {
            if !state.lane(pending.kind).poisoned {
                poison_mirror_state(&mut state, None);
            }
            persist_mirror_recovery_state(&mut state)?;
            anyhow::bail!("OMK mirror recovery sequence changed while replaying");
        }

        if let Err(error) = result {
            if omk_unavailable_error(&error) {
                state
                    .lane_mut(pending.kind)
                    .pending
                    .front_mut()
                    .expect("checked mirror replay disappeared")
                    .retry_not_before = Some(Instant::now() + MIRROR_RECOVERY_RETRY_DELAY);
                state.lane_mut(pending.kind).draining = false;
                refresh_mirror_dirty_flags(&state);
                warn!(
                    "event=mirror replay sequence={} domain={} method={} failed: {:#}",
                    pending.sequence,
                    pending.kind.label(),
                    event.method(),
                    error
                );
                return Err(error);
            } else {
                mark_mirror_state_dirty(pending.kind, event.method(), event.caller());
                poison_mirror_state(&mut state, Some(pending.kind));
            }
            warn!(
                "event=mirror replay sequence={} domain={} method={} failed: {:#}",
                pending.sequence,
                pending.kind.label(),
                event.method(),
                error
            );
            persist_mirror_recovery_state(&mut state)?;
            return Err(error);
        }

        let lane = state.lane_mut(pending.kind);
        lane.pending.pop_front();
        lane.draining = false;
        refresh_mirror_dirty_flags(&state);
        persist_mirror_recovery_state(&mut state)?;
        info!(
            "event=mirror replay sequence={} domain={} method={} succeeded; pending={}",
            pending.sequence,
            pending.kind.label(),
            event.method(),
            state.pending_len()
        );
    }
}

fn poison_mirror_state(state: &mut MirrorRecoveryState, kind: Option<MirrorStateKind>) {
    match kind {
        Some(kind) => {
            let lane = state.lane_mut(kind);
            lane.poisoned = true;
            lane.draining = false;
            lane.pending.clear();
        }
        None => {
            state.authorization.poisoned = true;
            state.authorization.draining = false;
            state.authorization.pending.clear();
            state.maintenance.poisoned = true;
            state.maintenance.draining = false;
            state.maintenance.pending.clear();
        }
    }
    refresh_mirror_dirty_flags(state);
}

fn remove_reserved_mirror_update(
    state: &mut MirrorRecoveryState,
    kind: MirrorStateKind,
    sequence: u64,
) {
    state
        .lane_mut(kind)
        .pending
        .retain(|pending| pending.sequence != sequence);
}

fn reserve_mirror_update(kind: MirrorStateKind) -> anyhow::Result<MirrorUpdateReservation> {
    let mut state = MIRROR_RECOVERY_STATE
        .lock()
        .expect("mirror recovery state poisoned");
    if state.lane(kind).poisoned {
        anyhow::bail!("{} mirror recovery is poisoned", kind.label());
    }
    if state.lane(kind).pending.len() >= MAX_PENDING_MIRROR_REPLAYS {
        poison_mirror_state(&mut state, Some(kind));
        warn!(
            "event=mirror {} recovery queue reached {} updates; OMK routes are fail-closed until process restart",
            kind.label(), MAX_PENDING_MIRROR_REPLAYS
        );
        persist_mirror_recovery_state(&mut state)?;
        anyhow::bail!("{} mirror recovery queue is full", kind.label());
    }
    let sequence = state.next_sequence;
    state.next_sequence = state.next_sequence.wrapping_add(1).max(1);
    state
        .lane_mut(kind)
        .pending
        .push_back(SequencedMirrorReplay {
            sequence,
            kind,
            event: None,
            retry_not_before: None,
        });
    refresh_mirror_dirty_flags(&state);
    persist_mirror_recovery_state(&mut state)?;
    Ok(MirrorUpdateReservation {
        kind,
        sequence,
        active: true,
    })
}

impl MirrorUpdateReservation {
    fn enqueue(
        mut self,
        method: impl std::fmt::Debug,
        caller: &CallerIdentity,
        event: MirrorReplayEvent,
    ) -> anyhow::Result<()> {
        let mut state = MIRROR_RECOVERY_STATE
            .lock()
            .expect("mirror recovery state poisoned");
        if state.lane(self.kind).poisoned {
            mark_mirror_state_dirty(self.kind, &method, caller);
            self.active = false;
            let persisted = persist_mirror_recovery_state(&mut state);
            drop(state);
            wake_mirror_recovery_worker();
            persisted?;
            anyhow::bail!("{} mirror recovery is poisoned", self.kind.label());
        }
        let sequence = self.sequence;
        let Some(pending) = state
            .lane_mut(self.kind)
            .pending
            .iter_mut()
            .find(|pending| pending.sequence == sequence)
        else {
            poison_mirror_state(&mut state, None);
            self.active = false;
            let persisted = persist_mirror_recovery_state(&mut state);
            drop(state);
            wake_mirror_recovery_worker();
            persisted?;
            anyhow::bail!("mirror update reservation disappeared");
        };
        if pending.kind != self.kind || pending.event.is_some() {
            poison_mirror_state(&mut state, None);
            self.active = false;
            let persisted = persist_mirror_recovery_state(&mut state);
            drop(state);
            wake_mirror_recovery_worker();
            persisted?;
            anyhow::bail!("mirror update reservation changed unexpectedly");
        }
        debug_assert_eq!(event.kind(), self.kind);
        pending.event = Some(event);
        pending.retry_not_before = None;
        info!(
            "event=mirror queued update sequence={} domain={} method={:?}",
            sequence,
            self.kind.label(),
            method
        );
        self.active = false;
        refresh_mirror_dirty_flags(&state);
        let persisted = persist_mirror_recovery_state(&mut state);
        drop(state);
        wake_mirror_recovery_worker();
        persisted?;
        Ok(())
    }

    fn cancel(mut self) {
        let mut state = MIRROR_RECOVERY_STATE
            .lock()
            .expect("mirror recovery state poisoned");
        remove_reserved_mirror_update(&mut state, self.kind, self.sequence);
        self.active = false;
        refresh_mirror_dirty_flags(&state);
        if let Err(error) = persist_mirror_recovery_state(&mut state) {
            warn!(
                "event=mirror failed to persist canceled {} reservation: {:#}",
                self.kind.label(),
                error
            );
        }
        drop(state);
        wake_mirror_recovery_worker();
    }
}

impl Drop for MirrorUpdateReservation {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut state = MIRROR_RECOVERY_STATE
            .lock()
            .expect("mirror recovery state poisoned");
        remove_reserved_mirror_update(&mut state, self.kind, self.sequence);
        poison_mirror_state(&mut state, Some(self.kind));
        if let Err(error) = persist_mirror_recovery_state(&mut state) {
            warn!(
                "event=mirror failed to persist lost {} reservation: {:#}",
                self.kind.label(),
                error
            );
        }
        warn!(
            "event=mirror {} update lost its System reply; OMK routes are fail-closed until process restart",
            self.kind.label()
        );
        self.active = false;
        drop(state);
        wake_mirror_recovery_worker();
    }
}

fn wake_mirror_recovery_worker() {
    if let Some(Ok(sender)) = MIRROR_RECOVERY_WORKER.get() {
        let _ = sender.try_send(());
    }
}

pub(super) fn start_mirror_recovery_worker() -> Result<(), String> {
    let worker = MIRROR_RECOVERY_WORKER.get_or_init(|| {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("omk-mirror-recovery".to_string())
            .spawn(move || mirror_recovery_worker(receiver))
            .map_err(|error| format!("failed to start mirror recovery worker: {error}"))?;
        Ok(sender)
    });
    match worker {
        Ok(sender) => {
            let _ = sender.try_send(());
            Ok(())
        }
        Err(error) => Err(error.clone()),
    }
}

fn mirror_recovery_worker(receiver: std::sync::mpsc::Receiver<()>) {
    loop {
        let _ = recover_mirror_state_with(&mut MirrorReplayEvent::execute);
        let wait = {
            let state = MIRROR_RECOVERY_STATE
                .lock()
                .expect("mirror recovery state poisoned");
            state.next_worker_wait(Instant::now())
        };
        match wait {
            Some(duration) if duration.is_zero() => continue,
            Some(duration) => match receiver.recv_timeout(duration) {
                Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            },
            None if receiver.recv().is_err() => return,
            None => {}
        }
    }
}

fn evaluate_caller(
    caller: &CallerIdentity,
    cfg: &config::InjectorConfig,
) -> crate::filter::FilterDecision {
    let preflight = filter::evaluate(
        &cfg.scoop,
        &cfg.filter,
        caller.uid,
        PackageResolution::Unknown,
    );
    if preflight.reason == FilterReason::RejectedAndroidPackage {
        return preflight;
    }

    let package_resolution = {
        let _guard = BypassGuard::enter();
        ipc::resolve_packages_for_uid(caller.uid)
    };
    let decision = filter::evaluate(&cfg.scoop, &cfg.filter, caller.uid, package_resolution);
    if decision.reason == FilterReason::Disabled {
        debug!(
            "event=decision package filter disabled; routing still follows per-method intercept settings"
        );
    }
    decision
}

fn service_request_key(request: &ParsedServiceRequest) -> Option<&KeyDescriptor> {
    match request {
        ParsedServiceRequest::GetKeyEntry { key }
        | ParsedServiceRequest::UpdateSubcomponent { key, .. }
        | ParsedServiceRequest::DeleteKey { key }
        | ParsedServiceRequest::Grant { key, .. }
        | ParsedServiceRequest::Ungrant { key, .. } => Some(key),
        _ => None,
    }
}

fn security_level_request_key(request: &ParsedSecurityLevelRequest) -> Option<&KeyDescriptor> {
    match request {
        ParsedSecurityLevelRequest::GenerateKey {
            key,
            attestation_key: Some(attestation_key),
            ..
        } if key.domain != Domain::BLOB => Some(attestation_key),
        ParsedSecurityLevelRequest::ImportWrappedKey { wrapping_key, .. } => Some(wrapping_key),
        ParsedSecurityLevelRequest::CreateOperation { key, .. }
        | ParsedSecurityLevelRequest::DeleteKey { key }
        | ParsedSecurityLevelRequest::ConvertStorageKeyToEphemeral { storage_key: key } => {
            Some(key)
        }
        _ => None,
    }
}

fn should_allow_omk_grant_descriptor_with_probe(
    grant: &KeyDescriptor,
    decision: &filter::FilterDecision,
    caller: &CallerIdentity,
    mut probe: impl FnMut(&CallerIdentity, &KeyDescriptor) -> anyhow::Result<bool>,
) -> anyhow::Result<bool> {
    if decision.allowed
        || !matches!(
            decision.reason,
            FilterReason::RejectedUnknownPackage | FilterReason::RejectedNotInScope
        )
    {
        return Ok(false);
    }

    if grant.domain != Domain::GRANT {
        return Ok(false);
    }

    ensure_mirror_state_recovered()?;
    probe(caller, grant)
}

fn probe_omk_grant(caller: &CallerIdentity, grant: &KeyDescriptor) -> anyhow::Result<bool> {
    let caller_info = caller.to_caller_info();
    let _guard = BypassGuard::enter();
    match ipc::with_omk_retry(|omk| Ok(omk.r#isOmkGrant(Some(&caller_info), grant)?)) {
        Ok(owned) => Ok(owned),
        Err(error) if omk_unavailable_error(&error) => {
            debug!(
                "event=decision OMK grant probe unavailable for uid={} pid={} grant_nspace={}: {:#}",
                caller.uid, caller.pid, grant.nspace, error
            );
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

fn should_allow_omk_grant_service_request_with_probe(
    request: &ParsedServiceRequest,
    decision: &filter::FilterDecision,
    caller: &CallerIdentity,
    mut probe: impl FnMut(&CallerIdentity, &KeyDescriptor) -> anyhow::Result<bool>,
) -> anyhow::Result<bool> {
    let Some(grant) = service_request_key(request) else {
        return Ok(false);
    };

    should_allow_omk_grant_descriptor_with_probe(grant, decision, caller, &mut probe)
}

fn should_allow_omk_grant_security_level_request_with_probe(
    request: &ParsedSecurityLevelRequest,
    decision: &filter::FilterDecision,
    caller: &CallerIdentity,
    mut probe: impl FnMut(&CallerIdentity, &KeyDescriptor) -> anyhow::Result<bool>,
) -> anyhow::Result<bool> {
    let Some(grant) = security_level_request_key(request) else {
        return Ok(false);
    };

    should_allow_omk_grant_descriptor_with_probe(grant, decision, caller, &mut probe)
}

fn precompute_omk_service_mutator_reply(
    request: &ParsedServiceRequest,
    caller: &CallerIdentity,
) -> OmkServicePrecompute {
    if let Err(error) = ensure_mirror_state_recovered() {
        warn!(
            "event=route OMK service {:?} blocked by unresolved mirror recovery for uid={} pid={}: {:#}",
            request.method(), caller.uid, caller.pid, error
        );
        return OmkServicePrecompute::Reply(PrecomputedServiceReply::Error(
            Status::new_service_specific_error(ResponseCode::SYSTEM_ERROR.0, None),
        ));
    }
    let caller_info = caller.to_caller_info();
    match request {
        ParsedServiceRequest::UpdateSubcomponent {
            key,
            public_cert,
            certificate_chain,
        } => {
            match ipc::with_omk_once(|omk| {
                Ok(omk.r#updateSubcomponent(
                    Some(&caller_info),
                    key,
                    public_cert.as_deref(),
                    certificate_chain.as_deref(),
                )?)
            }) {
                Ok(()) => {
                    OmkServicePrecompute::Reply(PrecomputedServiceReply::UpdateSubcomponentSuccess)
                }
                Err(error) if omk_unavailable_error(&error) => {
                    warn!(
                    "event=route OMK updateSubcomponent unavailable for uid={} pid={}: {:#}; leaving original system request untouched",
                    caller.uid, caller.pid, error
                );
                    OmkServicePrecompute::PreserveSystem
                }
                Err(error) => OmkServicePrecompute::Reply(PrecomputedServiceReply::Error(
                    precomputed_omk_error_reply(&error),
                )),
            }
        }
        ParsedServiceRequest::DeleteKey { key } => {
            match ipc::with_omk_once(|omk| Ok(omk.r#deleteKey(Some(&caller_info), key)?)) {
                Ok(()) => OmkServicePrecompute::Reply(PrecomputedServiceReply::DeleteKeySuccess),
                Err(error) if omk_unavailable_error(&error) => {
                    warn!(
                        "event=route OMK deleteKey unavailable for uid={} pid={}: {:#}; leaving original system request untouched",
                        caller.uid, caller.pid, error
                    );
                    OmkServicePrecompute::PreserveSystem
                }
                Err(error) => OmkServicePrecompute::Reply(PrecomputedServiceReply::Error(
                    precomputed_omk_error_reply(&error),
                )),
            }
        }
        ParsedServiceRequest::Grant { .. } | ParsedServiceRequest::Ungrant { .. } => {
            precompute_omk_grant_service_reply_with(
                request,
                caller,
                |caller_info, key, grantee_uid, access_vector| {
                    ipc::with_omk_once(|omk| {
                        Ok(omk.r#grant(Some(caller_info), key, grantee_uid, access_vector)?)
                    })
                },
                |caller_info, key, grantee_uid| {
                    ipc::with_omk_once(|omk| {
                        Ok(omk.r#ungrant(Some(caller_info), key, grantee_uid)?)
                    })
                },
            )
        }
        _ => unreachable!("only service mutators are precomputed"),
    }
}

fn precompute_omk_grant_service_reply_with(
    request: &ParsedServiceRequest,
    caller: &CallerIdentity,
    mut grant: impl FnMut(&CallerInfo, &KeyDescriptor, i32, i32) -> anyhow::Result<KeyDescriptor>,
    mut ungrant: impl FnMut(&CallerInfo, &KeyDescriptor, i32) -> anyhow::Result<()>,
) -> OmkServicePrecompute {
    let caller_info = caller.to_caller_info();
    match request {
        ParsedServiceRequest::Grant {
            key,
            grantee_uid,
            access_vector,
        } => match grant(&caller_info, key, *grantee_uid, *access_vector) {
            Ok(omk_grant) => {
                OmkServicePrecompute::Reply(PrecomputedServiceReply::GrantSuccess(omk_grant))
            }
            Err(error) if omk_unavailable_error(&error) => {
                warn!(
                    "event=route OMK grant unavailable for uid={} pid={}: {:#}; leaving original system request untouched",
                    caller.uid, caller.pid, error
                );
                OmkServicePrecompute::PreserveSystem
            }
            Err(error) => {
                warn!(
                    "event=route OMK grant failed for uid={} pid={}: {:#}; returning OMK error",
                    caller.uid, caller.pid, error
                );
                OmkServicePrecompute::Reply(PrecomputedServiceReply::Error(
                    precomputed_omk_error_reply(&error),
                ))
            }
        },
        ParsedServiceRequest::Ungrant { key, grantee_uid } => {
            match ungrant(&caller_info, key, *grantee_uid) {
                Ok(()) => OmkServicePrecompute::Reply(PrecomputedServiceReply::UngrantSuccess),
                Err(error) if omk_unavailable_error(&error) => {
                    warn!(
                        "event=route OMK ungrant unavailable for uid={} pid={}: {:#}; leaving original system request untouched",
                        caller.uid, caller.pid, error
                    );
                    OmkServicePrecompute::PreserveSystem
                }
                Err(error) => {
                    warn!(
                        "event=route OMK ungrant failed for uid={} pid={}: {:#}; returning OMK error",
                        caller.uid, caller.pid, error
                    );
                    OmkServicePrecompute::Reply(PrecomputedServiceReply::Error(
                        precomputed_omk_error_reply(&error),
                    ))
                }
            }
        }
        _ => unreachable!("only grant/ungrant requests are precomputed"),
    }
}

fn target_from_transaction(tr: &binder_transaction_data) -> Option<LocalBinderTarget> {
    let ptr = unsafe { tr.target.ptr };
    if ptr == 0 || tr.cookie == 0 {
        return None;
    }
    Some(LocalBinderTarget {
        ptr,
        cookie: tr.cookie,
    })
}

fn route_for_service_request(
    request: &ParsedServiceRequest,
    intercept: &config::InterceptConfig,
) -> RouteTarget {
    if identify::is_omk_service_route_enabled(request.method(), intercept) {
        RouteTarget::Omk
    } else {
        RouteTarget::System
    }
}

fn security_level_scoop_enabled(intercept: &config::InterceptConfig) -> bool {
    intercept.get_security_level || intercept.get_key_entry
}

fn mirror_state_dirty(kind: MirrorStateKind) -> bool {
    kind.dirty().load(Ordering::SeqCst)
}

fn is_known_keystore_interface(interface: &str) -> bool {
    identify::KNOWN_KEYSTORE_INTERFACES.contains(&interface)
}

fn mark_mirror_state_dirty(
    kind: MirrorStateKind,
    method: impl std::fmt::Debug,
    caller: &CallerIdentity,
) {
    let was_dirty = kind.dirty().swap(true, Ordering::SeqCst);
    warn!(
        "event=mirror marked OMK mirror state dirty after failed {} {:?} mirror for uid={} pid={}{}",
        kind.label(),
        method,
        caller.uid,
        caller.pid,
        if was_dirty { " (already dirty)" } else { "" }
    );
}

fn log_dirty_mirror_state(
    kind: MirrorStateKind,
    method: impl std::fmt::Debug,
    caller: &CallerIdentity,
) {
    if !mirror_state_dirty(kind) {
        return;
    }

    warn!(
        "event=mirror OMK {} mirror state remains dirty while handling successful system {:?} call for uid={} pid={}; prior divergence is not cleared",
        kind.label(),
        method,
        caller.uid,
        caller.pid
    );
}

pub(super) fn lookup_synthetic_target(target: LocalBinderTarget) -> Option<SyntheticTargetKind> {
    lookup_synthetic_target_info(target).map(|info| info.kind)
}

fn synthetic_target_interface(kind: SyntheticTargetKind) -> &'static str {
    match kind {
        SyntheticTargetKind::SecurityLevel => identify::KEYSTORE_SECURITY_LEVEL_INTERFACE,
        SyntheticTargetKind::Operation => identify::KEYSTORE_OPERATION_INTERFACE,
    }
}

fn lookup_synthetic_target_info(target: LocalBinderTarget) -> Option<SyntheticTargetInfo> {
    SYNTHETIC_TARGETS
        .lock()
        .expect("synthetic target map poisoned")
        .get(&target)
        .cloned()
}

#[cfg(test)]
fn allocate_test_target() -> LocalBinderTarget {
    let id = NEXT_TEST_TARGET_ID.fetch_add(1, Ordering::Relaxed);
    LocalBinderTarget {
        ptr: (0x1000_0000_u64 | id) as libc::c_ulong,
        cookie: (0x2000_0000_u64 | id) as libc::c_ulong,
    }
}

fn remember_operation_target(target: LocalBinderTarget, info: OperationTargetInfo) {
    let previous = OPERATION_TARGETS
        .lock()
        .expect("operation target map poisoned")
        .insert(target, info);
    if let Some(previous) = previous {
        if let Some(backend) = previous.backend {
            let _guard = BypassGuard::enter();
            if let Err(status) = backend.r#abort() {
                debug!(
                    "event=route previous OMK operation for carrier ptr=0x{:x} cookie=0x{:x} could not be aborted while replacing mapping: {}",
                    target.ptr, target.cookie, status
                );
            }
        }
    }
}

fn lookup_operation_target(target: LocalBinderTarget) -> Option<OperationTargetInfo> {
    OPERATION_TARGETS
        .lock()
        .expect("operation target map poisoned")
        .get(&target)
        .cloned()
}

fn forget_operation_target(target: LocalBinderTarget) {
    OPERATION_TARGETS
        .lock()
        .expect("operation target map poisoned")
        .remove(&target);
}

#[cfg(test)]
fn current_operation_retirement(target: LocalBinderTarget) -> Option<NativeBinderRetirement> {
    let generation = SYNTHETIC_TARGETS
        .lock()
        .expect("synthetic target map poisoned")
        .get(&target)
        .filter(|info| info.kind == SyntheticTargetKind::Operation)
        .and_then(|info| info.native_generation)?;
    Some(NativeBinderRetirement { target, generation })
}

pub(crate) fn drop_synthetic_operation_retirement(retirement: NativeBinderRetirement) {
    let (info, native) = {
        let mut binders = NATIVE_BINDERS.lock().expect("native binder map poisoned");
        let mut operations = OPERATION_TARGETS
            .lock()
            .expect("operation target map poisoned");
        let mut synthetic = SYNTHETIC_TARGETS
            .lock()
            .expect("synthetic target map poisoned");
        let matches_generation = binders
            .get(&retirement.target)
            .is_some_and(|native| native.retirement_generation() == retirement.generation)
            || synthetic.get(&retirement.target).is_some_and(|info| {
                info.kind == SyntheticTargetKind::Operation
                    && info.native_generation == Some(retirement.generation)
            });
        if !matches_generation {
            return;
        }
        let info = operations.remove(&retirement.target);
        synthetic.remove(&retirement.target);
        let native = binders.remove(&retirement.target);
        (info, native)
    };

    {
        let mut publications = OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned");
        if publications
            .get(&retirement.target)
            .is_some_and(|publication| publication.generation == retirement.generation)
        {
            publications.remove(&retirement.target);
        }
    }
    OPERATION_PUBLICATION_PROBES
        .lock()
        .expect("operation publication probe queue poisoned")
        .retain(|probe| {
            probe.target != retirement.target || probe.generation != retirement.generation
        });
    if let Some(native) = native.as_ref() {
        native.disarm_retirement();
    }
    abort_operation_target_info(retirement.target, info);
    drop(native);
}

#[cfg(test)]
fn drop_synthetic_operation_target(target: LocalBinderTarget) {
    if let Some(retirement) = current_operation_retirement(target) {
        drop_synthetic_operation_retirement(retirement);
        return;
    }
    OPERATION_PUBLICATIONS
        .lock()
        .expect("operation publication map poisoned")
        .remove(&target);
    let native = NATIVE_BINDERS
        .lock()
        .expect("native binder map poisoned")
        .remove(&target);
    if let Some(native) = native.as_ref() {
        native.disarm_retirement();
    }
    drop(native);

    retire_synthetic_operation_target(target);
}

fn release_native_operation_initial_strong(retirement: NativeBinderRetirement) {
    let native = {
        let mut binders = NATIVE_BINDERS.lock().expect("native binder map poisoned");
        binders
            .get(&retirement.target)
            .is_some_and(|native| native.retirement_generation() == retirement.generation)
            .then(|| binders.remove(&retirement.target))
            .flatten()
    };
    drop(native);
}

fn abort_operation_target_info(target: LocalBinderTarget, info: Option<OperationTargetInfo>) {
    let Some(info) = info else {
        debug!(
            "event=synthetic release for stale operation target ptr=0x{:x} cookie=0x{:x}",
            target.ptr, target.cookie
        );
        return;
    };
    if info.finalized {
        debug!(
            "event=synthetic release for finalized operation target ptr=0x{:x} cookie=0x{:x}",
            target.ptr, target.cookie
        );
        return;
    }
    let Some(backend) = info.backend else {
        return;
    };
    let _guard = BypassGuard::enter();
    if let Err(status) = backend.r#abort() {
        debug!(
            "event=synthetic drop abort for operation target ptr=0x{:x} cookie=0x{:x} failed: {}",
            target.ptr, target.cookie, status
        );
    }
}

#[cfg(test)]
fn retire_synthetic_operation_target(target: LocalBinderTarget) {
    let info = OPERATION_TARGETS
        .lock()
        .expect("operation target map poisoned")
        .remove(&target);
    SYNTHETIC_TARGETS
        .lock()
        .expect("synthetic target map poisoned")
        .remove(&target);
    if OPERATION_PUBLICATIONS
        .lock()
        .expect("operation publication map poisoned")
        .contains_key(&target)
    {
        let now = Instant::now();
        for probe in OPERATION_PUBLICATION_PROBES
            .lock()
            .expect("operation publication probe queue poisoned")
            .iter_mut()
            .filter(|probe| probe.target == target)
        {
            probe.not_before = now;
        }
        super::intercept::wake_operation_publication_worker();
    }

    abort_operation_target_info(target, info);
}

pub(super) fn retire_synthetic_operation_retirement(retirement: NativeBinderRetirement) {
    let info = {
        let mut operations = OPERATION_TARGETS
            .lock()
            .expect("operation target map poisoned");
        let mut synthetic = SYNTHETIC_TARGETS
            .lock()
            .expect("synthetic target map poisoned");
        let matches_generation = synthetic.get(&retirement.target).is_some_and(|info| {
            info.kind == SyntheticTargetKind::Operation
                && info.native_generation == Some(retirement.generation)
        });
        if !matches_generation {
            return;
        }
        synthetic.remove(&retirement.target);
        operations.remove(&retirement.target)
    };
    if OPERATION_PUBLICATIONS
        .lock()
        .expect("operation publication map poisoned")
        .get(&retirement.target)
        .is_some_and(|publication| publication.generation == retirement.generation)
    {
        let now = Instant::now();
        for probe in OPERATION_PUBLICATION_PROBES
            .lock()
            .expect("operation publication probe queue poisoned")
            .iter_mut()
            .filter(|probe| {
                probe.target == retirement.target && probe.generation == retirement.generation
            })
        {
            probe.not_before = now;
        }
        super::intercept::wake_operation_publication_worker();
    }
    abort_operation_target_info(retirement.target, info);
}

pub(crate) fn retire_native_operation_target(retirement: NativeBinderRetirement) {
    let info = {
        let mut operations = OPERATION_TARGETS
            .lock()
            .expect("operation target map poisoned");
        let mut synthetic = SYNTHETIC_TARGETS
            .lock()
            .expect("synthetic target map poisoned");
        let matches_generation = synthetic.get(&retirement.target).is_some_and(|info| {
            info.kind == SyntheticTargetKind::Operation
                && info.native_generation == Some(retirement.generation)
        });
        if !matches_generation {
            debug!(
                "event=synthetic ignored stale native operation retirement ptr=0x{:x} cookie=0x{:x} generation={}",
                retirement.target.ptr, retirement.target.cookie, retirement.generation
            );
            return;
        }
        synthetic.remove(&retirement.target);
        operations.remove(&retirement.target)
    };

    {
        let mut publications = OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned");
        if publications
            .get(&retirement.target)
            .is_some_and(|publication| publication.generation == retirement.generation)
        {
            publications.remove(&retirement.target);
        }
    }
    OPERATION_PUBLICATION_PROBES
        .lock()
        .expect("operation publication probe queue poisoned")
        .retain(|probe| {
            probe.target != retirement.target || probe.generation != retirement.generation
        });

    drop(info);
}

fn register_operation_publication(target: LocalBinderTarget) -> anyhow::Result<u64> {
    let mut publications = OPERATION_PUBLICATIONS
        .lock()
        .expect("operation publication map poisoned");
    if publications.len() >= MAX_PENDING_OPERATION_PUBLICATIONS {
        anyhow::bail!(
            "pending native operation publication limit reached ({MAX_PENDING_OPERATION_PUBLICATIONS})"
        );
    }
    let generation = NEXT_OPERATION_PUBLICATION_GENERATION.fetch_add(1, Ordering::Relaxed);
    publications.insert(
        target,
        OperationPublication {
            generation,
            acquire_pending: false,
            acquire_owned: false,
            connection: None,
            binder: None,
        },
    );
    Ok(generation)
}

#[cfg(test)]
pub(super) fn register_operation_publication_for_test(
    target: LocalBinderTarget,
) -> NativeBinderRetirement {
    let generation =
        register_operation_publication(target).expect("test operation publication should register");
    NativeBinderRetirement { target, generation }
}

fn finish_operation_publication(
    publications: &mut HashMap<LocalBinderTarget, OperationPublication>,
    retirement: NativeBinderRetirement,
) -> bool {
    let Some(publication) = publications.get(&retirement.target) else {
        return false;
    };
    if publication.generation != retirement.generation
        || !publication.acquire_owned
        || publication.binder.is_none()
    {
        return false;
    }
    publications.remove(&retirement.target);
    true
}

pub(super) fn mark_operation_publication_acquire_pending(
    target: LocalBinderTarget,
    connection: BinderStateKey,
) -> Option<NativeBinderRetirement> {
    let mut publications = OPERATION_PUBLICATIONS
        .lock()
        .expect("operation publication map poisoned");
    let publication = publications.get_mut(&target)?;
    if publication.connection != Some(connection)
        || publication.acquire_pending
        || publication.acquire_owned
    {
        return None;
    }
    publication.acquire_pending = true;
    Some(NativeBinderRetirement {
        target,
        generation: publication.generation,
    })
}

pub(super) fn mark_operation_publication_acquire_committed(retirement: NativeBinderRetirement) {
    let finished = {
        let mut publications = OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned");
        let Some(publication) = publications.get_mut(&retirement.target) else {
            return;
        };
        if publication.generation != retirement.generation || !publication.acquire_pending {
            return;
        }
        publication.acquire_pending = false;
        publication.acquire_owned = true;
        finish_operation_publication(&mut publications, retirement)
    };
    if finished {
        release_native_operation_initial_strong(retirement);
    }
}

pub(super) fn cancel_operation_publication_acquire_pending(retirement: NativeBinderRetirement) {
    if let Some(publication) = OPERATION_PUBLICATIONS
        .lock()
        .expect("operation publication map poisoned")
        .get_mut(&retirement.target)
        .filter(|publication| publication.generation == retirement.generation)
    {
        publication.acquire_pending = false;
    }
}

pub(super) fn operation_publication_acquire_is_pending(probe: OperationPublicationProbe) -> bool {
    OPERATION_PUBLICATIONS
        .lock()
        .expect("operation publication map poisoned")
        .get(&probe.target)
        .is_some_and(|publication| {
            publication.generation == probe.generation
                && publication.binder == Some(probe.binder)
                && publication.acquire_pending
        })
}

pub(super) fn operation_publication_pending_acquire(
    target: LocalBinderTarget,
    connection: BinderStateKey,
) -> Option<NativeBinderRetirement> {
    let publications = OPERATION_PUBLICATIONS
        .lock()
        .expect("operation publication map poisoned");
    let publication = publications.get(&target)?;
    (publication.connection == Some(connection) && publication.acquire_pending).then_some(
        NativeBinderRetirement {
            target,
            generation: publication.generation,
        },
    )
}

pub(super) fn mark_operation_publication_completed(
    retirement: NativeBinderRetirement,
    binder: BinderFdToken,
) {
    let (finished, probe) = {
        let mut publications = OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned");
        let Some(publication) = publications.get_mut(&retirement.target) else {
            return;
        };
        if publication.generation != retirement.generation
            || publication
                .connection
                .is_some_and(|connection| connection != binder.connection)
        {
            return;
        }
        publication.connection = Some(binder.connection);
        publication.binder = Some(binder);
        let finished = finish_operation_publication(&mut publications, retirement);
        let probe =
            publications
                .get(&retirement.target)
                .map(|publication| OperationPublicationProbe {
                    target: retirement.target,
                    binder,
                    generation: publication.generation,
                    not_before: Instant::now() + OPERATION_PUBLICATION_PROBE_GRACE,
                    query_failures: 0,
                });
        (finished, probe)
    };
    if finished {
        release_native_operation_initial_strong(retirement);
    }
    if let Some(probe) = probe {
        queue_operation_publication_probe(probe);
    }
}

pub(super) fn bind_operation_publication_connection(
    retirement: NativeBinderRetirement,
    connection: BinderStateKey,
) {
    if let Some(publication) = OPERATION_PUBLICATIONS
        .lock()
        .expect("operation publication map poisoned")
        .get_mut(&retirement.target)
        .filter(|publication| {
            publication.generation == retirement.generation
                && publication
                    .connection
                    .is_none_or(|bound| bound == connection)
        })
    {
        publication.connection = Some(connection);
    }
}

pub(super) fn retire_binder_connection_publications(connection: BinderStateKey) {
    let (targets, retired_targets) = {
        let mut publications = OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned");
        let targets = publications
            .iter()
            .filter_map(|(target, publication)| {
                (publication.connection == Some(connection)).then_some(NativeBinderRetirement {
                    target: *target,
                    generation: publication.generation,
                })
            })
            .collect::<Vec<_>>();
        let retired_targets = targets
            .iter()
            .copied()
            .filter(|retirement| {
                publications
                    .get(&retirement.target)
                    .is_some_and(|publication| !publication.acquire_pending)
            })
            .collect::<Vec<_>>();
        publications.retain(|_, publication| {
            publication.connection != Some(connection) || publication.acquire_pending
        });
        (targets, retired_targets)
    };
    if targets.is_empty() {
        return;
    }
    OPERATION_PUBLICATION_PROBES
        .lock()
        .expect("operation publication probe queue poisoned")
        .retain(|probe| {
            !targets.iter().any(|retirement| {
                probe.target == retirement.target && probe.generation == retirement.generation
            })
        });
    for retirement in retired_targets {
        drop_synthetic_operation_retirement(retirement);
    }
}

pub(super) fn finish_local_operation_publication(retirement: NativeBinderRetirement) {
    let removed = {
        let mut publications = OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned");
        publications
            .get(&retirement.target)
            .is_some_and(|publication| publication.generation == retirement.generation)
            .then(|| publications.remove(&retirement.target))
            .flatten()
            .is_some()
    };
    if removed {
        release_native_operation_initial_strong(retirement);
    }
}

pub(super) fn take_operation_publication_probe(now: Instant) -> Option<OperationPublicationProbe> {
    loop {
        let probe = {
            let mut probes = OPERATION_PUBLICATION_PROBES
                .lock()
                .expect("operation publication probe queue poisoned");
            let ready = probes.iter().position(|probe| probe.not_before <= now)?;
            probes.remove(ready)?
        };
        let eligible = {
            let publications = OPERATION_PUBLICATIONS
                .lock()
                .expect("operation publication map poisoned");
            publications.get(&probe.target).is_some_and(|publication| {
                publication.generation == probe.generation
                    && publication.binder == Some(probe.binder)
                    && !publication.acquire_owned
            })
        };
        if eligible {
            return Some(probe);
        }
    }
}

pub(super) fn finish_operation_publication_probe(
    mut probe: OperationPublicationProbe,
    node_exists: Result<bool, i32>,
    now: Instant,
) -> Option<NativeBinderRetirement> {
    {
        let mut publications = OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned");
        let publication = publications.get(&probe.target)?;
        if publication.generation != probe.generation
            || publication.binder != Some(probe.binder)
            || publication.acquire_owned
        {
            return None;
        }
        if !publication.acquire_pending && matches!(node_exists, Ok(false)) {
            publications.remove(&probe.target);
            return Some(NativeBinderRetirement {
                target: probe.target,
                generation: probe.generation,
            });
        }
    }
    let reached_max_backoff = if node_exists.is_err() {
        let previous_failures = probe.query_failures;
        probe.query_failures = probe
            .query_failures
            .saturating_add(1)
            .min(OPERATION_PUBLICATION_MAX_QUERY_BACKOFF_SHIFT);
        previous_failures < OPERATION_PUBLICATION_MAX_QUERY_BACKOFF_SHIFT
            && probe.query_failures == OPERATION_PUBLICATION_MAX_QUERY_BACKOFF_SHIFT
    } else {
        probe.query_failures = 0;
        false
    };
    if reached_max_backoff {
        warn!(
            "event=synthetic operation publication node query failed repeatedly; retaining the operation and native Binder until driver reference state is known ptr=0x{:x} cookie=0x{:x} fd={} failures={}",
            probe.target.ptr, probe.target.cookie, probe.binder.fd, probe.query_failures
        );
    }
    probe.not_before = now
        + OPERATION_PUBLICATION_REPROBE_DELAY
            .saturating_mul(1u32 << u32::from(probe.query_failures));
    queue_operation_publication_probe(probe);
    None
}

#[cfg(test)]
fn observe_synthetic_operation_release(target: LocalBinderTarget) {
    if lookup_synthetic_target(target) == Some(SyntheticTargetKind::Operation) {
        drop_synthetic_operation_target(target);
    }
}

fn mark_operation_target_finalized(target: LocalBinderTarget) {
    if let Some(info) = OPERATION_TARGETS
        .lock()
        .expect("operation target map poisoned")
        .get_mut(&target)
    {
        info.backend = None;
        info.finalized = true;
    }
}

fn operation_error_finalizes(status: &Status) -> bool {
    status.exception_code() != ExceptionCode::ServiceSpecific
        || status.service_specific_error() != ResponseCode::OPERATION_BUSY.0
}

fn operation_allows_aad(
    parameters: &[crate::android::hardware::security::keymint::KeyParameter::KeyParameter],
) -> bool {
    parameters.iter().any(|parameter| {
        parameter.tag
            == crate::android::hardware::security::keymint::Tag::Tag::BLOCK_MODE
            && matches!(
                parameter.value,
                crate::android::hardware::security::keymint::KeyParameterValue::KeyParameterValue::BlockMode(
                    crate::android::hardware::security::keymint::BlockMode::BlockMode::GCM
                )
        )
    })
}

fn native_binder_carrier(native: &NativeBinder) -> parcel::ReplyBinderCarrier {
    parcel::ReplyBinderCarrier {
        bytes: native.carrier().to_vec(),
        is_object: true,
    }
}

pub(super) fn lookup_native_binder(target: LocalBinderTarget) -> Option<Arc<NativeBinder>> {
    NATIVE_BINDERS
        .lock()
        .expect("native binder map poisoned")
        .get(&target)
        .cloned()
}

pub(super) fn lookup_native_binder_for(
    retirement: NativeBinderRetirement,
) -> Option<Arc<NativeBinder>> {
    let binders = NATIVE_BINDERS.lock().expect("native binder map poisoned");
    binders
        .get(&retirement.target)
        .filter(|native| native.retirement_generation() == retirement.generation)
        .cloned()
}

fn register_synthetic_operation_carrier(
    backend: AospOperationBinder,
    aad_allowed: bool,
    caller: &CallerIdentity,
) -> anyhow::Result<(parcel::ReplyBinderCarrier, NativeBinderRetirement)> {
    let native = Arc::new(create_native_operation_binder()?);
    let target = native.target();
    let carrier = native_binder_carrier(&native);
    let generation = register_operation_publication(target)?;
    let mut binders = NATIVE_BINDERS.lock().expect("native binder map poisoned");
    let mut operations = OPERATION_TARGETS
        .lock()
        .expect("operation target map poisoned");
    let mut synthetic = SYNTHETIC_TARGETS
        .lock()
        .expect("synthetic target map poisoned");
    let replaced_stale_system = if !synthetic.contains_key(&target)
        && !binders.contains_key(&target)
        && operations
            .get(&target)
            .is_some_and(|info| info.route == RouteTarget::System)
    {
        operations.remove(&target);
        true
    } else {
        false
    };
    if operations.contains_key(&target)
        || synthetic.contains_key(&target)
        || binders.contains_key(&target)
    {
        drop(synthetic);
        drop(operations);
        drop(binders);
        OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .remove(&target);
        anyhow::bail!(
            "native operation target collision for ptr=0x{:x} cookie=0x{:x}",
            target.ptr,
            target.cookie
        );
    }
    operations.insert(
        target,
        OperationTargetInfo {
            route: RouteTarget::Omk,
            aad_allowed,
            backend: Some(backend),
            finalized: false,
        },
    );
    synthetic.insert(
        target,
        SyntheticTargetInfo {
            kind: SyntheticTargetKind::Operation,
            caller: Some(caller.clone()),
            native_generation: Some(generation),
        },
    );
    binders.insert(target, native.clone());
    native.arm_retirement(generation);
    drop(binders);
    drop(synthetic);
    drop(operations);
    if replaced_stale_system {
        debug!(
            "event=synthetic replaced stale system operation mapping for reused target ptr=0x{:x} cookie=0x{:x}",
            target.ptr, target.cookie
        );
    }
    info!(
        "event=synthetic registered operation target ptr=0x{:x} cookie=0x{:x} aad_allowed={} uid={} pid={} sid='{}'",
        target.ptr, target.cookie, aad_allowed, caller.uid, caller.pid, caller.sid
    );
    Ok((carrier, NativeBinderRetirement { target, generation }))
}

fn register_synthetic_security_level_carrier(
    security_level: crate::android::hardware::security::keymint::SecurityLevel::SecurityLevel,
    source_method: ServiceMethod,
    caller: &CallerIdentity,
) -> anyhow::Result<parcel::ReplyBinderCarrier> {
    let (target, carrier) = {
        let mut targets = SYNTHETIC_SECURITY_LEVEL_TARGETS
            .lock()
            .expect("synthetic security-level target map poisoned");
        if let Some(target) = targets.get(&security_level) {
            let native = NATIVE_BINDERS
                .lock()
                .expect("native binder map poisoned")
                .get(target)
                .cloned();
            let carrier = if let Some(native) = native {
                native_binder_carrier(&native)
            } else {
                return Err(anyhow::anyhow!(
                    "missing cached security-level binder for ptr=0x{:x} cookie=0x{:x}",
                    target.ptr,
                    target.cookie
                ));
            };
            (*target, carrier)
        } else {
            let native = Arc::new(create_native_security_level_binder()?);
            let target = native.target();
            let carrier = native_binder_carrier(&native);
            let mut binders = NATIVE_BINDERS.lock().expect("native binder map poisoned");
            let mut synthetic = SYNTHETIC_TARGETS
                .lock()
                .expect("synthetic target map poisoned");
            if binders.contains_key(&target) || synthetic.contains_key(&target) {
                anyhow::bail!(
                    "native security-level binder collision for ptr=0x{:x} cookie=0x{:x}",
                    target.ptr,
                    target.cookie
                );
            }
            binders.insert(target, native);
            synthetic.insert(
                target,
                SyntheticTargetInfo {
                    kind: SyntheticTargetKind::SecurityLevel,
                    caller: None,
                    native_generation: None,
                },
            );
            targets.insert(security_level, target);
            (target, carrier)
        }
    };
    tracker::remember_security_level_target(target, SecurityLevelTargetInfo { security_level });
    info!(
        "event=synthetic registered/reused security-level target ptr=0x{:x} cookie=0x{:x} security_level={:?} source_method={:?} uid={} pid={} sid='{}'",
        target.ptr, target.cookie, security_level, source_method, caller.uid, caller.pid, caller.sid
    );
    Ok(carrier)
}

unsafe fn transaction_parts(tr: &binder_transaction_data) -> (*mut u8, usize, *mut usize, usize) {
    (
        tr.data.ptr.buffer as *mut u8,
        tr.data_size,
        tr.data.ptr.offsets as *mut usize,
        tr.offsets_size,
    )
}

unsafe fn register_operation_target_from_reply(
    tr: &binder_transaction_data,
    route: RouteTarget,
    backend: Option<AospOperationBinder>,
    aad_allowed: bool,
) -> anyhow::Result<bool> {
    let (data, data_size, offsets, offsets_size) = transaction_parts(tr);
    let carrier =
        parcel::extract_create_operation_reply_carrier(data, data_size, offsets, offsets_size)?;
    if !carrier.is_object {
        return Ok(false);
    }

    let target = parse_local_binder_target_from_parcel_bytes(&carrier.bytes)
        .ok_or_else(|| anyhow::anyhow!("failed to parse local operation carrier target"))?;
    remember_operation_target(
        target,
        OperationTargetInfo {
            route,
            aad_allowed,
            backend,
            finalized: false,
        },
    );
    info!(
        "event=route observed operation carrier ptr=0x{:x} cookie=0x{:x} preferred_route={:?} aad_allowed={}",
        target.ptr,
        target.cookie,
        route,
        aad_allowed,
    );
    Ok(true)
}

pub(super) unsafe fn handle_br_transaction(
    connection: BinderStateKey,
    tr: &mut binder_transaction_data,
    caller_sid: Option<String>,
    command_name: &str,
) -> bool {
    let expects_reply = (tr.flags & super::binder::TF_ONE_WAY) == 0;
    if expects_reply {
        push_pending_frame(connection);
    }

    if forward::is_bypassed() {
        debug!(
            "skipped {} because bypass is active: code=0x{:x} uid={} pid={}",
            command_name, tr.code, tr.sender_euid, tr.sender_pid
        );
        return false;
    }

    let cfg = config::get();
    if !cfg.main.enabled {
        debug!("event=decision injector disabled by config");
        return false;
    }

    let Some(parcel_bytes) = super::binder::transaction_data_bytes(tr) else {
        warn!(
            "event=decision null parcel buffer for {} code=0x{:x} uid={} pid={}",
            command_name, tr.code, tr.sender_euid, tr.sender_pid
        );
        return false;
    };

    let (data, data_size, offsets, offsets_size) = transaction_parts(tr);
    let request_interface =
        match parcel::peek_request_interface(data, data_size, offsets, offsets_size) {
            Ok(interface) => interface,
            Err(error) => {
                if parcel::contains_known_keystore_interface(parcel_bytes) {
                    debug!(
                        "event=decision failed to read keystore interface code=0x{:x}: {:#}",
                        tr.code, error
                    );
                }
                return false;
            }
        };
    if !is_known_keystore_interface(&request_interface) {
        return false;
    }
    let mut caller = CallerIdentity::new(tr.sender_euid.max(0) as u32, tr.sender_pid)
        .with_sid(caller_sid.unwrap_or_default());
    let caller_uid = caller.uid;
    // Authorization events are emitted by system auth components, not by the
    // app that later uses an auth-bound key. Mirror this global keystore state
    // after the system service accepts it; scoop still gates app key traffic.
    if request_interface == identify::KEYSTORE_AUTHORIZATION_INTERFACE {
        let request = match parcel::parse_authorization_request(
            data,
            data_size,
            offsets,
            offsets_size,
            tr.code,
        ) {
            Ok(request) => request,
            Err(error) => {
                debug!(
                    "event=decision failed to parse IKeystoreAuthorization request code=0x{:x}: {:#}",
                    tr.code, error
                );
                return false;
            }
        };

        let method = request.method();
        info!(
            "event=decision command={} authorization_method={:?} code=0x{:x} uid={} pid={} sid='{}'; mirroring auth state to OMK after system success",
            command_name,
            method,
            tr.code,
            caller.uid,
            caller.pid,
            caller.sid,
        );

        let requires_mirror = authorization_requires_mirror(&request);
        let mut pending = PendingAuthorizationCall {
            request,
            method,
            caller,
            mirror_update: None,
        };
        if requires_mirror {
            if expects_reply {
                match reserve_mirror_update(MirrorStateKind::Authorization) {
                    Ok(reservation) => pending.mirror_update = Some(reservation),
                    Err(error) => {
                        warn!(
                            "event=mirror failed to reserve durable authorization {:?} update for uid={} pid={}: {:#}; blocking System mutation",
                            method, caller_uid, pending.caller.pid, error
                        );
                        block_system_request(tr);
                        replace_top_pending(
                            connection,
                            PendingCall::PrecomputedAuthorization(
                                pending,
                                Status::new_service_specific_error(
                                    ResponseCode::SYSTEM_ERROR.0,
                                    None,
                                ),
                            ),
                        );
                        return true;
                    }
                }
            } else if let Err(error) = record_mirror_failure(
                MirrorStateKind::Authorization,
                method,
                &pending.caller,
                None,
            ) {
                warn!(
                    "event=mirror failed to persist one-way authorization {:?} marker for uid={} pid={}: {:#}; blocking System mutation",
                    method, caller_uid, pending.caller.pid, error
                );
                block_system_request(tr);
                return true;
            }
        }
        if expects_reply {
            replace_top_pending(connection, PendingCall::Authorization(pending));
        }
        return false;
    }

    // Maintenance calls mostly carry global keystore state, so mirror them after
    // system success. migrateKeyNamespace moves app keys, so scoop-routed callers
    // use OMK as the authoritative business path.
    if request_interface == identify::KEYSTORE_MAINTENANCE_INTERFACE {
        let request = match parcel::parse_maintenance_request(
            data,
            data_size,
            offsets,
            offsets_size,
            tr.code,
        ) {
            Ok(request) => request,
            Err(error) => {
                debug!(
                    "event=decision failed to parse IKeystoreMaintenance request code=0x{:x}: {:#}",
                    tr.code, error
                );
                return false;
            }
        };

        let method = request.method();
        let (route, packages, reason) = match &request {
            ParsedMaintenanceRequest::MigrateKeyNamespace { .. } => {
                let decision = evaluate_caller(&caller, &cfg);
                caller = caller.with_keybox_slot(cfg.keybox_slot_for_packages(&decision.packages));
                (
                    if decision.allowed {
                        RouteTarget::Omk
                    } else {
                        RouteTarget::System
                    },
                    decision.packages,
                    Some(decision.reason),
                )
            }
            _ => (RouteTarget::System, Vec::new(), None),
        };
        let mut pending = PendingMaintenanceCall {
            request,
            caller,
            route,
            mirror_update: None,
        };
        let mut precomputed = if pending.route == RouteTarget::Omk
            && matches!(
                &pending.request,
                ParsedMaintenanceRequest::MigrateKeyNamespace { .. }
            ) {
            match precompute_omk_migrate_reply(&pending) {
                OmkMigratePrecompute::Reply(reply) => {
                    block_system_request(tr);
                    Some(reply)
                }
                OmkMigratePrecompute::PreserveSystem => {
                    pending.route = RouteTarget::System;
                    None
                }
            }
        } else {
            None
        };
        let mut request_rewritten = precomputed.is_some();
        let route_note = if matches!(
            &pending.request,
            ParsedMaintenanceRequest::MigrateKeyNamespace { .. }
        ) {
            "using caller scoop decision as authoritative business path"
        } else {
            "mirroring maintenance state to OMK after system success"
        };
        info!(
            "event=decision command={} maintenance_method={:?} code=0x{:x} uid={} pid={} sid='{}' packages={:?} route={:?} reason={:?}; {}",
            command_name,
            method,
            tr.code,
            caller_uid,
            pending.caller.pid,
            pending.caller.sid,
            packages,
            pending.route,
            reason,
            route_note,
        );

        let requires_mirror =
            pending.route == RouteTarget::System && maintenance_requires_mirror(&pending.request);
        if requires_mirror {
            if expects_reply {
                match reserve_mirror_update(MirrorStateKind::Maintenance) {
                    Ok(reservation) => pending.mirror_update = Some(reservation),
                    Err(error) => {
                        warn!(
                            "event=mirror failed to reserve durable maintenance {:?} update for uid={} pid={}: {:#}; blocking System mutation",
                            method, caller_uid, pending.caller.pid, error
                        );
                        block_system_request(tr);
                        precomputed = Some(PrecomputedMaintenanceReply::Error(
                            Status::new_service_specific_error(ResponseCode::SYSTEM_ERROR.0, None),
                        ));
                        request_rewritten = true;
                    }
                }
            } else if let Err(error) =
                record_mirror_failure(MirrorStateKind::Maintenance, method, &pending.caller, None)
            {
                warn!(
                    "event=mirror failed to persist one-way maintenance {:?} marker for uid={} pid={}: {:#}; blocking System mutation",
                    method, caller_uid, pending.caller.pid, error
                );
                block_system_request(tr);
                return true;
            }
        }
        if !expects_reply {
            return request_rewritten;
        }
        if let Some(reply) = precomputed {
            replace_top_pending(
                connection,
                PendingCall::PrecomputedMaintenance(pending, reply),
            );
        } else {
            replace_top_pending(connection, PendingCall::Maintenance(pending));
        }
        return request_rewritten;
    }

    if request_interface == identify::KEYSTORE_SERVICE_INTERFACE {
        let decision = evaluate_caller(&caller, &cfg);
        caller = caller.with_keybox_slot(cfg.keybox_slot_for_packages(&decision.packages));
        let request =
            match parcel::parse_service_request(data, data_size, offsets, offsets_size, tr.code) {
                Ok(request) => request,
                Err(error) => {
                    debug!(
                        "event=decision failed to parse IKeystoreService request code=0x{:x}: {:#}",
                        tr.code, error
                    );
                    return false;
                }
            };

        let method = request.method();
        let original_code = tr.code;
        let allow_omk_grant = match should_allow_omk_grant_service_request_with_probe(
            &request,
            &decision,
            &caller,
            probe_omk_grant,
        ) {
            Ok(allow) => allow,
            Err(error) => {
                warn!(
                    "event=decision OMK grant ownership probe failed for uid={} pid={}: {:#}; returning SYSTEM_ERROR without executing System",
                    caller.uid, caller.pid, error
                );
                block_system_request(tr);
                if expects_reply {
                    let pending = PendingServiceCall {
                        request,
                        caller,
                        packages: decision.packages,
                        route: RouteTarget::Omk,
                    };
                    replace_top_pending(
                        connection,
                        PendingCall::PrecomputedService(
                            pending,
                            PrecomputedServiceReply::Error(Status::new_service_specific_error(
                                ResponseCode::SYSTEM_ERROR.0,
                                None,
                            )),
                        ),
                    );
                }
                return true;
            }
        };
        let mut route = if allow_omk_grant {
            RouteTarget::Omk
        } else {
            route_for_service_request(&request, &cfg.intercept)
        };
        if !decision.allowed && !allow_omk_grant {
            info!(
                "event=decision command={} service_method={:?} code=0x{:x} uid={} pid={} sid='{}' packages={:?} allowed=false reason={:?}; routing request to System",
                command_name,
                method,
                original_code,
                caller.uid,
                caller.pid,
                caller.sid,
                decision.packages,
                decision.reason,
            );
            route = RouteTarget::System;
        }
        if allow_omk_grant {
            info!(
                "event=decision command={} service_method={:?} code=0x{:x} uid={} pid={} sid='{}' packages={:?} allowed=true reason={:?} omk_grant=true",
                command_name,
                method,
                original_code,
                caller.uid,
                caller.pid,
                caller.sid,
                decision.packages,
                decision.reason,
            );
        }

        let precomputed_service_reply = if route == RouteTarget::Omk
            && matches!(
                method,
                ServiceMethod::UpdateSubcomponent
                    | ServiceMethod::Grant
                    | ServiceMethod::Ungrant
                    | ServiceMethod::DeleteKey
            ) {
            match precompute_omk_service_mutator_reply(&request, &caller) {
                OmkServicePrecompute::Reply(reply) => {
                    block_system_request(tr);
                    Some(reply)
                }
                OmkServicePrecompute::PreserveSystem => {
                    info!(
                        "event=route method={:?} uid={} pid={} route={:?} omk_unavailable=true; preserving original system request",
                        method, caller.uid, caller.pid, route
                    );
                    route = RouteTarget::System;
                    None
                }
            }
        } else {
            None
        };
        let request_rewritten = precomputed_service_reply.is_some();

        info!(
            "event=decision command={} service_method={:?} code=0x{:x} uid={} pid={} sid='{}' packages={:?} allowed={} reason={:?}",
            command_name,
            method,
            original_code,
            caller.uid,
            caller.pid,
            caller.sid,
            decision.packages,
            decision.allowed,
            decision.reason,
        );
        info!(
            "event=route method={:?} uid={} pid={} route={:?}",
            method, caller.uid, caller.pid, route
        );

        let pending = PendingServiceCall {
            request,
            caller,
            packages: decision.packages,
            route,
        };
        if !expects_reply {
            if let Some(reply) = precomputed_service_reply.as_ref() {
                if let Err(error) = build_precomputed_service_reply(reply) {
                    warn!(
                        "event=route failed to build one-way OMK service {:?} reply for uid={} pid={}: {:#}",
                        method, caller_uid, pending.caller.pid, error
                    );
                }
                return request_rewritten;
            }
        }
        if expects_reply {
            if let Some(reply) = precomputed_service_reply {
                replace_top_pending(connection, PendingCall::PrecomputedService(pending, reply));
            } else {
                replace_top_pending(connection, PendingCall::Service(pending));
            }
        }
        return request_rewritten;
    }

    let Some(target) = target_from_transaction(tr) else {
        debug!(
            "event=decision skipping keystore request without local target code=0x{:x} target={}",
            tr.code,
            format_target(tr)
        );
        return false;
    };

    if request_interface == identify::KEYSTORE_SECURITY_LEVEL_INTERFACE {
        let decision = evaluate_caller(&caller, &cfg);
        let Some(target_info) = tracker::lookup_security_level_target(target) else {
            debug!(
                "event=decision skipping IKeystoreSecurityLevel request for unmapped target ptr=0x{:x} cookie=0x{:x}",
                target.ptr, target.cookie
            );
            return false;
        };

        let request = match parcel::parse_security_level_request(
            data,
            data_size,
            offsets,
            offsets_size,
            tr.code,
        ) {
            Ok(request) => request,
            Err(error) => {
                debug!(
                    "event=decision failed to parse IKeystoreSecurityLevel request code=0x{:x}: {:#}",
                    tr.code, error
                );
                return false;
            }
        };

        let method = request.method();
        let allow_unknown_omk_route = match should_allow_omk_grant_security_level_request_with_probe(
            &request,
            &decision,
            &caller,
            probe_omk_grant,
        ) {
            Ok(allow) => allow,
            Err(error) => {
                warn!(
                    "event=decision OMK grant ownership probe failed for uid={} pid={}: {:#}; returning SYSTEM_ERROR without executing System",
                    caller.uid, caller.pid, error
                );
                block_system_request(tr);
                if expects_reply {
                    let pending = PendingSecurityLevelCall {
                        request,
                        caller,
                        packages: decision.packages,
                        route: RouteTarget::Omk,
                        security_level: target_info.security_level,
                    };
                    replace_top_pending(
                        connection,
                        PendingCall::PrecomputedSecurityLevel(
                            pending,
                            Box::new(Some(synthetic_fallback_reply())),
                        ),
                    );
                }
                return true;
            }
        };
        // Keystore2 shares each security-level Binder between getSecurityLevel and getKeyEntry.
        let scoop_enabled = security_level_scoop_enabled(&cfg.intercept);
        let route = if allow_unknown_omk_route || decision.allowed && scoop_enabled {
            RouteTarget::Omk
        } else {
            RouteTarget::System
        };
        if !decision.allowed && !allow_unknown_omk_route {
            info!(
                "event=decision command={} security_level_method={:?} code=0x{:x} uid={} pid={} sid='{}' packages={:?} allowed=false reason={:?} target=ptr:0x{:x}/cookie:0x{:x} security_level={:?}; routing request to System",
                command_name,
                method,
                tr.code,
                caller.uid,
                caller.pid,
                caller.sid,
                decision.packages,
                decision.reason,
                target.ptr,
                target.cookie,
                target_info.security_level,
            );
        }

        info!(
            "event=decision command={} security_level_method={:?} code=0x{:x} uid={} pid={} sid='{}' packages={:?} allowed={} reason={:?} target=ptr:0x{:x}/cookie:0x{:x} security_level={:?} scoop_enabled={} omk_derived_route={}",
            command_name,
            method,
            tr.code,
            caller.uid,
            caller.pid,
            caller.sid,
            decision.packages,
            decision.allowed,
            decision.reason,
            target.ptr,
            target.cookie,
            target_info.security_level,
            scoop_enabled,
            allow_unknown_omk_route,
        );
        info!(
            "event=route security_level_method={:?} uid={} pid={} route={:?} security_level={:?}",
            method, caller.uid, caller.pid, route, target_info.security_level
        );

        let mut pending = PendingSecurityLevelCall {
            request,
            caller,
            packages: decision.packages,
            route,
            security_level: target_info.security_level,
        };
        if expects_reply && pending.route == RouteTarget::Omk {
            let reply = match build_omk_security_level_reply(&pending, true) {
                Ok(Some(reply)) => reply,
                Ok(None) => {
                    info!(
                        "event=route security-level {:?} OMK unavailable for uid={} pid={}; preserving original system request",
                        method, caller_uid, pending.caller.pid
                    );
                    pending.route = RouteTarget::System;
                    replace_top_pending(connection, PendingCall::SecurityLevel(pending));
                    return false;
                }
                Err(error) => {
                    warn!(
                        "event=route failed to precompute authoritative OMK security-level {:?} reply for uid={} pid={}: {:#}; returning SYSTEM_ERROR without executing System",
                        method, caller_uid, pending.caller.pid, error
                    );
                    synthetic_fallback_reply()
                }
            };
            block_system_request(tr);
            replace_top_pending(
                connection,
                PendingCall::PrecomputedSecurityLevel(pending, Box::new(Some(reply))),
            );
            return true;
        }
        if !expects_reply
            && route == RouteTarget::Omk
            && can_execute_one_way(SyntheticTargetKind::SecurityLevel, tr.code)
        {
            return handle_omk_one_way_security_level_request(tr, &pending);
        }
        if expects_reply {
            replace_top_pending(connection, PendingCall::SecurityLevel(pending));
        }
        return false;
    }

    if request_interface == identify::KEYSTORE_OPERATION_INTERFACE {
        let Some(operation_target) = lookup_operation_target(target) else {
            debug!(
                "event=decision skipping IKeystoreOperation request for unmapped target ptr=0x{:x} cookie=0x{:x}",
                target.ptr, target.cookie
            );
            return false;
        };

        let request = match parcel::parse_operation_request(
            data,
            data_size,
            offsets,
            offsets_size,
            tr.code,
        ) {
            Ok(request) => request,
            Err(error) => {
                debug!(
                    "event=decision failed to parse IKeystoreOperation request code=0x{:x}: {:#}",
                    tr.code, error
                );
                return false;
            }
        };

        let method = request.method();

        info!(
            "event=decision command={} operation_method={:?} code=0x{:x} uid={} pid={} sid='{}' target=ptr:0x{:x}/cookie:0x{:x}",
            command_name,
            method,
            tr.code,
            caller.uid,
            caller.pid,
            caller.sid,
            target.ptr,
            target.cookie,
        );
        info!(
            "event=route operation_method={:?} uid={} pid={} route={:?}",
            method, caller.uid, caller.pid, operation_target.route
        );

        let pending = PendingOperationCall {
            request,
            caller,
            target,
        };
        if !expects_reply
            && operation_target.route == RouteTarget::Omk
            && can_execute_one_way(SyntheticTargetKind::Operation, tr.code)
        {
            return handle_omk_one_way_operation_request(tr, &pending);
        }
        if expects_reply {
            replace_top_pending(connection, PendingCall::Operation(pending));
        }
        return false;
    }

    debug!(
        "event=decision skipping unsupported keystore interface request code=0x{:x}",
        tr.code
    );

    false
}

unsafe fn handle_omk_one_way_security_level_request(
    tr: &mut binder_transaction_data,
    pending: &PendingSecurityLevelCall,
) -> bool {
    match build_security_level_reply_rewrite(tr, pending) {
        Ok(Some(_)) => {
            block_system_request(tr);
            true
        }
        Ok(None) => false,
        Err(error) => {
            warn!(
                "event=route failed to execute one-way OMK security-level {:?} for uid={} pid={}: {:#}; consuming original system request",
                pending.request.method(), pending.caller.uid, pending.caller.pid, error
            );
            block_system_request(tr);
            true
        }
    }
}

unsafe fn handle_omk_one_way_operation_request(
    tr: &mut binder_transaction_data,
    pending: &PendingOperationCall,
) -> bool {
    if let Err(error) = build_operation_reply_rewrite(pending) {
        warn!(
            "event=route failed to execute one-way OMK operation {:?} for uid={} pid={}: {:#}; consuming original system request",
            pending.request.method(), pending.caller.uid, pending.caller.pid, error
        );
    }
    block_system_request(tr);
    true
}

pub(super) unsafe fn handle_bc_reply(
    connection: BinderStateKey,
    tr: &mut binder_transaction_data,
) -> Option<u64> {
    let (frame_id, pending) = PENDING_REPLY_QUEUE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let frame = slot
            .iter_mut()
            .rev()
            .find(|frame| frame.connection == connection && !frame.claimed)?;
        frame.claimed = true;
        Some((frame.id, frame.pending.take()))
    })?;
    let Some(mut pending) = pending else {
        return Some(frame_id);
    };

    let original_data_size = tr.data_size;
    let original_offsets_size = tr.offsets_size;
    let original_flags = tr.flags;
    let original_objects = describe_transaction_objects(tr);

    let result = match &mut pending {
        PendingCall::Authorization(call) => {
            debug!(
                "event=reply handling authorization {:?} uid={} pid={}",
                call.method, call.caller.uid, call.caller.pid
            );
            build_authorization_reply_mirror(tr, call)
        }
        PendingCall::PrecomputedAuthorization(call, status) => {
            debug!(
                "event=reply handling precomputed authorization {:?} uid={} pid={}",
                call.method, call.caller.uid, call.caller.pid
            );
            build_omk_status_reply(status).map(Some)
        }
        PendingCall::Maintenance(call) => {
            debug!(
                "event=reply handling maintenance {:?} uid={} pid={}",
                call.request.method(),
                call.caller.uid,
                call.caller.pid
            );
            build_maintenance_reply_mirror(tr, call)
        }
        PendingCall::PrecomputedMaintenance(call, precomputed) => {
            debug!(
                "event=reply handling precomputed maintenance {:?} route={:?} uid={} pid={}",
                call.request.method(),
                call.route,
                call.caller.uid,
                call.caller.pid
            );
            build_precomputed_maintenance_reply(precomputed).map(Some)
        }
        PendingCall::Service(call) => {
            debug!(
                "event=reply handling service {:?} route={:?} uid={} pid={} packages={:?}",
                call.request.method(),
                call.route,
                call.caller.uid,
                call.caller.pid,
                call.packages
            );
            build_service_reply_rewrite(tr, call)
        }
        PendingCall::PrecomputedService(call, precomputed) => {
            debug!(
                "event=reply handling precomputed service {:?} route={:?} uid={} pid={} packages={:?}",
                call.request.method(), call.route, call.caller.uid, call.caller.pid, call.packages
            );
            build_precomputed_service_reply(precomputed).map(Some)
        }
        PendingCall::SecurityLevel(call) => {
            debug!(
                "event=reply handling security-level {:?} route={:?} uid={} pid={} packages={:?} security_level={:?}",
                call.request.method(), call.route, call.caller.uid, call.caller.pid, call.packages, call.security_level
            );
            build_security_level_reply_rewrite(tr, call)
        }
        PendingCall::PrecomputedSecurityLevel(call, reply) => {
            debug!(
                "event=reply handling precomputed security-level {:?} route={:?} uid={} pid={} packages={:?} security_level={:?}",
                call.request.method(), call.route, call.caller.uid, call.caller.pid, call.packages, call.security_level
            );
            reply
                .take()
                .ok_or_else(|| anyhow::anyhow!("precomputed security-level reply already consumed"))
                .map(Some)
        }
        PendingCall::Operation(call) => {
            debug!(
                "event=reply handling operation {:?} uid={} pid={} target=ptr:0x{:x}/cookie:0x{:x}",
                call.request.method(),
                call.caller.uid,
                call.caller.pid,
                call.target.ptr,
                call.target.cookie
            );
            build_operation_reply_rewrite(call)
        }
    };

    match result {
        Ok(Some(reply)) => {
            let (kind, method, uid, pid) = pending.reply_log_context();
            install_outbound_reply(connection, tr, reply);
            info!(
                "event=reply rewrote {} {} reply for uid={} pid={} original={{flags=0x{:x}, data_size={}, offsets_size={}, objects={}}} rewritten={{flags=0x{:x}, data_size={}, offsets_size={}, objects={}}}",
                kind,
                method,
                uid,
                pid,
                original_flags,
                original_data_size,
                original_offsets_size,
                original_objects,
                tr.flags,
                tr.data_size,
                tr.offsets_size,
                describe_transaction_objects(tr),
            );
        }
        Ok(None) => {
            let observed = match &pending {
                PendingCall::Service(call) if call.route == RouteTarget::Omk => {
                    observe_system_service_reply(tr, call)
                }
                PendingCall::SecurityLevel(call) if call.route == RouteTarget::Omk => {
                    observe_system_security_level_reply(tr, call)
                }
                _ => Ok(None),
            };
            match observed {
                Ok(Some(reply)) => install_outbound_reply(connection, tr, reply),
                Ok(None) => {}
                Err(error) => {
                    warn!(
                        "event=route failed to observe preserved system fallback reply: {:#}",
                        error
                    );
                }
            }
        }
        Err(error) => {
            if pending_preserves_system_on_rewrite_failure(&pending) {
                warn!(
                    "event=reply failed to rewrite pending reply: {:#}; keeping original system reply",
                    error
                );
            } else {
                warn!(
                    "event=reply failed to rewrite authoritative OMK reply: {:#}; returning SYSTEM_ERROR",
                    error
                );
                install_outbound_reply(connection, tr, synthetic_fallback_reply());
            }
        }
    }
    Some(frame_id)
}

fn pending_preserves_system_on_rewrite_failure(pending: &PendingCall) -> bool {
    match pending {
        PendingCall::Authorization(_) => true,
        PendingCall::PrecomputedAuthorization(_, _) => false,
        PendingCall::Maintenance(call) => call.route != RouteTarget::Omk,
        PendingCall::PrecomputedMaintenance(_, _) => false,
        PendingCall::Service(call) => call.route != RouteTarget::Omk,
        PendingCall::PrecomputedService(_, _) => false,
        PendingCall::SecurityLevel(call) => call.route != RouteTarget::Omk,
        PendingCall::PrecomputedSecurityLevel(_, _) => false,
        PendingCall::Operation(call) => lookup_operation_target(call.target)
            .is_some_and(|target| target.route == RouteTarget::System),
    }
}

fn register_security_level_carrier(
    carrier: &parcel::ReplyBinderCarrier,
    security_level: crate::android::hardware::security::keymint::SecurityLevel::SecurityLevel,
    source_method: ServiceMethod,
) -> anyhow::Result<()> {
    if !carrier.is_object {
        warn!(
            "event=route system security-level carrier for {:?} was null; skipping mapping",
            security_level
        );
        return Ok(());
    }

    let target = unsafe { parse_local_binder_target_from_parcel_bytes(&carrier.bytes) }
        .ok_or_else(|| anyhow::anyhow!("failed to parse local security-level carrier target"))?;
    tracker::remember_security_level_target(target, SecurityLevelTargetInfo { security_level });
    info!(
        "event=route registered security-level carrier ptr=0x{:x} cookie=0x{:x} security_level={:?} source_method={:?}",
        target.ptr, target.cookie, security_level, source_method
    );
    Ok(())
}

fn build_omk_status_reply(status: &Status) -> anyhow::Result<OutboundReply> {
    // OMK is the authoritative keystore backend, so its ServiceSpecific codes
    // are keystore ResponseCode / KeyMint ErrorCode values that must reach the
    // client verbatim (matching keystore2's own into_binder for Error::Rc/Km).
    // Every other status — success on an error path, a transport failure, or
    // any other binder exception — is mapped to a service-specific SYSTEM_ERROR,
    // matching AOSP error_to_serialized_error (Error::Binder/BinderTransaction
    // -> SYSTEM_ERROR); keystore2 never forwards a raw transport status_t.
    if status.exception_code() == ExceptionCode::ServiceSpecific {
        return parcel::build_status_reply(status);
    }

    Ok(synthetic_fallback_reply())
}

fn error_status(error: &anyhow::Error) -> Option<&Status> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<Status>())
}

fn error_status_code(error: &anyhow::Error) -> Option<StatusCode> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<StatusCode>().copied())
}

fn synthetic_parse_error_status_code(error: &anyhow::Error) -> StatusCode {
    error_status_code(error).unwrap_or(StatusCode::BadValue)
}

fn synthetic_parse_error_reply(status: StatusCode) -> anyhow::Result<SyntheticReply> {
    if status == StatusCode::UnexpectedNull {
        return Ok(synthetic_parcel_reply(parcel::build_status_reply(
            &Status::from(status),
        )?));
    }
    Ok(SyntheticReply::Status(status.into()))
}

fn build_omk_error_reply(error: &anyhow::Error) -> anyhow::Result<OutboundReply> {
    if let Some(status) = error_status(error) {
        return build_omk_status_reply(status);
    }

    // A bare StatusCode (no wrapped Status) is an injector-internal failure, not
    // an OMK business error (those arrive as Status, handled above). AOSP
    // keystore2 maps every bare StatusCode through map_binder_status_code ->
    // Error::BinderTransaction -> SYSTEM_ERROR unconditionally, ignoring the code
    // itself (error.rs map_binder_status_code/error_to_serialized_error); it
    // never returns a raw transport status_t nor a code-specific parcel.
    // OMK-unavailable codes are already filtered out earlier by
    // omk_unavailable_error. Errors without any status collapse to SYSTEM_ERROR
    // the same way.
    Ok(synthetic_fallback_reply())
}

fn precomputed_omk_error_reply(error: &anyhow::Error) -> Status {
    if let Some(status) = error_status(error) {
        if status.exception_code() == ExceptionCode::ServiceSpecific {
            return status.clone();
        }
    }

    // Mirror build_omk_error_reply: a bare StatusCode (no wrapped Status) is an
    // injector-internal failure. AOSP keystore2 maps any bare StatusCode through
    // map_binder_status_code -> Error::BinderTransaction -> SYSTEM_ERROR
    // (error.rs map_binder_status_code/error_to_serialized_error), regardless of
    // the code, so it is normalized to a service-specific SYSTEM_ERROR here.
    Status::new_service_specific_error(ResponseCode::SYSTEM_ERROR.0, None)
}

fn omk_unavailable_status(status: &Status) -> bool {
    status.exception_code() == ExceptionCode::TransactionFailed
        && omk_unavailable_status_code(status.transaction_error())
}

fn omk_unavailable_status_code(status: StatusCode) -> bool {
    ipc::is_rpc_cache_invalidating_status_code(status) || status == StatusCode::NameNotFound
}

fn omk_unavailable_error(error: &anyhow::Error) -> bool {
    if let Some(unavailable) = error.chain().find_map(|cause| {
        cause
            .downcast_ref::<Status>()
            .map(omk_unavailable_status)
            .or_else(|| {
                cause
                    .downcast_ref::<StatusCode>()
                    .map(|status| omk_unavailable_status_code(*status))
            })
    }) {
        return unavailable;
    }

    error.chain().any(|cause| {
        matches!(
            cause.to_string().as_str(),
            "failed to connect to omk service"
                | "failed to connect to omk_maintenance service"
                | "failed to connect to omk_authorization service"
        )
    })
}

fn build_omk_error_reply_or_preserve_system(
    error: &anyhow::Error,
) -> anyhow::Result<Option<OutboundReply>> {
    if omk_unavailable_error(error) {
        Ok(None)
    } else {
        build_omk_error_reply(error).map(Some)
    }
}

fn build_omk_status_reply_or_preserve_system(
    status: &Status,
) -> anyhow::Result<Option<OutboundReply>> {
    if omk_unavailable_status(status) {
        Ok(None)
    } else {
        build_omk_status_reply(status).map(Some)
    }
}

fn omk_error_reply_for_method(
    method: &str,
    caller: &CallerIdentity,
    error: &anyhow::Error,
) -> anyhow::Result<Option<OutboundReply>> {
    match build_omk_error_reply_or_preserve_system(error)? {
        Some(reply) => {
            warn!(
                "event=reply OMK {} failed for uid={} pid={}: {:#}; returning OMK error",
                method, caller.uid, caller.pid, error
            );
            Ok(Some(reply))
        }
        None => {
            warn!(
                "event=reply OMK {} unavailable for uid={} pid={}: {:#}; preserving original system reply",
                method, caller.uid, caller.pid, error
            );
            Ok(None)
        }
    }
}

fn omk_status_reply_for_method(
    method: &str,
    caller: &CallerIdentity,
    status: &Status,
) -> anyhow::Result<Option<OutboundReply>> {
    match build_omk_status_reply_or_preserve_system(status)? {
        Some(reply) => {
            warn!(
                "event=reply OMK {} failed for uid={} pid={}: {:#}; returning OMK error",
                method, caller.uid, caller.pid, status
            );
            Ok(Some(reply))
        }
        None => {
            warn!(
                "event=reply OMK {} unavailable for uid={} pid={}: {:#}; preserving original system reply",
                method, caller.uid, caller.pid, status
            );
            Ok(None)
        }
    }
}

fn build_service_specific_reply(code: i32) -> anyhow::Result<parcel::OwnedReply> {
    parcel::build_status_reply(&Status::new_service_specific_error(code, None))
}

fn invalid_operation_handle_reply() -> anyhow::Result<parcel::OwnedReply> {
    build_service_specific_reply(
        crate::android::hardware::security::keymint::ErrorCode::ErrorCode::INVALID_OPERATION_HANDLE
            .0,
    )
}

fn synthetic_fallback_reply() -> parcel::OwnedReply {
    build_service_specific_reply(ResponseCode::SYSTEM_ERROR.0)
        .expect("synthetic system-error status should serialize")
}

macro_rules! finalize_system_success {
    ($result:expr, $context:expr) => {
        match $result {
            Ok(value) => value,
            Err(error) => {
                warn!(
                    "event=route failed to finalize {} after System success: {:#}; replacing the reply with SYSTEM_ERROR",
                    $context, error
                );
                return Ok(Some(synthetic_fallback_reply()));
            }
        }
    };
}

unsafe fn malformed_system_success_reply(
    data: *mut u8,
    data_size: usize,
    offsets: *mut usize,
    offsets_size: usize,
    context: &str,
    error: anyhow::Error,
) -> anyhow::Result<Option<OutboundReply>> {
    if parcel::parse_reply_status(data, data_size, offsets, offsets_size)
        .is_ok_and(|status| !status.is_ok())
    {
        return Ok(None);
    }
    warn!(
        "event=route failed to decode {} after System success: {:#}; replacing the reply with SYSTEM_ERROR",
        context, error
    );
    Ok(Some(synthetic_fallback_reply()))
}

fn synthetic_unknown_transaction_reply() -> SyntheticReply {
    SyntheticReply::Status(StatusCode::UnknownTransaction.into())
}

fn synthetic_parcel_reply(reply: parcel::OwnedReply) -> SyntheticReply {
    SyntheticReply::Parcel(Box::new(reply))
}

fn synthetic_reply_from_outbound(reply: Option<OutboundReply>) -> SyntheticReply {
    synthetic_parcel_reply(reply.unwrap_or_else(synthetic_fallback_reply))
}

fn synthetic_unknown_transaction_reply_for(
    kind: SyntheticTargetKind,
    code: u32,
) -> Option<SyntheticReply> {
    let known = match kind {
        SyntheticTargetKind::SecurityLevel => {
            identify::security_level_method_from_code(code).is_some()
        }
        SyntheticTargetKind::Operation => identify::operation_method_from_code(code).is_some(),
    };
    (!known).then(synthetic_unknown_transaction_reply)
}

fn build_precomputed_service_reply(
    precomputed: &PrecomputedServiceReply,
) -> anyhow::Result<OutboundReply> {
    match precomputed {
        PrecomputedServiceReply::UpdateSubcomponentSuccess => Ok(parcel::build_void_reply()?),
        PrecomputedServiceReply::GrantSuccess(omk_grant) => {
            Ok(parcel::build_plain_reply(omk_grant)?)
        }
        PrecomputedServiceReply::UngrantSuccess | PrecomputedServiceReply::DeleteKeySuccess => {
            Ok(parcel::build_void_reply()?)
        }
        PrecomputedServiceReply::Error(status) => build_omk_status_reply(status),
    }
}

fn build_precomputed_maintenance_reply(
    precomputed: &PrecomputedMaintenanceReply,
) -> anyhow::Result<OutboundReply> {
    match precomputed {
        PrecomputedMaintenanceReply::Success => Ok(parcel::build_void_reply()?),
        PrecomputedMaintenanceReply::Error(status) => build_omk_status_reply(status),
    }
}

fn build_synthetic_aidl_metadata_reply(
    method: AidlMetadataMethod,
) -> anyhow::Result<SyntheticReply> {
    let metadata = keystore2_aidl_metadata()?;
    match method {
        AidlMetadataMethod::GetInterfaceHash => {
            let hash = metadata.hash.to_string();
            Ok(synthetic_parcel_reply(parcel::build_plain_reply(&hash)?))
        }
        AidlMetadataMethod::GetInterfaceVersion => Ok(synthetic_parcel_reply(
            parcel::build_plain_reply(&metadata.version)?,
        )),
    }
}

fn keystore2_aidl_metadata() -> anyhow::Result<Keystore2AidlMetadata> {
    match KEYSTORE2_AIDL_METADATA
        .get_or_init(|| resolve_keystore2_aidl_metadata().map_err(|error| format!("{error:#}")))
    {
        Ok(metadata) => Ok(*metadata),
        Err(error) => Err(anyhow::anyhow!("{}", error)),
    }
}

fn resolve_keystore2_aidl_metadata() -> anyhow::Result<Keystore2AidlMetadata> {
    let version = probe_keystore2_aidl_version_from_vintf().unwrap_or_else(|| {
        let fallback = fallback_keystore2_aidl_version_from_android();
        warn!(
            "event=synthetic failed to resolve {} AIDL version from VINTF; using Android-version fallback v{}",
            KEYSTORE2_HAL_NAME, fallback
        );
        fallback
    });
    let hash =
        kmr_common::consts::android_system_keystore2_aidl_hash(version).ok_or_else(|| {
            anyhow::anyhow!(
                "no precomputed {} AIDL hash for version {}",
                KEYSTORE2_HAL_NAME,
                version
            )
        })?;
    Ok(Keystore2AidlMetadata { version, hash })
}

fn fallback_keystore2_aidl_version_from_android() -> i32 {
    kmr_common::android_version::android_major_version()
        .and_then(kmr_common::consts::android_system_keystore2_aidl_version_for_android_major)
        .unwrap_or(kmr_common::consts::ANDROID_SYSTEM_KEYSTORE2_LATEST_AIDL_VERSION)
}

fn probe_keystore2_aidl_version_from_vintf() -> Option<i32> {
    for path in kmr_common::vintf::manifest_paths() {
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        match parse_keystore2_aidl_version_xml(&contents) {
            Ok(Some(version)) => return Some(version),
            Ok(None) => {}
            Err(error) => {
                warn!(
                    "event=synthetic ignoring invalid VINTF manifest {}: {error:#}",
                    path.display()
                );
            }
        }
    }
    None
}

fn parse_keystore2_aidl_version_xml(xml: &str) -> anyhow::Result<Option<i32>> {
    kmr_common::vintf::parse_aidl_hal_version_xml(
        xml,
        KEYSTORE2_HAL_NAME,
        KEYSTORE2_SERVICE_INTERFACE,
        KEYSTORE2_SERVICE_INSTANCE,
        normalize_keystore2_aidl_version,
    )
}

fn normalize_keystore2_aidl_version(version: i32) -> Option<i32> {
    (1..=kmr_common::consts::ANDROID_SYSTEM_KEYSTORE2_LATEST_AIDL_VERSION)
        .contains(&version)
        .then_some(version)
}

fn synthetic_debug_pid() -> i32 {
    unsafe { libc::getpid() }
}

unsafe fn synthetic_base_transaction_reply(
    kind: Option<SyntheticTargetKind>,
    target: LocalBinderTarget,
    tr: &binder_transaction_data,
) -> anyhow::Result<Option<SyntheticReply>> {
    let code = tr.code;
    let reply = match code {
        rsbinder::INTERFACE_TRANSACTION => match kind {
            Some(kind) => synthetic_parcel_reply(parcel::build_interface_descriptor_reply(
                synthetic_target_interface(kind),
            )?),
            None => synthetic_unknown_transaction_reply(),
        },
        rsbinder::PING_TRANSACTION
        | rsbinder::SHELL_COMMAND_TRANSACTION
        | rsbinder::SYSPROPS_TRANSACTION => synthetic_parcel_reply(parcel::build_empty_reply()),
        rsbinder::EXTENSION_TRANSACTION => {
            synthetic_parcel_reply(parcel::build_null_binder_reply()?)
        }
        rsbinder::DEBUG_PID_TRANSACTION => {
            synthetic_parcel_reply(parcel::build_raw_i32_reply(synthetic_debug_pid())?)
        }
        rsbinder::DUMP_TRANSACTION => {
            let (data, data_size, offsets, offsets_size) = transaction_parts(tr);
            if let Err(error) =
                parcel::validate_dump_request(data, data_size, offsets, offsets_size)
            {
                warn!(
                    "event=synthetic malformed DUMP_TRANSACTION for target ptr=0x{:x} cookie=0x{:x} kind={:?}: {:#}; returning BAD_TYPE",
                    target.ptr, target.cookie, kind, error
                );
                SyntheticReply::Status(StatusCode::BadType.into())
            } else {
                synthetic_parcel_reply(parcel::build_empty_reply())
            }
        }
        rsbinder::SET_RPC_CLIENT_TRANSACTION
        | rsbinder::START_RECORDING_TRANSACTION
        | rsbinder::STOP_RECORDING_TRANSACTION => {
            SyntheticReply::Status(StatusCode::InvalidOperation.into())
        }
        rsbinder::TWEET_TRANSACTION | rsbinder::LIKE_TRANSACTION => {
            synthetic_unknown_transaction_reply()
        }
        _ if !(rsbinder::FIRST_CALL_TRANSACTION..=rsbinder::LAST_CALL_TRANSACTION)
            .contains(&code) =>
        {
            synthetic_unknown_transaction_reply()
        }
        _ => return Ok(None),
    };
    Ok(Some(reply))
}

fn synthetic_transaction_caller(
    fallback: Option<&CallerIdentity>,
    tr: &binder_transaction_data,
    caller_sid: Option<String>,
) -> CallerIdentity {
    let uid = if tr.sender_euid >= 0 {
        tr.sender_euid as u32
    } else {
        fallback.map_or(0, |caller| caller.uid)
    };
    let pid = if tr.sender_pid != 0 {
        tr.sender_pid
    } else {
        fallback.map_or(0, |caller| caller.pid)
    };
    let sid = caller_sid
        .filter(|sid| !sid.is_empty())
        .or_else(|| fallback.map(|caller| caller.sid.clone()))
        .unwrap_or_default();
    CallerIdentity::new(uid, pid).with_sid(sid)
}

fn can_execute_one_way(kind: SyntheticTargetKind, code: u32) -> bool {
    match kind {
        SyntheticTargetKind::SecurityLevel => matches!(
            identify::security_level_method_from_code(code),
            Some(
                SecurityLevelMethod::GenerateKey
                    | SecurityLevelMethod::CreateOperation
                    | SecurityLevelMethod::ImportKey
                    | SecurityLevelMethod::ImportWrappedKey
                    | SecurityLevelMethod::DeleteKey
            )
        ),
        SyntheticTargetKind::Operation => matches!(
            identify::operation_method_from_code(code),
            Some(
                OperationMethod::UpdateAad
                    | OperationMethod::Update
                    | OperationMethod::Finish
                    | OperationMethod::Abort
            )
        ),
    }
}

pub(super) unsafe fn handle_synthetic_br_transaction(
    tr: &binder_transaction_data,
    caller_sid: Option<String>,
    command_name: &str,
) -> Option<SyntheticReply> {
    let target = target_from_transaction(tr)?;
    let info = lookup_synthetic_target_info(target)?;
    let kind = info.kind;

    let result = build_synthetic_br_transaction_reply(tr, target, info, caller_sid, command_name);
    let reply = match result {
        Ok(reply) => reply,
        Err(error) => {
            warn!(
                "event=synthetic failed to handle {} target=ptr:0x{:x}/cookie:0x{:x} kind={:?} code=0x{:x}: {:#}; returning SYSTEM_ERROR",
                command_name,
                target.ptr,
                target.cookie,
                kind,
                tr.code,
                error
            );
            if (tr.flags & super::binder::TF_ONE_WAY) != 0 {
                SyntheticReply::NoReply
            } else {
                synthetic_parcel_reply(synthetic_fallback_reply())
            }
        }
    };
    Some(reply)
}

unsafe fn build_synthetic_br_transaction_reply(
    tr: &binder_transaction_data,
    target: LocalBinderTarget,
    info: SyntheticTargetInfo,
    caller_sid: Option<String>,
    command_name: &str,
) -> anyhow::Result<SyntheticReply> {
    let expects_reply = (tr.flags & super::binder::TF_ONE_WAY) == 0;
    if !expects_reply && !can_execute_one_way(info.kind, tr.code) {
        return Ok(SyntheticReply::NoReply);
    }

    let reply =
        build_synthetic_br_transaction_reply_inner(tr, target, info, caller_sid, command_name)?;
    if expects_reply {
        Ok(reply)
    } else {
        Ok(SyntheticReply::NoReply)
    }
}

unsafe fn build_synthetic_br_transaction_reply_inner(
    tr: &binder_transaction_data,
    target: LocalBinderTarget,
    info: SyntheticTargetInfo,
    caller_sid: Option<String>,
    command_name: &str,
) -> anyhow::Result<SyntheticReply> {
    let kind = info.kind;
    if let Some(reply) = synthetic_base_transaction_reply(Some(kind), target, tr)? {
        return Ok(reply);
    }

    if let Some(method) = identify::aidl_metadata_method_from_code(tr.code) {
        let (data, data_size, offsets, offsets_size) = transaction_parts(tr);
        let request_interface = match parcel::parse_metadata_request_interface_allow_trailing(
            data,
            data_size,
            offsets,
            offsets_size,
        ) {
            Ok(Some(interface)) => interface,
            Ok(None) => return Ok(SyntheticReply::Status(StatusCode::BadType.into())),
            Err(error) => {
                warn!(
                        "event=synthetic failed to read AIDL metadata interface token for target ptr=0x{:x} cookie=0x{:x} kind={:?} code=0x{:x}: {:#}; returning BAD_TYPE",
                        target.ptr, target.cookie, kind, tr.code, error
                    );
                return Ok(SyntheticReply::Status(StatusCode::BadType.into()));
            }
        };
        let expected_interface = synthetic_target_interface(kind);
        if request_interface != expected_interface {
            warn!(
                "event=synthetic kind={:?} target ptr=0x{:x} cookie=0x{:x} received metadata request for unexpected interface {}; expected {}; returning BAD_TYPE",
                kind, target.ptr, target.cookie, request_interface, expected_interface
            );
            return Ok(SyntheticReply::Status(StatusCode::BadType.into()));
        }
        return build_synthetic_aidl_metadata_reply(method);
    }

    let (data, data_size, offsets, offsets_size) = transaction_parts(tr);
    let request_interface = match parcel::peek_request_interface_for_check(
        data,
        data_size,
        offsets,
        offsets_size,
    ) {
        Ok(Some(interface)) => interface,
        Ok(None) => return Ok(SyntheticReply::Status(StatusCode::BadType.into())),
        Err(error) => {
            warn!(
                "event=synthetic failed to read interface token for target ptr=0x{:x} cookie=0x{:x} kind={:?} code=0x{:x}: {:#}; returning BAD_TYPE",
                target.ptr, target.cookie, kind, tr.code, error
            );
            return Ok(SyntheticReply::Status(StatusCode::BadType.into()));
        }
    };
    let expected_interface = synthetic_target_interface(kind);
    if request_interface != expected_interface {
        warn!(
            "event=synthetic {:?} target ptr=0x{:x} cookie=0x{:x} received unexpected interface {}; expected {}; returning BAD_TYPE",
            kind, target.ptr, target.cookie, request_interface, expected_interface
        );
        return Ok(SyntheticReply::Status(StatusCode::BadType.into()));
    }
    if let Some(reply) = synthetic_unknown_transaction_reply_for(kind, tr.code) {
        return Ok(reply);
    }

    let cfg = config::get();
    if !cfg.main.enabled {
        warn!(
            "event=synthetic injector disabled while synthetic target ptr=0x{:x} cookie=0x{:x} is still live; returning SYSTEM_ERROR",
            target.ptr, target.cookie
        );
        return Ok(synthetic_parcel_reply(build_service_specific_reply(
            ResponseCode::SYSTEM_ERROR.0,
        )?));
    }

    let fallback = match kind {
        SyntheticTargetKind::SecurityLevel => None,
        SyntheticTargetKind::Operation => Some(
            info.caller
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("missing synthetic operation caller fallback"))?,
        ),
    };
    let mut caller = synthetic_transaction_caller(fallback, tr, caller_sid);
    let decision = evaluate_caller(&caller, &cfg);
    caller = caller.with_keybox_slot(cfg.keybox_slot_for_packages(&decision.packages));
    if kind == SyntheticTargetKind::SecurityLevel {
        let decision = evaluate_caller(&caller, &cfg);
        let request = match parcel::parse_security_level_request(
            data,
            data_size,
            offsets,
            offsets_size,
            tr.code,
        ) {
            Ok(request) => request,
            Err(error) => {
                let status = synthetic_parse_error_status_code(&error);
                warn!(
                    "event=synthetic failed to parse synthetic security-level request target=ptr:0x{:x}/cookie:0x{:x} code=0x{:x}: {:#}; returning {}",
                    target.ptr, target.cookie, tr.code, error, status
                );
                return synthetic_parse_error_reply(status);
            }
        };
        let method = request.method();
        if decision.allowed && !security_level_scoop_enabled(&cfg.intercept) {
            warn!(
                "event=synthetic security-level interception disabled while target ptr=0x{:x} cookie=0x{:x} is still live; returning SYSTEM_ERROR",
                target.ptr, target.cookie
            );
            return Ok(synthetic_parcel_reply(build_service_specific_reply(
                ResponseCode::SYSTEM_ERROR.0,
            )?));
        }
        let allow_omk_grant = should_allow_omk_grant_security_level_request_with_probe(
            &request,
            &decision,
            &caller,
            probe_omk_grant,
        )?;
        if !decision.allowed && !allow_omk_grant {
            info!(
                "event=synthetic rejected {} security-level {:?} uid={} pid={} sid='{}' target=ptr:0x{:x}/cookie:0x{:x} packages={:?} reason={:?}",
                command_name,
                method,
                caller.uid,
                caller.pid,
                caller.sid,
                target.ptr,
                target.cookie,
                decision.packages,
                decision.reason,
            );
            return Ok(synthetic_parcel_reply(build_service_specific_reply(
                ResponseCode::PERMISSION_DENIED.0,
            )?));
        }
        let target_info = tracker::lookup_security_level_target(target).ok_or_else(|| {
            anyhow::anyhow!(
                "missing synthetic security-level mapping for ptr=0x{:x} cookie=0x{:x}",
                target.ptr,
                target.cookie
            )
        })?;

        info!(
            "event=synthetic handling {} security-level {:?} uid={} pid={} target=ptr:0x{:x}/cookie=0x{:x} packages={:?} security_level={:?}",
            command_name,
            method,
            caller.uid,
            caller.pid,
            target.ptr,
            target.cookie,
            decision.packages,
            target_info.security_level,
        );

        let pending = PendingSecurityLevelCall {
            request,
            caller,
            packages: decision.packages,
            route: RouteTarget::Omk,
            security_level: target_info.security_level,
        };
        let reply = build_security_level_reply_rewrite(tr, &pending)?;
        return Ok(synthetic_reply_from_outbound(reply));
    }

    let request = match parcel::parse_operation_request(
        data,
        data_size,
        offsets,
        offsets_size,
        tr.code,
    ) {
        Ok(request) => request,
        Err(error) => {
            let status = synthetic_parse_error_status_code(&error);
            warn!(
                "event=synthetic failed to parse synthetic operation request target=ptr:0x{:x}/cookie:0x{:x} code=0x{:x}: {:#}; returning {}",
                target.ptr, target.cookie, tr.code, error, status
            );
            return synthetic_parse_error_reply(status);
        }
    };
    let method = request.method();

    info!(
        "event=synthetic handling {} operation {:?} uid={} pid={} target=ptr:0x{:x}/cookie:0x{:x}",
        command_name, method, caller.uid, caller.pid, target.ptr, target.cookie,
    );

    let pending = PendingOperationCall {
        request,
        caller,
        target,
    };
    let reply = build_operation_reply_rewrite(&pending)?;
    Ok(synthetic_reply_from_outbound(reply))
}

fn build_no_carrier_omk_key_entry_reply(
    entry: KeyEntryResponse,
    caller: &CallerIdentity,
) -> anyhow::Result<parcel::OwnedReply> {
    if entry.r#iSecurityLevel.is_none() {
        return parcel::build_key_entry_reply(entry);
    }

    let carrier = register_synthetic_security_level_carrier(
        entry.r#metadata.keySecurityLevel,
        ServiceMethod::GetKeyEntry,
        caller,
    )?;
    parcel::build_key_entry_reply_with_carrier_bytes(
        entry.r#metadata,
        &carrier.bytes,
        carrier.is_object,
    )
}

fn build_no_carrier_create_operation_reply(
    mut response: crate::android::system::keystore2::CreateOperationResponse::CreateOperationResponse,
    aad_allowed: bool,
    caller: &CallerIdentity,
    publish_operation_carrier: bool,
) -> anyhow::Result<parcel::OwnedReply> {
    if !publish_operation_carrier {
        let _guard = BypassGuard::enter();
        if let Some(operation) = response.r#iOperation.take() {
            if let Err(status) = operation.r#abort() {
                warn!(
                    "event=synthetic failed to abort OMK operation discarded after one-way createOperation: {}",
                    status
                );
            }
        }
        return parcel::build_void_reply();
    }

    let Some(operation) = response.r#iOperation.take() else {
        return parcel::build_create_operation_reply(response);
    };

    let abort_backend = operation.clone();
    let (carrier, retirement) = match register_synthetic_operation_carrier(
        operation,
        aad_allowed,
        caller,
    ) {
        Ok(registered) => registered,
        Err(error) => {
            let _guard = BypassGuard::enter();
            if let Err(status) = abort_backend.r#abort() {
                warn!(
                    "event=synthetic failed to abort OMK operation after carrier registration failed: {}",
                    status
                );
            }
            return Err(error);
        }
    };
    let reply = parcel::build_create_operation_reply_with_carrier_bytes(
        response.r#operationChallenge,
        response.r#parameters,
        response.r#upgradedBlob,
        &carrier.bytes,
        carrier.is_object,
    );
    match reply {
        Ok(mut reply) => {
            reply.native_operation = Some(retirement);
            Ok(reply)
        }
        Err(error) => {
            drop_synthetic_operation_retirement(retirement);
            Err(error)
        }
    }
}

fn build_direct_omk_security_level_reply(
    security_level: crate::android::hardware::security::keymint::SecurityLevel::SecurityLevel,
    pending: &PendingServiceCall,
) -> anyhow::Result<Option<OutboundReply>> {
    match ipc::with_omk_retry(|omk| Ok(omk.r#getSecurityLevel(security_level)?)) {
        Ok(_level) => {
            let carrier = register_synthetic_security_level_carrier(
                security_level,
                ServiceMethod::GetSecurityLevel,
                &pending.caller,
            )?;
            Ok(Some(
                parcel::build_get_security_level_reply_with_carrier_bytes(
                    &carrier.bytes,
                    carrier.is_object,
                )?,
            ))
        }
        Err(error) => omk_error_reply_for_method("getSecurityLevel", &pending.caller, &error),
    }
}

unsafe fn observe_system_service_reply(
    tr: &binder_transaction_data,
    pending: &PendingServiceCall,
) -> anyhow::Result<Option<OutboundReply>> {
    let (data, data_size, offsets, offsets_size) = transaction_parts(tr);
    match &pending.request {
        ParsedServiceRequest::GetSecurityLevel { security_level } => {
            let carrier = match parcel::extract_direct_binder_reply_carrier(
                data,
                data_size,
                offsets,
                offsets_size,
            ) {
                Ok(carrier) => carrier,
                Err(error) => {
                    return malformed_system_success_reply(
                        data,
                        data_size,
                        offsets,
                        offsets_size,
                        "getSecurityLevel reply",
                        error,
                    );
                }
            };
            finalize_system_success!(
                register_security_level_carrier(
                    &carrier,
                    *security_level,
                    ServiceMethod::GetSecurityLevel,
                ),
                "getSecurityLevel security-level carrier"
            );
        }
        ParsedServiceRequest::GetKeyEntry { .. } => {
            let (carrier, metadata) =
                match parcel::parse_key_entry_reply(data, data_size, offsets, offsets_size) {
                    Ok(response) => response,
                    Err(error) => {
                        return malformed_system_success_reply(
                            data,
                            data_size,
                            offsets,
                            offsets_size,
                            "getKeyEntry reply",
                            error,
                        );
                    }
                };
            finalize_system_success!(
                register_security_level_carrier(
                    &carrier,
                    metadata.r#keySecurityLevel,
                    ServiceMethod::GetKeyEntry,
                ),
                "getKeyEntry security-level carrier"
            );
        }
        _ => {}
    }
    Ok(None)
}

unsafe fn build_authorization_reply_mirror(
    tr: &binder_transaction_data,
    call: &mut PendingAuthorizationCall,
) -> anyhow::Result<Option<parcel::OwnedReply>> {
    let method = call.method;
    let (data, data_size, offsets, offsets_size) = transaction_parts(tr);
    let status = parcel::parse_reply_status(data, data_size, offsets, offsets_size)?;
    if !status.is_ok() {
        if let Some(reservation) = call.mirror_update.take() {
            reservation.cancel();
        }
        debug!(
            "event=mirror domain=authorization system {:?} failed with {}; skipping OMK mirror",
            method, status
        );
        return Ok(None);
    }
    if matches!(
        &call.request,
        ParsedAuthorizationRequest::GetAuthTokensForCredStore { .. }
            | ParsedAuthorizationRequest::GetLastAuthTime { .. }
    ) {
        debug!(
            "event=mirror domain=authorization {:?} is read-only; preserving system reply",
            method
        );
        return Ok(None);
    }
    log_dirty_mirror_state(MirrorStateKind::Authorization, method, &call.caller);
    let event = MirrorReplayEvent::Authorization {
        request: call.request.clone(),
        caller: call.caller.clone(),
    };
    let reservation = call
        .mirror_update
        .take()
        .ok_or_else(|| anyhow::anyhow!("authorization mirror reservation is missing"))?;
    match reservation.enqueue(method, &call.caller, event) {
        Ok(()) => {
            debug!(
                "event=mirror domain=authorization queued {:?} for OMK for uid={} pid={}",
                method, call.caller.uid, call.caller.pid
            );
            Ok(None)
        }
        Err(error) => {
            warn!(
                "event=mirror domain=authorization failed to queue {:?} for OMK for uid={} pid={}: {:#}",
                method, call.caller.uid, call.caller.pid, error
            );
            Ok(Some(synthetic_fallback_reply()))
        }
    }
}

fn execute_authorization_mirror(
    request: &ParsedAuthorizationRequest,
    caller: &CallerIdentity,
) -> anyhow::Result<()> {
    let caller_info = caller.to_caller_info();
    match request {
        ParsedAuthorizationRequest::AddAuthToken { auth_token } => {
            ipc::with_omk_authorization_once(|auth| {
                Ok(auth.r#addAuthToken(Some(&caller_info), auth_token)?)
            })
        }
        ParsedAuthorizationRequest::OnDeviceUnlocked { user_id, password } => {
            ipc::with_omk_authorization_once(|auth| {
                Ok(auth.r#onDeviceUnlocked(Some(&caller_info), *user_id, password.as_deref())?)
            })
        }
        ParsedAuthorizationRequest::OnDeviceLocked {
            user_id,
            unlocking_sids,
            weak_unlock_enabled,
        } => ipc::with_omk_authorization_once(|auth| {
            Ok(auth.r#onDeviceLocked(
                Some(&caller_info),
                *user_id,
                unlocking_sids,
                *weak_unlock_enabled,
            )?)
        }),
        ParsedAuthorizationRequest::OnUserStorageLocked { user_id } => {
            ipc::with_omk_authorization_once(|auth| {
                Ok(auth.r#onUserStorageLocked(Some(&caller_info), *user_id)?)
            })
        }
        ParsedAuthorizationRequest::OnWeakUnlockMethodsExpired { user_id } => {
            ipc::with_omk_authorization_once(|auth| {
                Ok(auth.r#onWeakUnlockMethodsExpired(Some(&caller_info), *user_id)?)
            })
        }
        ParsedAuthorizationRequest::OnNonLskfUnlockMethodsExpired { user_id } => {
            ipc::with_omk_authorization_once(|auth| {
                Ok(auth.r#onNonLskfUnlockMethodsExpired(Some(&caller_info), *user_id)?)
            })
        }
        ParsedAuthorizationRequest::GetAuthTokensForCredStore { .. }
        | ParsedAuthorizationRequest::GetLastAuthTime { .. } => Ok(()),
    }
}

fn precompute_omk_migrate_reply(call: &PendingMaintenanceCall) -> OmkMigratePrecompute {
    let ParsedMaintenanceRequest::MigrateKeyNamespace {
        source,
        destination,
    } = &call.request
    else {
        unreachable!(
            "OMK migrate precompute called for {:?}",
            call.request.method()
        );
    };
    debug_assert_eq!(call.route, RouteTarget::Omk);

    if let Err(error) = ensure_mirror_state_recovered() {
        warn!(
            "event=route OMK migrateKeyNamespace blocked by unresolved mirror recovery for uid={} pid={}: {:#}",
            call.caller.uid, call.caller.pid, error
        );
        return OmkMigratePrecompute::Reply(PrecomputedMaintenanceReply::Error(
            Status::new_service_specific_error(ResponseCode::SYSTEM_ERROR.0, None),
        ));
    }

    let method = call.request.method();
    let caller = call.caller.to_caller_info();
    let result = ipc::with_omk_maintenance_once(|maintenance| {
        Ok(maintenance.r#migrateKeyNamespace(Some(&caller), source, destination)?)
    });
    match result {
        Ok(()) => {
            debug!(
                "event=route OMK authoritative maintenance {:?} succeeded for uid={} pid={}",
                method, call.caller.uid, call.caller.pid
            );
            OmkMigratePrecompute::Reply(PrecomputedMaintenanceReply::Success)
        }
        Err(error) if omk_unavailable_error(&error) => {
            warn!(
                "event=route OMK migrateKeyNamespace unavailable for uid={} pid={}: {:#}; preserving original system request",
                call.caller.uid, call.caller.pid, error
            );
            OmkMigratePrecompute::PreserveSystem
        }
        Err(error) => {
            warn!(
                "event=route OMK migrateKeyNamespace failed for uid={} pid={}: {:#}; returning OMK error",
                call.caller.uid, call.caller.pid, error
            );
            OmkMigratePrecompute::Reply(PrecomputedMaintenanceReply::Error(
                precomputed_omk_error_reply(&error),
            ))
        }
    }
}

unsafe fn build_maintenance_reply_mirror(
    tr: &binder_transaction_data,
    call: &mut PendingMaintenanceCall,
) -> anyhow::Result<Option<OutboundReply>> {
    let method = call.request.method();
    let (data, data_size, offsets, offsets_size) = transaction_parts(tr);
    let status = parcel::parse_reply_status(data, data_size, offsets, offsets_size)?;
    if !status.is_ok() {
        if let Some(reservation) = call.mirror_update.take() {
            reservation.cancel();
        }
        debug!(
            "event=mirror domain=maintenance system {:?} failed with {}; skipping OMK mirror",
            method, status
        );
        return Ok(None);
    }
    if matches!(
        &call.request,
        ParsedMaintenanceRequest::MigrateKeyNamespace { .. }
    ) {
        return Ok(None);
    }
    if matches!(
        &call.request,
        ParsedMaintenanceRequest::GetState { .. }
            | ParsedMaintenanceRequest::OnDeviceOffBody
            | ParsedMaintenanceRequest::GetAppUidsAffectedBySid { .. }
    ) {
        debug!(
            "event=mirror domain=maintenance {:?} is read-only or has no OMK mirror endpoint; preserving system reply",
            method
        );
        return Ok(None);
    }
    log_dirty_mirror_state(MirrorStateKind::Maintenance, method, &call.caller);
    let event = MirrorReplayEvent::Maintenance {
        request: call.request.clone(),
        caller: call.caller.clone(),
    };
    let reservation = call
        .mirror_update
        .take()
        .ok_or_else(|| anyhow::anyhow!("maintenance mirror reservation is missing"))?;
    match reservation.enqueue(method, &call.caller, event) {
        Ok(()) => {
            debug!(
                "event=mirror domain=maintenance queued {:?} for OMK for uid={} pid={}",
                method, call.caller.uid, call.caller.pid
            );
            Ok(None)
        }
        Err(error) => {
            warn!(
                "event=mirror domain=maintenance failed to queue {:?} for OMK for uid={} pid={}: {:#}",
                method, call.caller.uid, call.caller.pid, error
            );
            Ok(Some(synthetic_fallback_reply()))
        }
    }
}

fn execute_maintenance_mirror(
    request: &ParsedMaintenanceRequest,
    caller: &CallerIdentity,
) -> anyhow::Result<()> {
    let caller_info = caller.to_caller_info();
    match request {
        ParsedMaintenanceRequest::OnUserAdded { user_id } => {
            ipc::with_omk_maintenance_once(|maintenance| {
                Ok(maintenance.r#onUserAdded(Some(&caller_info), *user_id)?)
            })
        }
        ParsedMaintenanceRequest::InitUserSuperKeys {
            user_id,
            password,
            allow_existing,
        } => ipc::with_omk_maintenance_once(|maintenance| {
            Ok(maintenance.r#initUserSuperKeys(
                Some(&caller_info),
                *user_id,
                password,
                *allow_existing,
            )?)
        }),
        ParsedMaintenanceRequest::OnUserRemoved { user_id } => {
            ipc::with_omk_maintenance_once(|maintenance| {
                Ok(maintenance.r#onUserRemoved(Some(&caller_info), *user_id)?)
            })
        }
        ParsedMaintenanceRequest::OnUserLskfRemoved { user_id } => {
            ipc::with_omk_maintenance_once(|maintenance| {
                Ok(maintenance.r#onUserLskfRemoved(Some(&caller_info), *user_id)?)
            })
        }
        ParsedMaintenanceRequest::ClearNamespace { domain, nspace } => {
            ipc::with_omk_maintenance_once(|maintenance| {
                Ok(maintenance.r#clearNamespace(Some(&caller_info), *domain, *nspace)?)
            })
        }
        ParsedMaintenanceRequest::EarlyBootEnded => ipc::with_omk_maintenance_once(|maintenance| {
            Ok(maintenance.r#earlyBootEnded(Some(&caller_info))?)
        }),
        ParsedMaintenanceRequest::MigrateKeyNamespace { .. } => {
            unreachable!("migrateKeyNamespace is handled before maintenance mirroring")
        }
        ParsedMaintenanceRequest::DeleteAllKeys => ipc::with_omk_maintenance_once(|maintenance| {
            Ok(maintenance.r#deleteAllKeys(Some(&caller_info))?)
        }),
        ParsedMaintenanceRequest::OnUserPasswordChanged { user_id, password } => {
            ipc::with_omk_maintenance_once(|maintenance| {
                Ok(maintenance.r#onUserPasswordChanged(
                    Some(&caller_info),
                    *user_id,
                    password.as_deref(),
                )?)
            })
        }
        ParsedMaintenanceRequest::GetState { .. }
        | ParsedMaintenanceRequest::OnDeviceOffBody
        | ParsedMaintenanceRequest::GetAppUidsAffectedBySid { .. } => Ok(()),
    }
}

unsafe fn build_service_reply_rewrite(
    tr: &binder_transaction_data,
    pending: &PendingServiceCall,
) -> anyhow::Result<Option<OutboundReply>> {
    if pending.route != RouteTarget::Omk {
        return observe_system_service_reply(tr, pending);
    }

    if matches!(
        &pending.request,
        ParsedServiceRequest::GetSecurityLevel { .. }
    ) {
        if let Err(error) = observe_system_service_reply(tr, pending).map(|_| ()) {
            warn!(
                "event=route failed to retain original System security-level target before OMK reply rewrite: {:#}",
                error
            );
        }
    }

    if let Err(error) = ensure_mirror_state_recovered() {
        warn!(
            "event=route OMK service {:?} blocked by unresolved mirror recovery for uid={} pid={}: {:#}",
            pending.request.method(), pending.caller.uid, pending.caller.pid, error
        );
        return Ok(Some(synthetic_fallback_reply()));
    }

    let caller = pending.caller.to_caller_info();
    let _guard = BypassGuard::enter();

    match &pending.request {
        ParsedServiceRequest::GetSecurityLevel { security_level } => {
            build_direct_omk_security_level_reply(*security_level, pending)
        }
        ParsedServiceRequest::GetKeyEntry { key } => {
            let entry = match ipc::with_omk_retry(|omk| Ok(omk.r#getKeyEntry(Some(&caller), key)?))
            {
                Ok(entry) => entry,
                Err(error) => {
                    return omk_error_reply_for_method("getKeyEntry", &pending.caller, &error);
                }
            };
            Ok(Some(build_no_carrier_omk_key_entry_reply(
                entry,
                &pending.caller,
            )?))
        }
        ParsedServiceRequest::UpdateSubcomponent {
            key,
            public_cert,
            certificate_chain,
        } => {
            match ipc::with_omk_once(|omk| {
                Ok(omk.r#updateSubcomponent(
                    Some(&caller),
                    key,
                    public_cert.as_deref(),
                    certificate_chain.as_deref(),
                )?)
            }) {
                Ok(()) => Ok(Some(parcel::build_void_reply()?)),
                Err(error) => {
                    omk_error_reply_for_method("updateSubcomponent", &pending.caller, &error)
                }
            }
        }
        ParsedServiceRequest::ListEntries { domain, nspace } => {
            match ipc::with_omk_retry(|omk| {
                Ok(omk.r#listEntries(Some(&caller), *domain, *nspace)?)
            }) {
                Ok(entries) => Ok(Some(parcel::build_plain_reply(&entries)?)),
                Err(error) => omk_error_reply_for_method("listEntries", &pending.caller, &error),
            }
        }
        ParsedServiceRequest::Grant {
            key,
            grantee_uid,
            access_vector,
        } => {
            let omk_grant = match ipc::with_omk_once(|omk| {
                Ok(omk.r#grant(Some(&caller), key, *grantee_uid, *access_vector)?)
            }) {
                Ok(omk_grant) => omk_grant,
                Err(error) => return omk_error_reply_for_method("grant", &pending.caller, &error),
            };
            Ok(Some(parcel::build_plain_reply(&omk_grant)?))
        }
        ParsedServiceRequest::Ungrant { key, grantee_uid } => {
            match ipc::with_omk_once(|omk| Ok(omk.r#ungrant(Some(&caller), key, *grantee_uid)?)) {
                Ok(()) => Ok(Some(parcel::build_void_reply()?)),
                Err(error) => omk_error_reply_for_method("ungrant", &pending.caller, &error),
            }
        }
        ParsedServiceRequest::GetNumberOfEntries { domain, nspace } => {
            match ipc::with_omk_retry(|omk| {
                Ok(omk.r#getNumberOfEntries(Some(&caller), *domain, *nspace)?)
            }) {
                Ok(count) => Ok(Some(parcel::build_plain_reply(&count)?)),
                Err(error) => {
                    omk_error_reply_for_method("getNumberOfEntries", &pending.caller, &error)
                }
            }
        }
        ParsedServiceRequest::ListEntriesBatched {
            domain,
            nspace,
            starting_past_alias,
        } => {
            match ipc::with_omk_retry(|omk| {
                Ok(omk.r#listEntriesBatched(
                    Some(&caller),
                    *domain,
                    *nspace,
                    starting_past_alias.as_deref(),
                )?)
            }) {
                Ok(entries) => Ok(Some(parcel::build_plain_reply(&entries)?)),
                Err(error) => {
                    omk_error_reply_for_method("listEntriesBatched", &pending.caller, &error)
                }
            }
        }
        ParsedServiceRequest::GetSupplementaryAttestationInfo { tag } => {
            match ipc::with_omk_retry(|omk| Ok(omk.r#getSupplementaryAttestationInfo(*tag)?)) {
                Ok(info) => Ok(Some(parcel::build_plain_reply(&info)?)),
                Err(error) => omk_error_reply_for_method(
                    "getSupplementaryAttestationInfo",
                    &pending.caller,
                    &error,
                ),
            }
        }
        ParsedServiceRequest::DeleteKey { key } => {
            match ipc::with_omk_once(|omk| Ok(omk.r#deleteKey(Some(&caller), key)?)) {
                Ok(()) => Ok(Some(parcel::build_void_reply()?)),
                Err(error) => omk_error_reply_for_method("deleteKey", &pending.caller, &error),
            }
        }
    }
}

unsafe fn observe_system_security_level_reply(
    tr: &binder_transaction_data,
    pending: &PendingSecurityLevelCall,
) -> anyhow::Result<Option<OutboundReply>> {
    let (data, data_size, offsets, offsets_size) = transaction_parts(tr);
    if let ParsedSecurityLevelRequest::CreateOperation {
        operation_parameters,
        ..
    } = &pending.request
    {
        if let Err(error) = register_operation_target_from_reply(
            tr,
            RouteTarget::System,
            None,
            operation_allows_aad(operation_parameters),
        ) {
            return malformed_system_success_reply(
                data,
                data_size,
                offsets,
                offsets_size,
                "createOperation reply",
                error,
            );
        }
    }
    Ok(None)
}

unsafe fn build_security_level_reply_rewrite(
    tr: &binder_transaction_data,
    pending: &PendingSecurityLevelCall,
) -> anyhow::Result<Option<OutboundReply>> {
    if pending.route != RouteTarget::Omk {
        return observe_system_security_level_reply(tr, pending);
    }

    build_omk_security_level_reply(pending, (tr.flags & super::binder::TF_ONE_WAY) == 0)
}

fn build_omk_security_level_reply(
    pending: &PendingSecurityLevelCall,
    publish_operation_carrier: bool,
) -> anyhow::Result<Option<OutboundReply>> {
    if let Err(error) = ensure_mirror_state_recovered() {
        warn!(
            "event=route OMK security-level {:?} blocked by unresolved mirror recovery for uid={} pid={}: {:#}",
            pending.request.method(), pending.caller.uid, pending.caller.pid, error
        );
        return Ok(Some(synthetic_fallback_reply()));
    }
    let caller = pending.caller.to_caller_info();
    let _guard = BypassGuard::enter();
    let omk_level =
        match ipc::with_omk_retry(|omk| Ok(omk.r#getOhMySecurityLevel(pending.security_level)?)) {
            Ok(level) => level,
            Err(error) => {
                let method = format!("security-level lookup {:?}", pending.security_level);
                return omk_error_reply_for_method(&method, &pending.caller, &error);
            }
        };

    match &pending.request {
        ParsedSecurityLevelRequest::CreateOperation {
            key,
            operation_parameters,
            forced,
        } => {
            let omk_response = match omk_level.r#createOperation(
                Some(&caller),
                key,
                operation_parameters,
                *forced,
            ) {
                Ok(response) => response,
                Err(error) => {
                    return omk_status_reply_for_method("createOperation", &pending.caller, &error);
                }
            };
            Ok(Some(build_no_carrier_create_operation_reply(
                omk_response,
                operation_allows_aad(operation_parameters),
                &pending.caller,
                publish_operation_carrier,
            )?))
        }
        ParsedSecurityLevelRequest::GenerateKey {
            key,
            attestation_key,
            params,
            flags,
            entropy,
        } => {
            match omk_level.r#generateKey(
                Some(&caller),
                key,
                attestation_key.as_ref(),
                params,
                *flags,
                entropy,
            ) {
                Ok(metadata) => Ok(Some(parcel::build_plain_reply(&metadata)?)),
                Err(error) => omk_status_reply_for_method("generateKey", &pending.caller, &error),
            }
        }
        ParsedSecurityLevelRequest::ImportKey {
            key,
            attestation_key,
            params,
            flags,
            key_data,
        } => {
            match omk_level.r#importKey(
                Some(&caller),
                key,
                attestation_key.as_ref(),
                params,
                *flags,
                key_data,
            ) {
                Ok(metadata) => Ok(Some(parcel::build_plain_reply(&metadata)?)),
                Err(error) => omk_status_reply_for_method("importKey", &pending.caller, &error),
            }
        }
        ParsedSecurityLevelRequest::ImportWrappedKey {
            key,
            wrapping_key,
            masking_key,
            params,
            authenticators,
        } => {
            match omk_level.r#importWrappedKey(
                Some(&caller),
                key,
                wrapping_key,
                masking_key.as_deref(),
                params,
                authenticators,
            ) {
                Ok(metadata) => Ok(Some(parcel::build_plain_reply(&metadata)?)),
                Err(error) => {
                    omk_status_reply_for_method("importWrappedKey", &pending.caller, &error)
                }
            }
        }
        ParsedSecurityLevelRequest::ConvertStorageKeyToEphemeral { storage_key } => match omk_level
            .r#convertStorageKeyToEphemeral(Some(&caller), storage_key)
        {
            Ok(response) => Ok(Some(parcel::build_plain_reply(&response)?)),
            Err(error) => {
                omk_status_reply_for_method("convertStorageKeyToEphemeral", &pending.caller, &error)
            }
        },
        ParsedSecurityLevelRequest::DeleteKey { key } => {
            match omk_level.r#deleteKey(Some(&caller), key) {
                Ok(()) => Ok(Some(parcel::build_void_reply()?)),
                Err(error) => omk_status_reply_for_method("deleteKey", &pending.caller, &error),
            }
        }
    }
}

fn build_operation_reply_rewrite(
    pending: &PendingOperationCall,
) -> anyhow::Result<Option<OutboundReply>> {
    let Some(target) = lookup_operation_target(pending.target) else {
        if lookup_synthetic_target(pending.target) == Some(SyntheticTargetKind::Operation) {
            debug!(
                "event=reply synthetic operation carrier ptr=0x{:x} cookie=0x{:x} has no live backend; returning INVALID_OPERATION_HANDLE",
                pending.target.ptr, pending.target.cookie
            );
            return Ok(Some(invalid_operation_handle_reply()?));
        }
        anyhow::bail!("missing operation target mapping");
    };

    if target.route == RouteTarget::System {
        if matches!(
            pending.request,
            ParsedOperationRequest::Finish { .. } | ParsedOperationRequest::Abort
        ) {
            forget_operation_target(pending.target);
        }
        return Ok(None);
    }

    if let Err(error) = ensure_mirror_state_recovered() {
        warn!(
            "event=route OMK operation {:?} blocked by unresolved mirror recovery for uid={} pid={}: {:#}",
            pending.request.method(), pending.caller.uid, pending.caller.pid, error
        );
        return Ok(Some(synthetic_fallback_reply()));
    }

    let Some(backend) = target.backend else {
        if target.finalized {
            if matches!(pending.request, ParsedOperationRequest::Abort) {
                debug!(
                    "event=reply cleanup abort for finalized OMK operation carrier ptr=0x{:x} cookie=0x{:x}; returning INVALID_OPERATION_HANDLE",
                    pending.target.ptr, pending.target.cookie
                );
                forget_operation_target(pending.target);
            }
            return Ok(Some(invalid_operation_handle_reply()?));
        }
        anyhow::bail!("missing OMK operation backend mapping");
    };
    let _guard = BypassGuard::enter();

    let reply = match &pending.request {
        ParsedOperationRequest::UpdateAad { aad_input } => {
            if !target.aad_allowed {
                debug!(
                    "event=reply OMK-owned updateAad rejected on a non-AAD-capable operation; returning OMK status reply"
                );
            }
            match backend.r#updateAad(aad_input) {
                Ok(()) => parcel::build_void_reply()?,
                Err(status) => {
                    if operation_error_finalizes(&status) {
                        mark_operation_target_finalized(pending.target);
                    }
                    build_omk_status_reply(&status)?
                }
            }
        }
        ParsedOperationRequest::Update { input } => match backend.r#update(input) {
            Ok(output) => parcel::build_plain_reply(&output)?,
            Err(status) => {
                if operation_error_finalizes(&status) {
                    mark_operation_target_finalized(pending.target);
                }
                build_omk_status_reply(&status)?
            }
        },
        ParsedOperationRequest::Finish { input, signature } => {
            match backend.r#finish(input.as_deref(), signature.as_deref()) {
                Ok(output) => {
                    mark_operation_target_finalized(pending.target);
                    parcel::build_plain_reply(&output)?
                }
                Err(status) => {
                    if operation_error_finalizes(&status) {
                        mark_operation_target_finalized(pending.target);
                    }
                    build_omk_status_reply(&status)?
                }
            }
        }
        ParsedOperationRequest::Abort => match backend.r#abort() {
            Ok(()) => {
                forget_operation_target(pending.target);
                parcel::build_void_reply()?
            }
            Err(status) => {
                if operation_error_finalizes(&status) {
                    mark_operation_target_finalized(pending.target);
                }
                build_omk_status_reply(&status)?
            }
        },
    };

    Ok(Some(reply))
}

unsafe fn install_outbound_reply(
    connection: BinderStateKey,
    tr: &mut binder_transaction_data,
    reply: OutboundReply,
) {
    if let Some(retirement) = reply.native_operation {
        bind_operation_publication_connection(retirement, connection);
    }
    OUTBOUND_REPLY_BUFFERS.with(|slot| {
        let mut buffers = slot.borrow_mut();
        buffers.push((connection, reply));
        let reply = &buffers.last().expect("outbound reply buffer just pushed").1;
        tr.flags &= !super::binder::TF_STATUS_CODE;
        tr.data_size = reply.data_size();
        tr.offsets_size = reply.offsets_size();
        tr.data.ptr.buffer = reply.data_ptr() as libc::c_ulong;
        tr.data.ptr.offsets = if reply.offsets.is_empty() {
            0
        } else {
            reply.offsets.as_ptr() as libc::c_ulong
        };
    });
}

fn block_system_request(tr: &mut binder_transaction_data) {
    // Keep the parcel intact; the interceptor owns any receive-buffer shadow.
    tr.code = u32::MAX;
}

pub(super) fn push_pending_frame(connection: BinderStateKey) {
    PENDING_REPLY_QUEUE.with(|slot| {
        slot.borrow_mut().push(PendingReplyFrame {
            id: NEXT_PENDING_REPLY_FRAME_ID.fetch_add(1, Ordering::Relaxed),
            connection,
            pending: None,
            claimed: false,
        });
    });
}

fn replace_top_pending(connection: BinderStateKey, pending: PendingCall) {
    PENDING_REPLY_QUEUE.with(|slot| {
        if let Some(frame) = slot
            .borrow_mut()
            .iter_mut()
            .rev()
            .find(|frame| frame.connection == connection && !frame.claimed)
        {
            frame.pending = Some(pending);
        }
    });
}

#[cfg(test)]
fn take_top_pending(connection: BinderStateKey) -> Option<Option<PendingCall>> {
    PENDING_REPLY_QUEUE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let position = slot
            .iter()
            .rposition(|frame| frame.connection == connection)?;
        Some(slot.remove(position).pending)
    })
}

fn remove_pending_reply_frame(frame_id: u64) {
    PENDING_REPLY_QUEUE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(position) = slot.iter().position(|frame| frame.id == frame_id) {
            slot.remove(position);
        }
    });
}

pub(super) fn commit_bc_reply(
    connection: BinderStateKey,
    frame_id: Option<u64>,
    data_ptr: usize,
) -> Option<NativeBinderRetirement> {
    if let Some(frame_id) = frame_id {
        remove_pending_reply_frame(frame_id);
    }
    OUTBOUND_REPLY_BUFFERS.with(|slot| {
        let mut buffers = slot.borrow_mut();
        let position = buffers.iter_mut().position(|(reply_fd, reply)| {
            *reply_fd == connection && reply.data_ptr() as usize == data_ptr
        })?;
        buffers.remove(position).1.native_operation.take()
    })
}

pub(super) fn abort_bc_reply(connection: BinderStateKey, frame_id: Option<u64>, data_ptr: usize) {
    if let Some(frame_id) = frame_id {
        remove_pending_reply_frame(frame_id);
    }
    OUTBOUND_REPLY_BUFFERS.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(position) = slot.iter().position(|(reply_fd, reply)| {
            *reply_fd == connection && reply.data_ptr() as usize == data_ptr
        }) {
            slot.remove(position);
        }
    });
}

pub(super) fn clear_outbound_reply_buffers(connection: BinderStateKey) {
    OUTBOUND_REPLY_BUFFERS.with(|slot| {
        slot.borrow_mut()
            .retain(|(reply_fd, _)| *reply_fd != connection);
    });
}

pub(super) fn clear_binder_fd_thread_state(connection: BinderStateKey) {
    PENDING_REPLY_QUEUE.with(|slot| {
        slot.borrow_mut()
            .retain(|frame| frame.connection != connection);
    });
    clear_outbound_reply_buffers(connection);
}

#[cfg(test)]
pub(super) fn reset_pending_reply_frames_for_test(connection: BinderStateKey, count: usize) {
    PENDING_REPLY_QUEUE.with(|slot| slot.borrow_mut().clear());
    for _ in 0..count {
        push_pending_frame(connection);
    }
}

#[cfg(test)]
pub(super) fn pending_reply_frame_count_for_test(connection: BinderStateKey) -> usize {
    PENDING_REPLY_QUEUE.with(|slot| {
        slot.borrow()
            .iter()
            .filter(|frame| frame.connection == connection)
            .count()
    })
}

#[cfg(test)]
pub(super) fn pending_reply_frame_claims_for_test(connection: BinderStateKey) -> Vec<bool> {
    PENDING_REPLY_QUEUE.with(|slot| {
        slot.borrow()
            .iter()
            .filter(|frame| frame.connection == connection)
            .map(|frame| frame.claimed)
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use std::mem::size_of;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use super::*;
    use rsbinder::{Interface, Status, StatusCode};

    use crate::{
        android::hardware::security::keymint::SecurityLevel::SecurityLevel,
        android::hardware::security::keymint::Tag::Tag,
        android::system::keystore2::CreateOperationResponse::CreateOperationResponse,
        android::system::keystore2::Domain::Domain,
        android::system::keystore2::EphemeralStorageKeyResponse::EphemeralStorageKeyResponse,
        android::system::keystore2::IKeystoreOperation::{
            BnKeystoreOperation, IKeystoreOperation as AospKeystoreOperation,
        },
        android::system::keystore2::IKeystoreSecurityLevel::{
            BnKeystoreSecurityLevel, IKeystoreSecurityLevel as AospKeystoreSecurityLevel,
        },
        android::system::keystore2::IKeystoreService::transactions as service_tx,
        android::system::keystore2::KeyDescriptor::KeyDescriptor,
        android::system::keystore2::KeyEntryResponse::KeyEntryResponse,
        android::system::keystore2::KeyMetadata::KeyMetadata,
        hook::binder::{
            binder_object_header, flat_binder_object, flat_binder_object_handle_or_ptr,
            BINDER_TYPE_BINDER, BINDER_TYPE_FD,
        },
        identify::MaintenanceMethod,
        route,
    };

    struct FakeAospSecurityLevel;

    impl Interface for FakeAospSecurityLevel {}

    impl AospKeystoreSecurityLevel for FakeAospSecurityLevel {
        fn r#createOperation(
            &self,
            _arg_key: &KeyDescriptor,
            _arg_operation_parameters:
                &[crate::android::hardware::security::keymint::KeyParameter::KeyParameter],
            _arg_forced: bool,
        ) -> rsbinder::status::Result<CreateOperationResponse> {
            Err(StatusCode::UnknownTransaction.into())
        }

        fn r#generateKey(
            &self,
            _arg_key: &KeyDescriptor,
            _arg_attestation_key: Option<&KeyDescriptor>,
            _arg_params:
                &[crate::android::hardware::security::keymint::KeyParameter::KeyParameter],
            _arg_flags: i32,
            _arg_entropy: &[u8],
        ) -> rsbinder::status::Result<KeyMetadata> {
            Err(StatusCode::UnknownTransaction.into())
        }

        fn r#importKey(
            &self,
            _arg_key: &KeyDescriptor,
            _arg_attestation_key: Option<&KeyDescriptor>,
            _arg_params:
                &[crate::android::hardware::security::keymint::KeyParameter::KeyParameter],
            _arg_flags: i32,
            _arg_key_data: &[u8],
        ) -> rsbinder::status::Result<KeyMetadata> {
            Err(StatusCode::UnknownTransaction.into())
        }

        fn r#importWrappedKey(
            &self,
            _arg_key: &KeyDescriptor,
            _arg_wrapping_key: &KeyDescriptor,
            _arg_masking_key: Option<&[u8]>,
            _arg_params:
                &[crate::android::hardware::security::keymint::KeyParameter::KeyParameter],
            _arg_authenticators:
                &[crate::android::system::keystore2::AuthenticatorSpec::AuthenticatorSpec],
        ) -> rsbinder::status::Result<KeyMetadata> {
            Err(StatusCode::UnknownTransaction.into())
        }

        fn r#convertStorageKeyToEphemeral(
            &self,
            _arg_storage_key: &KeyDescriptor,
        ) -> rsbinder::status::Result<EphemeralStorageKeyResponse> {
            Err(StatusCode::UnknownTransaction.into())
        }

        fn r#deleteKey(&self, _arg_key: &KeyDescriptor) -> rsbinder::status::Result<()> {
            Err(StatusCode::UnknownTransaction.into())
        }
    }

    fn fake_system_security_level_backend() -> route::AospSecurityLevelBinder {
        ensure_binder_process_state();
        BnKeystoreSecurityLevel::new_binder(FakeAospSecurityLevel)
    }

    fn ensure_binder_process_state() {
        let _ = rsbinder::ProcessState::init_default();
    }

    fn sample_key_descriptor() -> KeyDescriptor {
        KeyDescriptor {
            domain: Domain::APP,
            nspace: 7,
            alias: Some("alias".to_string()),
            blob: None,
        }
    }

    fn sample_service_requests() -> Vec<ParsedServiceRequest> {
        vec![
            ParsedServiceRequest::GetSecurityLevel {
                security_level: SecurityLevel::TRUSTED_ENVIRONMENT,
            },
            ParsedServiceRequest::GetKeyEntry {
                key: sample_key_descriptor(),
            },
            ParsedServiceRequest::UpdateSubcomponent {
                key: sample_key_descriptor(),
                public_cert: None,
                certificate_chain: None,
            },
            ParsedServiceRequest::ListEntries {
                domain: Domain::APP,
                nspace: 0,
            },
            ParsedServiceRequest::DeleteKey {
                key: sample_key_descriptor(),
            },
            ParsedServiceRequest::Grant {
                key: sample_key_descriptor(),
                grantee_uid: 12345,
                access_vector: 7,
            },
            ParsedServiceRequest::Ungrant {
                key: sample_key_descriptor(),
                grantee_uid: 12345,
            },
            ParsedServiceRequest::GetNumberOfEntries {
                domain: Domain::APP,
                nspace: 0,
            },
            ParsedServiceRequest::ListEntriesBatched {
                domain: Domain::APP,
                nspace: 0,
                starting_past_alias: Some("alias".to_string()),
            },
            ParsedServiceRequest::GetSupplementaryAttestationInfo {
                tag: Tag::MODULE_HASH,
            },
        ]
    }

    fn disabled_intercept_config() -> config::InterceptConfig {
        config::InterceptConfig {
            get_security_level: false,
            get_key_entry: false,
            update_subcomponent: false,
            list_entries: false,
            delete_key: false,
            grant: false,
            ungrant: false,
            get_number_of_entries: false,
            list_entries_batched: false,
            get_supplementary_attestation_info: false,
        }
    }

    struct TestOperationBackend {
        update_output: Vec<u8>,
        aborts: Arc<AtomicUsize>,
        update_aad_status: Option<Status>,
    }

    impl Interface for TestOperationBackend {}

    impl AospKeystoreOperation for TestOperationBackend {
        fn r#updateAad(&self, _aad_input: &[u8]) -> rsbinder::status::Result<()> {
            match self.update_aad_status.as_ref() {
                Some(status) => Err(status.clone()),
                None => Ok(()),
            }
        }

        fn r#update(&self, _input: &[u8]) -> rsbinder::status::Result<Option<Vec<u8>>> {
            Ok(Some(self.update_output.clone()))
        }

        fn r#finish(
            &self,
            _input: Option<&[u8]>,
            _signature: Option<&[u8]>,
        ) -> rsbinder::status::Result<Option<Vec<u8>>> {
            Ok(Some(self.update_output.clone()))
        }

        fn r#abort(&self) -> rsbinder::status::Result<()> {
            self.aborts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn raw_parts(reply: &mut parcel::OwnedReply) -> (*mut u8, usize, *mut usize, usize) {
        (
            reply.data_mut_ptr(),
            reply.data_size(),
            if reply.offsets.is_empty() {
                std::ptr::null_mut()
            } else {
                reply.offsets.as_mut_ptr()
            },
            reply.offsets_size(),
        )
    }

    fn assert_unknown_transaction_reply(reply: SyntheticReply) {
        let SyntheticReply::Status(status) = reply else {
            panic!("unknown transaction should be returned as a binder status code");
        };
        assert_eq!(status, i32::from(StatusCode::UnknownTransaction));
    }

    fn assert_synthetic_status(reply: SyntheticReply, expected: StatusCode) {
        let SyntheticReply::Status(status) = reply else {
            panic!("expected synthetic binder status");
        };
        assert_eq!(status, i32::from(expected));
    }

    fn assert_synthetic_ok_reply(reply: SyntheticReply, label: &str) {
        let SyntheticReply::Parcel(mut reply) = reply else {
            panic!("{label} should be a status-bearing parcel reply");
        };
        assert_eq!(reply.offsets_size(), 0);
        let (data, data_size, offsets, offsets_size) = raw_parts(&mut reply);
        let status = unsafe { parcel::parse_reply_status(data, data_size, offsets, offsets_size) }
            .expect("status reply should parse");
        assert!(status.is_ok(), "{label} should be OK");
    }

    fn assert_synthetic_empty_parcel_reply(reply: SyntheticReply, label: &str) {
        let SyntheticReply::Parcel(reply) = reply else {
            panic!("{label} should be an empty parcel reply");
        };
        assert_eq!(reply.data_size(), 0, "{label} data size");
        assert_eq!(reply.offsets_size(), 0, "{label} offsets size");
    }

    fn assert_synthetic_exception_reply(reply: SyntheticReply, expected: ExceptionCode) {
        let SyntheticReply::Parcel(mut reply) = reply else {
            panic!("expected synthetic status parcel reply");
        };
        let (data, data_size, offsets, offsets_size) = raw_parts(&mut reply);
        let status = unsafe { parcel::parse_reply_status(data, data_size, offsets, offsets_size) }
            .expect("status reply should parse");
        assert_eq!(status.exception_code(), expected);
    }

    fn assert_synthetic_raw_i32_reply(reply: SyntheticReply, expected: i32) {
        let SyntheticReply::Parcel(mut reply) = reply else {
            panic!("raw i32 reply should be a parcel");
        };
        assert_eq!(reply.data_size(), size_of::<i32>());
        assert_eq!(reply.offsets_size(), 0);
        let (data, _, _, _) = raw_parts(&mut reply);
        let value = unsafe { std::ptr::read_unaligned(data as *const i32) };
        assert_eq!(value, expected);
    }

    fn carrier_target(carrier: &parcel::ReplyBinderCarrier) -> LocalBinderTarget {
        unsafe { parse_local_binder_target_from_parcel_bytes(&carrier.bytes) }
            .expect("synthetic carrier should expose a native target")
    }

    fn request_parcel(interface: &str) -> rsbinder::Parcel {
        request_parcel_with_marker(interface, rsbinder::INTERFACE_HEADER)
    }

    fn request_parcel_with_marker(interface: &str, marker: u32) -> rsbinder::Parcel {
        let mut parcel = rsbinder::Parcel::new();
        parcel.write(&0i32).unwrap();
        parcel.write(&0i32).unwrap();
        parcel.write(&marker).unwrap();
        parcel.write(&interface.to_string()).unwrap();
        parcel
    }

    fn transaction_for_parcel(
        target: LocalBinderTarget,
        code: rsbinder::TransactionCode,
        parcel: &rsbinder::Parcel,
    ) -> binder_transaction_data {
        let mut tr: binder_transaction_data = unsafe { std::mem::zeroed() };
        tr.target.ptr = target.ptr;
        tr.data.ptr.buffer = parcel.as_ptr() as libc::c_ulong;
        tr.data.ptr.offsets = 0;
        tr.cookie = target.cookie;
        tr.code = code;
        tr.sender_euid = 10002;
        tr.sender_pid = 2000;
        tr.data_size = parcel.data_size();
        tr.offsets_size = 0;
        tr
    }

    fn transaction_for_raw_parts(
        target: LocalBinderTarget,
        code: rsbinder::TransactionCode,
        data: &mut [u8],
        offsets: &mut [usize],
    ) -> binder_transaction_data {
        let mut tr: binder_transaction_data = unsafe { std::mem::zeroed() };
        tr.target.ptr = target.ptr;
        tr.data.ptr.buffer = data.as_mut_ptr() as libc::c_ulong;
        tr.data.ptr.offsets = offsets.as_mut_ptr() as libc::c_ulong;
        tr.cookie = target.cookie;
        tr.code = code;
        tr.sender_euid = 10002;
        tr.sender_pid = 2000;
        tr.data_size = data.len();
        tr.offsets_size = std::mem::size_of_val(offsets);
        tr
    }

    fn dump_transaction_data(argc: i32, args: &[String]) -> Vec<u8> {
        let mut tail = rsbinder::Parcel::new();
        tail.write(&argc).unwrap();
        for arg in args {
            tail.write(arg).unwrap();
        }

        let mut data = vec![0u8; size_of::<flat_binder_object>() + tail.data_size()];
        let object = flat_binder_object {
            hdr: binder_object_header {
                type_: BINDER_TYPE_FD,
            },
            flags: 0,
            handle_or_ptr: flat_binder_object_handle_or_ptr { handle: 0 },
            cookie: 0,
        };
        unsafe {
            std::ptr::write_unaligned(data.as_mut_ptr() as *mut flat_binder_object, object);
            std::ptr::copy_nonoverlapping(
                tail.as_ptr(),
                data.as_mut_ptr().add(size_of::<flat_binder_object>()),
                tail.data_size(),
            );
        }
        data
    }

    static MIRROR_STATE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static ROUTE_STATE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn reset_mirror_state_for_tests() {
        *MIRROR_RECOVERY_STATE
            .lock()
            .expect("mirror recovery state poisoned") = MirrorRecoveryState::default();
        *MIRROR_RECOVERY_CONTROL
            .lock()
            .expect("mirror recovery control poisoned") = MirrorRecoveryControl::default();
        AUTHORIZATION_MIRROR_STATE_DIRTY.store(false, Ordering::SeqCst);
        MAINTENANCE_MIRROR_STATE_DIRTY.store(false, Ordering::SeqCst);
    }

    fn route_state_test_guard() -> (
        std::sync::MutexGuard<'static, ()>,
        std::sync::MutexGuard<'static, ()>,
        std::sync::MutexGuard<'static, ()>,
    ) {
        let mirror_guard = MIRROR_STATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tracker_guard = tracker::state_test_guard();
        let route_guard = ROUTE_STATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reset_mirror_state_for_tests();
        reset_route_state_for_tests();
        (mirror_guard, tracker_guard, route_guard)
    }

    fn reset_route_state_for_tests() {
        tracker::clear_state_for_tests();
        clear_operation_state_for_tests();
        PENDING_REPLY_QUEUE.with(|slot| slot.borrow_mut().clear());
        OUTBOUND_REPLY_BUFFERS.with(|slot| slot.borrow_mut().clear());
    }

    fn clear_synthetic_targets_for_tests() {
        SYNTHETIC_SECURITY_LEVEL_TARGETS
            .lock()
            .expect("synthetic security-level target map poisoned")
            .clear();
        SYNTHETIC_TARGETS
            .lock()
            .expect("synthetic target map poisoned")
            .clear();
        let natives = NATIVE_BINDERS
            .lock()
            .expect("native binder map poisoned")
            .drain()
            .map(|(_, native)| native)
            .collect::<Vec<_>>();
        for native in &natives {
            native.disarm_retirement();
        }
        drop(natives);
    }

    fn clear_operation_state_for_tests() {
        OPERATION_TARGETS
            .lock()
            .expect("operation target map poisoned")
            .clear();
        OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .clear();
        OPERATION_PUBLICATION_PROBES
            .lock()
            .expect("operation publication probe queue poisoned")
            .clear();
        clear_synthetic_targets_for_tests();
    }

    fn take_ready_operation_publication_probe() -> Option<OperationPublicationProbe> {
        take_operation_publication_probe(
            Instant::now()
                + OPERATION_PUBLICATION_PROBE_GRACE
                + OPERATION_PUBLICATION_REPROBE_DELAY,
        )
    }

    fn binder_token(fd: i32) -> BinderFdToken {
        BinderFdToken {
            fd,
            generation: 0,
            connection: fd as BinderStateKey,
        }
    }

    fn publication_retirement(target: LocalBinderTarget) -> NativeBinderRetirement {
        let generation = OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .get(&target)
            .expect("operation publication should exist")
            .generation;
        NativeBinderRetirement { target, generation }
    }

    fn complete_test_publication(target: LocalBinderTarget, fd: i32) -> NativeBinderRetirement {
        let retirement = publication_retirement(target);
        bind_operation_publication_connection(retirement, fd as BinderStateKey);
        mark_operation_publication_completed(retirement, binder_token(fd));
        retirement
    }

    fn finish_operation_publication_probe_for_test(
        probe: OperationPublicationProbe,
        node_exists: Result<bool, i32>,
    ) -> Option<NativeBinderRetirement> {
        finish_operation_publication_probe(probe, node_exists, Instant::now())
    }

    #[test]
    fn operation_publication_accepts_acquire_and_completion_in_either_order() {
        let _guard = route_state_test_guard();
        clear_operation_state_for_tests();

        let completion_first = allocate_test_target();
        register_operation_publication(completion_first).unwrap();
        let completion_first_retirement = complete_test_publication(completion_first, 10);
        assert!(OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .contains_key(&completion_first));
        assert!(take_operation_publication_probe(Instant::now()).is_none());
        let acquire_pending = std::thread::spawn(move || {
            mark_operation_publication_acquire_pending(completion_first, 10)
        })
        .join()
        .expect("cross-thread publication update should not panic");
        assert_eq!(acquire_pending, Some(completion_first_retirement));
        mark_operation_publication_acquire_committed(completion_first_retirement);
        assert!(!OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .contains_key(&completion_first));
        assert!(take_ready_operation_publication_probe().is_none());

        let acquire_first = allocate_test_target();
        register_operation_publication(acquire_first).unwrap();
        let acquire_first_retirement = publication_retirement(acquire_first);
        bind_operation_publication_connection(acquire_first_retirement, 11);
        assert_eq!(
            mark_operation_publication_acquire_pending(acquire_first, 11),
            Some(acquire_first_retirement)
        );
        mark_operation_publication_acquire_committed(acquire_first_retirement);
        assert!(OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .contains_key(&acquire_first));
        mark_operation_publication_completed(acquire_first_retirement, binder_token(11));
        assert!(!OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .contains_key(&acquire_first));
        assert!(take_ready_operation_publication_probe().is_none());

        let cancelled = allocate_test_target();
        register_operation_publication(cancelled).unwrap();
        let cancelled_retirement = complete_test_publication(cancelled, 12);
        let probe = take_ready_operation_publication_probe().unwrap();
        assert_eq!(
            mark_operation_publication_acquire_pending(cancelled, 12),
            Some(cancelled_retirement)
        );
        let requeued_at = Instant::now();
        assert_eq!(
            finish_operation_publication_probe(probe, Ok(false), requeued_at),
            None
        );
        cancel_operation_publication_acquire_pending(cancelled_retirement);
        let probe =
            take_operation_publication_probe(requeued_at + OPERATION_PUBLICATION_REPROBE_DELAY)
                .unwrap();
        assert_eq!(probe.target, cancelled);
        assert_eq!(
            finish_operation_publication_probe_for_test(probe, Ok(false)),
            Some(cancelled_retirement)
        );

        let retired = allocate_test_target();
        register_operation_publication(retired).unwrap();
        let retired_retirement = complete_test_publication(retired, 13);
        let probe = take_ready_operation_publication_probe().unwrap();
        assert_eq!(
            mark_operation_publication_acquire_pending(retired, 13),
            Some(retired_retirement)
        );
        retire_binder_connection_publications(13);
        assert!(operation_publication_acquire_is_pending(probe));
        assert_eq!(
            finish_operation_publication_probe_for_test(probe, Err(libc::ESTALE)),
            None
        );
        assert!(OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .contains_key(&retired));
        mark_operation_publication_acquire_committed(retired_retirement);
        assert!(!OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .contains_key(&retired));

        let abandoned = allocate_test_target();
        register_operation_publication(abandoned).unwrap();
        complete_test_publication(abandoned, 14);
        retire_binder_connection_publications(14);
        assert!(!OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .contains_key(&abandoned));
    }

    #[test]
    fn stale_operation_retirement_does_not_remove_reused_target_generation() {
        let _guard = route_state_test_guard();
        clear_operation_state_for_tests();

        let target = allocate_test_target();
        let first = register_operation_publication_for_test(target);
        SYNTHETIC_TARGETS
            .lock()
            .expect("synthetic target map poisoned")
            .insert(
                target,
                SyntheticTargetInfo {
                    kind: SyntheticTargetKind::Operation,
                    caller: None,
                    native_generation: Some(first.generation),
                },
            );
        drop_synthetic_operation_retirement(first);

        let second = register_operation_publication_for_test(target);
        OPERATION_TARGETS
            .lock()
            .expect("operation target map poisoned")
            .insert(
                target,
                OperationTargetInfo {
                    route: RouteTarget::Omk,
                    aad_allowed: false,
                    backend: None,
                    finalized: false,
                },
            );
        SYNTHETIC_TARGETS
            .lock()
            .expect("synthetic target map poisoned")
            .insert(
                target,
                SyntheticTargetInfo {
                    kind: SyntheticTargetKind::Operation,
                    caller: None,
                    native_generation: Some(second.generation),
                },
            );
        mark_operation_publication_completed(second, binder_token(15));

        drop_synthetic_operation_retirement(first);

        assert_eq!(publication_retirement(target), second);
        assert!(OPERATION_TARGETS
            .lock()
            .expect("operation target map poisoned")
            .contains_key(&target));
        assert_eq!(
            SYNTHETIC_TARGETS
                .lock()
                .expect("synthetic target map poisoned")
                .get(&target)
                .and_then(|info| info.native_generation),
            Some(second.generation)
        );
        assert!(OPERATION_PUBLICATION_PROBES
            .lock()
            .expect("operation publication probe queue poisoned")
            .iter()
            .any(|probe| probe.target == target && probe.generation == second.generation));
    }

    #[test]
    fn pending_operation_publications_are_bounded() {
        let _guard = route_state_test_guard();
        clear_operation_state_for_tests();

        for _ in 0..MAX_PENDING_OPERATION_PUBLICATIONS {
            register_operation_publication(allocate_test_target()).unwrap();
        }
        assert!(register_operation_publication(allocate_test_target()).is_err());

        clear_operation_state_for_tests();
    }

    #[test]
    fn operation_publication_probe_skips_a_deferred_front_entry() {
        let _guard = route_state_test_guard();
        clear_operation_state_for_tests();

        let deferred = allocate_test_target();
        register_operation_publication(deferred).unwrap();
        let deferred_retirement = complete_test_publication(deferred, 20);
        let probe = take_ready_operation_publication_probe().unwrap();
        let requeued_at = Instant::now();
        assert_eq!(
            finish_operation_publication_probe(probe, Ok(true), requeued_at),
            None
        );

        let ready = allocate_test_target();
        register_operation_publication(ready).unwrap();
        let ready_retirement = complete_test_publication(ready, 21);
        let probe =
            take_operation_publication_probe(requeued_at + OPERATION_PUBLICATION_PROBE_GRACE * 2)
                .unwrap();
        assert_eq!(probe.target, ready);
        assert_eq!(
            finish_operation_publication_probe_for_test(probe, Ok(false)),
            Some(ready_retirement)
        );

        let probe =
            take_operation_publication_probe(requeued_at + OPERATION_PUBLICATION_REPROBE_DELAY)
                .unwrap();
        assert_eq!(probe.target, deferred);
        assert_eq!(
            finish_operation_publication_probe_for_test(probe, Ok(false)),
            Some(deferred_retirement)
        );
        clear_operation_state_for_tests();
    }

    #[test]
    fn operation_publication_reclaim_is_lock_free_and_race_safe() {
        let _guard = route_state_test_guard();
        clear_operation_state_for_tests();

        let stale = allocate_test_target();
        register_operation_publication(stale).unwrap();
        let stale_retirement = complete_test_publication(stale, 20);
        assert_eq!(
            mark_operation_publication_acquire_pending(stale, 20),
            Some(stale_retirement)
        );
        mark_operation_publication_acquire_committed(stale_retirement);

        let live = allocate_test_target();
        register_operation_publication(live).unwrap();
        let live_retirement = complete_test_publication(live, 21);
        let first = take_ready_operation_publication_probe().unwrap();
        assert_eq!(first.target, live);
        assert!(OPERATION_PUBLICATIONS.try_lock().is_ok());
        let requeued_at = Instant::now();
        assert_eq!(
            finish_operation_publication_probe(first, Ok(true), requeued_at),
            None
        );
        assert!(take_operation_publication_probe(
            requeued_at + OPERATION_PUBLICATION_REPROBE_DELAY / 2
        )
        .is_none());
        let probe =
            take_operation_publication_probe(requeued_at + OPERATION_PUBLICATION_REPROBE_DELAY)
                .unwrap();
        assert_eq!(
            mark_operation_publication_acquire_pending(live, 21),
            Some(live_retirement)
        );
        assert_eq!(
            finish_operation_publication_probe_for_test(probe, Ok(false)),
            None
        );
        mark_operation_publication_acquire_committed(live_retirement);
        assert!(!OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .contains_key(&live));

        clear_operation_state_for_tests();
        let reused = LocalBinderTarget {
            ptr: 0x6400,
            cookie: 0x7400,
        };
        register_operation_publication(reused).unwrap();
        complete_test_publication(reused, 23);
        let stale = take_ready_operation_publication_probe().unwrap();
        assert_eq!(stale.target, reused);
        register_operation_publication(reused).unwrap();
        complete_test_publication(reused, 23);
        assert_eq!(
            finish_operation_publication_probe_for_test(stale, Ok(false)),
            None
        );
        assert!(OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .contains_key(&reused));
        clear_operation_state_for_tests();
        let missing = allocate_test_target();
        register_operation_publication(missing).unwrap();
        let missing_retirement = complete_test_publication(missing, 24);
        assert!(take_operation_publication_probe(Instant::now()).is_none());
        let probe = take_ready_operation_publication_probe().unwrap();
        assert!(OPERATION_PUBLICATIONS.try_lock().is_ok());
        assert_eq!(
            finish_operation_publication_probe_for_test(probe, Ok(false)),
            Some(missing_retirement)
        );
        assert!(!OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .contains_key(&missing));

        clear_operation_state_for_tests();
        let retry_after_error = allocate_test_target();
        register_operation_publication(retry_after_error).unwrap();
        let retry_retirement = complete_test_publication(retry_after_error, 25);
        let probe = take_ready_operation_publication_probe().unwrap();
        let requeued_at = Instant::now();
        assert_eq!(
            finish_operation_publication_probe(probe, Err(libc::EIO), requeued_at),
            None
        );
        assert_eq!(
            OPERATION_PUBLICATION_PROBES
                .lock()
                .expect("operation publication probe queue poisoned")
                .iter()
                .filter(|probe| probe.target == retry_after_error)
                .count(),
            1
        );
        let second_error_at = requeued_at + OPERATION_PUBLICATION_REPROBE_DELAY * 2;
        let probe = take_operation_publication_probe(second_error_at).unwrap();
        assert_eq!(probe.query_failures, 1);
        assert_eq!(
            finish_operation_publication_probe_for_test(probe, Ok(false)),
            Some(retry_retirement)
        );

        clear_operation_state_for_tests();
        ensure_binder_process_state();
        let aborts = Arc::new(AtomicUsize::new(0));
        let backend = BnKeystoreOperation::new_binder(TestOperationBackend {
            update_output: Vec::new(),
            aborts: aborts.clone(),
            update_aad_status: None,
        });
        let (_, persistent_query_error) =
            register_synthetic_operation_carrier(backend, false, &CallerIdentity::new(10002, 2000))
                .expect("synthetic operation carrier should register");
        mark_operation_publication_completed(persistent_query_error, binder_token(26));
        let mut attempt_at = Instant::now() + OPERATION_PUBLICATION_PROBE_GRACE;
        for failure in 1..=OPERATION_PUBLICATION_MAX_QUERY_BACKOFF_SHIFT + 2 {
            let probe = take_operation_publication_probe(attempt_at).unwrap();
            assert_eq!(probe.target, persistent_query_error.target);
            assert_eq!(
                finish_operation_publication_probe(probe, Err(libc::EPERM), attempt_at),
                None
            );
            let queued = OPERATION_PUBLICATION_PROBES
                .lock()
                .expect("operation publication probe queue poisoned")
                .iter()
                .find(|probe| probe.target == persistent_query_error.target)
                .copied()
                .unwrap();
            let expected_failures = failure.min(OPERATION_PUBLICATION_MAX_QUERY_BACKOFF_SHIFT);
            assert_eq!(queued.query_failures, expected_failures);
            assert_eq!(
                queued.not_before.duration_since(attempt_at),
                OPERATION_PUBLICATION_REPROBE_DELAY
                    .saturating_mul(1u32 << u32::from(expected_failures))
            );
            attempt_at = queued.not_before;
        }
        assert!(OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .contains_key(&persistent_query_error.target));
        assert_eq!(aborts.load(Ordering::SeqCst), 0);
        assert!(lookup_operation_target(persistent_query_error.target).is_some());
        assert_eq!(
            lookup_synthetic_target(persistent_query_error.target),
            Some(SyntheticTargetKind::Operation)
        );
        assert!(lookup_native_binder(persistent_query_error.target).is_some());
        let probe = take_operation_publication_probe(attempt_at).unwrap();
        assert_eq!(
            finish_operation_publication_probe_for_test(probe, Ok(false)),
            Some(persistent_query_error)
        );
        drop_synthetic_operation_retirement(persistent_query_error);
        assert_eq!(aborts.load(Ordering::SeqCst), 1);
        assert!(lookup_operation_target(persistent_query_error.target).is_none());
        assert!(lookup_synthetic_target(persistent_query_error.target).is_none());
        assert!(lookup_native_binder(persistent_query_error.target).is_none());

        clear_operation_state_for_tests();
        let terminal_tombstone = allocate_test_target();
        register_operation_publication(terminal_tombstone).unwrap();
        let terminal_retirement = complete_test_publication(terminal_tombstone, 27);
        let in_flight_probe = take_ready_operation_publication_probe().unwrap();
        retire_synthetic_operation_target(terminal_tombstone);
        let requeued_at = Instant::now();
        assert_eq!(
            finish_operation_publication_probe(in_flight_probe, Ok(true), requeued_at),
            None
        );
        assert!(OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .contains_key(&terminal_tombstone));
        assert_eq!(
            mark_operation_publication_acquire_pending(terminal_tombstone, 27),
            Some(terminal_retirement)
        );
        mark_operation_publication_acquire_committed(terminal_retirement);
        assert!(!OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .contains_key(&terminal_tombstone));
        assert!(take_operation_publication_probe(
            requeued_at + OPERATION_PUBLICATION_REPROBE_DELAY
        )
        .is_none());

        clear_operation_state_for_tests();
        let terminal_without_node = allocate_test_target();
        register_operation_publication(terminal_without_node).unwrap();
        let terminal_without_node_retirement = complete_test_publication(terminal_without_node, 28);
        let probe = take_ready_operation_publication_probe().unwrap();
        retire_synthetic_operation_target(terminal_without_node);
        assert_eq!(
            finish_operation_publication_probe_for_test(probe, Ok(false)),
            Some(terminal_without_node_retirement)
        );
        assert!(!OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .contains_key(&terminal_without_node));
    }

    #[test]
    fn intercepted_operation_transaction_does_not_replace_acquire_ack() {
        ensure_binder_process_state();
        let _guard = route_state_test_guard();
        clear_operation_state_for_tests();

        let aborts = Arc::new(AtomicUsize::new(0));
        let backend = BnKeystoreOperation::new_binder(TestOperationBackend {
            update_output: Vec::new(),
            aborts: aborts.clone(),
            update_aad_status: None,
        });
        let (_, retirement) =
            register_synthetic_operation_carrier(backend, false, &CallerIdentity::new(10002, 2000))
                .expect("synthetic operation carrier should register");
        let target = retirement.target;
        bind_operation_publication_connection(retirement, 26);
        mark_operation_publication_completed(retirement, binder_token(26));
        assert_eq!(
            mark_operation_publication_acquire_pending(target, 26),
            Some(retirement)
        );
        assert!(OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .contains_key(&target));

        let request = request_parcel(identify::KEYSTORE_OPERATION_INTERFACE);
        let tr = transaction_for_parcel(
            target,
            identify::AIDL_GET_INTERFACE_VERSION_TRANSACTION,
            &request,
        );
        assert!(unsafe { handle_synthetic_br_transaction(&tr, None, "BR_TRANSACTION") }.is_some());
        assert!(OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .contains_key(&target));
        let acquired = lookup_native_binder(target).expect("host should own the published Binder");

        mark_operation_publication_acquire_committed(retirement);
        assert!(!OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .contains_key(&target));
        assert!(lookup_native_binder(target).is_none());
        assert!(lookup_operation_target(target).is_some());
        assert_eq!(
            lookup_synthetic_target(target),
            Some(SyntheticTargetKind::Operation)
        );

        drop(acquired);
        assert!(lookup_operation_target(target).is_none());
        assert_eq!(aborts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn local_operation_publication_handoff_releases_initial_strong() {
        ensure_binder_process_state();
        let _guard = route_state_test_guard();
        clear_operation_state_for_tests();

        let aborts = Arc::new(AtomicUsize::new(0));
        let backend = BnKeystoreOperation::new_binder(TestOperationBackend {
            update_output: Vec::new(),
            aborts: aborts.clone(),
            update_aad_status: None,
        });
        let (_, retirement) =
            register_synthetic_operation_carrier(backend, false, &CallerIdentity::new(10002, 2000))
                .expect("synthetic operation carrier should register");
        let target = retirement.target;
        assert!(OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .contains_key(&target));
        let acquired = lookup_native_binder(target).expect("host should own the published Binder");

        finish_local_operation_publication(retirement);

        assert!(!OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .contains_key(&target));
        assert!(lookup_native_binder(target).is_none());
        assert!(lookup_operation_target(target).is_some());
        assert_eq!(aborts.load(Ordering::SeqCst), 0);

        drop(acquired);
        assert!(lookup_operation_target(target).is_none());
        assert_eq!(aborts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn native_retirement_releases_unfinished_publication() {
        ensure_binder_process_state();
        let _guard = route_state_test_guard();
        clear_operation_state_for_tests();

        let aborts = Arc::new(AtomicUsize::new(0));
        let backend = BnKeystoreOperation::new_binder(TestOperationBackend {
            update_output: Vec::new(),
            aborts: aborts.clone(),
            update_aad_status: None,
        });
        let (_, retirement) =
            register_synthetic_operation_carrier(backend, false, &CallerIdentity::new(10002, 2000))
                .expect("synthetic operation carrier should register");
        let target = retirement.target;
        bind_operation_publication_connection(retirement, 27);
        mark_operation_publication_completed(retirement, binder_token(27));

        release_native_operation_initial_strong(retirement);

        assert!(!OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .contains_key(&target));
        assert!(OPERATION_PUBLICATION_PROBES
            .lock()
            .expect("operation publication probe queue poisoned")
            .iter()
            .all(|probe| probe.target != target));
        assert!(lookup_operation_target(target).is_none());
        assert!(lookup_synthetic_target(target).is_none());
        assert_eq!(aborts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn terminal_reply_retires_backend_before_native_publication() {
        ensure_binder_process_state();
        let _guard = route_state_test_guard();
        clear_operation_state_for_tests();

        let aborts = Arc::new(AtomicUsize::new(0));
        let backend = BnKeystoreOperation::new_binder(TestOperationBackend {
            update_output: Vec::new(),
            aborts: aborts.clone(),
            update_aad_status: None,
        });
        let (_, retirement) =
            register_synthetic_operation_carrier(backend, false, &CallerIdentity::new(10002, 2000))
                .expect("synthetic operation carrier should register");
        let target = retirement.target;
        bind_operation_publication_connection(retirement, 29);
        mark_operation_publication_completed(retirement, binder_token(29));
        assert!(take_operation_publication_probe(Instant::now()).is_none());

        retire_synthetic_operation_retirement(retirement);

        assert_eq!(aborts.load(Ordering::SeqCst), 1);
        assert!(lookup_operation_target(target).is_none());
        assert!(lookup_synthetic_target(target).is_none());
        assert!(OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .contains_key(&target));
        assert!(lookup_native_binder(target).is_some());

        let probe = take_operation_publication_probe(Instant::now())
            .expect("terminal retirement should make the queued probe immediately ready");
        assert_eq!(probe.target, target);
        assert_eq!(
            finish_operation_publication_probe_for_test(probe, Ok(false)),
            Some(retirement)
        );

        drop_synthetic_operation_retirement(retirement);
        assert_eq!(aborts.load(Ordering::SeqCst), 1);
        assert!(lookup_native_binder(target).is_none());
    }

    #[test]
    fn untracked_native_target_is_not_intercepted() {
        let _guard = route_state_test_guard();
        let target = LocalBinderTarget {
            ptr: 0x1234,
            cookie: 0x5678,
        };
        let empty = rsbinder::Parcel::new();
        let tr = transaction_for_parcel(target, rsbinder::FIRST_CALL_TRANSACTION, &empty);
        assert!(unsafe { handle_synthetic_br_transaction(&tr, None, "BR_TRANSACTION") }.is_none());
    }

    #[test]
    fn synthetic_base_transactions_do_not_require_interface_token() {
        let target = LocalBinderTarget {
            ptr: 0x4100,
            cookie: 0x5100,
        };
        let empty = rsbinder::Parcel::new();
        let ping_tr = transaction_for_parcel(target, rsbinder::PING_TRANSACTION, &empty);
        let ping = unsafe {
            synthetic_base_transaction_reply(Some(SyntheticTargetKind::Operation), target, &ping_tr)
        }
        .expect("ping handling should not fail")
        .expect("ping should produce a reply");
        let SyntheticReply::Parcel(ping) = ping else {
            panic!("ping should be an empty parcel reply");
        };
        assert_eq!(ping.data_size(), 0);
        assert_eq!(ping.offsets_size(), 0);

        let mut debug_pid_tr =
            transaction_for_parcel(target, rsbinder::DEBUG_PID_TRANSACTION, &empty);
        debug_pid_tr.sender_pid = if synthetic_debug_pid() == 2000 {
            2001
        } else {
            2000
        };
        let debug_pid = unsafe {
            synthetic_base_transaction_reply(
                Some(SyntheticTargetKind::Operation),
                target,
                &debug_pid_tr,
            )
        }
        .expect("debug pid handling should not fail")
        .expect("debug pid should produce a reply");
        assert_synthetic_raw_i32_reply(debug_pid, synthetic_debug_pid());
        assert_ne!(synthetic_debug_pid(), debug_pid_tr.sender_pid);

        let shell_tr = transaction_for_parcel(target, rsbinder::SHELL_COMMAND_TRANSACTION, &empty);
        let shell = unsafe {
            synthetic_base_transaction_reply(
                Some(SyntheticTargetKind::Operation),
                target,
                &shell_tr,
            )
        }
        .expect("shell handling should not fail")
        .expect("shell should produce a reply");
        assert_synthetic_empty_parcel_reply(shell, "shell");

        let sysprops_tr = transaction_for_parcel(target, rsbinder::SYSPROPS_TRANSACTION, &empty);
        let sysprops = unsafe {
            synthetic_base_transaction_reply(
                Some(SyntheticTargetKind::Operation),
                target,
                &sysprops_tr,
            )
        }
        .expect("sysprops handling should not fail")
        .expect("sysprops should produce a reply");
        assert_synthetic_empty_parcel_reply(sysprops, "sysprops");

        let start_recording_tr =
            transaction_for_parcel(target, rsbinder::START_RECORDING_TRANSACTION, &empty);
        let start_recording = unsafe {
            synthetic_base_transaction_reply(
                Some(SyntheticTargetKind::Operation),
                target,
                &start_recording_tr,
            )
        }
        .expect("start-recording handling should not fail")
        .expect("start-recording should produce a reply");
        let SyntheticReply::Status(status) = start_recording else {
            panic!("unsupported base transaction should be a binder status code");
        };
        assert_eq!(status, i32::from(StatusCode::InvalidOperation));

        let malformed_dump_tr = transaction_for_parcel(target, rsbinder::DUMP_TRANSACTION, &empty);
        let malformed_dump = unsafe {
            synthetic_base_transaction_reply(
                Some(SyntheticTargetKind::Operation),
                target,
                &malformed_dump_tr,
            )
        }
        .expect("dump handling should not fail")
        .expect("dump should produce a reply");
        let SyntheticReply::Status(status) = malformed_dump else {
            panic!("malformed dump should be a binder status code");
        };
        assert_eq!(status, i32::from(StatusCode::BadType));

        let mut dump_data = dump_transaction_data(0, &[]);
        let mut offsets = [0usize];
        let dump_tr = transaction_for_raw_parts(
            target,
            rsbinder::DUMP_TRANSACTION,
            &mut dump_data,
            &mut offsets,
        );
        let dump = unsafe {
            synthetic_base_transaction_reply(Some(SyntheticTargetKind::Operation), target, &dump_tr)
        }
        .expect("dump handling should not fail")
        .expect("dump should produce a reply");
        assert_synthetic_empty_parcel_reply(dump, "valid dump");

        let mut missing_argc_data = dump_transaction_data(0, &[]);
        missing_argc_data.truncate(size_of::<flat_binder_object>());
        let missing_argc_tr = transaction_for_raw_parts(
            target,
            rsbinder::DUMP_TRANSACTION,
            &mut missing_argc_data,
            &mut offsets,
        );
        let missing_argc = unsafe {
            synthetic_base_transaction_reply(
                Some(SyntheticTargetKind::Operation),
                target,
                &missing_argc_tr,
            )
        }
        .expect("missing argc dump handling should not fail")
        .expect("missing argc dump should produce a reply");
        assert_synthetic_empty_parcel_reply(missing_argc, "missing argc dump");

        let mut dump_with_args_data = dump_transaction_data(1, &[String::from("--proto")]);
        let dump_with_args_tr = transaction_for_raw_parts(
            target,
            rsbinder::DUMP_TRANSACTION,
            &mut dump_with_args_data,
            &mut offsets,
        );
        let dump_with_args = unsafe {
            synthetic_base_transaction_reply(
                Some(SyntheticTargetKind::Operation),
                target,
                &dump_with_args_tr,
            )
        }
        .expect("dump with args handling should not fail")
        .expect("dump with args should produce a reply");
        assert_synthetic_empty_parcel_reply(dump_with_args, "valid dump with args");

        let mut missing_arg_data = dump_transaction_data(1, &[]);
        let missing_arg_tr = transaction_for_raw_parts(
            target,
            rsbinder::DUMP_TRANSACTION,
            &mut missing_arg_data,
            &mut offsets,
        );
        let missing_arg = unsafe {
            synthetic_base_transaction_reply(
                Some(SyntheticTargetKind::Operation),
                target,
                &missing_arg_tr,
            )
        }
        .expect("missing arg dump handling should not fail")
        .expect("missing arg dump should produce a reply");
        assert_synthetic_empty_parcel_reply(missing_arg, "missing arg dump");

        let mut negative_argc_data = dump_transaction_data(-1, &[]);
        let negative_argc_tr = transaction_for_raw_parts(
            target,
            rsbinder::DUMP_TRANSACTION,
            &mut negative_argc_data,
            &mut offsets,
        );
        let negative_argc = unsafe {
            synthetic_base_transaction_reply(
                Some(SyntheticTargetKind::Operation),
                target,
                &negative_argc_tr,
            )
        }
        .expect("negative argc dump handling should not fail")
        .expect("negative argc dump should produce a reply");
        assert_synthetic_empty_parcel_reply(negative_argc, "negative argc dump");

        let mut trailing_dump_data = dump_transaction_data(0, &[]);
        trailing_dump_data.extend_from_slice(&0x4f4d4bu32.to_ne_bytes());
        let trailing_dump_tr = transaction_for_raw_parts(
            target,
            rsbinder::DUMP_TRANSACTION,
            &mut trailing_dump_data,
            &mut offsets,
        );
        let trailing_dump = unsafe {
            synthetic_base_transaction_reply(
                Some(SyntheticTargetKind::Operation),
                target,
                &trailing_dump_tr,
            )
        }
        .expect("trailing dump handling should not fail")
        .expect("trailing dump should produce a reply");
        assert_synthetic_empty_parcel_reply(trailing_dump, "trailing dump");

        let mut trailing_object_dump_data = dump_transaction_data(0, &[]);
        while !trailing_object_dump_data
            .len()
            .is_multiple_of(size_of::<usize>())
        {
            trailing_object_dump_data.push(0);
        }
        let trailing_object_offset = trailing_object_dump_data.len();
        trailing_object_dump_data
            .resize(trailing_object_offset + size_of::<flat_binder_object>(), 0);
        let trailing_object = flat_binder_object {
            hdr: binder_object_header {
                type_: BINDER_TYPE_BINDER,
            },
            flags: 0,
            handle_or_ptr: flat_binder_object_handle_or_ptr { binder: 0 },
            cookie: 0,
        };
        unsafe {
            std::ptr::write_unaligned(
                trailing_object_dump_data
                    .as_mut_ptr()
                    .add(trailing_object_offset) as *mut flat_binder_object,
                trailing_object,
            );
        }
        let mut trailing_object_offsets = [0usize, trailing_object_offset];
        let trailing_object_dump_tr = transaction_for_raw_parts(
            target,
            rsbinder::DUMP_TRANSACTION,
            &mut trailing_object_dump_data,
            &mut trailing_object_offsets,
        );
        let trailing_object_dump = unsafe {
            synthetic_base_transaction_reply(
                Some(SyntheticTargetKind::Operation),
                target,
                &trailing_object_dump_tr,
            )
        }
        .expect("trailing object dump handling should not fail")
        .expect("trailing object dump should produce a reply");
        assert_synthetic_empty_parcel_reply(trailing_object_dump, "trailing object dump");

        let unknown_tr = transaction_for_parcel(target, u32::MAX, &empty);
        let unknown = unsafe {
            synthetic_base_transaction_reply(
                Some(SyntheticTargetKind::Operation),
                target,
                &unknown_tr,
            )
        }
        .expect("unknown outside call range should not fail")
        .expect("unknown outside call range should produce a reply");
        assert_unknown_transaction_reply(unknown);
    }

    #[test]
    fn synthetic_aidl_metadata_contract() {
        let target = LocalBinderTarget {
            ptr: 0x6234,
            cookie: 0xa678,
        };
        let request = request_parcel(identify::KEYSTORE_OPERATION_INTERFACE);
        let tr = transaction_for_parcel(
            target,
            identify::AIDL_GET_INTERFACE_VERSION_TRANSACTION,
            &request,
        );
        let info = SyntheticTargetInfo {
            kind: SyntheticTargetKind::Operation,
            caller: Some(CallerIdentity::new(10002, 2000)),
            native_generation: None,
        };

        let reply = unsafe {
            build_synthetic_br_transaction_reply(&tr, target, info, None, "BR_TRANSACTION")
        }
        .expect("operation metadata version should be handled");
        let SyntheticReply::Parcel(mut reply) = reply else {
            panic!("operation metadata version should return a parcel reply");
        };
        let version: i32 = parcel::parse_owned_success_reply(&mut reply)
            .expect("metadata version reply should parse");
        let expected = keystore2_aidl_metadata().expect("keystore2 metadata should resolve");
        assert_eq!(version, expected.version);

        let target = LocalBinderTarget {
            ptr: 0x7234,
            cookie: 0xb678,
        };
        let request = request_parcel(identify::KEYSTORE_SERVICE_INTERFACE);
        let tr = transaction_for_parcel(
            target,
            identify::AIDL_GET_INTERFACE_HASH_TRANSACTION,
            &request,
        );
        let info = SyntheticTargetInfo {
            kind: SyntheticTargetKind::Operation,
            caller: Some(CallerIdentity::new(10002, 2000)),
            native_generation: None,
        };

        let reply = unsafe {
            build_synthetic_br_transaction_reply(&tr, target, info, None, "BR_TRANSACTION")
        }
        .expect("wrong metadata interface should be handled");
        assert_synthetic_status(reply, StatusCode::BadType);

        for (index, code) in [
            (0, identify::AIDL_GET_INTERFACE_VERSION_TRANSACTION),
            (1, identify::AIDL_GET_INTERFACE_HASH_TRANSACTION),
        ] {
            let target = LocalBinderTarget {
                ptr: 0x7254 + index,
                cookie: 0xb698 + index,
            };
            let request = request_parcel_with_marker(identify::KEYSTORE_OPERATION_INTERFACE, 0);
            let tr = transaction_for_parcel(target, code, &request);
            let info = SyntheticTargetInfo {
                kind: SyntheticTargetKind::Operation,
                caller: Some(CallerIdentity::new(10002, 2000)),
                native_generation: None,
            };

            let reply = unsafe {
                build_synthetic_br_transaction_reply(&tr, target, info, None, "BR_TRANSACTION")
            }
            .expect("bad metadata marker should be handled");
            assert_synthetic_status(reply, StatusCode::BadType);
        }

        let expected = keystore2_aidl_metadata().expect("keystore2 metadata should resolve");

        for (index, label, code) in [
            (
                0,
                "operation version",
                identify::AIDL_GET_INTERFACE_VERSION_TRANSACTION,
            ),
            (
                1,
                "operation hash",
                identify::AIDL_GET_INTERFACE_HASH_TRANSACTION,
            ),
        ] {
            let target = LocalBinderTarget {
                ptr: 0x7334 + index,
                cookie: 0xb778 + index,
            };
            let mut request = request_parcel(identify::KEYSTORE_OPERATION_INTERFACE);
            request.write(&0x4f4d4bi32).unwrap();
            let tr = transaction_for_parcel(target, code, &request);
            let info = SyntheticTargetInfo {
                kind: SyntheticTargetKind::Operation,
                caller: Some(CallerIdentity::new(10002, 2000)),
                native_generation: None,
            };

            let reply = unsafe {
                build_synthetic_br_transaction_reply(&tr, target, info, None, "BR_TRANSACTION")
            }
            .unwrap_or_else(|error| {
                panic!("{label} trailing metadata should be handled: {error:#}")
            });
            let SyntheticReply::Parcel(mut reply) = reply else {
                panic!("{label} trailing metadata should return a parcel reply");
            };
            if code == identify::AIDL_GET_INTERFACE_HASH_TRANSACTION {
                let hash: String =
                    parcel::parse_owned_success_reply(&mut reply).unwrap_or_else(|error| {
                        panic!("{label} metadata hash reply should parse: {error:#}")
                    });
                assert_eq!(hash, expected.hash, "{label}");
            } else {
                let version: i32 =
                    parcel::parse_owned_success_reply(&mut reply).unwrap_or_else(|error| {
                        panic!("{label} metadata version reply should parse: {error:#}")
                    });
                assert_eq!(version, expected.version, "{label}");
            }
        }
    }

    #[test]
    fn synthetic_operation_missing_args_keep_not_enough_data_status() {
        let _guard = route_state_test_guard();

        let target = LocalBinderTarget {
            ptr: 0x1234,
            cookie: 0x5678,
        };
        let aborts = Arc::new(AtomicUsize::new(0));
        let backend = BnKeystoreOperation::new_binder(TestOperationBackend {
            update_output: vec![5, 6, 7],
            aborts: aborts.clone(),
            update_aad_status: None,
        });
        remember_operation_target(
            target,
            OperationTargetInfo {
                route: RouteTarget::Omk,
                aad_allowed: true,
                backend: Some(backend),
                finalized: false,
            },
        );

        for (label, code) in [
            (
                "updateAad",
                crate::android::system::keystore2::IKeystoreOperation::transactions::r#updateAad,
            ),
            (
                "update",
                crate::android::system::keystore2::IKeystoreOperation::transactions::r#update,
            ),
            (
                "finish",
                crate::android::system::keystore2::IKeystoreOperation::transactions::r#finish,
            ),
        ] {
            let request = request_parcel(identify::KEYSTORE_OPERATION_INTERFACE);
            let tr = transaction_for_parcel(target, code, &request);
            let info = SyntheticTargetInfo {
                kind: SyntheticTargetKind::Operation,
                caller: Some(CallerIdentity::new(10002, 2000)),
                native_generation: None,
            };

            let reply = unsafe {
                build_synthetic_br_transaction_reply(&tr, target, info, None, "BR_TRANSACTION")
            }
            .unwrap_or_else(|error| panic!("{label} missing args should be handled: {error:#}"));
            assert_synthetic_status(reply, StatusCode::NotEnoughData);
            assert!(
                lookup_operation_target(target).is_some(),
                "{label} missing args must not finalize the operation"
            );
        }
        assert_eq!(aborts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn synthetic_operation_trailing_abort_finalizes_operation() {
        let _guard = route_state_test_guard();

        let target = LocalBinderTarget {
            ptr: 0x1334,
            cookie: 0x5778,
        };
        let aborts = Arc::new(AtomicUsize::new(0));
        let backend = BnKeystoreOperation::new_binder(TestOperationBackend {
            update_output: vec![5, 6, 7],
            aborts: aborts.clone(),
            update_aad_status: None,
        });
        remember_operation_target(
            target,
            OperationTargetInfo {
                route: RouteTarget::Omk,
                aad_allowed: true,
                backend: Some(backend),
                finalized: false,
            },
        );

        let mut request = request_parcel(identify::KEYSTORE_OPERATION_INTERFACE);
        request.write(&0x4f4d4bi32).unwrap();
        let tr = transaction_for_parcel(
            target,
            crate::android::system::keystore2::IKeystoreOperation::transactions::r#abort,
            &request,
        );
        let info = SyntheticTargetInfo {
            kind: SyntheticTargetKind::Operation,
            caller: Some(CallerIdentity::new(10002, 2000)),
            native_generation: None,
        };

        let reply = unsafe {
            build_synthetic_br_transaction_reply(&tr, target, info, None, "BR_TRANSACTION")
        }
        .expect("trailing abort should be handled without fallback");
        assert_synthetic_ok_reply(reply, "trailing abort");
        assert!(
            lookup_operation_target(target).is_none(),
            "trailing abort must finalize the operation"
        );
        assert_eq!(aborts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn synthetic_operation_bad_interface_marker_rejects_abort() {
        let _guard = route_state_test_guard();

        let target = LocalBinderTarget {
            ptr: 0x1335,
            cookie: 0x5779,
        };
        let aborts = Arc::new(AtomicUsize::new(0));
        let backend = BnKeystoreOperation::new_binder(TestOperationBackend {
            update_output: vec![5, 6, 7],
            aborts: aborts.clone(),
            update_aad_status: None,
        });
        remember_operation_target(
            target,
            OperationTargetInfo {
                route: RouteTarget::Omk,
                aad_allowed: true,
                backend: Some(backend),
                finalized: false,
            },
        );

        let request = request_parcel_with_marker(identify::KEYSTORE_OPERATION_INTERFACE, 0);
        let tr = transaction_for_parcel(
            target,
            crate::android::system::keystore2::IKeystoreOperation::transactions::r#abort,
            &request,
        );
        let info = SyntheticTargetInfo {
            kind: SyntheticTargetKind::Operation,
            caller: Some(CallerIdentity::new(10002, 2000)),
            native_generation: None,
        };

        let reply = unsafe {
            build_synthetic_br_transaction_reply(&tr, target, info, None, "BR_TRANSACTION")
        }
        .expect("bad abort marker should be handled");
        assert_synthetic_status(reply, StatusCode::BadType);
        assert!(
            lookup_operation_target(target).is_some(),
            "bad marker abort must not finalize the operation"
        );
        assert_eq!(aborts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn operation_dispatch_uses_recorded_caller_identity() {
        let _guard = route_state_test_guard();

        for (index, label, code) in [
            (
                0,
                "updateAad",
                crate::android::system::keystore2::IKeystoreOperation::transactions::r#updateAad,
            ),
            (
                1,
                "update",
                crate::android::system::keystore2::IKeystoreOperation::transactions::r#update,
            ),
            (
                2,
                "finish",
                crate::android::system::keystore2::IKeystoreOperation::transactions::r#finish,
            ),
            (
                3,
                "abort",
                crate::android::system::keystore2::IKeystoreOperation::transactions::r#abort,
            ),
        ] {
            let target = LocalBinderTarget {
                ptr: 0x1434 + index,
                cookie: 0x5878 + index,
            };
            let aborts = Arc::new(AtomicUsize::new(0));
            let backend = BnKeystoreOperation::new_binder(TestOperationBackend {
                update_output: vec![5, 6, 7],
                aborts: aborts.clone(),
                update_aad_status: None,
            });
            remember_operation_target(
                target,
                OperationTargetInfo {
                    route: RouteTarget::Omk,
                    aad_allowed: true,
                    backend: Some(backend),
                    finalized: false,
                },
            );

            let mut request = request_parcel(identify::KEYSTORE_OPERATION_INTERFACE);
            match label {
                "updateAad" | "update" => request.write(&vec![1u8]).unwrap(),
                "finish" => {
                    request.write(&None::<Vec<u8>>).unwrap();
                    request.write(&None::<Vec<u8>>).unwrap();
                }
                "abort" => {}
                _ => unreachable!("covered operation test method"),
            }
            let mut tr = transaction_for_parcel(target, code, &request);
            tr.sender_euid = 99999;
            tr.sender_pid = 3456;
            let info = SyntheticTargetInfo {
                kind: SyntheticTargetKind::Operation,
                caller: Some(CallerIdentity::new(10002, 2000)),
                native_generation: None,
            };

            let reply = unsafe {
                build_synthetic_br_transaction_reply(&tr, target, info, None, "BR_TRANSACTION")
            }
            .unwrap_or_else(|error| panic!("{label} should be handled: {error:#}"));
            assert_synthetic_ok_reply(reply, label);

            if label == "abort" {
                assert_eq!(aborts.load(Ordering::SeqCst), 1);
                assert!(
                    lookup_operation_target(target).is_none(),
                    "abort should clear the operation mapping"
                );
            }
        }

        reset_route_state_for_tests();

        let target = LocalBinderTarget {
            ptr: 0x1534,
            cookie: 0x5978,
        };
        remember_operation_target(
            target,
            OperationTargetInfo {
                route: RouteTarget::Omk,
                aad_allowed: true,
                backend: None,
                finalized: false,
            },
        );

        let mut request = request_parcel(identify::KEYSTORE_OPERATION_INTERFACE);
        request.write(&vec![1u8]).unwrap();
        let mut tr = transaction_for_parcel(
            target,
            crate::android::system::keystore2::IKeystoreOperation::transactions::r#update,
            &request,
        );
        tr.sender_euid = 99999;
        tr.sender_pid = 3456;

        let fd = 37;
        let rewritten = unsafe { handle_br_transaction(fd, &mut tr, None, "BR_TRANSACTION") };
        assert!(!rewritten);
        let Some(Some(PendingCall::Operation(pending))) = take_top_pending(fd) else {
            panic!("tracked operation should still enqueue a pending operation call");
        };
        assert_eq!(pending.target, target);
        assert_eq!(pending.caller.uid, 99999);
        assert!(matches!(
            pending.request,
            ParsedOperationRequest::Update { .. }
        ));
    }

    #[test]
    fn synthetic_unexpected_null_parse_errors_are_status_replies() {
        let _guard = route_state_test_guard();

        let operation_target = LocalBinderTarget {
            ptr: 0x2237,
            cookie: 0x6681,
        };
        let operation_info = SyntheticTargetInfo {
            kind: SyntheticTargetKind::Operation,
            caller: Some(CallerIdentity::new(10002, 2000)),
            native_generation: None,
        };
        for length in [-1i32, -2i32] {
            let mut invalid_input_request = request_parcel(identify::KEYSTORE_OPERATION_INTERFACE);
            invalid_input_request.write(&length).unwrap();
            let invalid_input_tr = transaction_for_parcel(
                operation_target,
                crate::android::system::keystore2::IKeystoreOperation::transactions::r#update,
                &invalid_input_request,
            );
            let reply = unsafe {
                build_synthetic_br_transaction_reply(
                    &invalid_input_tr,
                    operation_target,
                    operation_info.clone(),
                    None,
                    "BR_TRANSACTION",
                )
            }
            .expect("invalid operation update should be handled");
            assert_synthetic_exception_reply(reply, ExceptionCode::NullPointer);
        }

        let security_level_target = LocalBinderTarget {
            ptr: 0x2238,
            cookie: 0x6682,
        };
        let mut invalid_key_request = request_parcel(identify::KEYSTORE_SECURITY_LEVEL_INTERFACE);
        invalid_key_request.write(&2i32).unwrap();
        let invalid_key_tr = transaction_for_parcel(
            security_level_target,
            crate::android::system::keystore2::IKeystoreSecurityLevel::transactions::r#deleteKey,
            &invalid_key_request,
        );
        let reply = unsafe {
            build_synthetic_br_transaction_reply(
                &invalid_key_tr,
                security_level_target,
                SyntheticTargetInfo {
                    kind: SyntheticTargetKind::SecurityLevel,
                    caller: None,
                    native_generation: None,
                },
                None,
                "BR_TRANSACTION",
            )
        }
        .expect("invalid key presence flag should be handled");
        assert_synthetic_exception_reply(reply, ExceptionCode::NullPointer);
    }

    #[test]
    fn synthetic_operation_unexpected_interface_returns_bad_type_status() {
        let _guard = route_state_test_guard();

        let target = LocalBinderTarget {
            ptr: 0x3234,
            cookie: 0x7678,
        };
        let request = request_parcel(identify::KEYSTORE_SERVICE_INTERFACE);
        let tr = transaction_for_parcel(
            target,
            crate::android::system::keystore2::IKeystoreOperation::transactions::r#abort,
            &request,
        );
        let info = SyntheticTargetInfo {
            kind: SyntheticTargetKind::Operation,
            caller: Some(CallerIdentity::new(10002, 2000)),
            native_generation: None,
        };

        let reply = unsafe {
            build_synthetic_br_transaction_reply(&tr, target, info, None, "BR_TRANSACTION")
        }
        .expect("unexpected interface should be handled without fallback");
        assert_synthetic_status(reply, StatusCode::BadType);
    }

    #[test]
    fn plain_omk_error_becomes_system_error_reply() {
        let error = anyhow::anyhow!("plain OMK failure");
        let mut reply =
            build_omk_error_reply(&error).expect("plain error should produce a status reply");
        let (data, data_size, offsets, offsets_size) = raw_parts(&mut reply);
        let parsed = unsafe { parcel::parse_reply_status(data, data_size, offsets, offsets_size) }
            .expect("status reply should parse");

        assert_eq!(
            parsed.exception_code(),
            rsbinder::ExceptionCode::ServiceSpecific
        );
        assert_eq!(
            parsed.service_specific_error(),
            ResponseCode::SYSTEM_ERROR.0
        );
    }

    #[test]
    fn contextual_omk_status_error_keeps_service_specific_code() {
        let status = Status::new_service_specific_error(ResponseCode::PERMISSION_DENIED.0, None);
        let error = anyhow::Error::new(status).context("wrapped OMK failure");
        let mut reply =
            build_omk_error_reply(&error).expect("wrapped status should produce a status reply");
        let (data, data_size, offsets, offsets_size) = raw_parts(&mut reply);
        let parsed = unsafe { parcel::parse_reply_status(data, data_size, offsets, offsets_size) }
            .expect("status reply should parse");

        assert_eq!(
            parsed.exception_code(),
            rsbinder::ExceptionCode::ServiceSpecific
        );
        assert_eq!(
            parsed.service_specific_error(),
            ResponseCode::PERMISSION_DENIED.0
        );
    }

    #[test]
    fn unavailable_omk_errors_preserve_system_reply() {
        for status in [
            StatusCode::DeadObject,
            StatusCode::RpcError,
            StatusCode::NotEnoughData,
            StatusCode::NoInit,
            StatusCode::NameNotFound,
            StatusCode::Errno(libc::ECONNREFUSED),
            StatusCode::Errno(libc::EPIPE),
        ] {
            let error = anyhow::Error::new(Status::from(status));
            assert!(
                build_omk_error_reply_or_preserve_system(&error)
                    .expect("unavailable OMK errors should classify cleanly")
                    .is_none(),
                "{status:?} after retry means OMK is unavailable, not authoritative"
            );
        }

        for (message, unavailable) in [
            ("failed to connect to omk service", true),
            ("failed to connect to omk_maintenance service", true),
            ("failed to connect to omk_authorization service", true),
            ("failed to connect to omk service: permission denied", false),
            (
                "failed to connect to omk_maintenance service: permission denied",
                false,
            ),
            (
                "failed to connect to omk_authorization service: permission denied",
                false,
            ),
        ] {
            assert_eq!(
                omk_unavailable_error(&anyhow::anyhow!(message)),
                unavailable,
                "OMK connection marker classification mismatch for {message}"
            );
        }

        for marker in [
            "failed to connect to omk service",
            "failed to connect to omk_maintenance service",
            "failed to connect to omk_authorization service",
        ] {
            let error = anyhow::Error::new(StatusCode::PermissionDenied).context(marker);
            assert!(
                !omk_unavailable_error(&error),
                "typed permission error must override connection marker {marker}"
            );
        }
        let stale = anyhow::Error::new(StatusCode::DeadObject)
            .context("failed to connect to omk_authorization service");
        assert!(omk_unavailable_error(&stale));

        let missing_service = anyhow::Error::new(StatusCode::NameNotFound);
        assert!(
            build_omk_error_reply_or_preserve_system(&missing_service)
                .expect("missing RPC service should classify cleanly")
                .is_none(),
            "missing OMK RPC service means OMK is unavailable"
        );

        for status in [
            StatusCode::DeadObject,
            StatusCode::RpcError,
            StatusCode::NotEnoughData,
        ] {
            for error in [
                anyhow::Error::new(Status::from(status)).context("wrapped status"),
                anyhow::Error::new(status).context("wrapped status code"),
            ] {
                assert!(build_omk_error_reply_or_preserve_system(&error)
                    .expect("wrapped unavailable errors should classify cleanly")
                    .is_none());
            }
        }

        let local = anyhow::anyhow!("plain OMK failure");
        assert!(
            build_omk_error_reply_or_preserve_system(&local)
                .expect("plain OMK errors should classify cleanly")
                .is_some(),
            "non-connection OMK errors must be returned instead of preserving system"
        );
    }

    #[test]
    fn owned_outbound_reply_clears_tf_status_code() {
        let fd = 31;
        clear_outbound_reply_buffers(fd);
        let mut tr: binder_transaction_data = unsafe { std::mem::zeroed() };
        tr.flags = crate::hook::binder::TF_STATUS_CODE;
        let reply = parcel::build_void_reply().expect("void reply should build");
        let data_size = reply.data_size();
        let offsets_size = reply.offsets_size();
        let data = reply.data_ptr() as libc::c_ulong;
        let offsets = reply.offsets.as_ptr() as libc::c_ulong;

        unsafe { install_outbound_reply(fd, &mut tr, reply) };

        assert_eq!(tr.flags & crate::hook::binder::TF_STATUS_CODE, 0);
        assert_eq!(tr.data_size, data_size);
        assert_eq!(tr.offsets_size, offsets_size);
        assert_eq!(unsafe { tr.data.ptr.buffer }, data);
        assert_eq!(
            unsafe { tr.data.ptr.offsets },
            if offsets_size == 0 { 0 } else { offsets }
        );
        clear_outbound_reply_buffers(fd);
    }

    #[test]
    fn outbound_reply_cleanup_is_isolated_by_binder_fd() {
        let first_fd = 32;
        let second_fd = 33;
        clear_outbound_reply_buffers(first_fd);
        clear_outbound_reply_buffers(second_fd);

        let mut first_tr: binder_transaction_data = unsafe { std::mem::zeroed() };
        unsafe {
            install_outbound_reply(
                first_fd,
                &mut first_tr,
                parcel::build_void_reply().expect("first reply should build"),
            )
        };

        let target = LocalBinderTarget {
            ptr: 0x1234,
            cookie: 0x5678,
        };
        let retirement = NativeBinderRetirement {
            target,
            generation: 1,
        };
        let mut second_reply = parcel::build_void_reply().expect("second reply should build");
        second_reply.native_operation = Some(retirement);
        let mut second_tr: binder_transaction_data = unsafe { std::mem::zeroed() };
        unsafe { install_outbound_reply(second_fd, &mut second_tr, second_reply) };
        let second_data = unsafe { second_tr.data.ptr.buffer as usize };

        clear_outbound_reply_buffers(first_fd);
        assert_eq!(
            commit_bc_reply(second_fd, None, second_data),
            Some(retirement)
        );
        clear_outbound_reply_buffers(second_fd);
    }

    #[test]
    fn omk_grant_blocker_preserves_inbound_binder_buffer() {
        let mut tr: binder_transaction_data = unsafe { std::mem::zeroed() };
        tr.code = service_tx::r#grant;
        tr.data_size = 64;
        tr.offsets_size = size_of::<usize>();
        tr.data.ptr.buffer = 0x1000;
        tr.data.ptr.offsets = 0x2000;

        block_system_request(&mut tr);

        assert_eq!(tr.code, u32::MAX);
        assert_eq!(tr.data_size, 64);
        assert_eq!(tr.offsets_size, size_of::<usize>());
        assert_eq!(unsafe { tr.data.ptr.buffer }, 0x1000);
        assert_eq!(unsafe { tr.data.ptr.offsets }, 0x2000);
    }

    #[test]
    fn reachable_non_stale_omk_status_code_errors_become_system_error_reply() {
        // AOSP keystore2 maps a bare transport StatusCode through
        // map_binder_status_code -> Error::BinderTransaction -> SYSTEM_ERROR,
        // surfaced as a service-specific parcel, never a raw transport status.
        for status in [
            StatusCode::TimedOut,
            StatusCode::PermissionDenied,
            StatusCode::UnknownTransaction,
        ] {
            let error = anyhow::Error::new(status);
            let mut reply = build_omk_error_reply_or_preserve_system(&error)
                .expect("reachable OMK errors should classify cleanly")
                .expect("non-stale OMK errors must replace system reply");
            let (data, data_size, offsets, offsets_size) = raw_parts(&mut reply);
            let parsed =
                unsafe { parcel::parse_reply_status(data, data_size, offsets, offsets_size) }
                    .expect("status reply should parse");
            assert_eq!(
                parsed.exception_code(),
                rsbinder::ExceptionCode::ServiceSpecific
            );
            assert_eq!(
                parsed.service_specific_error(),
                ResponseCode::SYSTEM_ERROR.0
            );
        }
    }

    #[test]
    fn grant_and_ungrant_preserve_system_only_when_omk_unavailable() {
        let _guard = route_state_test_guard();
        let request = ParsedServiceRequest::Grant {
            key: sample_key_descriptor(),
            grantee_uid: 12345,
            access_vector: 7,
        };
        let caller = CallerIdentity::new(1000, 2000);

        let result = precompute_omk_grant_service_reply_with(
            &request,
            &caller,
            |_, _, _, _| Err(anyhow::Error::new(Status::from(StatusCode::RpcError))),
            |_, _, _| panic!("grant requests must not call ungrant"),
        );

        assert!(matches!(result, OmkServicePrecompute::PreserveSystem));

        let request = ParsedServiceRequest::Ungrant {
            key: sample_key_descriptor(),
            grantee_uid: 12345,
        };
        let caller = CallerIdentity::new(1000, 2000);

        let result = precompute_omk_grant_service_reply_with(
            &request,
            &caller,
            |_, _, _, _| panic!("ungrant requests must not call grant"),
            |_, _, _| Err(anyhow::Error::new(Status::from(StatusCode::NotEnoughData))),
        );

        assert!(matches!(result, OmkServicePrecompute::PreserveSystem));
    }

    #[test]
    fn grant_precompute_returns_reachable_omk_business_error() {
        let request = ParsedServiceRequest::Grant {
            key: sample_key_descriptor(),
            grantee_uid: 12345,
            access_vector: 7,
        };
        let caller = CallerIdentity::new(1000, 2000);

        let result = precompute_omk_grant_service_reply_with(
            &request,
            &caller,
            |_, _, _, _| {
                Err(anyhow::Error::new(Status::new_service_specific_error(
                    7, None,
                )))
            },
            |_, _, _| panic!("grant requests must not call ungrant"),
        );

        let OmkServicePrecompute::Reply(PrecomputedServiceReply::Error(status)) = result else {
            panic!("reachable OMK business error should be precomputed as an authoritative status");
        };
        assert_eq!(
            status.exception_code(),
            rsbinder::ExceptionCode::ServiceSpecific
        );
        assert_eq!(status.service_specific_error(), 7);
    }

    #[test]
    fn mirror_dirty_state_is_sticky_and_scoped() {
        let _guard = MIRROR_STATE_TEST_LOCK
            .lock()
            .expect("mirror state test lock poisoned");
        reset_mirror_state_for_tests();
        let caller = CallerIdentity::new(1000, 2000);

        mark_mirror_state_dirty(
            MirrorStateKind::Authorization,
            AuthorizationMethod::AddAuthToken,
            &caller,
        );

        assert!(mirror_state_dirty(MirrorStateKind::Authorization));
        assert!(!mirror_state_dirty(MirrorStateKind::Maintenance));

        mark_mirror_state_dirty(
            MirrorStateKind::Maintenance,
            MaintenanceMethod::OnUserAdded,
            &caller,
        );

        assert!(mirror_state_dirty(MirrorStateKind::Authorization));
        assert!(mirror_state_dirty(MirrorStateKind::Maintenance));
        reset_mirror_state_for_tests();
    }

    #[test]
    fn mirror_recovery_retries_sensitive_events_in_sequence() {
        let _guard = MIRROR_STATE_TEST_LOCK
            .lock()
            .expect("mirror state test lock poisoned");
        reset_mirror_state_for_tests();
        let caller = CallerIdentity::new(1000, 2000);
        reserve_mirror_update(MirrorStateKind::Maintenance)
            .expect("onUserAdded should reserve")
            .enqueue(
                MaintenanceMethod::OnUserAdded,
                &caller,
                MirrorReplayEvent::Maintenance {
                    request: ParsedMaintenanceRequest::OnUserAdded { user_id: 10 },
                    caller: caller.clone(),
                },
            )
            .expect("onUserAdded should queue");
        reserve_mirror_update(MirrorStateKind::Maintenance)
            .expect("initUserSuperKeys should reserve")
            .enqueue(
                MaintenanceMethod::InitUserSuperKeys,
                &caller,
                MirrorReplayEvent::Maintenance {
                    request: ParsedMaintenanceRequest::InitUserSuperKeys {
                        user_id: 10,
                        password: vec![1, 2, 3],
                        allow_existing: false,
                    },
                    caller: caller.clone(),
                },
            )
            .expect("initUserSuperKeys should queue");
        reserve_mirror_update(MirrorStateKind::Authorization)
            .expect("onUserStorageLocked should reserve")
            .enqueue(
                AuthorizationMethod::OnUserStorageLocked,
                &caller,
                MirrorReplayEvent::Authorization {
                    request: ParsedAuthorizationRequest::OnUserStorageLocked { user_id: 10 },
                    caller: caller.clone(),
                },
            )
            .expect("onUserStorageLocked should queue");

        let mut executed = Vec::new();
        let mut fail_sensitive = true;
        assert!(ensure_mirror_state_recovered_with(|event| {
            let method = event.method();
            executed.push(method.clone());
            if fail_sensitive && method == "InitUserSuperKeys" {
                fail_sensitive = false;
                return Err(anyhow::Error::new(StatusCode::DeadObject));
            }
            Ok(())
        })
        .is_err());
        assert_eq!(
            executed,
            vec!["OnUserAdded".to_string(), "InitUserSuperKeys".to_string()]
        );
        {
            let mut state = MIRROR_RECOVERY_STATE
                .lock()
                .expect("mirror recovery state poisoned");
            assert_eq!(state.maintenance.pending.len(), 1);
            assert_eq!(state.authorization.pending.len(), 1);
            assert!(!state.maintenance.poisoned);
            let pending = state
                .maintenance
                .pending
                .front_mut()
                .expect("sensitive replay should remain queued");
            assert!(pending.retry_not_before.is_some());
            let Some(MirrorReplayEvent::Maintenance {
                request: ParsedMaintenanceRequest::InitUserSuperKeys { password, .. },
                ..
            }) = pending.event.as_ref()
            else {
                panic!("sensitive replay should retain its in-memory event");
            };
            assert_eq!(password.as_slice(), &[1, 2, 3]);
            pending.retry_not_before = None;
        }

        ensure_mirror_state_recovered_with(|event| {
            executed.push(event.method());
            Ok(())
        })
        .expect("in-memory mirror events should drain in global sequence order");

        assert_eq!(
            executed,
            vec![
                "OnUserAdded".to_string(),
                "InitUserSuperKeys".to_string(),
                "InitUserSuperKeys".to_string(),
                "OnUserStorageLocked".to_string(),
            ]
        );
        assert!(!mirror_state_dirty(MirrorStateKind::Authorization));
        assert!(!mirror_state_dirty(MirrorStateKind::Maintenance));
        let state = MIRROR_RECOVERY_STATE
            .lock()
            .expect("mirror recovery state poisoned");
        assert_eq!(state.pending_len(), 0);
        assert!(!state.authorization.draining);
        assert!(!state.maintenance.draining);
        drop(state);
        reset_mirror_state_for_tests();
    }

    #[test]
    fn queued_mirror_update_is_drained_outside_the_reply_path() {
        let _guard = MIRROR_STATE_TEST_LOCK
            .lock()
            .expect("mirror state test lock poisoned");
        reset_mirror_state_for_tests();
        let caller = CallerIdentity::new(1000, 2000);
        let request = ParsedAuthorizationRequest::OnDeviceLocked {
            user_id: 10,
            unlocking_sids: vec![11],
            weak_unlock_enabled: false,
        };
        let event = MirrorReplayEvent::Authorization {
            request: request.clone(),
            caller: caller.clone(),
        };
        let reservation = reserve_mirror_update(MirrorStateKind::Authorization)
            .expect("authorization update should reserve");
        reservation
            .enqueue(AuthorizationMethod::OnDeviceLocked, &caller, event)
            .expect("successful System mutation should queue without executing RPC");
        {
            let state = MIRROR_RECOVERY_STATE
                .lock()
                .expect("mirror recovery state poisoned");
            assert_eq!(state.authorization.pending.len(), 1);
            assert!(state.authorization.pending[0].event.is_some());
        }

        assert!(ensure_mirror_state_recovered().is_err());

        let mut replayed = 0;
        ensure_mirror_state_recovered_with(|_| {
            replayed += 1;
            Ok(())
        })
        .expect("queued mirror event should be replayed after the active update exits");
        assert_eq!(replayed, 1);
        let state = MIRROR_RECOVERY_STATE
            .lock()
            .expect("mirror recovery state poisoned");
        assert!(!state.authorization.draining);
        assert!(state.authorization.pending.is_empty());
        drop(state);
        reset_mirror_state_for_tests();
    }

    #[test]
    fn non_ok_reply_cancels_mirror_reservations() {
        let _guard = MIRROR_STATE_TEST_LOCK
            .lock()
            .expect("mirror state test lock poisoned");
        reset_mirror_state_for_tests();
        let caller = CallerIdentity::new(1000, 2000);
        let mut reply = parcel::build_status_reply(&Status::new_service_specific_error(
            ResponseCode::SYSTEM_ERROR.0,
            None,
        ))
        .expect("error reply should build");
        let (data, data_size, offsets, offsets_size) = raw_parts(&mut reply);
        let mut tr: binder_transaction_data = unsafe { std::mem::zeroed() };
        tr.data.ptr.buffer = data as libc::c_ulong;
        tr.data.ptr.offsets = offsets as libc::c_ulong;
        tr.data_size = data_size;
        tr.offsets_size = offsets_size;

        let mut authorization = PendingAuthorizationCall {
            request: ParsedAuthorizationRequest::AddAuthToken {
                auth_token: Default::default(),
            },
            method: AuthorizationMethod::AddAuthToken,
            caller: caller.clone(),
            mirror_update: Some(
                reserve_mirror_update(MirrorStateKind::Authorization)
                    .expect("authorization update should reserve"),
            ),
        };
        assert!(
            unsafe { build_authorization_reply_mirror(&tr, &mut authorization) }
                .expect("authorization error reply should parse")
                .is_none()
        );
        assert!(authorization.mirror_update.is_none());
        assert!(ensure_mirror_state_recovered_with(|_| unreachable!()).is_ok());

        let mut maintenance = PendingMaintenanceCall {
            request: ParsedMaintenanceRequest::OnUserAdded { user_id: 10 },
            caller,
            route: RouteTarget::System,
            mirror_update: Some(
                reserve_mirror_update(MirrorStateKind::Maintenance)
                    .expect("maintenance update should reserve"),
            ),
        };
        assert!(
            unsafe { build_maintenance_reply_mirror(&tr, &mut maintenance) }
                .expect("maintenance error reply should parse")
                .is_none()
        );
        assert!(maintenance.mirror_update.is_none());
        assert!(ensure_mirror_state_recovered_with(|_| unreachable!()).is_ok());
        reset_mirror_state_for_tests();
    }

    #[test]
    fn mirror_recovery_waits_for_earlier_cross_lane_system_reply() {
        let _guard = MIRROR_STATE_TEST_LOCK
            .lock()
            .expect("mirror state test lock poisoned");
        reset_mirror_state_for_tests();

        assert!(authorization_requires_mirror(
            &ParsedAuthorizationRequest::OnUserStorageLocked { user_id: 10 }
        ));
        assert!(maintenance_requires_mirror(
            &ParsedMaintenanceRequest::OnUserRemoved { user_id: 11 }
        ));
        let authorization = reserve_mirror_update(MirrorStateKind::Authorization)
            .expect("authorization update should reserve");
        let maintenance = reserve_mirror_update(MirrorStateKind::Maintenance)
            .expect("maintenance update should reserve");
        let caller = CallerIdentity::new(1000, 2000);
        maintenance
            .enqueue(
                MaintenanceMethod::OnUserRemoved,
                &caller,
                MirrorReplayEvent::Maintenance {
                    request: ParsedMaintenanceRequest::OnUserRemoved { user_id: 11 },
                    caller: caller.clone(),
                },
            )
            .expect("later System reply should publish without waiting for the first");

        let mut replayed = 0;
        assert!(ensure_mirror_state_recovered_with(|_| {
            replayed += 1;
            Ok(())
        })
        .is_err());
        assert_eq!(
            replayed, 0,
            "an unfinished earlier authorization slot must block later maintenance replay"
        );

        authorization.cancel();
        ensure_mirror_state_recovered_with(|_| {
            replayed += 1;
            Ok(())
        })
        .expect("canceling the unfinished System request should unblock maintenance");
        assert_eq!(replayed, 1);
        reset_mirror_state_for_tests();
    }

    #[test]
    fn mirror_retry_deadline_cannot_be_bypassed_by_an_early_wake() {
        let _guard = MIRROR_STATE_TEST_LOCK
            .lock()
            .expect("mirror state test lock poisoned");
        reset_mirror_state_for_tests();
        let caller = CallerIdentity::new(1000, 2000);
        reserve_mirror_update(MirrorStateKind::Authorization)
            .expect("authorization update should reserve")
            .enqueue(
                AuthorizationMethod::OnDeviceLocked,
                &caller,
                MirrorReplayEvent::Authorization {
                    request: ParsedAuthorizationRequest::OnDeviceLocked {
                        user_id: 10,
                        unlocking_sids: vec![11],
                        weak_unlock_enabled: false,
                    },
                    caller: caller.clone(),
                },
            )
            .expect("System reply should publish the replay");

        let mut attempts = 0;
        assert!(ensure_mirror_state_recovered_with(|_| {
            attempts += 1;
            Err(anyhow::Error::new(StatusCode::DeadObject))
        })
        .is_err());
        assert_eq!(attempts, 1);

        reserve_mirror_update(MirrorStateKind::Maintenance)
            .expect("maintenance update should reserve")
            .enqueue(
                MaintenanceMethod::OnUserRemoved,
                &caller,
                MirrorReplayEvent::Maintenance {
                    request: ParsedMaintenanceRequest::OnUserRemoved { user_id: 11 },
                    caller: caller.clone(),
                },
            )
            .expect("other lane should accept a replay while authorization is delayed");
        let mut replayed = Vec::new();
        assert!(ensure_mirror_state_recovered_with(|event| {
            replayed.push(event.method());
            Ok(())
        })
        .is_err());
        assert_eq!(attempts, 1, "retry must wait for its monotonic deadline");
        assert!(
            replayed.is_empty(),
            "an early wake must not replay a later cross-lane sequence"
        );
        reset_mirror_state_for_tests();
    }

    #[test]
    fn mirror_recovery_never_queues_password_events() {
        let _guard = MIRROR_STATE_TEST_LOCK
            .lock()
            .expect("mirror state test lock poisoned");
        reset_mirror_state_for_tests();
        let call = PendingAuthorizationCall {
            request: ParsedAuthorizationRequest::OnDeviceUnlocked {
                user_id: 10,
                password: Some(vec![1, 2, 3]),
            },
            method: AuthorizationMethod::OnDeviceUnlocked,
            caller: CallerIdentity::new(1000, 2000),
            mirror_update: None,
        };
        assert!(replayable_authorization_event(&call).is_none());
        record_mirror_failure(
            MirrorStateKind::Authorization,
            call.method,
            &call.caller,
            replayable_authorization_event(&call),
        )
        .expect("non-replayable event should persist a poison marker");

        let state = MIRROR_RECOVERY_STATE
            .lock()
            .expect("mirror recovery state poisoned");
        assert!(state.authorization.pending.is_empty());
        assert!(state.authorization.poisoned);
        drop(state);
        assert!(ensure_mirror_state_recovered_with(|_| {
            panic!("poisoned recovery must not replay an event")
        })
        .is_err());
        reset_mirror_state_for_tests();
    }

    #[test]
    fn mirror_recovery_ledger_is_crash_safe_without_persisting_secrets() {
        let _guard = MIRROR_STATE_TEST_LOCK
            .lock()
            .expect("mirror state test lock poisoned");
        reset_mirror_state_for_tests();
        let path = std::env::temp_dir().join(format!(
            "omk-injector-mirror-recovery-{}.toml",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(crate::utils::private_temp_path(&path));
        init_mirror_recovery_at(&path).expect("empty mirror ledger should initialize");
        let boot_id = current_mirror_boot_id().expect("kernel boot ID should be available");

        let caller = CallerIdentity::new(1000, 2000);
        reserve_mirror_update(MirrorStateKind::Authorization)
            .expect("authorization update should reserve durably")
            .enqueue(
                AuthorizationMethod::OnDeviceLocked,
                &caller,
                MirrorReplayEvent::Authorization {
                    request: ParsedAuthorizationRequest::OnDeviceLocked {
                        user_id: 10,
                        unlocking_sids: vec![11],
                        weak_unlock_enabled: false,
                    },
                    caller: caller.clone(),
                },
            )
            .expect("safe mirror event should persist");
        for (method, request) in [
            (
                MaintenanceMethod::OnUserRemoved,
                ParsedMaintenanceRequest::OnUserRemoved { user_id: 10 },
            ),
            (
                MaintenanceMethod::EarlyBootEnded,
                ParsedMaintenanceRequest::EarlyBootEnded,
            ),
        ] {
            reserve_mirror_update(MirrorStateKind::Maintenance)
                .expect("maintenance update should reserve durably")
                .enqueue(
                    method,
                    &caller,
                    MirrorReplayEvent::Maintenance {
                        request,
                        caller: caller.clone(),
                    },
                )
                .expect("safe maintenance event should persist");
        }
        let loaded =
            load_mirror_recovery_state(&path, &boot_id).expect("safe replay should reload");
        assert_eq!(loaded.authorization.pending.len(), 1);
        assert_eq!(
            loaded.authorization.pending[0]
                .event
                .as_ref()
                .expect("safe replay should retain its event")
                .method(),
            "OnDeviceLocked"
        );
        let previous_boot = load_mirror_recovery_state(&path, "previous-boot")
            .expect("previous-boot replay should load");
        assert!(previous_boot.authorization.pending.is_empty());
        assert_eq!(previous_boot.maintenance.pending.len(), 1);
        assert_eq!(
            previous_boot.maintenance.pending[0]
                .event
                .as_ref()
                .expect("durable maintenance replay should remain")
                .method(),
            "OnUserRemoved"
        );
        ensure_mirror_state_recovered_with(|_| Ok(()))
            .expect("safe replay should commit its durable removal");

        let awaiting = reserve_mirror_update(MirrorStateKind::Maintenance)
            .expect("maintenance update should reserve durably");
        std::mem::forget(awaiting);
        assert!(
            load_mirror_recovery_state(&path, &boot_id)
                .expect("awaiting reservation should reload")
                .maintenance
                .poisoned,
            "a crash before the System reply must remain fail-closed"
        );
        assert!(
            load_mirror_recovery_state(&path, "previous-boot")
                .expect("previous-boot maintenance reservation should reload")
                .maintenance
                .poisoned,
            "durable maintenance uncertainty must remain fail-closed across boots"
        );

        reset_mirror_state_for_tests();
        std::fs::remove_file(&path).expect("mirror ledger should be removable");
        init_mirror_recovery_at(&path).expect("fresh mirror ledger should initialize");
        reserve_mirror_update(MirrorStateKind::Authorization)
            .expect("sensitive update should reserve durably")
            .enqueue(
                AuthorizationMethod::OnDeviceUnlocked,
                &caller,
                MirrorReplayEvent::Authorization {
                    request: ParsedAuthorizationRequest::OnDeviceUnlocked {
                        user_id: 10,
                        password: Some(vec![91, 92, 93]),
                    },
                    caller: caller.clone(),
                },
            )
            .expect("sensitive update should persist only a poison marker");
        let contents = std::fs::read_to_string(&path).expect("mirror ledger should be readable");
        assert!(!contents.contains("password"));
        assert!(
            load_mirror_recovery_state(&path, &boot_id)
                .expect("sensitive marker should reload")
                .authorization
                .poisoned,
            "a sensitive in-memory replay must become poison after restart"
        );
        let awaiting_authorization = reserve_mirror_update(MirrorStateKind::Authorization)
            .expect("authorization update should reserve durably");
        std::mem::forget(awaiting_authorization);
        let previous_boot = load_mirror_recovery_state(&path, "previous-boot")
            .expect("previous-boot authorization state should reload");
        assert!(previous_boot.authorization.pending.is_empty());
        assert!(
            !previous_boot.authorization.poisoned,
            "boot-scoped authorization poison and uncertainty must clear on a new boot"
        );

        reset_mirror_state_for_tests();
        std::fs::remove_file(&path).expect("mirror-ledger test file should be removed");
        let _ = std::fs::remove_file(crate::utils::private_temp_path(&path));
    }

    #[test]
    fn publication_deadline_wait_does_not_need_an_ioctl_wakeup() {
        let _guard = route_state_test_guard();
        clear_operation_state_for_tests();
        let now = Instant::now();
        queue_operation_publication_probe(OperationPublicationProbe {
            target: allocate_test_target(),
            binder: binder_token(91),
            generation: 1,
            not_before: now + Duration::from_millis(50),
            query_failures: 0,
        });

        std::thread::spawn(crate::hook::intercept::wait_for_operation_publication_deadline)
            .join()
            .expect("deadline wait thread should not panic");
        assert!(Instant::now().duration_since(now) >= Duration::from_millis(40));
        assert!(take_operation_publication_probe(Instant::now()).is_none());
        clear_operation_state_for_tests();
    }

    #[test]
    fn reachable_omk_status_error_becomes_authoritative_reply() {
        let status = Status::new_service_specific_error(ResponseCode::PERMISSION_DENIED.0, None);
        let reply = build_omk_status_reply_or_preserve_system(&status)
            .expect("service-specific status should build")
            .expect("reachable OMK status should replace system");
        let mut reply = reply;
        let (data, data_size, offsets, offsets_size) = raw_parts(&mut reply);
        let parsed = unsafe { parcel::parse_reply_status(data, data_size, offsets, offsets_size) }
            .expect("status reply should parse");

        assert_eq!(
            parsed.exception_code(),
            rsbinder::ExceptionCode::ServiceSpecific
        );
        assert_eq!(
            parsed.service_specific_error(),
            ResponseCode::PERMISSION_DENIED.0
        );
    }

    #[test]
    fn service_route_respects_intercept_configuration() {
        let _guard = route_state_test_guard();
        let intercept = config::InterceptConfig::default();

        for request in sample_service_requests() {
            assert_eq!(
                route_for_service_request(&request, &intercept),
                RouteTarget::Omk,
                "{:?} should use the OMK backend when the method is enabled",
                request.method()
            );
        }

        let intercept = disabled_intercept_config();

        for request in sample_service_requests() {
            assert_eq!(
                route_for_service_request(&request, &intercept),
                RouteTarget::System,
                "{:?} should use the system backend when the method is disabled",
                request.method()
            );
        }

        let mut intercept = disabled_intercept_config();
        intercept.get_key_entry = true;
        assert!(security_level_scoop_enabled(&intercept));
        intercept.get_key_entry = false;
        intercept.get_security_level = true;
        assert!(security_level_scoop_enabled(&intercept));
        intercept.get_security_level = false;
        assert!(!security_level_scoop_enabled(&intercept));
    }

    #[test]
    fn service_route_uses_omk_for_key_id() {
        let key = KeyDescriptor {
            domain: Domain::KEY_ID,
            nspace: 123,
            alias: None,
            blob: None,
        };

        assert_eq!(
            route_for_service_request(
                &ParsedServiceRequest::GetKeyEntry { key },
                &config::InterceptConfig::default(),
            ),
            RouteTarget::Omk
        );
    }

    #[test]
    fn unconfirmed_grant_readback_stays_rejected() {
        let _guard = route_state_test_guard();
        let decision = crate::filter::FilterDecision {
            allowed: false,
            reason: crate::filter::FilterReason::RejectedNotInScope,
            packages: vec!["com.grantee".to_string()],
        };
        let caller = CallerIdentity::new(10002, 2000);
        let grant = KeyDescriptor {
            domain: Domain::GRANT,
            nspace: 123,
            alias: None,
            blob: None,
        };

        assert!(!should_allow_omk_grant_service_request_with_probe(
            &ParsedServiceRequest::GetKeyEntry { key: grant },
            &decision,
            &caller,
            |_, _| Ok(false),
        )
        .expect("unconfirmed grant lookup should succeed"));
        assert!(!should_allow_omk_grant_service_request_with_probe(
            &ParsedServiceRequest::GetKeyEntry {
                key: sample_key_descriptor(),
            },
            &decision,
            &caller,
            |_, _| -> anyhow::Result<bool> { panic!("non-GRANT descriptors must not be probed") },
        )
        .expect("non-GRANT lookup should succeed"));
    }

    #[test]
    fn soft_rejected_grant_probe_error_is_fail_closed() {
        let _guard = route_state_test_guard();
        let grant = KeyDescriptor {
            domain: Domain::GRANT,
            nspace: 123,
            alias: None,
            blob: None,
        };
        let decision = crate::filter::FilterDecision {
            allowed: false,
            reason: crate::filter::FilterReason::RejectedUnknownPackage,
            packages: vec![],
        };
        let caller = CallerIdentity::new(10002, 2000);

        let error = should_allow_omk_grant_service_request_with_probe(
            &ParsedServiceRequest::GetKeyEntry { key: grant },
            &decision,
            &caller,
            |_, _| Err(anyhow::anyhow!("reachable probe failure")),
        )
        .expect_err("non-unavailable probe errors must block System fallback");

        assert!(error.to_string().contains("reachable probe failure"));
    }

    #[test]
    fn soft_rejected_grant_positive_probe_works_for_service_and_security_level() {
        let _guard = route_state_test_guard();
        let grant = KeyDescriptor {
            domain: Domain::GRANT,
            nspace: 123,
            alias: None,
            blob: None,
        };
        let decision = crate::filter::FilterDecision {
            allowed: false,
            reason: crate::filter::FilterReason::RejectedUnknownPackage,
            packages: vec![],
        };
        let caller = CallerIdentity::new(10002, 2000);

        assert!(should_allow_omk_grant_service_request_with_probe(
            &ParsedServiceRequest::GetKeyEntry { key: grant.clone() },
            &decision,
            &caller,
            |probe_caller, probe_grant| {
                assert_eq!(probe_caller.uid, caller.uid);
                assert_eq!(probe_grant, &grant);
                Ok(true)
            },
        )
        .expect("positive grant probe should succeed"));

        let grant = KeyDescriptor {
            domain: Domain::GRANT,
            nspace: 321,
            alias: None,
            blob: None,
        };
        let decision = crate::filter::FilterDecision {
            allowed: false,
            reason: crate::filter::FilterReason::RejectedNotInScope,
            packages: vec!["com.grantee".to_string()],
        };

        assert!(should_allow_omk_grant_security_level_request_with_probe(
            &ParsedSecurityLevelRequest::CreateOperation {
                key: grant.clone(),
                operation_parameters: vec![],
                forced: false,
            },
            &decision,
            &caller,
            |probe_caller, probe_grant| {
                assert_eq!(probe_caller.uid, caller.uid);
                assert_eq!(probe_grant, &grant);
                Ok(true)
            },
        )
        .expect("positive grant probe should succeed"));
    }

    #[test]
    fn denylisted_grant_readback_does_not_probe_omk() {
        let _guard = route_state_test_guard();
        let grant = KeyDescriptor {
            domain: Domain::GRANT,
            nspace: 123,
            alias: None,
            blob: None,
        };
        let decision = crate::filter::FilterDecision {
            allowed: false,
            reason: crate::filter::FilterReason::RejectedByDenylist,
            packages: vec!["com.blocked".to_string()],
        };
        let caller = CallerIdentity::new(10002, 2000);

        assert!(!should_allow_omk_grant_service_request_with_probe(
            &ParsedServiceRequest::GetKeyEntry { key: grant },
            &decision,
            &caller,
            |_, _| -> anyhow::Result<bool> {
                panic!("denylisted callers must not probe OMK grants")
            },
        )
        .expect("denylisted grant lookup should succeed"));
    }

    #[test]
    fn pending_reply_queue_consumes_nested_requests_from_the_top() {
        let _guard = route_state_test_guard();
        let fd = 38;
        PENDING_REPLY_QUEUE.with(|slot| slot.borrow_mut().clear());
        assert!(take_top_pending(fd).is_none());

        push_pending_frame(fd);
        assert!(matches!(take_top_pending(fd), Some(None)));

        push_pending_frame(fd);
        replace_top_pending(
            fd,
            PendingCall::Service(PendingServiceCall {
                request: ParsedServiceRequest::GetSecurityLevel {
                    security_level: SecurityLevel::TRUSTED_ENVIRONMENT,
                },
                caller: CallerIdentity::new(1000, 2000),
                packages: vec!["com.example".to_string()],
                route: RouteTarget::Omk,
            }),
        );

        push_pending_frame(fd);
        replace_top_pending(
            fd,
            PendingCall::Service(PendingServiceCall {
                request: ParsedServiceRequest::GetKeyEntry {
                    key: sample_key_descriptor(),
                },
                caller: CallerIdentity::new(1000, 2000),
                packages: vec!["com.example".to_string()],
                route: RouteTarget::Omk,
            }),
        );

        let Some(Some(PendingCall::Service(first))) = take_top_pending(fd) else {
            panic!("top call should be a service request");
        };
        assert_eq!(first.request.method(), ServiceMethod::GetKeyEntry);

        let Some(Some(PendingCall::Service(second))) = take_top_pending(fd) else {
            panic!("outer call should be a service request");
        };
        assert_eq!(second.request.method(), ServiceMethod::GetSecurityLevel);
        assert!(take_top_pending(fd).is_none());

        let legacy = PendingCall::Authorization(PendingAuthorizationCall {
            request: ParsedAuthorizationRequest::OnDeviceUnlocked {
                user_id: 10,
                password: None,
            },
            method: AuthorizationMethod::LegacyOnLockScreenEvent,
            caller: CallerIdentity::new(1000, 2000),
            mirror_update: None,
        });
        assert_eq!(legacy.reply_log_context().1, "LegacyOnLockScreenEvent");
    }

    #[test]
    fn pending_reply_queue_claims_only_the_matching_binder_fd() {
        let _guard = route_state_test_guard();
        let first_fd = 39;
        let second_fd = 40;
        PENDING_REPLY_QUEUE.with(|slot| slot.borrow_mut().clear());
        push_pending_frame(first_fd);
        push_pending_frame(second_fd);

        let mut tr: binder_transaction_data = unsafe { std::mem::zeroed() };
        let first_frame = unsafe { handle_bc_reply(first_fd, &mut tr) };
        assert!(first_frame.is_some());
        assert_eq!(pending_reply_frame_claims_for_test(first_fd), vec![true]);
        assert_eq!(pending_reply_frame_claims_for_test(second_fd), vec![false]);

        abort_bc_reply(first_fd, first_frame, 0);
        let second_frame = unsafe { handle_bc_reply(second_fd, &mut tr) };
        assert!(second_frame.is_some());
        abort_bc_reply(second_fd, second_frame, 0);
        assert!(PENDING_REPLY_QUEUE.with(|slot| slot.borrow().is_empty()));
    }

    #[test]
    fn native_reply_sentinel_preserves_the_outer_pending_frame() {
        let _guard = route_state_test_guard();
        let fd = 41;
        PENDING_REPLY_QUEUE.with(|slot| slot.borrow_mut().clear());
        push_pending_frame(fd);
        push_pending_frame(fd);

        let mut tr: binder_transaction_data = unsafe { std::mem::zeroed() };
        let native_frame = unsafe { handle_bc_reply(fd, &mut tr) };
        assert!(native_frame.is_some());
        assert_eq!(pending_reply_frame_claims_for_test(fd), vec![false, true]);

        abort_bc_reply(fd, native_frame, 0);
        assert_eq!(pending_reply_frame_claims_for_test(fd), vec![false]);
        PENDING_REPLY_QUEUE.with(|slot| slot.borrow_mut().clear());
    }

    #[test]
    fn rewrite_failures_preserve_system_for_maintenance_and_non_omk_routes() {
        let _guard = route_state_test_guard();
        clear_operation_state_for_tests();

        let caller = CallerIdentity::new(1000, 2000);
        let service_omk = PendingCall::Service(PendingServiceCall {
            request: ParsedServiceRequest::GetKeyEntry {
                key: sample_key_descriptor(),
            },
            caller: caller.clone(),
            packages: vec!["com.example".to_string()],
            route: RouteTarget::Omk,
        });
        let service_system = PendingCall::Service(PendingServiceCall {
            request: ParsedServiceRequest::GetKeyEntry {
                key: sample_key_descriptor(),
            },
            caller: caller.clone(),
            packages: vec!["com.example".to_string()],
            route: RouteTarget::System,
        });
        assert!(!pending_preserves_system_on_rewrite_failure(&service_omk));
        assert!(pending_preserves_system_on_rewrite_failure(&service_system));

        let security_omk = PendingCall::SecurityLevel(PendingSecurityLevelCall {
            request: ParsedSecurityLevelRequest::CreateOperation {
                key: sample_key_descriptor(),
                operation_parameters: vec![],
                forced: false,
            },
            caller: caller.clone(),
            packages: vec!["com.example".to_string()],
            route: RouteTarget::Omk,
            security_level: SecurityLevel::TRUSTED_ENVIRONMENT,
        });
        let security_system = PendingCall::SecurityLevel(PendingSecurityLevelCall {
            request: ParsedSecurityLevelRequest::CreateOperation {
                key: sample_key_descriptor(),
                operation_parameters: vec![],
                forced: false,
            },
            caller: caller.clone(),
            packages: vec!["com.example".to_string()],
            route: RouteTarget::System,
            security_level: SecurityLevel::TRUSTED_ENVIRONMENT,
        });
        assert!(!pending_preserves_system_on_rewrite_failure(&security_omk));
        assert!(pending_preserves_system_on_rewrite_failure(
            &security_system
        ));

        let omk_target = LocalBinderTarget {
            ptr: 0x1111,
            cookie: 0x2222,
        };
        remember_operation_target(
            omk_target,
            OperationTargetInfo {
                route: RouteTarget::Omk,
                aad_allowed: true,
                backend: None,
                finalized: false,
            },
        );
        let system_target = LocalBinderTarget {
            ptr: 0x3333,
            cookie: 0x4444,
        };
        remember_operation_target(
            system_target,
            OperationTargetInfo {
                route: RouteTarget::System,
                aad_allowed: true,
                backend: None,
                finalized: false,
            },
        );
        let operation_omk = PendingCall::Operation(PendingOperationCall {
            request: ParsedOperationRequest::Update { input: vec![1] },
            caller: caller.clone(),
            target: omk_target,
        });
        let operation_system = PendingCall::Operation(PendingOperationCall {
            request: ParsedOperationRequest::Update { input: vec![1] },
            caller: caller.clone(),
            target: system_target,
        });
        assert!(!pending_preserves_system_on_rewrite_failure(&operation_omk));
        assert!(pending_preserves_system_on_rewrite_failure(
            &operation_system
        ));

        let authorization = PendingCall::Authorization(PendingAuthorizationCall {
            request: ParsedAuthorizationRequest::AddAuthToken {
                auth_token: Default::default(),
            },
            method: AuthorizationMethod::AddAuthToken,
            caller: caller.clone(),
            mirror_update: None,
        });
        let maintenance_request = ParsedMaintenanceRequest::MigrateKeyNamespace {
            source: sample_key_descriptor(),
            destination: sample_key_descriptor(),
        };
        assert!(pending_preserves_system_on_rewrite_failure(&authorization));
        let ordinary_maintenance = PendingCall::Maintenance(PendingMaintenanceCall {
            request: ParsedMaintenanceRequest::OnUserAdded { user_id: 10 },
            caller: caller.clone(),
            route: RouteTarget::System,
            mirror_update: None,
        });
        assert!(pending_preserves_system_on_rewrite_failure(
            &ordinary_maintenance
        ));

        let maintenance_system = PendingCall::Maintenance(PendingMaintenanceCall {
            request: maintenance_request.clone(),
            caller: caller.clone(),
            route: RouteTarget::System,
            mirror_update: None,
        });
        assert!(pending_preserves_system_on_rewrite_failure(
            &maintenance_system
        ));

        let maintenance_omk = PendingCall::Maintenance(PendingMaintenanceCall {
            request: maintenance_request,
            caller,
            route: RouteTarget::Omk,
            mirror_update: None,
        });
        assert!(!pending_preserves_system_on_rewrite_failure(
            &maintenance_omk
        ));
    }

    #[test]
    fn synthetic_transaction_caller_uses_registered_sid_when_secctx_is_absent() {
        let fallback = CallerIdentity::new(10002, 2000).with_sid("u:r:untrusted_app:s0:c123,c456");
        let mut tr: binder_transaction_data = unsafe { std::mem::zeroed() };
        tr.sender_euid = 10002;
        tr.sender_pid = 3456;

        let caller = synthetic_transaction_caller(Some(&fallback), &tr, None);
        assert_eq!(caller.uid, 10002);
        assert_eq!(caller.pid, 3456);
        assert_eq!(caller.sid, fallback.sid);

        let caller = synthetic_transaction_caller(
            Some(&fallback),
            &tr,
            Some("u:r:platform_app:s0:c1,c2".to_string()),
        );
        assert_eq!(caller.sid, "u:r:platform_app:s0:c1,c2");

        tr.sender_euid = -1;
        tr.sender_pid = 0;
        let caller = synthetic_transaction_caller(Some(&fallback), &tr, None);
        assert_eq!(
            (caller.uid, caller.pid, caller.sid),
            (10002, 2000, fallback.sid)
        );

        tr.sender_pid = -1;
        let caller = synthetic_transaction_caller(None, &tr, None);
        assert_eq!((caller.uid, caller.pid, caller.sid.as_str()), (0, -1, ""));
    }

    #[test]
    fn no_carrier_omk_key_entry_reply_uses_synthetic_security_level_mapping() {
        ensure_binder_process_state();
        let _guard = route_state_test_guard();
        clear_synthetic_targets_for_tests();

        let security_level = SecurityLevel::TRUSTED_ENVIRONMENT;
        let metadata = KeyMetadata {
            key: KeyDescriptor {
                domain: Domain::KEY_ID,
                nspace: 0x1234,
                alias: None,
                blob: None,
            },
            keySecurityLevel: security_level,
            authorizations: vec![],
            certificate: None,
            certificateChain: None,
            modificationTimeMs: 0,
        };
        let caller = CallerIdentity::new(10002, 2000).with_sid("u:r:untrusted_app:s0:c123,c456");

        let mut reply = build_no_carrier_omk_key_entry_reply(
            KeyEntryResponse {
                r#iSecurityLevel: Some(fake_system_security_level_backend()),
                metadata,
            },
            &caller,
        )
        .expect("no-carrier OMK key-entry reply should serialize");
        let (reply_data, reply_data_size, reply_offsets, reply_offsets_size) =
            raw_parts(&mut reply);
        let (carrier, parsed_metadata) = unsafe {
            parcel::parse_key_entry_reply(
                reply_data,
                reply_data_size,
                reply_offsets,
                reply_offsets_size,
            )
        }
        .expect("rewritten key-entry metadata should parse");
        assert_eq!(parsed_metadata.r#key.nspace, 0x1234);
        assert_eq!(parsed_metadata.r#keySecurityLevel, security_level);

        let target = unsafe { parse_local_binder_target_from_parcel_bytes(&carrier.bytes) }
            .expect("synthetic security-level carrier should expose a native target");
        assert!(lookup_native_binder(target).is_some());
        assert_eq!(
            lookup_synthetic_target(target),
            Some(SyntheticTargetKind::SecurityLevel)
        );
        let synthetic_info =
            lookup_synthetic_target_info(target).expect("synthetic target should be tracked");
        assert_eq!(synthetic_info.kind, SyntheticTargetKind::SecurityLevel);
        assert!(synthetic_info.caller.is_none());
        let target_info = tracker::lookup_security_level_target(target)
            .expect("synthetic security-level target should be tracked");
        assert_eq!(target_info.security_level, security_level);
    }

    #[test]
    fn no_carrier_create_operation_reply_uses_synthetic_operation_mapping() {
        ensure_binder_process_state();
        let _guard = route_state_test_guard();
        clear_operation_state_for_tests();

        let omk_aborts = Arc::new(AtomicUsize::new(0));
        let caller = CallerIdentity::new(10002, 2000).with_sid("u:r:untrusted_app:s0:c123,c456");

        let omk_backend = BnKeystoreOperation::new_binder(TestOperationBackend {
            update_output: vec![9, 9, 9],
            aborts: omk_aborts.clone(),
            update_aad_status: None,
        });
        let mut rewritten = build_no_carrier_create_operation_reply(
            CreateOperationResponse {
                r#iOperation: Some(omk_backend),
                r#operationChallenge: None,
                r#parameters: None,
                r#upgradedBlob: Some(vec![7, 7]),
            },
            true,
            &caller,
            true,
        )
        .expect("no-carrier createOperation reply should serialize");

        let (rewritten_data, rewritten_data_size, rewritten_offsets, rewritten_offsets_size) =
            raw_parts(&mut rewritten);
        let carrier = unsafe {
            parcel::extract_create_operation_reply_carrier(
                rewritten_data,
                rewritten_data_size,
                rewritten_offsets,
                rewritten_offsets_size,
            )
        }
        .expect("synthetic operation carrier should parse");
        let target = unsafe { parse_local_binder_target_from_parcel_bytes(&carrier.bytes) }
            .expect("synthetic operation carrier should expose a local target");
        assert!(lookup_native_binder(target).is_some());
        assert_eq!(
            lookup_synthetic_target(target),
            Some(SyntheticTargetKind::Operation)
        );
        let target_info =
            lookup_operation_target(target).expect("synthetic operation target should be tracked");
        assert_eq!(target_info.route, RouteTarget::Omk);
        assert!(target_info.aad_allowed);
        assert!(target_info.backend.is_some());
        let synthetic_info =
            lookup_synthetic_target_info(target).expect("synthetic target should be tracked");
        let synthetic_caller = synthetic_info
            .caller
            .as_ref()
            .expect("synthetic operation target should keep caller fallback");
        assert_eq!(synthetic_caller.sid, caller.sid);
        assert_eq!(synthetic_caller.uid, caller.uid);
        assert!(synthetic_info.native_generation.is_some());

        let update_reply = build_operation_reply_rewrite(&PendingOperationCall {
            request: ParsedOperationRequest::Update {
                input: vec![4, 5, 6],
            },
            caller: CallerIdentity::new(1000, 2000),
            target,
        })
        .expect("synthetic update rewrite should succeed")
        .expect("synthetic update should return an OMK-owned reply");
        let mut update_reply = update_reply;
        let (update_data, update_data_size, update_offsets, update_offsets_size) =
            raw_parts(&mut update_reply);
        let update_output: Option<Vec<u8>> = unsafe {
            parcel::parse_success_reply(
                update_data,
                update_data_size,
                update_offsets,
                update_offsets_size,
            )
        }
        .expect("synthetic update reply should deserialize");
        assert_eq!(update_output.as_deref(), Some(&[9, 9, 9][..]));

        let abort_reply = build_operation_reply_rewrite(&PendingOperationCall {
            request: ParsedOperationRequest::Abort,
            caller: CallerIdentity::new(1000, 2000),
            target,
        })
        .expect("synthetic abort rewrite should succeed")
        .expect("synthetic abort should return an OMK-owned reply");
        let mut abort_reply = abort_reply;
        let (abort_data, abort_data_size, abort_offsets, abort_offsets_size) =
            raw_parts(&mut abort_reply);
        let abort_status = unsafe {
            parcel::parse_reply_status(
                abort_data,
                abort_data_size,
                abort_offsets,
                abort_offsets_size,
            )
        }
        .expect("synthetic abort reply should deserialize");
        assert!(abort_status.is_ok());
        assert_eq!(omk_aborts.load(Ordering::SeqCst), 1);
        assert!(
            lookup_operation_target(target).is_none(),
            "abort should clear the synthetic operation mapping"
        );
        assert!(
            lookup_synthetic_target(target) == Some(SyntheticTargetKind::Operation),
            "abort should keep the synthetic target so stale calls receive a native-style error"
        );

        let stale_update_reply = build_operation_reply_rewrite(&PendingOperationCall {
            request: ParsedOperationRequest::Update {
                input: b"after_abort".to_vec(),
            },
            caller: CallerIdentity::new(1000, 2000),
            target,
        })
        .expect("stale synthetic update rewrite should succeed")
        .expect("stale synthetic update should return a native-style error");
        let mut stale_update_reply = stale_update_reply;
        let (stale_data, stale_data_size, stale_offsets, stale_offsets_size) =
            raw_parts(&mut stale_update_reply);
        let stale_status = unsafe {
            parcel::parse_reply_status(
                stale_data,
                stale_data_size,
                stale_offsets,
                stale_offsets_size,
            )
        }
        .expect("stale update reply should deserialize");
        assert_eq!(
            stale_status.exception_code(),
            rsbinder::ExceptionCode::ServiceSpecific
        );
        assert_eq!(
            stale_status.service_specific_error(),
            crate::android::hardware::security::keymint::ErrorCode::ErrorCode::INVALID_OPERATION_HANDLE.0
        );
    }

    #[test]
    fn one_way_create_operation_aborts_without_publishing_carrier() {
        ensure_binder_process_state();
        let _guard = route_state_test_guard();
        clear_operation_state_for_tests();

        let aborts = Arc::new(AtomicUsize::new(0));
        let backend = BnKeystoreOperation::new_binder(TestOperationBackend {
            update_output: Vec::new(),
            aborts: aborts.clone(),
            update_aad_status: None,
        });
        let reply = build_no_carrier_create_operation_reply(
            CreateOperationResponse {
                r#iOperation: Some(backend),
                r#operationChallenge: None,
                r#parameters: None,
                r#upgradedBlob: None,
            },
            false,
            &CallerIdentity::new(10002, 2000),
            false,
        )
        .expect("one-way createOperation should execute and discard its operation");

        assert_eq!(aborts.load(Ordering::SeqCst), 1);
        assert!(reply.native_operation.is_none());
        assert_eq!(reply.offsets_size(), 0);
        assert!(OPERATION_TARGETS
            .lock()
            .expect("operation target map poisoned")
            .is_empty());
        assert!(SYNTHETIC_TARGETS
            .lock()
            .expect("synthetic target map poisoned")
            .is_empty());
        assert!(NATIVE_BINDERS
            .lock()
            .expect("native binder map poisoned")
            .is_empty());
        assert!(OPERATION_PUBLICATIONS
            .lock()
            .expect("operation publication map poisoned")
            .is_empty());
        assert!(OPERATION_PUBLICATION_PROBES
            .lock()
            .expect("operation publication probe queue poisoned")
            .is_empty());
    }

    #[test]
    fn one_way_synthetic_operation_abort_finalizes_mapping() {
        let _guard = route_state_test_guard();
        clear_operation_state_for_tests();

        let aborts = Arc::new(AtomicUsize::new(0));
        let backend = BnKeystoreOperation::new_binder(TestOperationBackend {
            update_output: vec![9],
            aborts: aborts.clone(),
            update_aad_status: None,
        });
        let caller = CallerIdentity::new(10002, 2000).with_sid("u:r:untrusted_app:s0:c123,c456");
        let (carrier, _) = register_synthetic_operation_carrier(backend, true, &caller)
            .expect("operation carrier should register");
        let target = carrier_target(&carrier);
        let service_specific_error = |reply: SyntheticReply| -> i32 {
            let SyntheticReply::Parcel(mut reply) = reply else {
                panic!("expected status parcel reply");
            };
            let (data, data_size, offsets, offsets_size) = raw_parts(&mut reply);
            let status =
                unsafe { parcel::parse_reply_status(data, data_size, offsets, offsets_size) }
                    .expect("status reply should parse");
            assert_eq!(
                status.exception_code(),
                rsbinder::ExceptionCode::ServiceSpecific
            );
            status.service_specific_error()
        };

        let abort_request = request_parcel(identify::KEYSTORE_OPERATION_INTERFACE);
        let mut abort_tr = transaction_for_parcel(
            target,
            crate::android::system::keystore2::IKeystoreOperation::transactions::r#abort,
            &abort_request,
        );
        abort_tr.flags |= crate::hook::binder::TF_ONE_WAY;

        let abort_reply =
            unsafe { handle_synthetic_br_transaction(&abort_tr, None, "BR_TRANSACTION") }
                .expect("one-way abort should be consumed");
        assert!(matches!(abort_reply, SyntheticReply::NoReply));
        assert_eq!(aborts.load(Ordering::SeqCst), 1);
        assert!(lookup_operation_target(target).is_none());

        let mut update_request = request_parcel(identify::KEYSTORE_OPERATION_INTERFACE);
        update_request.write(&vec![1u8]).unwrap();
        let update_tr = transaction_for_parcel(
            target,
            crate::android::system::keystore2::IKeystoreOperation::transactions::r#update,
            &update_request,
        );
        let update_reply =
            unsafe { handle_synthetic_br_transaction(&update_tr, None, "BR_TRANSACTION") }
                .expect("post-abort update should be handled");
        assert_eq!(
            service_specific_error(update_reply),
            crate::android::hardware::security::keymint::ErrorCode::ErrorCode::INVALID_OPERATION_HANDLE.0
        );

        let abort_again_request = request_parcel(identify::KEYSTORE_OPERATION_INTERFACE);
        let abort_again_tr = transaction_for_parcel(
            target,
            crate::android::system::keystore2::IKeystoreOperation::transactions::r#abort,
            &abort_again_request,
        );
        let abort_again_reply =
            unsafe { handle_synthetic_br_transaction(&abort_again_tr, None, "BR_TRANSACTION") }
                .expect("post-abort abort should be handled");
        assert_eq!(
            service_specific_error(abort_again_reply),
            crate::android::hardware::security::keymint::ErrorCode::ErrorCode::INVALID_OPERATION_HANDLE.0
        );
    }

    #[test]
    fn synthetic_operation_release_aborts_once_and_clears_mapping() {
        ensure_binder_process_state();
        let _guard = route_state_test_guard();
        clear_operation_state_for_tests();

        let aborts = Arc::new(AtomicUsize::new(0));
        let backend = BnKeystoreOperation::new_binder(TestOperationBackend {
            update_output: vec![9, 9, 9],
            aborts: aborts.clone(),
            update_aad_status: None,
        });
        let caller = CallerIdentity::new(10002, 2000).with_sid("u:r:untrusted_app:s0:c123,c456");
        let (carrier, _) = register_synthetic_operation_carrier(backend, true, &caller)
            .expect("operation carrier should register");
        let target = carrier_target(&carrier);

        assert!(lookup_operation_target(target).is_some());
        assert_eq!(
            lookup_synthetic_target(target),
            Some(SyntheticTargetKind::Operation)
        );

        observe_synthetic_operation_release(target);
        assert_eq!(aborts.load(Ordering::SeqCst), 1);
        assert!(
            lookup_operation_target(target).is_none(),
            "release should clear the live operation mapping"
        );
        assert!(lookup_synthetic_target(target).is_none());

        observe_synthetic_operation_release(target);
        assert_eq!(
            aborts.load(Ordering::SeqCst),
            1,
            "repeated release must not abort twice"
        );
    }

    #[test]
    fn raw_create_operation_reply_still_does_not_register_operation_mapping() {
        ensure_binder_process_state();
        let _guard = route_state_test_guard();
        clear_operation_state_for_tests();

        let omk_operation = BnKeystoreOperation::new_binder(TestOperationBackend {
            update_output: vec![9, 9, 9],
            aborts: Arc::new(AtomicUsize::new(0)),
            update_aad_status: None,
        });
        let mut reply = parcel::build_create_operation_reply(CreateOperationResponse {
            r#iOperation: Some(omk_operation),
            r#operationChallenge: None,
            r#parameters: None,
            r#upgradedBlob: Some(vec![7, 7]),
        })
        .expect("direct OMK createOperation reply should serialize");
        let (data, data_size, offsets, offsets_size) = raw_parts(&mut reply);
        let parsed: CreateOperationResponse =
            unsafe { parcel::parse_success_reply(data, data_size, offsets, offsets_size) }
                .expect("direct OMK createOperation reply should parse");

        assert!(parsed.r#iOperation.is_some());
        assert_eq!(parsed.r#upgradedBlob.as_deref(), Some(&[7, 7][..]));
        assert!(
            OPERATION_TARGETS
                .lock()
                .expect("operation target map poisoned")
                .is_empty(),
            "direct OMK replies should not register a fake system carrier mapping"
        );
    }

    #[test]
    fn invalid_update_aad_respects_route() {
        let _guard = route_state_test_guard();
        clear_operation_state_for_tests();

        let target = LocalBinderTarget {
            ptr: 0x1234,
            cookie: 0x5678,
        };
        remember_operation_target(
            target,
            OperationTargetInfo {
                route: RouteTarget::System,
                aad_allowed: false,
                backend: None,
                finalized: false,
            },
        );

        let reply = build_operation_reply_rewrite(&PendingOperationCall {
            request: ParsedOperationRequest::UpdateAad {
                aad_input: vec![1, 2, 3],
            },
            caller: CallerIdentity::new(1000, 2000),
            target,
        })
        .expect("updateAad rewrite should succeed");
        assert!(
            reply.is_none(),
            "system invalid updateAad should preserve the original native reply parcel"
        );
        assert_eq!(
            lookup_operation_target(target).unwrap().route,
            RouteTarget::System
        );

        reset_route_state_for_tests();
        ensure_binder_process_state();

        let target = LocalBinderTarget {
            ptr: 0x1234,
            cookie: 0x5678,
        };
        let backend = BnKeystoreOperation::new_binder(TestOperationBackend {
            update_output: vec![1, 2, 3],
            aborts: Arc::new(AtomicUsize::new(0)),
            update_aad_status: Some(Status::new_service_specific_error(7, None)),
        });
        remember_operation_target(
            target,
            OperationTargetInfo {
                route: RouteTarget::Omk,
                aad_allowed: false,
                backend: Some(backend),
                finalized: false,
            },
        );

        let reply = build_operation_reply_rewrite(&PendingOperationCall {
            request: ParsedOperationRequest::UpdateAad {
                aad_input: vec![1, 2, 3],
            },
            caller: CallerIdentity::new(1000, 2000),
            target,
        })
        .expect("updateAad rewrite should succeed")
        .expect("OMK invalid updateAad should return an OMK-owned reply");
        let mut reply = reply;
        let (data, data_size, offsets, offsets_size) = raw_parts(&mut reply);
        let status = unsafe { parcel::parse_reply_status(data, data_size, offsets, offsets_size) }
            .expect("invalid updateAad reply should deserialize to a binder status");
        assert_eq!(
            status.exception_code(),
            rsbinder::ExceptionCode::ServiceSpecific
        );
        assert_eq!(status.service_specific_error(), 7);
        assert_eq!(
            lookup_operation_target(target).unwrap().route,
            RouteTarget::Omk
        );
    }

    #[test]
    fn omk_route_operation_transaction_error_uses_omk_status_mapping() {
        ensure_binder_process_state();
        let _guard = route_state_test_guard();
        clear_operation_state_for_tests();

        let target = LocalBinderTarget {
            ptr: 0x1234,
            cookie: 0x5678,
        };
        let backend = BnKeystoreOperation::new_binder(TestOperationBackend {
            update_output: vec![1, 2, 3],
            aborts: Arc::new(AtomicUsize::new(0)),
            update_aad_status: Some(StatusCode::UnknownTransaction.into()),
        });
        remember_operation_target(
            target,
            OperationTargetInfo {
                route: RouteTarget::Omk,
                aad_allowed: false,
                backend: Some(backend),
                finalized: false,
            },
        );

        let reply = build_operation_reply_rewrite(&PendingOperationCall {
            request: ParsedOperationRequest::UpdateAad {
                aad_input: vec![1, 2, 3],
            },
            caller: CallerIdentity::new(1000, 2000),
            target,
        })
        .expect("transaction status should be normalized into a reply")
        .expect("OMK transaction status should return an OMK-owned reply");
        // A transport-level failure talking to the OMK operation backend is
        // reported as a service-specific SYSTEM_ERROR, matching AOSP keystore2.
        let mut reply = reply;
        let (data, data_size, offsets, offsets_size) = raw_parts(&mut reply);
        let parsed = unsafe { parcel::parse_reply_status(data, data_size, offsets, offsets_size) }
            .expect("status reply should parse");
        assert_eq!(
            parsed.exception_code(),
            rsbinder::ExceptionCode::ServiceSpecific
        );
        assert_eq!(
            parsed.service_specific_error(),
            ResponseCode::SYSTEM_ERROR.0
        );
    }

    #[test]
    fn omk_route_finish_rejects_late_cleanup_abort() {
        let _guard = route_state_test_guard();
        clear_operation_state_for_tests();

        let aborts = Arc::new(AtomicUsize::new(0));
        let target = LocalBinderTarget {
            ptr: 0x1234,
            cookie: 0x5678,
        };
        let backend = BnKeystoreOperation::new_binder(TestOperationBackend {
            update_output: vec![5, 6, 7],
            aborts: aborts.clone(),
            update_aad_status: None,
        });
        remember_operation_target(
            target,
            OperationTargetInfo {
                route: RouteTarget::Omk,
                aad_allowed: true,
                backend: Some(backend),
                finalized: false,
            },
        );

        let reply = build_operation_reply_rewrite(&PendingOperationCall {
            request: ParsedOperationRequest::Finish {
                input: Some(vec![1, 2, 3]),
                signature: None,
            },
            caller: CallerIdentity::new(1000, 2000),
            target,
        })
        .expect("finish rewrite should succeed")
        .expect("OMK finish should return an OMK-owned reply");
        let mut reply = reply;
        let (data, data_size, offsets, offsets_size) = raw_parts(&mut reply);
        let output: Option<Vec<u8>> =
            unsafe { parcel::parse_success_reply(data, data_size, offsets, offsets_size) }
                .expect("finish reply should deserialize");

        assert_eq!(output.as_deref(), Some(&[5, 6, 7][..]));
        let target_info = lookup_operation_target(target)
            .expect("finish should keep a finalized cleanup tombstone");
        assert_eq!(target_info.route, RouteTarget::Omk);
        assert!(target_info.finalized);
        assert!(target_info.backend.is_none());

        let cleanup_reply = build_operation_reply_rewrite(&PendingOperationCall {
            request: ParsedOperationRequest::Abort,
            caller: CallerIdentity::new(1000, 2000),
            target,
        })
        .expect("cleanup abort rewrite should succeed")
        .expect("cleanup abort should return an OMK-owned reply");
        let mut cleanup_reply = cleanup_reply;
        let (cleanup_data, cleanup_data_size, cleanup_offsets, cleanup_offsets_size) =
            raw_parts(&mut cleanup_reply);
        let cleanup_status = unsafe {
            parcel::parse_reply_status(
                cleanup_data,
                cleanup_data_size,
                cleanup_offsets,
                cleanup_offsets_size,
            )
        }
        .expect("cleanup abort reply should deserialize");

        assert_eq!(
            cleanup_status.exception_code(),
            rsbinder::ExceptionCode::ServiceSpecific
        );
        assert_eq!(
            cleanup_status.service_specific_error(),
            crate::android::hardware::security::keymint::ErrorCode::ErrorCode::INVALID_OPERATION_HANDLE.0
        );
        assert_eq!(
            aborts.load(Ordering::SeqCst),
            0,
            "finalized cleanup should not abort the already-finished backend again"
        );
        assert!(
            lookup_operation_target(target).is_none(),
            "cleanup abort should clear the finalized tombstone"
        );
    }

    #[test]
    fn omk_route_abort_clears_operation_mapping() {
        ensure_binder_process_state();
        let _guard = route_state_test_guard();
        clear_operation_state_for_tests();

        let aborts = Arc::new(AtomicUsize::new(0));
        let target = LocalBinderTarget {
            ptr: 0x1234,
            cookie: 0x5678,
        };
        let backend = BnKeystoreOperation::new_binder(TestOperationBackend {
            update_output: vec![5, 6, 7],
            aborts: aborts.clone(),
            update_aad_status: None,
        });
        remember_operation_target(
            target,
            OperationTargetInfo {
                route: RouteTarget::Omk,
                aad_allowed: true,
                backend: Some(backend),
                finalized: false,
            },
        );

        let reply = build_operation_reply_rewrite(&PendingOperationCall {
            request: ParsedOperationRequest::Abort,
            caller: CallerIdentity::new(1000, 2000),
            target,
        })
        .expect("abort rewrite should succeed")
        .expect("OMK abort should return an OMK-owned reply");
        let mut reply = reply;
        let (data, data_size, offsets, offsets_size) = raw_parts(&mut reply);
        let status = unsafe { parcel::parse_reply_status(data, data_size, offsets, offsets_size) }
            .expect("abort reply should deserialize");

        assert!(status.is_ok());
        assert_eq!(aborts.load(Ordering::SeqCst), 1);
        assert!(
            lookup_operation_target(target).is_none(),
            "abort should clear the operation mapping"
        );
    }

    #[test]
    fn one_way_dispatch_policy_allows_side_effects() {
        assert!(can_execute_one_way(
            SyntheticTargetKind::Operation,
            crate::android::system::keystore2::IKeystoreOperation::transactions::r#update
        ));
        assert!(can_execute_one_way(
            SyntheticTargetKind::Operation,
            crate::android::system::keystore2::IKeystoreOperation::transactions::r#finish
        ));
        assert!(can_execute_one_way(
            SyntheticTargetKind::Operation,
            crate::android::system::keystore2::IKeystoreOperation::transactions::r#abort
        ));
        assert!(can_execute_one_way(
            SyntheticTargetKind::Operation,
            crate::android::system::keystore2::IKeystoreOperation::transactions::r#updateAad
        ));
        assert!(can_execute_one_way(
            SyntheticTargetKind::SecurityLevel,
            crate::android::system::keystore2::IKeystoreSecurityLevel::transactions::r#importKey
        ));
        assert!(can_execute_one_way(
            SyntheticTargetKind::SecurityLevel,
            crate::android::system::keystore2::IKeystoreSecurityLevel::transactions::r#importWrappedKey
        ));
        assert!(can_execute_one_way(
            SyntheticTargetKind::SecurityLevel,
            crate::android::system::keystore2::IKeystoreSecurityLevel::transactions::r#generateKey
        ));
        assert!(can_execute_one_way(
            SyntheticTargetKind::SecurityLevel,
            crate::android::system::keystore2::IKeystoreSecurityLevel::transactions::r#createOperation
        ));
        assert!(!can_execute_one_way(
            SyntheticTargetKind::SecurityLevel,
            crate::android::system::keystore2::IKeystoreSecurityLevel::transactions::r#convertStorageKeyToEphemeral
        ));
        assert!(can_execute_one_way(
            SyntheticTargetKind::SecurityLevel,
            crate::android::system::keystore2::IKeystoreSecurityLevel::transactions::r#deleteKey
        ));
    }
}
