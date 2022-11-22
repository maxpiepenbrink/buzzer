use std::{
    env::{remove_var, var, vars},
    str::FromStr,
    time::Duration,
};

use opentelemetry::{
    sdk::{
        trace::{self, RandomIdGenerator, Sampler},
        Resource,
    },
    trace::TraceError,
    KeyValue,
};
use opentelemetry_otlp::WithExportConfig;
use tonic::metadata::{MetadataKey, MetadataMap};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, Registry};
use url::Url;

// I like this pattern well enough so im copying it!
// https://github.com/open-telemetry/opentelemetry-rust/blob/main/examples/external-otlp-tonic-tokio/src/main.rs#L32
const ENDPOINT: &str = "OTLP_TONIC_ENDPOINT";
const HEADER_PREFIX: &str = "OTLP_TONIC_";

fn get_otlp_params() -> (Url, MetadataMap) {
    // get host url
    let endpoint = var(ENDPOINT).unwrap_or_else(|_| {
        panic!(
            "You must specify and endpoint to connect to with the variable {:?}.",
            ENDPOINT
        )
    });
    let endpoint = Url::parse(&endpoint).expect("endpoint is not a valid url");
    remove_var(ENDPOINT);

    // build the header map
    let mut metadata = MetadataMap::new();
    for (key, value) in vars()
        .filter(|(name, _)| name.starts_with(HEADER_PREFIX))
        .map(|(name, value)| {
            let header_name = name
                .strip_prefix(HEADER_PREFIX)
                .map(|h| h.replace('_', "-"))
                .map(|h| h.to_ascii_lowercase())
                .unwrap();
            (header_name, value)
        })
    {
        metadata.insert(MetadataKey::from_str(&key).unwrap(), value.parse().unwrap());
    }

    return (endpoint, metadata);
}

pub fn init_tracer() -> Result<(), TraceError> {
    let (endpoint, metadata) = get_otlp_params();

    let tracer = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(endpoint.as_str())
                .with_timeout(Duration::from_secs(3))
                .with_metadata(metadata),
        )
        .with_trace_config(
            trace::config()
                .with_sampler(Sampler::AlwaysOn)
                .with_id_generator(RandomIdGenerator::default())
                .with_max_events_per_span(64)
                .with_max_attributes_per_span(16)
                .with_max_events_per_span(16)
                .with_resource(Resource::new(vec![KeyValue::new("service.name", "buzzer")])),
        )
        .install_batch(opentelemetry::runtime::Tokio)?;

    let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

    // Use the tracing subscriber `Registry`, or any other subscriber
    // that impls `LookupSpan`
    let _subscriber = Registry::default()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "buzzer=info,tower_http=trace".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .with(telemetry)
        .init();

    Ok(())
}
