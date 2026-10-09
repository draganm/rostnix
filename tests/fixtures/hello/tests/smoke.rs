#[test]
fn greeting_is_expected_json() {
    assert_eq!(hello::greeting().unwrap(), r#"{"greeting":"hello","n":42}"#);
}
