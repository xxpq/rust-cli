-- 会话库与本机 Node SQLite 同一套表结构（docs/specs/rust-cli/pg-store.md §12）：表名、列名与含义取自 Node 24 条迁移之后的
-- 最终结构，另加 tenant_id（主键、唯一键、外键都带它）、row_seq（替代 SQLite 的 rowid 排序）、session 的租约列，以及只在
-- 云端的写入回执表。JSON 列保持 text，内容与本机逐字节相同。
create schema if not exists zcode;

create table zcode.schema_version (
    version integer primary key,
    checksum text not null
);

create table zcode.session (
    tenant_id text not null,
    id text not null,
    project_id text not null,
    workspace_id text,
    parent_id text,
    slug text not null,
    directory text not null,
    path text,
    title text not null,
    version text not null,
    share_url text,
    summary_additions bigint,
    summary_deletions bigint,
    summary_files bigint,
    summary_diffs text,
    revert text,
    permission text,
    time_created bigint not null,
    time_updated bigint not null,
    time_compacting bigint,
    time_archived bigint,
    task_type text not null default 'interactive',
    title_source text not null default 'first_input'
        check (title_source in ('default', 'first_input', 'generated', 'custom')),
    title_message_id text,
    time_title_updated bigint,
    trace_id text,
    time_deleted bigint,
    last_outcome text,
    owner_id text,
    owner_epoch bigint not null default 0,
    lease_until timestamptz,
    primary key (tenant_id, id)
);
create index session_project_idx on zcode.session (tenant_id, project_id);
create index session_workspace_idx on zcode.session (tenant_id, workspace_id);
create index session_parent_idx on zcode.session (tenant_id, parent_id);
create index session_task_type_idx on zcode.session (tenant_id, task_type);
create index session_trace_idx on zcode.session (tenant_id, trace_id);

create table zcode.message (
    tenant_id text not null,
    id text not null,
    session_id text not null,
    time_created bigint not null,
    time_updated bigint not null,
    data text not null,
    sequence bigint,
    row_seq bigint generated always as identity,
    primary key (tenant_id, id),
    foreign key (tenant_id, session_id) references zcode.session (tenant_id, id) on delete cascade
);
create index message_session_time_created_id_idx on zcode.message (tenant_id, session_id, time_created, id);
create index message_session_sequence_idx on zcode.message (tenant_id, session_id, sequence, time_created, id);

create table zcode.part (
    tenant_id text not null,
    id text not null,
    message_id text not null,
    session_id text not null,
    time_created bigint not null,
    time_updated bigint not null,
    data text not null,
    sequence bigint,
    row_seq bigint generated always as identity,
    primary key (tenant_id, id),
    foreign key (tenant_id, message_id) references zcode.message (tenant_id, id) on delete cascade
);
create index part_message_id_id_idx on zcode.part (tenant_id, message_id, id);
create index part_session_idx on zcode.part (tenant_id, session_id);
create index part_message_sequence_idx on zcode.part (tenant_id, message_id, sequence, time_created, id);
create index part_session_message_sequence_idx on zcode.part (tenant_id, session_id, message_id, sequence);

-- Node 迁移 0015 的 message_sequence_autofill / part_sequence_autofill：sequence 为空时按会话或消息取 max + 1。
create function zcode.message_sequence_autofill() returns trigger language plpgsql as $$
begin
    if new.sequence is null then
        select coalesce(max(sequence), -1) + 1 into new.sequence
        from zcode.message where tenant_id = new.tenant_id and session_id = new.session_id;
    end if;
    return new;
end $$;
create trigger message_sequence_autofill before insert on zcode.message
    for each row execute function zcode.message_sequence_autofill();

create function zcode.part_sequence_autofill() returns trigger language plpgsql as $$
begin
    if new.sequence is null then
        select coalesce(max(sequence), -1) + 1 into new.sequence
        from zcode.part where tenant_id = new.tenant_id and message_id = new.message_id;
    end if;
    return new;
end $$;
create trigger part_sequence_autofill before insert on zcode.part
    for each row execute function zcode.part_sequence_autofill();

create table zcode.todo (
    tenant_id text not null,
    session_id text not null,
    content text not null,
    status text not null,
    priority text not null,
    position bigint not null,
    time_created bigint not null,
    time_updated bigint not null,
    primary key (tenant_id, session_id, position),
    foreign key (tenant_id, session_id) references zcode.session (tenant_id, id) on delete cascade
);
create index todo_session_idx on zcode.todo (tenant_id, session_id);

create table zcode.session_entry (
    tenant_id text not null,
    id text not null,
    session_id text not null,
    type text not null,
    time_created bigint not null,
    time_updated bigint not null,
    data text not null,
    row_seq bigint generated always as identity,
    primary key (tenant_id, id),
    foreign key (tenant_id, session_id) references zcode.session (tenant_id, id) on delete cascade
);
create index session_entry_session_idx on zcode.session_entry (tenant_id, session_id);
create index session_entry_session_type_idx on zcode.session_entry (tenant_id, session_id, type);
create index session_entry_time_created_idx on zcode.session_entry (tenant_id, time_created);

create table zcode.permission (
    tenant_id text not null,
    project_id text not null,
    time_created bigint not null,
    time_updated bigint not null,
    data text not null,
    primary key (tenant_id, project_id)
);

create table zcode.input_history (
    tenant_id text not null,
    id text not null,
    project_id text not null,
    session_id text,
    text text not null,
    kind text not null,
    time_created bigint not null,
    attachments text,
    row_seq bigint generated always as identity,
    primary key (tenant_id, id)
);
create index input_history_project_time_idx on zcode.input_history (tenant_id, project_id, time_created desc, id desc);
create index input_history_time_idx on zcode.input_history (tenant_id, time_created desc, id desc);

create table zcode.local_setting (
    tenant_id text not null,
    scope text not null,
    scope_id text not null,
    namespace text not null,
    key text not null,
    value text not null,
    schema_version bigint not null,
    time_created bigint not null,
    time_updated bigint not null,
    primary key (tenant_id, scope, scope_id, namespace, key)
);
create index local_setting_scope_idx on zcode.local_setting (tenant_id, scope, scope_id);
create index local_setting_namespace_key_idx on zcode.local_setting (tenant_id, namespace, key);

create table zcode.session_target (
    tenant_id text not null,
    session_id text not null,
    target_id text not null,
    objective text not null,
    status text not null check (status in ('active', 'paused', 'budget_limited', 'complete')),
    token_budget bigint,
    tokens_used bigint not null default 0,
    time_used_seconds bigint not null default 0,
    time_created bigint not null,
    time_updated bigint not null,
    summary_title text,
    active_input_id text,
    active_run_started_at bigint,
    active_run_last_seen_at bigint,
    primary key (tenant_id, session_id),
    foreign key (tenant_id, session_id) references zcode.session (tenant_id, id) on delete cascade
);

create table zcode.workflow_definition (
    tenant_id text not null,
    id text not null,
    name text not null,
    source text not null check (source in ('builtin', 'user')),
    trusted bigint not null default 0 check (trusted in (0, 1)),
    enabled bigint not null default 1 check (enabled in (0, 1)),
    script_path text,
    script_hash text not null,
    meta_json text not null,
    time_created bigint not null,
    time_updated bigint not null,
    scope text not null default 'explicit' check (scope in ('builtin', 'explicit', 'project', 'user')),
    primary key (tenant_id, id)
);
create index workflow_definition_source_idx on zcode.workflow_definition (tenant_id, source, enabled);

create table zcode.workflow_run (
    tenant_id text not null,
    id text not null,
    definition_id text,
    name text not null,
    kind text not null default 'script',
    parent_session_id text,
    cwd text not null,
    script_path text,
    script_hash text not null,
    args_json text,
    args_hash text,
    status text not null check (status in ('pending', 'running', 'paused', 'completed', 'failed', 'cancelled')),
    current_phase text,
    budget_total bigint,
    budget_spent bigint not null default 0,
    stats_json text,
    failure_json text,
    time_created bigint not null,
    time_started bigint,
    time_updated bigint not null,
    time_completed bigint,
    primary key (tenant_id, id),
    foreign key (tenant_id, parent_session_id) references zcode.session (tenant_id, id)
        on delete set null (parent_session_id)
);
create index workflow_run_parent_session_idx on zcode.workflow_run (tenant_id, parent_session_id);
create index workflow_run_cwd_status_idx on zcode.workflow_run (tenant_id, cwd, status, time_updated desc);
create index workflow_run_definition_idx on zcode.workflow_run (tenant_id, definition_id);

create table zcode.workflow_activity (
    tenant_id text not null,
    id text not null,
    run_id text not null,
    parent_activity_id text,
    call_index bigint not null,
    call_path text not null,
    attempt bigint not null default 1,
    type text not null,
    phase text,
    label text,
    input_hash text not null,
    prompt text,
    opts_json text,
    status text not null check (status in ('queued', 'running', 'completed', 'failed', 'skipped', 'cancelled', 'cached', 'lost')),
    child_session_id text,
    result_json text,
    error_json text,
    time_created bigint not null,
    time_started bigint,
    time_updated bigint not null,
    time_completed bigint,
    primary key (tenant_id, id),
    unique (tenant_id, run_id, call_path, attempt),
    foreign key (tenant_id, run_id) references zcode.workflow_run (tenant_id, id) on delete cascade,
    foreign key (tenant_id, child_session_id) references zcode.session (tenant_id, id)
        on delete set null (child_session_id)
);
create index workflow_activity_run_status_idx on zcode.workflow_activity (tenant_id, run_id, status, call_index);
create index workflow_activity_child_session_idx on zcode.workflow_activity (tenant_id, child_session_id);

create table zcode.workflow_event (
    tenant_id text not null,
    id text not null,
    run_id text not null,
    sequence bigint not null,
    type text not null,
    phase text,
    activity_id text,
    payload_json text,
    time_created bigint not null,
    primary key (tenant_id, id),
    unique (tenant_id, run_id, sequence),
    foreign key (tenant_id, run_id) references zcode.workflow_run (tenant_id, id) on delete cascade,
    foreign key (tenant_id, activity_id) references zcode.workflow_activity (tenant_id, id)
        on delete set null (activity_id)
);
create index workflow_event_run_sequence_idx on zcode.workflow_event (tenant_id, run_id, sequence);

create table zcode.session_task_link (
    tenant_id text not null,
    id text not null,
    root_workflow_run_id text,
    parent_link_id text,
    activity_id text,
    parent_session_id text,
    child_session_id text not null,
    role text not null,
    depth bigint not null default 0,
    path text not null,
    phase text,
    label text,
    agent_type text,
    model text,
    status text not null,
    time_created bigint not null,
    time_updated bigint not null,
    primary key (tenant_id, id),
    unique (tenant_id, child_session_id),
    foreign key (tenant_id, root_workflow_run_id) references zcode.workflow_run (tenant_id, id) on delete cascade,
    foreign key (tenant_id, parent_link_id) references zcode.session_task_link (tenant_id, id) on delete cascade,
    foreign key (tenant_id, activity_id) references zcode.workflow_activity (tenant_id, id)
        on delete set null (activity_id),
    foreign key (tenant_id, parent_session_id) references zcode.session (tenant_id, id)
        on delete set null (parent_session_id),
    foreign key (tenant_id, child_session_id) references zcode.session (tenant_id, id) on delete cascade
);
create index session_task_link_root_workflow_idx on zcode.session_task_link (tenant_id, root_workflow_run_id, depth, path);
create index session_task_link_parent_idx on zcode.session_task_link (tenant_id, parent_link_id);
create index session_task_link_activity_idx on zcode.session_task_link (tenant_id, activity_id);

create table zcode.model_usage (
    tenant_id text not null,
    id text not null,
    logical_request_id text not null,
    attempt_index bigint not null default 0,
    session_id text not null,
    turn_id text,
    trace_id text,
    span_id text,
    assistant_message_id text,
    parent_user_message_id text,
    query_source text not null,
    provider_id text not null,
    model_id text not null,
    variant text,
    agent text,
    mode text,
    task_type text,
    status text not null check (status in ('running', 'completed', 'error', 'cancelled')),
    started_at bigint not null,
    first_token_at bigint,
    completed_at bigint,
    duration_ms bigint,
    time_to_first_token_ms bigint,
    finish_reason text,
    tool_call_count bigint not null default 0,
    input_tokens bigint not null default 0,
    output_tokens bigint not null default 0,
    reasoning_tokens bigint not null default 0,
    cache_creation_input_tokens bigint not null default 0,
    cache_read_input_tokens bigint not null default 0,
    provider_total_tokens bigint,
    computed_total_tokens bigint not null default 0,
    retry_count bigint not null default 0,
    retryable bigint not null default 0 check (retryable in (0, 1)),
    cancelled_by_user bigint not null default 0 check (cancelled_by_user in (0, 1)),
    context_exceeded bigint not null default 0 check (context_exceeded in (0, 1)),
    error_type text,
    error_code text,
    error_message text,
    raw_usage_json text,
    provider_metadata_json text,
    primary key (tenant_id, id),
    foreign key (tenant_id, session_id) references zcode.session (tenant_id, id) on delete cascade
);
create index model_usage_started_model_idx on zcode.model_usage (tenant_id, started_at, provider_id, model_id);
create index model_usage_session_turn_idx on zcode.model_usage (tenant_id, session_id, turn_id);
create index model_usage_trace_idx on zcode.model_usage (tenant_id, trace_id);
create index model_usage_query_source_idx on zcode.model_usage (tenant_id, query_source);

create table zcode.turn_usage (
    tenant_id text not null,
    session_id text not null,
    turn_id text not null,
    trace_id text,
    user_message_id text,
    status text not null check (status in ('running', 'completed', 'error', 'cancelled')),
    started_at bigint not null,
    first_model_start_at bigint,
    first_token_at bigint,
    completed_at bigint,
    duration_ms bigint,
    time_to_first_token_ms bigint,
    model_request_count bigint not null default 0,
    model_retry_count bigint not null default 0,
    tool_call_count bigint not null default 0,
    tool_error_count bigint not null default 0,
    input_tokens bigint not null default 0,
    output_tokens bigint not null default 0,
    reasoning_tokens bigint not null default 0,
    cache_creation_input_tokens bigint not null default 0,
    cache_read_input_tokens bigint not null default 0,
    computed_total_tokens bigint not null default 0,
    retryable bigint not null default 0 check (retryable in (0, 1)),
    cancelled_by_user bigint not null default 0 check (cancelled_by_user in (0, 1)),
    context_exceeded bigint not null default 0 check (context_exceeded in (0, 1)),
    error_type text,
    error_code text,
    primary key (tenant_id, session_id, turn_id),
    foreign key (tenant_id, session_id) references zcode.session (tenant_id, id) on delete cascade
);
create index turn_usage_started_idx on zcode.turn_usage (tenant_id, started_at);

create table zcode.tool_usage (
    tenant_id text not null,
    id text not null,
    session_id text not null,
    turn_id text,
    trace_id text,
    tool_call_id text not null,
    tool_name text not null,
    side_effect_scope text,
    read_only bigint check (read_only in (0, 1)),
    destructive bigint check (destructive in (0, 1)),
    approval_status text,
    status text not null check (status in ('running', 'completed', 'error', 'cancelled')),
    started_at bigint not null,
    first_output_at bigint,
    completed_at bigint,
    duration_ms bigint,
    time_to_first_output_ms bigint,
    exit_code bigint,
    output_bytes bigint not null default 0,
    stdout_bytes bigint not null default 0,
    stderr_bytes bigint not null default 0,
    truncated bigint not null default 0 check (truncated in (0, 1)),
    retry_count bigint not null default 0,
    retryable bigint not null default 0 check (retryable in (0, 1)),
    cancelled_by_user bigint not null default 0 check (cancelled_by_user in (0, 1)),
    error_type text,
    error_code text,
    error_message text,
    primary key (tenant_id, id),
    unique (tenant_id, session_id, tool_call_id),
    foreign key (tenant_id, session_id) references zcode.session (tenant_id, id) on delete cascade
);
create index tool_usage_started_tool_idx on zcode.tool_usage (tenant_id, started_at, tool_name);
create index tool_usage_session_turn_idx on zcode.tool_usage (tenant_id, session_id, turn_id);

create table zcode.session_input (
    tenant_id text not null,
    id text not null,
    session_id text not null,
    kind text not null,
    delivery text not null check (delivery in ('startNow', 'guide', 'queue')),
    payload text not null,
    admitted_sequence bigint not null,
    promoted_sequence bigint,
    promoted_message_id text,
    status text not null check (status in ('admitted', 'promoted', 'cancelled', 'discarded', 'failed')),
    status_reason text,
    time_created bigint not null,
    time_updated bigint not null,
    row_seq bigint generated always as identity,
    primary key (tenant_id, id),
    foreign key (tenant_id, session_id) references zcode.session (tenant_id, id) on delete cascade
);
create index session_input_session_admitted_idx on zcode.session_input (tenant_id, session_id, admitted_sequence);
create index session_input_session_status_idx on zcode.session_input (tenant_id, session_id, status);

create table zcode.dwf_run (
    tenant_id text not null,
    id text not null,
    parent_session_id text,
    cwd text,
    name text,
    script_text text,
    script_hash text,
    args_json text,
    tool_call_id text,
    resumed_from text,
    caps_max_concurrency bigint not null,
    spent_tokens bigint not null default 0,
    status text not null check (status in ('pending', 'running', 'completed', 'failed', 'cancelled')),
    result_json text,
    failure_json text,
    time_created bigint not null,
    time_updated bigint not null,
    primary key (tenant_id, id)
);
create index dwf_run_cwd_idx on zcode.dwf_run (tenant_id, cwd, time_updated);

create table zcode.dwf_actor (
    tenant_id text not null,
    id bigint generated by default as identity,
    run_id text not null,
    site_id text not null,
    ordinal bigint not null,
    name text,
    persona_json text,
    resolved_model text,
    session_id text,
    time_created bigint not null,
    time_updated bigint not null,
    primary key (tenant_id, id),
    unique (tenant_id, run_id, site_id, ordinal),
    foreign key (tenant_id, run_id) references zcode.dwf_run (tenant_id, id) on delete cascade
);
create index dwf_actor_run_idx on zcode.dwf_actor (tenant_id, run_id);

create table zcode.dwf_node (
    tenant_id text not null,
    id bigint generated by default as identity,
    run_id text not null,
    site_id text not null,
    ordinal bigint not null,
    kind text not null check (kind in ('ask', 'world-read', 'world-run', 'report', 'artifact')),
    actor_site_id text,
    actor_ordinal bigint,
    actor_seq bigint,
    input_hash text not null,
    input_json text,
    status text not null check (status in ('running', 'completed', 'failed')),
    result_json text,
    error_json text,
    stats_json text,
    message_boundary bigint,
    artifact_id text,
    time_created bigint not null,
    time_updated bigint not null,
    primary key (tenant_id, id),
    unique (tenant_id, run_id, site_id, ordinal),
    foreign key (tenant_id, run_id) references zcode.dwf_run (tenant_id, id) on delete cascade
);
create index dwf_node_run_idx on zcode.dwf_node (tenant_id, run_id);
create index dwf_node_artifact_idx on zcode.dwf_node (tenant_id, run_id, artifact_id);

create table zcode.dwf_event (
    tenant_id text not null,
    id bigint generated by default as identity,
    run_id text not null,
    sequence bigint not null,
    type text not null,
    payload_json text not null,
    time_created bigint not null,
    primary key (tenant_id, id),
    unique (tenant_id, run_id, sequence),
    foreign key (tenant_id, run_id) references zcode.dwf_run (tenant_id, id) on delete cascade
);

create table zcode.zcode_database_identity (
    tenant_id text not null,
    scope text not null,
    identity text not null,
    primary key (tenant_id, scope)
);

-- 只在云端：写入回执（§4），按 write_id 核对重复写入与提交结果未知。
create table zcode.zcode_write_receipt (
    tenant_id text not null,
    write_id text not null,
    session_id text,
    operation text not null,
    fingerprint text not null,
    result text not null,
    time_created bigint not null,
    primary key (tenant_id, write_id),
    foreign key (tenant_id, session_id) references zcode.session (tenant_id, id) on delete cascade
);

-- 业务角色不是 owner；FORCE 同时防止以后误用表 owner 的普通连接绕过策略。
do $$
declare name text;
begin
    foreach name in array array[
        'session', 'message', 'part', 'todo', 'session_entry', 'permission', 'input_history', 'local_setting',
        'session_target', 'workflow_definition', 'workflow_run', 'workflow_activity', 'workflow_event',
        'session_task_link', 'model_usage', 'turn_usage', 'tool_usage', 'session_input', 'dwf_run', 'dwf_actor',
        'dwf_node', 'dwf_event', 'zcode_database_identity', 'zcode_write_receipt'
    ] loop
        execute format('alter table zcode.%I enable row level security', name);
        execute format('alter table zcode.%I force row level security', name);
        execute format('create policy tenant_scope on zcode.%I using (tenant_id = current_setting(''zcode.tenant_id'', true)) with check (tenant_id = current_setting(''zcode.tenant_id'', true))', name);
    end loop;
end $$;
