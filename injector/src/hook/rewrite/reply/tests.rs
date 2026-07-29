use std::mem::size_of;

use rsbinder::{ExceptionCode, Status, StatusCode};

use super::*;
use crate::hook::rewrite::tests::*;

pub(super) fn assert_unknown_transaction_reply(reply: SyntheticReply) {
    let SyntheticReply::Status(status) = reply else {
        panic!("unknown transaction should be returned as a binder status code");
    };
    assert_eq!(status, i32::from(StatusCode::UnknownTransaction));
}

pub(super) fn assert_synthetic_status(reply: SyntheticReply, expected: StatusCode) {
    let SyntheticReply::Status(status) = reply else {
        panic!("expected synthetic binder status");
    };
    assert_eq!(status, i32::from(expected));
}

pub(super) fn assert_synthetic_ok_reply(reply: SyntheticReply, label: &str) {
    let SyntheticReply::Parcel(mut reply) = reply else {
        panic!("{label} should be a status-bearing parcel reply");
    };
    assert_eq!(reply.offsets_size(), 0);
    let (data, data_size, offsets, offsets_size) = raw_parts(&mut reply);
    let status = unsafe { parcel::parse_reply_status(data, data_size, offsets, offsets_size) }
        .expect("status reply should parse");
    assert!(status.is_ok(), "{label} should be OK");
}

pub(super) fn assert_synthetic_empty_parcel_reply(reply: SyntheticReply, label: &str) {
    let SyntheticReply::Parcel(reply) = reply else {
        panic!("{label} should be an empty parcel reply");
    };
    assert_eq!(reply.data_size(), 0, "{label} data size");
    assert_eq!(reply.offsets_size(), 0, "{label} offsets size");
}

pub(super) fn assert_synthetic_exception_reply(reply: SyntheticReply, expected: ExceptionCode) {
    let SyntheticReply::Parcel(mut reply) = reply else {
        panic!("expected synthetic status parcel reply");
    };
    let (data, data_size, offsets, offsets_size) = raw_parts(&mut reply);
    let status = unsafe { parcel::parse_reply_status(data, data_size, offsets, offsets_size) }
        .expect("status reply should parse");
    assert_eq!(status.exception_code(), expected);
}

pub(super) fn assert_synthetic_raw_i32_reply(reply: SyntheticReply, expected: i32) {
    let SyntheticReply::Parcel(mut reply) = reply else {
        panic!("raw i32 reply should be a parcel");
    };
    assert_eq!(reply.data_size(), size_of::<i32>());
    assert_eq!(reply.offsets_size(), 0);
    let (data, _, _, _) = raw_parts(&mut reply);
    let value = unsafe { std::ptr::read_unaligned(data as *const i32) };
    assert_eq!(value, expected);
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
