use orion_configuration::config::{Bootstrap, Config, deserialize_yaml};
use orion_error::{Context, Error};
use std::{fs::File, path::PathBuf};

#[test]
fn empty_config() {
    let _cfg: Config = deserialize_yaml(&PathBuf::from("tests/yaml/config.yaml")).unwrap();
}

#[test]
fn bad_config() {
    let r: Result<Config, _> = deserialize_yaml(&PathBuf::from("tests/yaml/config_bad.yaml"));
    assert!(r.is_err());
}

#[test]
fn access_log_file_default() -> Result<(), Error> {
    let bootstrap = Bootstrap::deserialize_from_envoy(
        File::open("tests/access_log/hcm_file_default.yaml").with_context_msg("failed to open bootstrap.yaml")?,
    )
    .with_context_msg("failed to convert envoy to orion");
    bootstrap.map(|_| ())
}

#[test]
fn access_log_file_with_format() -> Result<(), Error> {
    let bootstrap = Bootstrap::deserialize_from_envoy(
        File::open("tests/access_log/hcm_file_with_format.yaml").with_context_msg("failed to open file")?,
    )
    .with_context_msg("failed to convert envoy to orion");
    bootstrap.map(|_| ())
}

#[test]
fn access_log_file_bad_name() -> Result<(), Error> {
    let bootstrap = Bootstrap::deserialize_from_envoy(
        File::open("tests/access_log/hcm_file_bad_name.yaml").with_context_msg("failed to open file")?,
    )
    .with_context_msg("failed to convert envoy to orion");
    let err = bootstrap.unwrap_err();
    println!("{err:?}");
    Ok(())
}

#[test]
fn access_log_file_name_mismatch() -> Result<(), Error> {
    let bootstrap = Bootstrap::deserialize_from_envoy(
        File::open("tests/access_log/hcm_file_name_mismatch.yaml").with_context_msg("failed to open file")?,
    )
    .with_context_msg("failed to convert envoy to orion");
    let err = bootstrap.unwrap_err();
    println!("{err:?}");
    Ok(())
}

#[test]
fn access_log_file_with_bad_format() -> Result<(), Error> {
    let bootstrap = Bootstrap::deserialize_from_envoy(
        File::open("tests/access_log/hcm_file_with_bad_format.yaml").with_context_msg("failed to open file")?,
    )
    .with_context_msg("failed to convert envoy to orion");
    let err = bootstrap.unwrap_err();
    println!("{err:?}");
    Ok(())
}

#[test]
fn access_log_file_without_path() -> Result<(), Error> {
    let bootstrap = Bootstrap::deserialize_from_envoy(
        File::open("tests/access_log/hcm_file_without_path.yaml").with_context_msg("failed to open file")?,
    )
    .with_context_msg("failed to convert envoy to orion");
    let err = bootstrap.unwrap_err();
    println!("{err:?}");
    Ok(())
}
