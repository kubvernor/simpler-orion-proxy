#![recursion_limit = "128"]
// SPDX-FileCopyrightText: © 2025 Huawei Cloud Computing Technologies Co., Ltd
// SPDX-License-Identifier: Apache-2.0
//
// Copyright 2025 Huawei Cloud Computing Technologies Co., Ltd
//
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
//

pub mod configuration;

pub mod access_log;
mod body;
pub mod clusters;
mod listeners;
mod secrets;
pub(crate) mod thread_local;
pub(crate) mod trace;
mod transport;
pub(crate) mod utils;

use std::sync::OnceLock;

use listeners::listeners_manager;
use orion_configuration::config::Runtime;
use serde::Serialize;
use tokio::{sync::mpsc, task::JoinSet};

pub use crate::configuration::get_listeners_and_clusters;

pub use clusters::{
    ClusterLoadAssignmentBuilder,
    cluster::PartialClusterType,
    health::{EndpointHealthUpdate, HealthCheckManager},
    load_assignment::PartialClusterLoadAssignment,
};
pub use listeners::listener::ListenerFactory;
pub use listeners_manager::{ListenerConfigurationChange, ListenersManager, RouteConfigurationChange};
pub use orion_configuration::config::network_filters::http_connection_manager::RouteConfiguration;
use orion_configuration::config::{
    Bootstrap, Cluster, Listener as ListenerConfig,
    cluster::LocalityLbEndpoints as LocalityLbEndpointsConfig,
    network_filters::http_connection_manager::{RouteSpecifier, http_filters::HttpFilter},
    secret::Secret,
};
pub use secrets::SecretManager;
pub(crate) use transport::AsyncStream;

pub type Error = orion_error::Error;
pub type Result<T> = ::core::result::Result<T, Error>;

pub use crate::body::poly_body::PolyBody;

pub static RUNTIME_CONFIG: OnceLock<Runtime> = OnceLock::new();

#[allow(clippy::expect_used, clippy::missing_panics_doc)]
pub fn runtime_config() -> &'static Runtime {
    RUNTIME_CONFIG.get().expect("Called runtime_config without setting RUNTIME_CONFIG first")
}

pub struct ConversionContext<'a, T> {
    envoy_object: T,
    secret_manager: &'a SecretManager,
}
impl<'a, T> ConversionContext<'a, T> {
    pub fn new(ctx: (T, &'a SecretManager)) -> Self {
        Self { envoy_object: ctx.0, secret_manager: ctx.1 }
    }
}

pub struct ConfigurationReceivers {
    listener_configuration_receiver: mpsc::Receiver<ListenerConfigurationChange>,
    route_configuration_receiver: mpsc::Receiver<RouteConfigurationChange>,
}

#[derive(Clone, Debug)]
pub struct ConfigurationSenders {
    pub listener_configuration_sender: mpsc::Sender<ListenerConfigurationChange>,
    pub route_configuration_sender: mpsc::Sender<RouteConfigurationChange>,
}

impl ConfigurationReceivers {
    pub fn new(
        listener_configuration_receiver: mpsc::Receiver<ListenerConfigurationChange>,
        route_configuration_receiver: mpsc::Receiver<RouteConfigurationChange>,
    ) -> Self {
        Self { listener_configuration_receiver, route_configuration_receiver }
    }
}

impl ConfigurationSenders {
    pub fn new(
        listener_configuration_sender: mpsc::Sender<ListenerConfigurationChange>,
        route_configuration_sender: mpsc::Sender<RouteConfigurationChange>,
    ) -> Self {
        Self { listener_configuration_sender, route_configuration_sender }
    }
}

#[derive(Debug, Default, Serialize, Clone)]
pub struct ConfigDump {
    pub bootstrap: Option<Bootstrap>,
    #[serde(skip_serializing_if = "Option::is_none", default = "Default::default")]
    pub listeners: Option<Vec<ListenerConfig>>,
    #[serde(skip_serializing_if = "Option::is_none", default = "Default::default")]
    pub clusters: Option<Vec<Cluster>>,
    #[serde(skip_serializing_if = "Option::is_none", default = "Default::default")]
    pub ecds_filter_http: Option<Vec<HttpFilter>>,
    #[serde(skip_serializing_if = "Option::is_none", default = "Default::default")]
    pub endpoints: Option<Vec<LocalityLbEndpointsConfig>>,
    #[serde(skip_serializing_if = "Option::is_none", default = "Default::default")]
    pub routes: Option<Vec<RouteSpecifier>>,
    #[serde(skip_serializing_if = "Option::is_none", default = "Default::default")]
    pub secrets: Option<Vec<Secret>>,
}

pub fn new_configuration_channel(capacity: usize) -> (ConfigurationSenders, ConfigurationReceivers) {
    let (listener_tx, listener_rx) = mpsc::channel::<ListenerConfigurationChange>(capacity);
    let (route_tx, route_rx) = mpsc::channel::<RouteConfigurationChange>(capacity);
    (ConfigurationSenders::new(listener_tx, route_tx), ConfigurationReceivers::new(listener_rx, route_rx))
}

pub fn start_ng_on_joinset(configuration_receivers: ConfigurationReceivers) -> Result<JoinSet<Result<()>>> {
    let ConfigurationReceivers { listener_configuration_receiver, route_configuration_receiver } =
        configuration_receivers;

    let mut set = JoinSet::new();

    set.spawn(async move {
        let listeners_manager = ListenersManager::new(listener_configuration_receiver, route_configuration_receiver);
        if let Err(err) = listeners_manager.start().await {
            tracing::warn!("{err}");
        }
        Ok(())
    });

    Ok(set)
}
