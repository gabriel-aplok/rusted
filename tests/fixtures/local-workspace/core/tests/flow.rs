#[test]
fn flow_uses_dev_dependency() {
    assert_eq!(core::answer(), 42);
    assert_eq!(devdep::base(), 35);
}
