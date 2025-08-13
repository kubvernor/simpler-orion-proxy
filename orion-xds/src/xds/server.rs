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

use std::{net::SocketAddr, pin::Pin};

use atomic_take::AtomicTake;
use orion_data_plane_api::envoy_data_plane_api::{
    envoy::service::discovery::v3::{
        DeltaDiscoveryRequest, DeltaDiscoveryResponse, DiscoveryRequest, DiscoveryResponse, Resource, ResourceName,
        aggregated_discovery_service_server::{AggregatedDiscoveryService, AggregatedDiscoveryServiceServer},
    },
    tonic::{self, IntoStreamingRequest, Response, Status, transport::Server},
};
use tokio::sync::mpsc::{self, Receiver};
use tokio_stream::{Stream, StreamExt, wrappers::ReceiverStream};
use tracing::info;

use crate::xds::{self, model::XdsError};

pub enum ServerAction {
    Add(Resource),
    Remove(Resource),
}

pub type ResourceAction = ServerAction;
#[derive(Debug)]
pub struct AggregateServer {
    delta_resources_rx: AtomicTake<Receiver<ServerAction>>,
    stream_resources_rx: AtomicTake<Receiver<ServerAction>>,
}

impl AggregateServer {
    pub fn new(delta_resources_rx: Receiver<ServerAction>, stream_resources_rx: Receiver<ServerAction>) -> Self {
        Self {
            delta_resources_rx: AtomicTake::new(delta_resources_rx),
            stream_resources_rx: AtomicTake::new(stream_resources_rx),
        }
    }
}

type AggregatedDiscoveryServiceResult<T> = std::result::Result<Response<T>, Status>;

#[tonic::async_trait]
impl AggregatedDiscoveryService for AggregateServer {
    type StreamAggregatedResourcesStream =
        Pin<Box<dyn Stream<Item = std::result::Result<DiscoveryResponse, Status>> + Send>>;

    async fn stream_aggregated_resources(
        &self,
        req: tonic::Request<tonic::Streaming<DiscoveryRequest>>,
    ) -> AggregatedDiscoveryServiceResult<Self::StreamAggregatedResourcesStream> {
        info!("AggregateServer::stream_aggregated_resources");
        info!("\tclient connected from: {:?}", req.remote_addr());

        let (tx, rx) = mpsc::channel(128);
        let mut resources_rx =
            self.stream_resources_rx.take().ok_or(Status::internal("Resource stream is unavailable"))?;
        tokio::spawn(async move {
            while let Some(action) = resources_rx.recv().await {
                let item = match action {
                    xds::server::ServerAction::Add(resource) => {
                        let Some(resource) = resource.resource else {
                            continue;
                        };
                        DiscoveryResponse {
                            type_url: resource.type_url.clone(),
                            resources: vec![resource],
                            nonce: uuid::Uuid::new_v4().to_string(),
                            ..Default::default()
                        }
                    },
                    xds::server::ServerAction::Remove(resource) => {
                        let Some(resource) = resource.resource else {
                            continue;
                        };
                        DiscoveryResponse {
                            type_url: resource.type_url,
                            nonce: uuid::Uuid::new_v4().to_string(),
                            ..Default::default()
                        }
                    },
                };

                match tx.send(std::result::Result::<_, Status>::Ok(item)).await {
                    Ok(()) => {
                        // item (server response) was queued to be send to client
                    },
                    Err(_item) => {
                        // output_stream was build from rx and both are dropped
                        break;
                    },
                }
                info!("\tclient disconnected");
            }
        });

        let mut incoming_stream = req.into_streaming_request().into_inner();
        tokio::spawn(async move {
            while let Some(item) = incoming_stream.next().await {
                info!("Sever : Got item {item:?}");
            }
            info!("Sever sice closed");
        });

        let output_stream = ReceiverStream::new(rx);
        Ok(Response::new(Box::pin(output_stream) as Self::StreamAggregatedResourcesStream))
    }

    type DeltaAggregatedResourcesStream =
        Pin<Box<dyn Stream<Item = std::result::Result<DeltaDiscoveryResponse, Status>> + Send>>;

    async fn delta_aggregated_resources(
        &self,
        req: tonic::Request<tonic::Streaming<DeltaDiscoveryRequest>>,
    ) -> AggregatedDiscoveryServiceResult<Self::DeltaAggregatedResourcesStream> {
        info!("AggregateServer::delta_aggregated_resources");
        info!("\tclient connected from: {:?}", req.remote_addr());

        // spawn and channel are required if you want handle "disconnect" functionality
        // the `out_stream` will not be polled after client disconnect
        let (tx, rx) = mpsc::channel(128);
        let mut resources_rx = self.delta_resources_rx.take().ok_or(Status::internal("Delta stream is unavailable"))?;
        tokio::spawn(async move {
            while let Some(action) = resources_rx.recv().await {
                let item = match action {
                    xds::server::ServerAction::Add(r) => {
                        let Some(ref resource) = r.resource else {
                            continue;
                        };
                        DeltaDiscoveryResponse {
                            type_url: resource.type_url.clone(),
                            resources: vec![r],
                            nonce: uuid::Uuid::new_v4().to_string(),
                            system_version_info: "system_version_info".to_owned(),
                            ..Default::default()
                        }
                    },
                    xds::server::ServerAction::Remove(r) => {
                        let Some(resource) = r.resource else {
                            continue;
                        };
                        DeltaDiscoveryResponse {
                            type_url: resource.type_url.clone(),
                            nonce: uuid::Uuid::new_v4().to_string(),
                            system_version_info: "system_version_info".to_owned(),
                            removed_resource_names: vec![ResourceName {
                                name: r.name.clone(),
                                dynamic_parameter_constraints: None,
                            }],
                            removed_resources: vec![r.name],
                            ..Default::default()
                        }
                    },
                };

                match tx.send(std::result::Result::<_, Status>::Ok(item)).await {
                    Ok(()) => {
                        // item (server response) was queued to be send to client
                    },
                    Err(_item) => {
                        // output_stream was build from rx and both are dropped
                        break;
                    },
                }
            }
            info!("\tclient disconnected");
        });

        let mut incoming_stream = req.into_streaming_request().into_inner();
        tokio::spawn(async move {
            while let Some(Ok(item)) = incoming_stream.next().await {
                info!("Sever : Got item {item:?}");
            }
            info!("Sever side closed");
        });

        let output_stream = ReceiverStream::new(rx);
        Ok(Response::new(Box::pin(output_stream) as Self::DeltaAggregatedResourcesStream))
    }
}

pub async fn start_aggregate_server(
    addr: SocketAddr,
    delta_resources_rx: Receiver<ResourceAction>,
    stream_resources_rx: Receiver<ResourceAction>,
) -> Result<(), XdsError> {
    info!("Server started {addr:?}");
    let server = AggregateServer::new(delta_resources_rx, stream_resources_rx);
    let aggregate_server = AggregatedDiscoveryServiceServer::new(server);
    let server =
        Server::builder().concurrency_limit_per_connection(256).add_service(aggregate_server).serve(addr).await;
    info!("Server exited {server:?}");
    Ok(())
}
