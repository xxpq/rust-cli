//! 两种单引号写法（原来分别在 proc、hooks、ssh-connect 与 tools、env-local 的用例）。

use super::*;

#[test]
fn single_quote_always_quotes() {
    assert_eq!(single_quote("it's"), "'it'\\''s'");
    assert_eq!(single_quote("/tmp/w/dir"), "'/tmp/w/dir'");
    assert_eq!(single_quote(""), "''");
}

#[test]
fn quote_leaves_plain_words_alone() {
    assert_eq!(quote("/tmp/w/dir"), "/tmp/w/dir");
    assert_eq!(quote("/usr/local/bin/zcode"), "/usr/local/bin/zcode");
    assert_eq!(quote("C:/x+y,z@w=v"), "C:/x+y,z@w=v");
    assert_eq!(quote("/tmp/my dir"), "'/tmp/my dir'");
    assert_eq!(quote("/a b/zcode"), "'/a b/zcode'");
    assert_eq!(quote("it's"), "'it'\"'\"'s'");
    assert_eq!(quote("/it's/zcode"), r#"'/it'"'"'s/zcode'"#);
    assert_eq!(quote(""), "''");
}
