use orion_configuration::config::{deserialize_yaml, Config};
use std::path::PathBuf;

#[test]
fn empty_config() {
    let _cfg: Config = deserialize_yaml(&PathBuf::from("tests/config.yaml")).unwrap();
}

#[test]
fn bad_config() {
    let r: Result<Config, _> = deserialize_yaml(&PathBuf::from("tests/config_bad.yaml"));
    assert!(r.is_err());
}
