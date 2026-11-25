use super::*;

#[test]
fn the_names_round_trip_like_encode_uri_component() {
    let key = if cfg!(windows) {
        r"C:\Users\dev\my app"
    } else {
        "/Users/dev/my app (1)"
    };
    let topic = workspace_topic(key, WORKSPACE_AGENT);
    if !cfg!(windows) {
        // 与 JS 的 encodeURIComponent("/Users/dev/my app (1)") 相同
        assert_eq!(topic, "ws/%2FUsers%2Fdev%2Fmy%20app%20(1)/agent");
    }
    assert_eq!(
        parse(&topic),
        Ok(Topic::WorkspaceAgent {
            key: key.to_owned()
        })
    );
}

#[test]
fn only_the_served_topics_parse() {
    assert_eq!(parse("agent"), Ok(Topic::Agent));
    assert_eq!(parse("nowhere"), Err(TopicError::NotFound));
    // FEED 起会话列表也提供（protocol-v5.md §14）；别的工作区 topic 仍不提供
    if !cfg!(windows) {
        assert_eq!(
            parse("ws/%2Fa/sessions"),
            Ok(Topic::WorkspaceSessions {
                key: "/a".to_owned()
            })
        );
    }
    // REQUEST-VERIFY 起待校验的索引也提供（protocol-v5.md §35）
    if !cfg!(windows) {
        assert_eq!(
            parse("ws/%2Fa/verifications"),
            Ok(Topic::WorkspaceVerifications {
                key: "/a".to_owned()
            })
        );
    }
    assert!(matches!(
        parse("ws/not-absolute/sessions"),
        Err(TopicError::Invalid(_))
    ));
    assert_eq!(parse("ws/%2Fa/config"), Err(TopicError::NotFound));
    assert_eq!(parse("ws//agent"), Err(TopicError::NotFound));
    assert!(matches!(
        parse("ws/not-absolute/agent"),
        Err(TopicError::Invalid(_))
    ));
    assert!(matches!(parse("ws/%ZZ/agent"), Err(TopicError::Invalid(_))));
    assert!(matches!(parse("ws/%FF/agent"), Err(TopicError::Invalid(_))));
}
