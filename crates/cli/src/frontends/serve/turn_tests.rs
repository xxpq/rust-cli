//! serve 的一轮：用户目录名。

use super::*;

#[test]
fn plain_user_ids_name_their_directory_and_others_are_hashed() {
    assert_eq!(user_dir("12345"), "12345");
    assert_eq!(user_dir("user.name_1-x"), "user.name_1-x");
    for odd in ["../etc", "a/b", "..", ".", "", "名字", &"x".repeat(65)] {
        let dir = user_dir(odd);
        assert!(
            dir.starts_with("u-") && dir.len() == 2 + USER_DIGEST_LEN,
            "{odd:?} → {dir}"
        );
    }
    assert_eq!(user_dir("a/b"), user_dir("a/b"));
    assert_ne!(user_dir("a/b"), user_dir("a/c"));
}
