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

use super::{
    filterchain::{ConnectionHandler, FilterchainBuilder, FilterchainType},
    listeners_manager::TlsContextChange,
};
use crate::{
    ConversionContext, Error, Result, RouteConfigurationChange,
    listeners::filter_state::DownstreamConnectionMetadata,
    secrets::{TlsConfigurator, WantsToBuildServer},
    transport::{AsyncStream, ProxyProtocolReader, bind_device::BindDevice, tls_inspector},
};
use opentelemetry::KeyValue;
use orion_configuration::config::{
    listener::{FilterChainMatch, Listener as ListenerConfig, MatchResult},
    listener_filters::DownstreamProxyProtocolConfig,
};
use orion_metrics::{
    metrics::{http, listeners},
    with_histogram, with_metric,
};
use rustls::ServerConfig;
use scopeguard::defer;
use std::{
    collections::HashMap,
    fmt::Debug,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{
    net::{TcpListener, TcpSocket},
    sync::broadcast::{self},
};
use tracing::{debug, info, warn};

#[derive(Debug, Clone)]
struct PartialListener {
    name: &'static str,
    socket_address: std::net::SocketAddr,
    bind_device: Option<BindDevice>,
    filter_chains: HashMap<FilterChainMatch, FilterchainBuilder>,
    with_tls_inspector: bool,
    proxy_protocol_config: Option<DownstreamProxyProtocolConfig>,
}
#[derive(Debug, Clone)]
pub struct ListenerFactory {
    listener: PartialListener,
}

impl TryFrom<ConversionContext<'_, ListenerConfig>> for PartialListener {
    type Error = Error;
    fn try_from(ctx: ConversionContext<'_, ListenerConfig>) -> std::result::Result<Self, Self::Error> {
        let ConversionContext { envoy_object: listener, secret_manager } = ctx;
        let name = orion_interner::to_static_str(&listener.name);
        let addr = listener.address;
        let with_tls_inspector = listener.with_tls_inspector;
        let proxy_protocol_config = listener.proxy_protocol_config;
        debug!("Listener {name} :TLS Inspector is {with_tls_inspector}");

        let filter_chains: HashMap<FilterChainMatch, _> = listener
            .filter_chains
            .into_iter()
            .map(|f| FilterchainBuilder::try_from(ConversionContext::new((f.1, secret_manager))).map(|x| (f.0, x)))
            .collect::<Result<_>>()?;
        let bind_device = listener.bind_device;

        if !with_tls_inspector {
            let has_server_names = filter_chains.keys().any(|m| !m.server_names.is_empty());
            if has_server_names {
                return Err((format!(
                    "Listener '{name}' has server_names in filter_chain_match, but no TLS inspector so matches would always fail"
                )).into());
            }
        }

        Ok(PartialListener {
            name,
            socket_address: addr,
            bind_device,
            filter_chains,
            with_tls_inspector,
            proxy_protocol_config,
        })
    }
}

impl ListenerFactory {
    pub fn make_listener(
        self,
        route_updates_receiver: broadcast::Receiver<RouteConfigurationChange>,
        secret_updates_receiver: broadcast::Receiver<TlsContextChange>,
    ) -> Result<Listener> {
        let PartialListener {
            name,
            socket_address,
            bind_device,
            filter_chains,
            with_tls_inspector,
            proxy_protocol_config,
        } = self.listener;

        let filter_chains = filter_chains
            .into_iter()
            .map(|fc| fc.1.with_listener_name(name).build().map(|x| (fc.0, x)))
            .collect::<Result<HashMap<_, _>>>()?;

        Ok(Listener {
            name,
            socket_address,
            bind_device,
            filter_chains,
            with_tls_inspector,
            proxy_protocol_config,
            route_updates_receiver,
            secret_updates_receiver,
        })
    }
}

impl TryFrom<ConversionContext<'_, ListenerConfig>> for ListenerFactory {
    type Error = Error;
    fn try_from(ctx: ConversionContext<'_, ListenerConfig>) -> std::result::Result<Self, Self::Error> {
        let listener = PartialListener::try_from(ctx)?;
        Ok(Self { listener })
    }
}

#[derive(Debug)]
pub struct Listener {
    name: &'static str,
    socket_address: std::net::SocketAddr,
    bind_device: Option<BindDevice>,
    pub filter_chains: HashMap<FilterChainMatch, FilterchainType>,
    with_tls_inspector: bool,
    proxy_protocol_config: Option<DownstreamProxyProtocolConfig>,
    route_updates_receiver: broadcast::Receiver<RouteConfigurationChange>,
    secret_updates_receiver: broadcast::Receiver<TlsContextChange>,
}

impl Listener {
    #[cfg(test)]
    pub(crate) fn test_listener(
        name: &'static str,
        route_rx: broadcast::Receiver<RouteConfigurationChange>,
        secret_rx: broadcast::Receiver<TlsContextChange>,
    ) -> Self {
        use std::net::{IpAddr, Ipv4Addr};
        Listener {
            name,
            socket_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 0),
            bind_device: None,
            filter_chains: HashMap::new(),
            with_tls_inspector: false,
            proxy_protocol_config: None,
            route_updates_receiver: route_rx,
            secret_updates_receiver: secret_rx,
        }
    }

    pub fn get_name(&self) -> &'static str {
        self.name
    }
    pub fn get_socket(&self) -> (&std::net::SocketAddr, Option<&BindDevice>) {
        (&self.socket_address, self.bind_device.as_ref())
    }

    pub async fn start(self) -> Error {
        let Self {
            name,
            socket_address: local_address,
            bind_device,
            filter_chains,
            with_tls_inspector,
            proxy_protocol_config,
            mut route_updates_receiver,
            mut secret_updates_receiver,
        } = self;
        let listener = match configure_and_start_tcp_listener(local_address, bind_device.as_ref()) {
            Ok(x) => x,
            Err(e) => return e,
        };

        info!("listener '{name}' started: {local_address}");
        let mut filter_chains = Arc::new(filter_chains);
        let proxy_protocol_config = proxy_protocol_config.map(Arc::new);
        let listener_name = name;

        loop {
            tokio::select! {
                // here we accept a connection, and then start processing it.
                //  we spawn early so that we don't block other connections from being accepted due to a slow client
                maybe_stream = listener.accept() => {
                    match maybe_stream {
                        Ok((stream, peer_addr)) => {
                            let start = std::time::Instant::now();

                            // This is a new downstream connection...
                            let shard_id = std::thread::current().id();
                            with_metric!(listeners::DOWNSTREAM_CX_TOTAL, add, 1, shard_id,&[KeyValue::new("listener", listener_name)]);
                            with_metric!(listeners::DOWNSTREAM_CX_ACTIVE, add, 1, shard_id,&[KeyValue::new("listener", listener_name)]);

                            let filter_chains = Arc::clone(&filter_chains);
                            let proxy_protocol_config = proxy_protocol_config.clone();
                            // spawn a separate task for handling this client<->proxy connection
                            // we spawn before we know if we want to process this route because we might need to run the tls_inspector which could
                            // stall if the client is slow to send the ClientHello and end up blocking the acceptance of new connections
                            //
                            //  we could optimize a little here by either splitting up the filter_chain selection and rbac into the parts that can run
                            // before we have the ClientHello and the ones after. since we might already have enough info to decide to drop the connection
                            // or pick a specific filter_chain to run, or we could simply if-else on the with_tls_inspector variable.
                            tokio::spawn(Self::process_listener_update(name, filter_chains, with_tls_inspector, proxy_protocol_config, local_address, peer_addr, Box::new(stream), start));
                        },
                        Err(e) => {warn!("failed to accept tcp connection: {e}");}
                    }
                },
                maybe_route_update = route_updates_receiver.recv() => {
                    //todo: add context to the error here once orion-error lands
                    match maybe_route_update {
                        Ok(route_update) => {Self::process_route_update(name, &filter_chains, route_update)},
                        Err(e) => {return e.into();}
                    }
                },
                maybe_secret_update = secret_updates_receiver.recv() => {
                    match maybe_secret_update {
                        Ok(secret_update) => {
                            // todo: possibly expensive clone - may need to rethink this structure
                            let mut filter_chains_clone = filter_chains.as_ref().clone();
                            Self::process_secret_update(name, &mut filter_chains_clone, secret_update);
                            filter_chains = Arc::new(filter_chains_clone);
                        }
                        Err(e) => {return e.into();}
                    }
                }
            }
        }
    }

    fn select_filterchain<'a, T>(
        filter_chains: &'a HashMap<FilterChainMatch, T>,
        downstream_metadata: &DownstreamConnectionMetadata,
        server_name: Option<&str>,
    ) -> Result<Option<&'a T>> {
        let source_addr = downstream_metadata.peer_address();
        let destination_addr = downstream_metadata.local_address();
        fn match_subitem<'a, F: Fn(&FilterChainMatch, T) -> MatchResult, T: Copy>(
            function: F,
            comparand: T,
            iter: impl Iterator<Item = &'a FilterChainMatch>,
            scratchpad: &mut [MatchResult],
            possible_filters: &mut [bool],
        ) {
            let mut best_match = MatchResult::FailedMatch;
            // check all filters still in the running, skipping over those already eliminated
            for (i, match_config) in iter.enumerate().filter(|(i, _)| possible_filters[*i]) {
                let match_result = function(match_config, comparand);
                //mark the outcome of this iteration, and keep track of the best result
                scratchpad[i] = match_result;
                if match_result > best_match {
                    best_match = match_result;
                }
            }
            // now trim all the results that failed to match, or were less specific than the best match
            for i in 0..scratchpad.len() {
                if scratchpad[i] != best_match || scratchpad[i] == MatchResult::FailedMatch {
                    possible_filters[i] = false;
                }
            }
        }

        //todo: smallvec? other optimization?
        let mut possible_filters = vec![true; filter_chains.len()];
        let mut scratchpad = vec![MatchResult::NoRule; filter_chains.len()];

        match_subitem(
            FilterChainMatch::matches_destination_port,
            destination_addr.port(),
            filter_chains.keys(),
            &mut scratchpad,
            &mut possible_filters,
        );

        match_subitem(
            FilterChainMatch::matches_destination_ip,
            destination_addr.ip(),
            filter_chains.keys(),
            &mut scratchpad,
            &mut possible_filters,
        );

        match_subitem(
            FilterChainMatch::matches_server_name,
            server_name.unwrap_or_default(),
            filter_chains.keys(),
            &mut scratchpad,
            &mut possible_filters,
        );

        match_subitem(
            FilterChainMatch::matches_source_ip,
            source_addr.ip(),
            filter_chains.keys(),
            &mut scratchpad,
            &mut possible_filters,
        );

        match_subitem(
            FilterChainMatch::matches_source_port,
            source_addr.port(),
            filter_chains.keys(),
            &mut scratchpad,
            &mut possible_filters,
        );

        let mut possible_filters = possible_filters
            .into_iter()
            .zip(filter_chains.iter())
            .filter_map(|(include, item)| include.then_some(item.1));

        let first_match = possible_filters.next();
        if possible_filters.next().is_some() {
            Err("multiple filterchains matched a single connection. This is a bug in orion!".into())
        } else {
            Ok(first_match)
        }
    }

    async fn process_listener_update(
        listener_name: &'static str,
        filter_chains: Arc<HashMap<FilterChainMatch, FilterchainType>>,
        with_tls_inspector: bool,
        proxy_protocol_config: Option<Arc<DownstreamProxyProtocolConfig>>,
        local_address: SocketAddr,
        peer_addr: SocketAddr,
        mut stream: AsyncStream,
        start_instant: std::time::Instant,
    ) -> Result<()> {
        let shard_id = std::thread::current().id();

        let ssl = AtomicBool::new(false);
        defer! {
            with_metric!(listeners::DOWNSTREAM_CX_DESTROY, add, 1, shard_id, &[KeyValue::new("listener", listener_name)]);
            with_metric!(listeners::DOWNSTREAM_CX_ACTIVE, sub, 1, shard_id, &[KeyValue::new("listener", listener_name)]);
            if ssl.load(Ordering::Relaxed) {
                with_metric!(http::DOWNSTREAM_CX_SSL_ACTIVE, add, 1, shard_id, &[KeyValue::new("listener", listener_name)]);
            }
            let ms = u64::try_from(start_instant.elapsed().as_millis())
                .unwrap_or(u64::MAX);
            with_histogram!(listeners::DOWNSTREAM_CX_LENGTH_MS, record, ms, &[KeyValue::new("listener", listener_name)]);
        }

        let downstream_metadata = if let Some(config) = proxy_protocol_config.as_ref() {
            let reader = ProxyProtocolReader::new(Arc::clone(config));
            let (metadata, new_stream) = reader.try_read_proxy_header(stream, local_address, peer_addr).await?;
            stream = new_stream;
            metadata
        } else {
            DownstreamConnectionMetadata::FromSocket { peer_address: peer_addr, local_address }
        };
        let downstream_metadata = Arc::new(downstream_metadata);

        let server_name = if with_tls_inspector {
            let (tls_result, rewound_stream) = tls_inspector::inspect_client_hello(stream).await;
            stream = rewound_stream;
            match tls_result {
                crate::transport::tls_inspector::InspectorResult::Success(sni) => {
                    debug!("{listener_name} : Detected TLS server name: {sni}");
                    with_metric!(
                        http::DOWNSTREAM_CX_SSL_TOTAL,
                        add,
                        1,
                        shard_id,
                        &[KeyValue::new("listener", listener_name)]
                    );
                    with_metric!(
                        http::DOWNSTREAM_CX_SSL_ACTIVE,
                        add,
                        1,
                        shard_id,
                        &[KeyValue::new("listener", listener_name)]
                    );
                    ssl.store(true, Ordering::Relaxed);
                    Some(sni)
                },
                crate::transport::tls_inspector::InspectorResult::SuccessNoSni => {
                    debug!("{listener_name} : No TLS server name indication present");
                    with_metric!(
                        http::DOWNSTREAM_CX_SSL_TOTAL,
                        add,
                        1,
                        shard_id,
                        &[KeyValue::new("listener", listener_name)]
                    );
                    with_metric!(
                        http::DOWNSTREAM_CX_SSL_ACTIVE,
                        add,
                        1,
                        shard_id,
                        &[KeyValue::new("listener", listener_name)]
                    );
                    ssl.store(true, Ordering::Relaxed);
                    None
                },
                crate::transport::tls_inspector::InspectorResult::TlsError(e) => {
                    debug!("{listener_name} : No TLS handshake: Error: {e}");
                    None
                },
            }
        } else {
            None
        };

        let selected_filterchain =
            Self::select_filterchain(&filter_chains, &downstream_metadata, server_name.as_deref())?;
        if let Some(filterchain) = selected_filterchain {
            debug!(
                "{listener_name} : mapping connection from {peer_addr} to filter chain {}",
                filterchain.filter_chain().name
            );
            if let Some(stream) = filterchain.apply_rbac(stream, &downstream_metadata, server_name.as_deref()) {
                return filterchain
                    .start_filterchain(stream, downstream_metadata, shard_id, listener_name, start_instant)
                    .await;
            }
            debug!("{listener_name} : dropped connection from {peer_addr} due to rbac");
        } else {
            with_metric!(
                listeners::NO_FILTER_CHAIN_MATCH,
                add,
                1,
                shard_id,
                &[KeyValue::new("listener", listener_name)]
            );
            warn!("{listener_name} : No match for {peer_addr} {local_address}");
        }
        Ok(())
    }

    //could secrets and routes also be updated through a CachedWatch?
    // they only need to be updated when they're read after all and could work with
    fn process_secret_update(
        listener_name: &str,
        filter_chains: &mut HashMap<FilterChainMatch, FilterchainType>,
        secret_update: TlsContextChange,
    ) {
        match secret_update {
            TlsContextChange::Updated((secret_id, secret)) => {
                for chain in filter_chains.values_mut() {
                    let filterchain = &mut chain.config;
                    if let Some(tls_configurator) = filterchain.tls_configurator.clone() {
                        let maybe_configurator = TlsConfigurator::<ServerConfig, WantsToBuildServer>::update(
                            tls_configurator,
                            &secret_id,
                            secret.clone(),
                        );
                        if let Ok(new_tls_configurator) = maybe_configurator {
                            filterchain.tls_configurator = Some(new_tls_configurator);
                        } else {
                            let msg = format!(
                                "{listener_name} Couldn't update a secret for filterchain {} {:?}",
                                filterchain.name,
                                maybe_configurator.err()
                            );
                            warn!("{msg}");
                        }
                    }
                }
            },
        }
    }

    fn process_route_update(
        listener_name: &str,
        filter_chains: &HashMap<FilterChainMatch, FilterchainType>,
        route_update: RouteConfigurationChange,
    ) {
        match route_update {
            RouteConfigurationChange::Added((id, route)) => {
                for chain in filter_chains.values() {
                    if let ConnectionHandler::Http(http_manager) = &chain.handler {
                        let route_id = http_manager.get_route_id();
                        if let Some(route_id) = route_id {
                            if route_id == id {
                                debug!("{listener_name} Route updated {id} {route:?}");
                                http_manager.update_route(route.clone());
                            }
                        } else {
                            debug!("{listener_name} Got route update but id doesn't match {route_id:?} {id}");
                        }
                    }
                }
            },
            RouteConfigurationChange::Removed(id) => {
                for chain in filter_chains.values() {
                    if let ConnectionHandler::Http(http_manager) = &chain.handler {
                        if let Some(route_id) = http_manager.get_route_id() {
                            if route_id == id {
                                http_manager.remove_route();
                            }
                        }
                    }
                }
            },
        }
    }
}

fn configure_and_start_tcp_listener(addr: SocketAddr, device: Option<&BindDevice>) -> Result<TcpListener> {
    let socket = if addr.is_ipv4() { TcpSocket::new_v4()? } else { TcpSocket::new_v6()? };
    socket.set_reuseaddr(true)?;
    socket.set_keepalive(true)?;

    if let Some(device) = device {
        crate::transport::bind_device::bind_device(&socket, device)?;
    }

    #[cfg(all(unix, not(target_os = "solaris"), not(target_os = "illumos")))]
    socket.set_reuseport(true)?;
    socket.bind(addr)?;

    Ok(socket.listen(128)?)
}

#[cfg(test)]
mod tests {
    use orion_configuration::config::listener::{FilterChainMatch as FilterChainMatchConfig, ServerNameMatch};
    use orion_data_plane_api::{
        decode::from_yaml, envoy_data_plane_api::envoy::config::listener::v3::FilterChainMatch as EnvoyFilterChainMatch,
    };

    use crate::SecretManager;

    use super::*;
    use orion_data_plane_api::envoy_data_plane_api::envoy::config::listener::v3::Listener as EnvoyListener;

    use std::{net::Ipv4Addr, str::FromStr};
    use tracing_test::traced_test;

    #[test]
    fn listener_bind_device() {
        const LISTENER: &str = r#"
name: listener_https
address:
  socket_address: { address: 0.0.0.0, port_value: 8443 }
filter_chains:
  - name: filter_chain
    filters:
      - name: https_gateway
        typedConfig:
          "@type": type.googleapis.com/envoy.extensions.filters.network.http_connection_manager.v3.HttpConnectionManager
          codec_type: HTTP1
          stat_prefix: http
          httpFilters:
          - name: envoy.filters.http.router
            typed_config:
              "@type": type.googleapis.com/envoy.extensions.filters.http.router.v3.Router
              start_child_span: false
          route_config:
            name: basic_https_route
            virtual_hosts:
              - name: backend_https
                domains: ["*"]
socket_options:
  - description: "bind to interface virt1"
    level: 1
    name: 25
    # utf8 string 'virt1' bytes encoded as base64
    buf_value: dmlydDE=
"#;

        let envoy_listener: EnvoyListener = from_yaml(LISTENER).unwrap();
        let listener = envoy_listener.try_into().unwrap();
        let secrets_manager = SecretManager::new();
        let ctx = ConversionContext::new((listener, &secrets_manager));
        let l = PartialListener::try_from(ctx).unwrap();
        let expected_bind_device = Some(BindDevice::from_str("virt1").unwrap());

        assert_eq!(&l.bind_device, &expected_bind_device);
    }

    #[test]
    fn match_fallback_sni() {
        let fcm = [
            (
                FilterChainMatch {
                    destination_port: None,
                    destination_prefix_ranges: Vec::new(),
                    server_names: vec![
                        ServerNameMatch::from_str("host1.test").unwrap(),
                        ServerNameMatch::from_str("host2.test").unwrap(),
                    ],
                    source_prefix_ranges: Vec::new(),
                    source_ports: Vec::new(),
                },
                0,
            ),
            (FilterChainMatch::default(), 1),
        ];
        let hashmap: HashMap<_, _> = fcm.iter().cloned().collect();
        let metadata = DownstreamConnectionMetadata::FromSocket {
            peer_address: (Ipv4Addr::new(127, 0, 0, 1), 33000).into(),
            local_address: (Ipv4Addr::LOCALHOST, 8443).into(),
        };
        let selected = Listener::select_filterchain(&hashmap, &metadata, None).unwrap();
        assert_eq!(selected.copied(), Some(1));
    }

    #[traced_test]
    #[test]
    fn sni_match_without_inspector_fails() {
        const LISTENER: &str = r#"
name: listener_https
address:
  socket_address: { address: 0.0.0.0, port_value: 8443 }
filter_chains:
  - name: filter_chain_https1
    filter_chain_match:
      server_names: [hostname.example]
    filters:
      - name: https_gateway
        typedConfig:
          "@type": type.googleapis.com/envoy.extensions.filters.network.http_connection_manager.v3.HttpConnectionManager
          codec_type: HTTP1
          stat_prefix: http
          httpFilters:
            - name: envoy.filters.http.router
              typed_config:
                "@type": type.googleapis.com/envoy.extensions.filters.http.router.v3.Router
                start_child_span: false
          route_config:
            name: basic_https_route
            virtual_hosts:
              - name: backend_https
                domains: ["*"]
"#;

        let envoy_listener: EnvoyListener = from_yaml(LISTENER).unwrap();
        let listener = envoy_listener.try_into().unwrap();
        let secrets_man = SecretManager::new();

        let conv = ConversionContext { envoy_object: listener, secret_manager: &secrets_man };
        let r = PartialListener::try_from(conv);
        let err = r.unwrap_err();
        assert!(
            err.to_string()
                .contains("has server_names in filter_chain_match, but no TLS inspector so matches would always fail")
        );
    }

    #[traced_test]
    #[test]
    fn filter_chain_multiple() {
        let m: EnvoyFilterChainMatch = from_yaml(
            "
        server_names: [host.test, \"*.wildcard\"]
        destination_port: 443
        source_ports: [3300]
        prefix_ranges: [{address_prefix: 127.0.0.1, prefix_len: 32}]
        ",
        )
        .unwrap();
        let m = std::iter::once((m.try_into().unwrap(), ())).collect();
        let metadata = DownstreamConnectionMetadata::FromSocket {
            peer_address: (Ipv4Addr::LOCALHOST, 3300).into(),
            local_address: (Ipv4Addr::LOCALHOST, 443).into(),
        };
        let good_host = Some("host.test");
        assert!(matches!(Listener::select_filterchain(&m, &metadata, good_host), Ok(Some(()))));
        assert!(matches!(Listener::select_filterchain(&m, &metadata, Some("a.wildcard")), Ok(Some(()))));
        assert!(matches!(Listener::select_filterchain(&m, &metadata, None), Ok(None)));
    }

    #[test]
    fn most_specific_wins() {
        let l: EnvoyListener = from_yaml(
            "
        name: listener
        filter_chains:
        - filter_chain_match:
            server_names: [this.is.more.specific]
        - filter_chain_match:
            server_names: [\"*.more.specific\"]
        - filter_chain_match:
            server_names: [\"*.specific\"]
        - filter_chain_match:
            server_names: []
        ",
        )
        .unwrap();
        //     let listener : Listener = l.try_into().unwrap();
        let m = l
            .filter_chains
            .into_iter()
            .enumerate()
            .map(|(i, fc)| {
                fc.filter_chain_match
                    .map(FilterChainMatchConfig::try_from)
                    .transpose()
                    .map(|x| (x.unwrap_or_default(), i))
            })
            .collect::<std::result::Result<HashMap<_, _>, _>>()
            .unwrap();
        let metadata = DownstreamConnectionMetadata::FromSocket {
            peer_address: (Ipv4Addr::new(127, 0, 0, 1), 33000).into(),
            local_address: (Ipv4Addr::LOCALHOST, 8443).into(),
        };
        assert_eq!(Listener::select_filterchain(&m, &metadata, None).unwrap().copied(), Some(3));
        assert_eq!(
            Listener::select_filterchain(&m, &metadata, Some("this.is.more.specific")).unwrap().copied(),
            Some(0)
        );
        assert_eq!(
            Listener::select_filterchain(&m, &metadata, Some("not.this.is.more.specific")).unwrap().copied(),
            Some(1)
        );
        assert_eq!(Listener::select_filterchain(&m, &metadata, Some("is.more.specific")).unwrap().copied(), Some(1));

        assert_eq!(Listener::select_filterchain(&m, &metadata, Some("more.specific")).unwrap().copied(), Some(2));
        assert_eq!(
            Listener::select_filterchain(&m, &metadata, Some("this.is.less.specific")).unwrap().copied(),
            Some(2)
        );

        assert_eq!(Listener::select_filterchain(&m, &metadata, Some("hello.world")).unwrap().copied(), Some(3));
    }
}
