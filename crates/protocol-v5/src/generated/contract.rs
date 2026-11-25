//! 排版之前的摘要：cc5dcd368fadfd36
//! 生成物，不要手改：能力表、错误码表、方法表与契约摘要（docs/zcode-protocol-v5/11-contract-codegen.md §1）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

/// 契约指纹（03 §3.7）：与 `@zcode/protocol` 的 `CONTRACT_DIGEST` 同值。initialize 带来的指纹与它相等，这条链路才严格读
/// （03 §3.8）；也进 `agent` 的 `diagnostics.protocolDigest`（诊断）。
pub const CONTRACT_DIGEST: &str = "c6407a802d5b2627";

/// 能力的事实源在哪一层。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Cli,
    Host,
}

/// 能力注册表的一项（`packages/protocol/src/caps/capabilities.ts`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capability {
    pub id: &'static str,
    /// 仍受支持的修订，升序。
    pub revisions: &'static [u32],
    pub owner: Owner,
}

/// 能力注册表（按 id 排序）。
pub const CAPABILITIES: &[Capability] = &[
    Capability {
        id: "account",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "account.overlay",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "account.profile",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "agent",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "attachments",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "automation",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "bots",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "catalog.mcp",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "catalog.plugins",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "catalog.plugins.reference",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "catalog.skills",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "catalog.slash",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "catalog.workflows",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "cloud.config",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "core",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "core.patch",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "cua",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "devtools",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "directory",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "feedback",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "fs",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "git",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "groups",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "highspeed",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "hooks.pretrust",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "import",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "marketing",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "mcp.oauth",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "mcp.trust",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "memory",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "onboarding",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "presence",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "providers",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "providers.catalog",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "provisioning",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "remote.sources",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "remote.sources.connect",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "session",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "session.deletion",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "session.execution",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "session.fork",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "session.goal",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "session.purge",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "session.queue",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "session.report",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "session.rewind",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "session.usage",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "session.workflow",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "settings",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "settings.sync",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "settings.view",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "share",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "subscription",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "sync.receive",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "system",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "tasks",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "telemetry",
        revisions: &[1],
        owner: Owner::Cli,
    },
    Capability {
        id: "terminal",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "usage",
        revisions: &[1],
        owner: Owner::Host,
    },
    Capability {
        id: "workspace.config",
        revisions: &[1],
        owner: Owner::Cli,
    },
];

/// 已登记的错误码与可重试性（`packages/protocol/src/errors.ts` 的 KNOWN_ERRORS，按码排序）。
pub const KNOWN_ERRORS: &[(&str, bool)] = &[
    ("admission.activeTurn", false),
    ("admission.activeWorks", false),
    ("admission.busy", false),
    ("admission.compactLocked", false),
    ("admission.compacting", false),
    ("admission.dispositionNotAllowed", false),
    ("admission.draft", false),
    ("admission.goalNotActive", false),
    ("admission.goalNotPaused", false),
    ("admission.goalVerifying", false),
    ("admission.inputCancelled", false),
    ("admission.inputDiscardedOnRestart", false),
    ("admission.inputRejected", false),
    ("admission.modelUnavailable", false),
    ("admission.noGoal", false),
    ("admission.notLatestRow", false),
    ("admission.notRunning", false),
    ("admission.pendingInteraction", false),
    ("admission.planGoalExclusive", false),
    ("admission.queueChoiceRequired", false),
    ("admission.queueHeadEditLocked", false),
    ("admission.queueItemEditLocked", false),
    ("admission.queueItemNotEditable", false),
    ("admission.queueItemReserved", false),
    ("admission.queuePromotionBusy", false),
    ("admission.rowKindMismatch", false),
    ("admission.rowNotStable", false),
    ("admission.sessionDeleted", false),
    ("admission.sideChat", false),
    ("admission.stopTargetChanged", false),
    ("deadline.exceeded", false),
    ("handshake.contractMismatch", false),
    ("handshake.missingCapabilities", false),
    ("handshake.serverUnavailable", true),
    ("handshake.unauthorized", false),
    ("internal.failed", false),
    ("link.closed", true),
    ("link.messageTooLarge", false),
    ("link.protocolViolation", false),
    ("precondition.alreadyResolved", false),
    ("precondition.alreadyRunning", false),
    ("precondition.cancelNotSupported", false),
    ("precondition.capabilityUnsupported", false),
    ("precondition.catalogItemExists", false),
    ("precondition.catalogItemReadOnly", false),
    ("precondition.checksumMismatch", false),
    ("precondition.chunkConflict", false),
    ("precondition.chunkGap", false),
    ("precondition.commandIdConflict", false),
    ("precondition.commitInProgress", false),
    ("precondition.compileFailed", false),
    ("precondition.connectCancelled", false),
    ("precondition.emptyObjective", false),
    ("precondition.forkTargetAmbiguous", false),
    ("precondition.heldQueueChanged", false),
    ("precondition.hookBlockedByPolicy", false),
    ("precondition.hookReviewSuperseded", false),
    ("precondition.hookSnapshotMismatch", false),
    ("precondition.hookTrustDisabled", false),
    ("precondition.hookTrustStoreCorrupt", false),
    ("precondition.hookTrustUnavailable", false),
    ("precondition.inputEmpty", false),
    ("precondition.invalid", false),
    ("precondition.invalidCommandId", false),
    ("precondition.invalidMode", false),
    ("precondition.invalidPayload", false),
    ("precondition.invalidWorkflowArgs", false),
    ("precondition.invalidWorkflowName", false),
    ("precondition.latestQueryEditOnly", false),
    ("precondition.marketplaceNotFound", false),
    ("precondition.mcpDeclarationChanged", false),
    ("precondition.mcpOAuthFailed", false),
    ("precondition.mcpOAuthUnavailable", false),
    ("precondition.mcpTrustUnavailable", false),
    ("precondition.missingBoundaries", false),
    ("precondition.modelUnavailable", false),
    ("precondition.modelUnbound", false),
    ("precondition.notAuthorized", false),
    ("precondition.notConfigurable", false),
    ("precondition.notFound", false),
    ("precondition.notHighspeed", false),
    ("precondition.notMedia", false),
    ("precondition.notResumable", false),
    ("precondition.notRunning", false),
    ("precondition.operationActive", false),
    ("precondition.operationUnknown", false),
    ("precondition.payloadTooLarge", false),
    ("precondition.pluginNotFound", false),
    ("precondition.pluginOperationCancelled", false),
    ("precondition.pluginOperationFailed", false),
    ("precondition.pluginOptionInvalid", false),
    ("precondition.providerNotInRegistry", false),
    ("precondition.queueEditLockLost", false),
    ("precondition.queuePromotionCommitFailed", false),
    ("precondition.rangeInvalid", false),
    ("precondition.restoreWarning", false),
    ("precondition.rewindUnavailable", false),
    ("precondition.savedWorkflowNotFound", false),
    ("precondition.scriptMismatch", false),
    ("precondition.scriptMissing", false),
    ("precondition.sessionBusy", true),
    ("precondition.sessionExists", false),
    ("precondition.sharedContextNotPending", false),
    ("precondition.sizeExceeded", false),
    ("precondition.stagingCapacityExceeded", false),
    ("precondition.startFailed", false),
    ("precondition.storeUnavailable", true),
    ("precondition.superseded", false),
    ("precondition.tooLarge", false),
    ("precondition.tooManyUploads", false),
    ("precondition.unchanged", false),
    ("precondition.uploadIncomplete", false),
    ("precondition.versionMismatch", false),
    ("precondition.workspaceMismatch", false),
    ("precondition.workspaceRequired", false),
    ("query.stale", false),
    ("query.unsupported", false),
    ("source.unavailable", true),
];

/// 已登记的错误码（名字 = 码的 SCREAMING_SNAKE_CASE）。
pub mod codes {
    pub const ADMISSION_ACTIVE_TURN: &str = "admission.activeTurn";
    pub const ADMISSION_ACTIVE_WORKS: &str = "admission.activeWorks";
    pub const ADMISSION_BUSY: &str = "admission.busy";
    pub const ADMISSION_COMPACT_LOCKED: &str = "admission.compactLocked";
    pub const ADMISSION_COMPACTING: &str = "admission.compacting";
    pub const ADMISSION_DISPOSITION_NOT_ALLOWED: &str = "admission.dispositionNotAllowed";
    pub const ADMISSION_DRAFT: &str = "admission.draft";
    pub const ADMISSION_GOAL_NOT_ACTIVE: &str = "admission.goalNotActive";
    pub const ADMISSION_GOAL_NOT_PAUSED: &str = "admission.goalNotPaused";
    pub const ADMISSION_GOAL_VERIFYING: &str = "admission.goalVerifying";
    pub const ADMISSION_INPUT_CANCELLED: &str = "admission.inputCancelled";
    pub const ADMISSION_INPUT_DISCARDED_ON_RESTART: &str = "admission.inputDiscardedOnRestart";
    pub const ADMISSION_INPUT_REJECTED: &str = "admission.inputRejected";
    pub const ADMISSION_MODEL_UNAVAILABLE: &str = "admission.modelUnavailable";
    pub const ADMISSION_NO_GOAL: &str = "admission.noGoal";
    pub const ADMISSION_NOT_LATEST_ROW: &str = "admission.notLatestRow";
    pub const ADMISSION_NOT_RUNNING: &str = "admission.notRunning";
    pub const ADMISSION_PENDING_INTERACTION: &str = "admission.pendingInteraction";
    pub const ADMISSION_PLAN_GOAL_EXCLUSIVE: &str = "admission.planGoalExclusive";
    pub const ADMISSION_QUEUE_CHOICE_REQUIRED: &str = "admission.queueChoiceRequired";
    pub const ADMISSION_QUEUE_HEAD_EDIT_LOCKED: &str = "admission.queueHeadEditLocked";
    pub const ADMISSION_QUEUE_ITEM_EDIT_LOCKED: &str = "admission.queueItemEditLocked";
    pub const ADMISSION_QUEUE_ITEM_NOT_EDITABLE: &str = "admission.queueItemNotEditable";
    pub const ADMISSION_QUEUE_ITEM_RESERVED: &str = "admission.queueItemReserved";
    pub const ADMISSION_QUEUE_PROMOTION_BUSY: &str = "admission.queuePromotionBusy";
    pub const ADMISSION_ROW_KIND_MISMATCH: &str = "admission.rowKindMismatch";
    pub const ADMISSION_ROW_NOT_STABLE: &str = "admission.rowNotStable";
    pub const ADMISSION_SESSION_DELETED: &str = "admission.sessionDeleted";
    pub const ADMISSION_SIDE_CHAT: &str = "admission.sideChat";
    pub const ADMISSION_STOP_TARGET_CHANGED: &str = "admission.stopTargetChanged";
    pub const DEADLINE_EXCEEDED: &str = "deadline.exceeded";
    pub const HANDSHAKE_CONTRACT_MISMATCH: &str = "handshake.contractMismatch";
    pub const HANDSHAKE_MISSING_CAPABILITIES: &str = "handshake.missingCapabilities";
    pub const HANDSHAKE_SERVER_UNAVAILABLE: &str = "handshake.serverUnavailable";
    pub const HANDSHAKE_UNAUTHORIZED: &str = "handshake.unauthorized";
    pub const INTERNAL_FAILED: &str = "internal.failed";
    pub const LINK_CLOSED: &str = "link.closed";
    pub const LINK_MESSAGE_TOO_LARGE: &str = "link.messageTooLarge";
    pub const LINK_PROTOCOL_VIOLATION: &str = "link.protocolViolation";
    pub const PRECONDITION_ALREADY_RESOLVED: &str = "precondition.alreadyResolved";
    pub const PRECONDITION_ALREADY_RUNNING: &str = "precondition.alreadyRunning";
    pub const PRECONDITION_CANCEL_NOT_SUPPORTED: &str = "precondition.cancelNotSupported";
    pub const PRECONDITION_CAPABILITY_UNSUPPORTED: &str = "precondition.capabilityUnsupported";
    pub const PRECONDITION_CATALOG_ITEM_EXISTS: &str = "precondition.catalogItemExists";
    pub const PRECONDITION_CATALOG_ITEM_READ_ONLY: &str = "precondition.catalogItemReadOnly";
    pub const PRECONDITION_CHECKSUM_MISMATCH: &str = "precondition.checksumMismatch";
    pub const PRECONDITION_CHUNK_CONFLICT: &str = "precondition.chunkConflict";
    pub const PRECONDITION_CHUNK_GAP: &str = "precondition.chunkGap";
    pub const PRECONDITION_COMMAND_ID_CONFLICT: &str = "precondition.commandIdConflict";
    pub const PRECONDITION_COMMIT_IN_PROGRESS: &str = "precondition.commitInProgress";
    pub const PRECONDITION_COMPILE_FAILED: &str = "precondition.compileFailed";
    pub const PRECONDITION_CONNECT_CANCELLED: &str = "precondition.connectCancelled";
    pub const PRECONDITION_EMPTY_OBJECTIVE: &str = "precondition.emptyObjective";
    pub const PRECONDITION_FORK_TARGET_AMBIGUOUS: &str = "precondition.forkTargetAmbiguous";
    pub const PRECONDITION_HELD_QUEUE_CHANGED: &str = "precondition.heldQueueChanged";
    pub const PRECONDITION_HOOK_BLOCKED_BY_POLICY: &str = "precondition.hookBlockedByPolicy";
    pub const PRECONDITION_HOOK_REVIEW_SUPERSEDED: &str = "precondition.hookReviewSuperseded";
    pub const PRECONDITION_HOOK_SNAPSHOT_MISMATCH: &str = "precondition.hookSnapshotMismatch";
    pub const PRECONDITION_HOOK_TRUST_DISABLED: &str = "precondition.hookTrustDisabled";
    pub const PRECONDITION_HOOK_TRUST_STORE_CORRUPT: &str = "precondition.hookTrustStoreCorrupt";
    pub const PRECONDITION_HOOK_TRUST_UNAVAILABLE: &str = "precondition.hookTrustUnavailable";
    pub const PRECONDITION_INPUT_EMPTY: &str = "precondition.inputEmpty";
    pub const PRECONDITION_INVALID: &str = "precondition.invalid";
    pub const PRECONDITION_INVALID_COMMAND_ID: &str = "precondition.invalidCommandId";
    pub const PRECONDITION_INVALID_MODE: &str = "precondition.invalidMode";
    pub const PRECONDITION_INVALID_PAYLOAD: &str = "precondition.invalidPayload";
    pub const PRECONDITION_INVALID_WORKFLOW_ARGS: &str = "precondition.invalidWorkflowArgs";
    pub const PRECONDITION_INVALID_WORKFLOW_NAME: &str = "precondition.invalidWorkflowName";
    pub const PRECONDITION_LATEST_QUERY_EDIT_ONLY: &str = "precondition.latestQueryEditOnly";
    pub const PRECONDITION_MARKETPLACE_NOT_FOUND: &str = "precondition.marketplaceNotFound";
    pub const PRECONDITION_MCP_DECLARATION_CHANGED: &str = "precondition.mcpDeclarationChanged";
    pub const PRECONDITION_MCP_OAUTH_FAILED: &str = "precondition.mcpOAuthFailed";
    pub const PRECONDITION_MCP_OAUTH_UNAVAILABLE: &str = "precondition.mcpOAuthUnavailable";
    pub const PRECONDITION_MCP_TRUST_UNAVAILABLE: &str = "precondition.mcpTrustUnavailable";
    pub const PRECONDITION_MISSING_BOUNDARIES: &str = "precondition.missingBoundaries";
    pub const PRECONDITION_MODEL_UNAVAILABLE: &str = "precondition.modelUnavailable";
    pub const PRECONDITION_MODEL_UNBOUND: &str = "precondition.modelUnbound";
    pub const PRECONDITION_NOT_AUTHORIZED: &str = "precondition.notAuthorized";
    pub const PRECONDITION_NOT_CONFIGURABLE: &str = "precondition.notConfigurable";
    pub const PRECONDITION_NOT_FOUND: &str = "precondition.notFound";
    pub const PRECONDITION_NOT_HIGHSPEED: &str = "precondition.notHighspeed";
    pub const PRECONDITION_NOT_MEDIA: &str = "precondition.notMedia";
    pub const PRECONDITION_NOT_RESUMABLE: &str = "precondition.notResumable";
    pub const PRECONDITION_NOT_RUNNING: &str = "precondition.notRunning";
    pub const PRECONDITION_OPERATION_ACTIVE: &str = "precondition.operationActive";
    pub const PRECONDITION_OPERATION_UNKNOWN: &str = "precondition.operationUnknown";
    pub const PRECONDITION_PAYLOAD_TOO_LARGE: &str = "precondition.payloadTooLarge";
    pub const PRECONDITION_PLUGIN_NOT_FOUND: &str = "precondition.pluginNotFound";
    pub const PRECONDITION_PLUGIN_OPERATION_CANCELLED: &str =
        "precondition.pluginOperationCancelled";
    pub const PRECONDITION_PLUGIN_OPERATION_FAILED: &str = "precondition.pluginOperationFailed";
    pub const PRECONDITION_PLUGIN_OPTION_INVALID: &str = "precondition.pluginOptionInvalid";
    pub const PRECONDITION_PROVIDER_NOT_IN_REGISTRY: &str = "precondition.providerNotInRegistry";
    pub const PRECONDITION_QUEUE_EDIT_LOCK_LOST: &str = "precondition.queueEditLockLost";
    pub const PRECONDITION_QUEUE_PROMOTION_COMMIT_FAILED: &str =
        "precondition.queuePromotionCommitFailed";
    pub const PRECONDITION_RANGE_INVALID: &str = "precondition.rangeInvalid";
    pub const PRECONDITION_RESTORE_WARNING: &str = "precondition.restoreWarning";
    pub const PRECONDITION_REWIND_UNAVAILABLE: &str = "precondition.rewindUnavailable";
    pub const PRECONDITION_SAVED_WORKFLOW_NOT_FOUND: &str = "precondition.savedWorkflowNotFound";
    pub const PRECONDITION_SCRIPT_MISMATCH: &str = "precondition.scriptMismatch";
    pub const PRECONDITION_SCRIPT_MISSING: &str = "precondition.scriptMissing";
    pub const PRECONDITION_SESSION_BUSY: &str = "precondition.sessionBusy";
    pub const PRECONDITION_SESSION_EXISTS: &str = "precondition.sessionExists";
    pub const PRECONDITION_SHARED_CONTEXT_NOT_PENDING: &str =
        "precondition.sharedContextNotPending";
    pub const PRECONDITION_SIZE_EXCEEDED: &str = "precondition.sizeExceeded";
    pub const PRECONDITION_STAGING_CAPACITY_EXCEEDED: &str = "precondition.stagingCapacityExceeded";
    pub const PRECONDITION_START_FAILED: &str = "precondition.startFailed";
    pub const PRECONDITION_STORE_UNAVAILABLE: &str = "precondition.storeUnavailable";
    pub const PRECONDITION_SUPERSEDED: &str = "precondition.superseded";
    pub const PRECONDITION_TOO_LARGE: &str = "precondition.tooLarge";
    pub const PRECONDITION_TOO_MANY_UPLOADS: &str = "precondition.tooManyUploads";
    pub const PRECONDITION_UNCHANGED: &str = "precondition.unchanged";
    pub const PRECONDITION_UPLOAD_INCOMPLETE: &str = "precondition.uploadIncomplete";
    pub const PRECONDITION_VERSION_MISMATCH: &str = "precondition.versionMismatch";
    pub const PRECONDITION_WORKSPACE_MISMATCH: &str = "precondition.workspaceMismatch";
    pub const PRECONDITION_WORKSPACE_REQUIRED: &str = "precondition.workspaceRequired";
    pub const QUERY_STALE: &str = "query.stale";
    pub const QUERY_UNSUPPORTED: &str = "query.unsupported";
    pub const SOURCE_UNAVAILABLE: &str = "source.unavailable";
}

/// core@1 的方法名（`CORE_METHODS`）。
pub const COMMAND: &str = "command";
pub const CREDIT: &str = "credit";
pub const EXIT: &str = "exit";
pub const FRAME: &str = "frame";
pub const INITIALIZE: &str = "initialize";
pub const PING: &str = "ping";
pub const QUERY: &str = "query";
pub const SERVICE: &str = "service";
pub const SHUTDOWN: &str = "shutdown";
pub const SUBSCRIBE: &str = "subscribe";
pub const SUBSCRIPTION_FAILED: &str = "subscription.failed";
pub const UNSUBSCRIBE: &str = "unsubscribe";

/// 请求类的方法（有回复）。
pub const REQUESTS: &[&str] = &[
    COMMAND,
    INITIALIZE,
    PING,
    QUERY,
    SERVICE,
    SHUTDOWN,
    SUBSCRIBE,
    UNSUBSCRIBE,
];

/// 通知类的方法（没有回复）。
pub const NOTIFICATIONS: &[&str] = &[CREDIT, EXIT, FRAME, SUBSCRIPTION_FAILED];

/// 方法 → 它的参数在 core@1 里的 schema 名（严格链路上按它找入站消息的 schema，03 §3.8）。
pub const METHOD_PARAMS: &[(&str, &str)] = &[
    (COMMAND, "CommandEnvelopeSchema"),
    (CREDIT, "CreditSchema"),
    (EXIT, "ExitSchema"),
    (FRAME, "FrameSchema"),
    (INITIALIZE, "InitializeParamsSchema"),
    (PING, "NoParamsSchema"),
    (QUERY, "QueryEnvelopeSchema"),
    (SERVICE, "ServiceEnvelopeSchema"),
    (SHUTDOWN, "NoParamsSchema"),
    (SUBSCRIBE, "SubscribeSchema"),
    (SUBSCRIPTION_FAILED, "SubscriptionFailedSchema"),
    (UNSUBSCRIBE, "UnsubscribeSchema"),
];
