use async_trait::async_trait;
use log::info;
use pingora_core::services::{listening::Service as ListeningService, background::background_service};
use std::{sync::Arc, time::Duration};
use structopt::StructOpt;

use pingora_core::server::configuration::Opt;
use pingora_core::server::Server;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_core::Result;
use pingora_load_balancing::{health_check, selection::RoundRobin, LoadBalancer};
use pingora_proxy::{ProxyHttp, Session};

pub struct LB(Arc<LoadBalancer<RoundRobin>>);

#[async_trait]
impl ProxyHttp for LB {
    type CTX = ();
    fn new_ctx(&self) -> Self::CTX {}

    async fn upstream_peer(&self, _session: &mut Session, _ctx: &mut ()) -> Result<Box<HttpPeer>> {
        let upstream = self
            .0
            .select(b"", 256) // hash doesn't matter
            .unwrap();

        info!("upstream peer is: {:?}", upstream);

        if upstream.addr.to_string().contains(":443") {
            let peer = Box::new(HttpPeer::new(upstream, true, String::from("")));
            Ok(peer)
        } else {
            let peer = Box::new(HttpPeer::new(upstream, false, String::from("")));
            Ok(peer)
        }
    }
}

#[derive(StructOpt)]
struct MyOpt {
    #[structopt(short, long)]
    backend: Vec<String>,

    #[structopt(flatten)]
    base_opts: Opt
}

impl Default for MyOpt {
    fn default() -> Self {
        MyOpt::from_args()
    }
}

fn main() {
    env_logger::init();

    // Read command line arguments
    let my_opts = MyOpt::default();
    let mut my_server = Server::new(Some(my_opts.base_opts)).unwrap();
    my_server.bootstrap();

    let mut upstreams = LoadBalancer::try_from_iter(my_opts.backend).unwrap();

    // Add health check in the background so that unreachable servers are never selected
    let hc = health_check::TcpHealthCheck::new();
    upstreams.set_health_check(hc);
    upstreams.health_check_frequency = Some(Duration::from_secs(1));

    let background = background_service("health check", upstreams);
    let upstreams = background.task();

    // TCP proxy
    let mut lb = pingora_proxy::http_proxy_service(&my_server.configuration, LB(upstreams));
    lb.add_tcp("0.0.0.0:6188");

    // HTTP/2 with TLS proxy
    let cert_path = format!("{}/server.pem", env!("CARGO_MANIFEST_DIR"));
    let key_path = format!("{}/server.key", env!("CARGO_MANIFEST_DIR"));

    let mut tls_settings = pingora_core::listeners::TlsSettings::intermediate(&cert_path, &key_path).unwrap();
    tls_settings.enable_h2();
    lb.add_tls_with_settings("0.0.0.0:6189", None, tls_settings);

    // Prometheus metrics
    let mut prometheus_service_http = ListeningService::prometheus_http_service();
    prometheus_service_http.add_tcp("127.0.0.1:6150");

    // Add services
    my_server.add_service(lb);
    my_server.add_service(background);
    my_server.add_service(prometheus_service_http);

    // Launch server
    my_server.run_forever();
}
