//! v5 的错误（01 §4.3）：`ProtocolError{code, message, retryable, outcome?, details?}`，经 JSON-RPC 的 `error.data` 带出
//! （`error.code` 是 [`PROTOCOL_ERROR_CODE`]）。可重试性取生成的错误码表，不手写。

use rust_cli_protocol_core::rpc::RpcError;
use serde_json::Value;

use crate::consts::PROTOCOL_ERROR_CODE;
use crate::generated::contract::KNOWN_ERRORS;
use crate::generated::core_v1::{ProtocolError, ProtocolErrorOutcome};

/// 请求失败的结果（装箱：ProtocolError 不小，函数的 Result 里放指针）。
pub type Fault = Box<ProtocolError>;

/// 一个已登记的错误码的错误；码没有登记时按不可重试（与 errors.ts 的兜底同：只有 link 与 source 是瞬态，V0 不用它们）。
pub fn protocol_error(code: &str, message: impl Into<String>) -> ProtocolError {
    let retryable = KNOWN_ERRORS
        .iter()
        .find(|(known, _)| *known == code)
        .is_some_and(|(_, retryable)| *retryable);
    debug_assert!(
        KNOWN_ERRORS.iter().any(|(known, _)| *known == code),
        "unregistered error code {code}"
    );
    ProtocolError {
        code: code.to_owned(),
        message: message.into(),
        retryable,
        outcome: None,
        details: None,
    }
}

/// 没有产生副作用（`outcome: not_applied`）。
pub fn not_applied(mut error: ProtocolError) -> ProtocolError {
    error.outcome = Some(ProtocolErrorOutcome::NotApplied);
    error
}

pub fn with_details(mut error: ProtocolError, details: Value) -> ProtocolError {
    error.details = Some(details);
    error
}

/// JSON-RPC 的错误回复。
pub fn to_rpc(error: &ProtocolError) -> RpcError {
    let data = serde_json::to_value(error).unwrap_or(Value::Null);
    RpcError::new(PROTOCOL_ERROR_CODE, error.message.clone()).with_data(data)
}
