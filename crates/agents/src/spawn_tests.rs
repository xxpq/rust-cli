//! 子代理的模型选择（工作线 F49）：团队项目跟父运行。

use rust_cli_base::models::{GLM_5_3_ACCOUNT, GLM_5_3_FLASH_ACCOUNT};
use rust_cli_kernel_api::model::{ModelConnection, ModelSelection};

use super::selection;

#[test]
fn a_child_keeps_the_team_project_of_the_parent() {
    let parent = ModelSelection {
        reasoning: Some("high".to_owned()),
        connection: Some(Box::new(ModelConnection {
            product_id: "prod-1".to_owned(),
            organization_id: "org-t".to_owned(),
            project_id: "p-t".to_owned(),
        })),
        ..ModelSelection::new(
            "account:bigmodel-team-coding-plan".to_owned(),
            GLM_5_3_ACCOUNT.to_owned(),
        )
    };
    assert_eq!(selection(&parent, Some("inherit")), parent);
    let child = selection(&parent, Some(GLM_5_3_FLASH_ACCOUNT));
    assert_eq!(child.model, GLM_5_3_FLASH_ACCOUNT);
    // 换了模型：档位是缺省（平台层取最后一档），不沿用父模型的档位。
    assert_eq!(child.reasoning, None);
    assert_eq!(child.connection, parent.connection);
}

mod execution {
    use rust_cli_base::models::GLM_5_3_ACCOUNT;
    use rust_cli_kernel_api::model::ModelSelection;
    use rust_cli_kernel_api::turn_execution::{
        ExecutionFallback, FallbackRule, RunExecution, TurnExecution,
    };

    use rust_cli_kernel_api::testing::ScriptedModel;

    use crate::runs::RunInfo;
    use crate::spawn::{execution, selections};

    fn fast() -> ModelSelection {
        ModelSelection::new("account:zai-highspeed-card", GLM_5_3_ACCOUNT)
    }

    fn parent(run: &RunExecution) -> RunInfo {
        RunInfo {
            session: rust_cli_kernel_api::ids::SessionId::new("sess_1"),
            workspace: crate::test_support::workspace(),
            selection: ModelSelection::new("account:zai-team-coding-plan", GLM_5_3_ACCOUNT),
            execution: Some(run.clone()),
            preferences: Default::default(),
            prompt: Default::default(),
        }
    }

    fn card() -> RunExecution {
        RunExecution::new(TurnExecution {
            selection: fast(),
            credential: Some("card-1".to_owned()),
            fallback: Some(ExecutionFallback {
                rules: vec![FallbackRule {
                    reason: "failed".to_owned(),
                    provider_code: None,
                }],
                unavailable: "unavailable".to_owned(),
            }),
        })
    }

    /// 父运行还没退回：沿用父运行模型的子代理用执行选择（换上自己的档位）与引用，不带退回规则（照常重试）。
    #[tokio::test]
    async fn a_subagent_follows_the_execution_before_the_fallback() {
        let run = card();
        let model = ScriptedModel::new(Vec::new());
        let (_, followed) = selections(&model, &parent(&run), (false, None), Some("low")).await;
        let followed = followed.unwrap();
        assert_eq!(
            followed.selection,
            ModelSelection {
                reasoning: Some("low".to_owned()),
                ..fast()
            }
        );
        assert_eq!(followed.credential.as_deref(), Some("card-1"));
        assert_eq!(followed.fallback, None);
        assert!(execution(&parent(&run), (false, Some("inherit"))).is_some());
        let (_, forked) = selections(&model, &parent(&run), (true, None), Some("low")).await;
        assert_eq!(
            forked.unwrap().selection,
            fast(),
            "fork 总用父运行的模型与档位"
        );
    }

    /// 退回之后启动的子代理用会话的模型（agents.md §4.1）；指定了别的模型的子代理也是（加速 provider 的目录里只有卡的模型）。
    #[test]
    fn a_subagent_spawned_after_the_fallback_uses_the_session_model() {
        let run = card();
        assert!(execution(&parent(&run), (false, Some("haiku"))).is_none());
        run.fall_back();
        assert!(execution(&parent(&run), (false, None)).is_none());
    }

    /// MO-2（model-options.md §4.4、§12.2，D3）：effort 意图按子代理的模型可选的档位对档（`--effort` 的规则）；对不上时
    /// 跟父运行的档位。修 bug：以前意图原样写进选择，glm-5.1 上 `effort: high` 让子代理的请求报
    /// `reasoning level high is not offered`。
    #[tokio::test]
    async fn the_effort_intent_takes_a_level_the_model_offers() {
        let levels = |names: &[&str]| Some(names.iter().map(|name| (*name).to_owned()).collect());
        let mut info = parent(&RunExecution::new(TurnExecution {
            selection: fast(),
            credential: None,
            fallback: None,
        }));
        info.execution = None;
        info.selection.reasoning = Some("enabled".to_owned());
        let glm = ScriptedModel::new(Vec::new()).levels(levels(&["disabled", "enabled"]));
        let (chosen, _) = selections(&glm, &info, (false, None), Some("high")).await;
        assert_eq!(chosen.reasoning.as_deref(), Some("enabled"));
        let five = ScriptedModel::new(Vec::new()).levels(levels(&["low", "high"]));
        let (chosen, _) = selections(&five, &info, (false, None), Some("max")).await;
        assert_eq!(chosen.reasoning.as_deref(), Some("high"));
        let (chosen, _) = selections(&five, &info, (false, None), None).await;
        assert_eq!(chosen.reasoning.as_deref(), Some("enabled"));
    }
}
