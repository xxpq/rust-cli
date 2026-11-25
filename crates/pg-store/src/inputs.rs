//! 输入账本（pg-store.md §3）放在 Node 的 `session_input` 表（§12.2）：`id` 是 command_id，`admitted_sequence` 是接纳顺序，
//! 状态 `admitted` / `promoted` / `cancelled`。`payload` 存原输入、内容摘要与接纳回执（Node 不读本工程写的输入行）。
use crate::entities::session_input as input_entity;
use crate::orm::error as orm_error;
use async_trait::async_trait;
use rust_cli_base::js::json::stringify;
use rust_cli_kernel_api::ids::{CommandId, MessageId, SessionId};
use rust_cli_kernel_api::input_journal::{
    InputAdmission, InputJournal, InputState, JournalInput, is_journal_input, promoted,
};
use rust_cli_kernel_api::session_api::{Ack, Command, CommandEnvelope, Delivery};
use rust_cli_kernel_api::store::{Fact, StoreError};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, Set};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::transaction::Tx;
use crate::{PgSessionStore, encode, fingerprint, number, storage, unsigned};

/// `session_input.payload` 的内容。
#[derive(Serialize, Deserialize)]
struct Payload {
    envelope: CommandEnvelope,
    fingerprint: String,
    receipt: Ack,
}

fn delivery(envelope: &CommandEnvelope) -> &'static str {
    match &envelope.command {
        Command::Submit { delivery, .. } | Command::SubmitContent { delivery, .. } => {
            match delivery {
                Delivery::Queue => "queue",
                Delivery::Steer => "guide",
            }
        }
        _ => "queue",
    }
}

fn input(row: input_entity::Model) -> Result<(JournalInput, String), StoreError> {
    let payload: Payload =
        serde_json::from_str(&row.payload).map_err(|_| storage("invalid stored input"))?;
    Ok((
        JournalInput {
            envelope: payload.envelope,
            sequence: unsigned(row.admitted_sequence)?,
            state: match row.status.as_str() {
                "admitted" => InputState::Pending,
                "promoted" => InputState::Promoted,
                "cancelled" => InputState::Cancelled,
                _ => return Err(storage("invalid input state")),
            },
            receipt: payload.receipt,
            promoted_message: row.promoted_message_id.map(MessageId::new),
        },
        payload.fingerprint,
    ))
}

impl PgSessionStore {
    async fn stored_input(
        &self,
        tx: &Tx,
        session: &SessionId,
        command: &str,
    ) -> Result<Option<input_entity::Model>, StoreError> {
        self.find::<input_entity::Entity>()
            .filter(input_entity::Column::SessionId.eq(session.as_str()))
            .filter(input_entity::Column::Id.eq(command))
            .one(tx)
            .await
            .map_err(orm_error)
    }
}

#[async_trait]
impl InputJournal for PgSessionStore {
    async fn admit(
        &self,
        session: &SessionId,
        envelope: &CommandEnvelope,
    ) -> Result<InputAdmission, StoreError> {
        if !is_journal_input(&envelope.command) {
            return Err(storage("command is not a durable input"));
        }
        // delivery 由在线阶段裁决，重连重试时可从 steer 变为 queue；内容身份不包含它。
        let mut identity = envelope.clone();
        match &mut identity.command {
            Command::Submit { delivery, .. } | Command::SubmitContent { delivery, .. } => {
                *delivery = Delivery::Queue
            }
            _ => {}
        }
        let hash = fingerprint(&identity)?;
        let mut tx = self.begin(false).await?;
        self.live(&mut tx, session, true).await?;
        if let Some(row) = self
            .stored_input(&tx, session, envelope.id.as_str())
            .await?
        {
            let (input, stored) = input(row)?;
            if stored != hash {
                return Err(StoreError::Conflict {
                    command_id: envelope.id.as_str().into(),
                });
            }
            return Ok(InputAdmission {
                input,
                duplicate: true,
            });
        }
        // 会话行已加锁：同一会话的接纳顺序在锁内取 max + 1。
        let sequence: i64 = crate::orm::native(
            "select coalesce(max(admitted_sequence), 0) + 1 as next from zcode.session_input
             where tenant_id = $1 and session_id = $2",
        )
        .bind(self.tenant())
        .bind(session.as_str())
        .fetch_one(&tx)
        .await
        .map_err(orm_error)?
        .get("next");
        let receipt = Ack::Accepted {
            result: rust_cli_kernel_api::session_api::CommandResult::Done,
        };
        let now = number(self.clock.now_ms())?;
        let payload = Payload {
            envelope: envelope.clone(),
            fingerprint: hash,
            receipt: receipt.clone(),
        };
        input_entity::Entity::insert(self.scoped(input_entity::ActiveModel {
            id: Set(envelope.id.as_str().into()),
            session_id: Set(session.as_str().into()),
            kind: Set("rust-cli/command".into()),
            delivery: Set(delivery(envelope).into()),
            payload: Set(stringify(&encode(&payload)?)),
            admitted_sequence: Set(sequence),
            promoted_sequence: Set(None),
            promoted_message_id: Set(None),
            status: Set("admitted".into()),
            status_reason: Set(None),
            time_created: Set(now),
            time_updated: Set(now),
            ..Default::default()
        })?)
        .exec(&tx)
        .await
        .map_err(orm_error)?;
        let result = InputAdmission {
            input: JournalInput {
                envelope: envelope.clone(),
                sequence: unsigned(sequence)?,
                state: InputState::Pending,
                receipt,
                promoted_message: None,
            },
            duplicate: false,
        };
        self.finish(tx).await?;
        Ok(result)
    }

    async fn pending(&self, session: &SessionId) -> Result<Vec<JournalInput>, StoreError> {
        let mut tx = self.begin(true).await?;
        self.live(&mut tx, session, false).await?;
        let rows = self
            .find::<input_entity::Entity>()
            .filter(input_entity::Column::SessionId.eq(session.as_str()))
            .filter(input_entity::Column::Status.eq("admitted"))
            .order_by_asc(input_entity::Column::AdmittedSequence)
            .all(&tx)
            .await
            .map_err(orm_error)?;
        let result = rows
            .into_iter()
            .map(|row| input(row).map(|(input, _)| input))
            .collect::<Result<_, _>>()?;
        tx.commit().await.map_err(orm_error)?;
        Ok(result)
    }

    async fn input(
        &self,
        session: &SessionId,
        command: &CommandId,
    ) -> Result<Option<JournalInput>, StoreError> {
        let mut tx = self.begin(true).await?;
        self.live(&mut tx, session, false).await?;
        let row = self.stored_input(&tx, session, command.as_str()).await?;
        let result = row.map(input).transpose()?.map(|(input, _)| input);
        tx.commit().await.map_err(orm_error)?;
        Ok(result)
    }

    async fn cancel(&self, session: &SessionId, commands: &[CommandId]) -> Result<(), StoreError> {
        let mut tx = self.begin(false).await?;
        self.live(&mut tx, session, true).await?;
        let now = number(self.clock.now_ms())?;
        for command in commands {
            let state = self
                .stored_input(&tx, session, command.as_str())
                .await?
                .map(|row| row.status);
            if state.as_deref().is_none_or(|state| state == "promoted") {
                return Err(StoreError::Conflict {
                    command_id: command.as_str().into(),
                });
            }
            self.update::<input_entity::Entity>()
                .col_expr(input_entity::Column::Status, Expr::val("cancelled"))
                .col_expr(input_entity::Column::TimeUpdated, Expr::val(now))
                .filter(input_entity::Column::SessionId.eq(session.as_str()))
                .filter(input_entity::Column::Id.eq(command.as_str()))
                .filter(input_entity::Column::Status.eq("admitted"))
                .exec(&tx)
                .await
                .map_err(orm_error)?;
        }
        self.finish(tx).await
    }

    /// 输入的提升与消息在同一个事务里（§4）：随消息的那一批带上 `Fact::InputsPromoted`，`apply` 里处理。
    fn promotion(&self, commands: Vec<CommandId>, message: &MessageId) -> Vec<Fact> {
        promoted(commands, message)
    }
}

impl PgSessionStore {
    /// `Fact::InputsPromoted`：这些输入已提升为 `message`，与消息同事务（§4）。
    pub(crate) async fn promote_inputs(
        &self,
        tx: &Tx,
        session: &SessionId,
        commands: &[CommandId],
        message: &MessageId,
    ) -> Result<(), StoreError> {
        // 插话是 queued_command attachment，本地斜杠输入也可能展开成其他持久消息：只要求消息已落库。
        let stored = self
            .find::<crate::entities::message::Entity>()
            .filter(crate::entities::message::Column::Id.eq(message.as_str()))
            .filter(crate::entities::message::Column::SessionId.eq(session.as_str()))
            .one(tx)
            .await
            .map_err(orm_error)?;
        if stored.is_none() {
            return Err(storage(format!("promoted message {message} is not stored")));
        }
        let now = number(self.clock.now_ms())?;
        for command in commands {
            let changed = self
                .update::<input_entity::Entity>()
                .col_expr(input_entity::Column::Status, Expr::val("promoted"))
                .col_expr(
                    input_entity::Column::PromotedMessageId,
                    Expr::val(message.as_str()),
                )
                .col_expr(input_entity::Column::TimeUpdated, Expr::val(now))
                .filter(input_entity::Column::SessionId.eq(session.as_str()))
                .filter(input_entity::Column::Id.eq(command.as_str()))
                .filter(
                    Condition::any()
                        .add(input_entity::Column::Status.eq("admitted"))
                        .add(
                            Condition::all()
                                .add(input_entity::Column::Status.eq("promoted"))
                                .add(input_entity::Column::PromotedMessageId.eq(message.as_str())),
                        ),
                )
                .exec(tx)
                .await
                .map_err(orm_error)?
                .rows_affected;
            if changed == 0 {
                return Err(StoreError::Conflict {
                    command_id: command.as_str().into(),
                });
            }
        }
        Ok(())
    }
}
