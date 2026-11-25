//! 本机项目规则集（docs/specs/rust-cli/shell-permissions.md §5）：Node
//! `repositories/local-settings.ts` 的 `getProjectPermission` / `saveProjectPermission` 与
//! `core/src/tool/executor/permission-rules.ts` 的 `applyPermissionUpdates`。
//!
//! 值存在 `local_setting(scope='project', scope_id=<projectId>, namespace='permission', key='ruleset')`，
//! 没有这一行时读旧表 `permission(project_id)`。值是 `{version: 1, allow?, deny?, ask?, …}`，规则是
//! `{toolName, ruleContent?}`。写入只改 `local_setting`（Node 相同），键序同 Node 的对象展开。
//!
//! Node 群协作会话（修复线 F2，Node `loadProjectPermissionRuleset` / `persistProjectPermissionUpdates`）：会话行的
//! `permission.scope` 是 `session` 时，规则集就是会话行的 `permission`（同样的键），“总是允许”只写它，不读也不写
//! 项目规则集。以前续写这样的会话时规则写进项目、读的也是项目的。

use rust_cli_base::js::json::stringify;
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::permission::{PermissionRule, ProjectRules, RuleBehavior};
use rust_cli_kernel_api::store::StoreError;
use serde_json::{Map, Value, json};

use crate::facts::Commit;
use crate::rows::Rows;
use crate::storage;

/// Node 存的值（JSON 文本）：`local_setting` 优先，没有时读旧 `permission` 表；都没有时为 None。
fn stored(conn: &dyn Rows, project: &str) -> Result<Option<String>, StoreError> {
    let setting = conn.read_setting("project", project, "permission", "ruleset")?;
    if setting.is_some() {
        return Ok(setting);
    }
    conn.legacy_permission(project)
}

fn rules(value: &Value, key: &str) -> Vec<PermissionRule> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let tool = item.get("toolName")?.as_str()?.to_owned();
                    let content = item
                        .get("ruleContent")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    Some(PermissionRule { tool, content })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 读本项目的规则集；值读不出时按空处理（写入时才报错，不覆盖）。
pub fn read(conn: &dyn Rows, project: &str) -> Result<ProjectRules, StoreError> {
    let Some(text) = stored(conn, project)? else {
        return Ok(ProjectRules::default());
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Ok(ProjectRules::default());
    };
    Ok(parsed(&value))
}

/// 规则集里记住的附加工作目录的键（permission-updates.md §2.3）。Node 不认，
/// 写回时保留它（`{...当前, version: 1}`）。
const DIRECTORIES: &str = "additionalDirectories";

fn parsed(value: &Value) -> ProjectRules {
    let directories = value
        .get(DIRECTORIES)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(EnvPath::new))
                .collect()
        })
        .unwrap_or_default();
    ProjectRules {
        allow: rules(value, "allow"),
        deny: rules(value, "deny"),
        ask: rules(value, "ask"),
        additional_directories: directories,
    }
}

/// 会话行的 `permission`，是 Node 群协作授权（`scope = "session"`）时为 Some。
pub fn scoped(permission: Option<&Value>) -> Option<&Map<String, Value>> {
    permission?
        .as_object()
        .filter(|permission| permission.get("scope").and_then(Value::as_str) == Some("session"))
}

/// Node 群协作会话自己的规则集（`SessionStore::session_rules`）；其他会话为 None。
pub fn session_read(conn: &dyn Rows, session: &str) -> Result<Option<ProjectRules>, StoreError> {
    let row = conn.session(session)?;
    Ok(row
        .as_ref()
        .and_then(|row| scoped(row.permission.as_ref()))
        .map(|permission| parsed(&Value::Object(permission.clone()))))
}

fn behavior_key(behavior: RuleBehavior) -> &'static str {
    match behavior {
        RuleBehavior::Allow => "allow",
        RuleBehavior::Deny => "deny",
        RuleBehavior::Ask => "ask",
    }
}

fn node_rule(rule: &PermissionRule) -> Value {
    let mut object = Map::new();
    object.insert("toolName".into(), json!(rule.tool));
    if let Some(content) = &rule.content {
        object.insert("ruleContent".into(), json!(content));
    }
    Value::Object(object)
}

/// Node 的去重键 `toolName + "\0" + (ruleContent ?? "")`。
fn rule_key(rule: &Value) -> String {
    let name = rule
        .get("toolName")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let content = rule
        .get("ruleContent")
        .and_then(Value::as_str)
        .unwrap_or_default();
    format!("{name}\u{0}{content}")
}

/// 一类规则的改法：Node `applyPermissionUpdates` 的追加；`/permissions` 与 hook 的 `removeRules` 的删除（Node 没有删除，按同一个
/// 去重键去掉，tui-manage.md §1.1）；hook 的 `replaceRules` 先清空（permission-updates.md §2.3）。
pub struct Edit<'a> {
    pub clear: bool,
    pub added: &'a [PermissionRule],
    pub removed: &'a [PermissionRule],
}

impl<'a> Edit<'a> {
    pub fn add(added: &'a [PermissionRule]) -> Self {
        Self {
            clear: false,
            added,
            removed: &[],
        }
    }

    pub fn replace(rules: &'a [PermissionRule]) -> Self {
        Self {
            clear: true,
            added: rules,
            removed: &[],
        }
    }

    pub fn remove(removed: &'a [PermissionRule]) -> Self {
        Self {
            clear: false,
            added: &[],
            removed,
        }
    }
}

/// `{...当前, version: 1}`，再改 `behavior` 键：先删（或清空）再追加并去重（保留先出现的）。
pub fn apply(current: Map<String, Value>, behavior: RuleBehavior, edit: &Edit<'_>) -> Value {
    let Edit {
        clear,
        added,
        removed,
    } = *edit;
    let mut next = current;
    next.insert("version".into(), json!(1));
    let key = behavior_key(behavior);
    let existing = if clear {
        Vec::new()
    } else {
        next.get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let gone: std::collections::HashSet<String> = removed
        .iter()
        .map(|rule| rule_key(&node_rule(rule)))
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut deduped = Vec::new();
    for rule in existing
        .into_iter()
        .filter(|rule| !gone.contains(&rule_key(rule)))
        .chain(added.iter().map(node_rule))
    {
        if seen.insert(rule_key(&rule)) {
            deduped.push(rule);
        }
    }
    next.insert(key.into(), Value::Array(deduped));
    Value::Object(next)
}

/// 在提交事务里改本会话所在项目的规则集（Node `persistProjectPermissionUpdates`；替换与删除是 hook 的更新，
/// permission-updates.md §2.3）。当前值读不出时整批失败，不覆盖（Node 在这里抛错，这次授权没有记住）。
pub fn edit(ctx: &Commit<'_>, behavior: RuleBehavior, edit: &Edit<'_>) -> Result<(), StoreError> {
    let row = ctx.conn.session(ctx.session)?;
    let scoped = row
        .as_ref()
        .and_then(|row| scoped(row.permission.as_ref()))
        .cloned();
    if let Some(permission) = scoped {
        let update = crate::rows::Update {
            id: ctx.session.to_owned(),
            permission: Some(apply(permission, behavior, edit)),
            ..Default::default()
        };
        ctx.conn.update_session(&update, ctx.now as i64)?;
        return Ok(());
    }
    let project = session_project(ctx)?;
    let current = current_object(ctx.conn, &project)?;
    let next = apply(current, behavior, edit);
    write_setting(ctx, &project, "ruleset", &next)
}

/// `{...当前, version: 1}`，`additionalDirectories` 先去掉 `removed`、再追加去重 `added`（permission-updates.md §2.3）。
fn directories(current: Map<String, Value>, added: &[EnvPath], removed: &[EnvPath]) -> Value {
    let mut next = current;
    next.insert("version".into(), json!(1));
    let mut kept: Vec<Value> = next
        .get(DIRECTORIES)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|dir| {
            !removed
                .iter()
                .any(|gone| dir.as_str() == Some(gone.as_str()))
        })
        .collect();
    for dir in added {
        let value = json!(dir.as_str());
        if !kept.contains(&value) {
            kept.push(value);
        }
    }
    next.insert(DIRECTORIES.into(), Value::Array(kept));
    Value::Object(next)
}

/// 在提交事务里改本会话所在项目的记住的目录。总写项目的规则集：Node 群协作会话的会话行只有规则与模式。
pub fn remember(
    ctx: &Commit<'_>,
    added: &[EnvPath],
    removed: &[EnvPath],
) -> Result<(), StoreError> {
    let project = session_project(ctx)?;
    let current = current_object(ctx.conn, &project)?;
    write_setting(
        ctx,
        &project,
        "ruleset",
        &directories(current, added, removed),
    )
}

/// 没有会话时记住目录（`LocalSettings::remember_directories`）：调用方负责事务（本机 `begin immediate`），在事务里读、改、upsert。
pub fn remember_in(
    conn: &dyn Rows,
    project: &str,
    now: u64,
    added: &[EnvPath],
) -> Result<(), StoreError> {
    let current = current_object(conn, project)?;
    upsert(
        conn,
        now,
        project,
        "ruleset",
        &directories(current, added, &[]),
    )
}

/// 当前规则集的 JSON 对象；没有时为空对象，不是对象时报错（不覆盖）。
pub fn current_object(conn: &dyn Rows, project: &str) -> Result<Map<String, Value>, StoreError> {
    match stored(conn, project)? {
        None => Ok(Map::new()),
        Some(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(object)) => Ok(object),
            Ok(_) | Err(_) => Err(storage(format!(
                "the permission ruleset of {project} is not a JSON object"
            ))),
        },
    }
}

/// `/permissions` 的修改（tui-manage.md §1.1）：不经会话，调用方负责事务（本机 `begin immediate`）；读、改、upsert，返回落库后的
/// 规则集。
pub fn change(
    conn: &dyn Rows,
    project: &str,
    now: u64,
    change: &rust_cli_kernel_api::store::ProjectRulesChange,
) -> Result<ProjectRules, StoreError> {
    let current = current_object(conn, project)?;
    let edit = Edit {
        clear: false,
        added: &change.add,
        removed: &change.remove,
    };
    let next = apply(current, change.behavior, &edit);
    upsert(conn, now, project, "ruleset", &next)?;
    read(conn, project)
}

/// 本会话所在项目的 ID（会话行的 `project_id`）。
pub fn session_project(ctx: &Commit<'_>) -> Result<String, StoreError> {
    ctx.conn
        .session_project_id(ctx.session)?
        .ok_or_else(|| storage("session project missing"))
}

/// Node `writeLocalSetting`：`local_setting(project, <项目>, permission, <key>)` 的 upsert，
/// `schema_version` 1，已有行只改 value、schema_version 与 time_updated。
pub fn write_setting(
    ctx: &Commit<'_>,
    project: &str,
    key: &str,
    value: &Value,
) -> Result<(), StoreError> {
    upsert(ctx.conn, ctx.now, project, key, value)
}

pub fn upsert(
    conn: &dyn Rows,
    now: u64,
    project: &str,
    key: &str,
    value: &Value,
) -> Result<(), StoreError> {
    let now = i64::try_from(now).unwrap_or(i64::MAX);
    conn.write_setting(
        ("project", project, "permission", key),
        stringify(value),
        now,
    )?;
    Ok(())
}
