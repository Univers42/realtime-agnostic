/* ************************************************************************** */
/*                                                                            */
/*                                                        :::      ::::::::   */
/*   server.rs                                          :+:      :+:    :+:   */
/*                                                    +:+ +:+         +:+     */
/*   By: dlesieur <dlesieur@student.42.fr>          +#+  +:+       +#+        */
/*                                                +#+#+#+#+#+   +#+           */
/*   Created: 2026/04/07 11:13:47 by dlesieur          #+#    #+#             */
/*   Updated: 2026/04/07 11:23:06 by dlesieur         ###   ########.fr       */
/*                                                                            */
/* ************************************************************************** */

//! Server assembly — wires every crate together into a running HTTP/WS server.
//!
//! This module is the **composition root** of the system. It reads
//! [`ServerConfig`], instantiates the event bus,
//! auth provider, router, fan-out pool, database producers, and HTTP routes,
//! then binds a TCP listener.
//!
//! ## Start-up sequence
//!
//! 1. Build [`EventBus`] + publisher
//! 2. Build [`AuthProvider`]
//! 3. Build [`SubscriptionRegistry`], [`SequenceGenerator`], [`ConnectionManager`]
//! 4. Spawn [`FanOutWorkerPool`]
//! 5. Build [`EventRouter`] and connect it to the bus subscriber
//! 6. For each database entry in config, create a CDC producer via [`ProducerRegistry`]
//! 7. Build Axum router and start listening

use std::sync::Arc;

use axum::{
    routing::{get, post},
    Router,
};
use realtime_auth::NoAuthProvider;
use realtime_bus_inprocess::InProcessBus;
use realtime_core::{AuthProvider, DatabaseProducer, EventBus, EventBusPublisher};
use realtime_engine::{
    ProducerRegistry,
    registry::SubscriptionRegistry,
    router::EventRouter,
    sequence::SequenceGenerator,
};
use realtime_gateway::{
    connection::ConnectionManager,
    fanout::FanOutWorkerPool,
    rest_api,
    ws_handler::{self, AppState},
};
use tower_http::cors::CorsLayer;
use tracing::{error, info};

use crate::config::{AuthConfig, EventBusConfig, ServerConfig};

/// Build the default [`ProducerRegistry`] with built-in adapters.
///
/// Registers PostgreSQL and MongoDB factories so that the config-driven
/// `databases` array can reference `"postgresql"` or `"mongodb"` by name.
///
/// To add a new adapter, call `registry.register(Box::new(YourFactory))`.
pub fn default_producer_registry() -> ProducerRegistry {
    let registry = ProducerRegistry::new();
    registry.register(Box::new(realtime_db_postgres::PostgresFactory));
    registry.register(Box::new(realtime_db_mongodb::MongoFactory));
    // To add a new adapter:
    // registry.register(Box::new(realtime_db_mysql::MysqlFactory));
    // registry.register(Box::new(realtime_db_redis::RedisFactory));
    registry
}

/// Spawn a database CDC producer as a background task.
///
/// Calls [`DatabaseProducer::start()`] and forwards each emitted
/// [`EventEnvelope`] to the event bus via `bus_pub.publish()`.
///
/// # Arguments
///
/// * `producer` — A boxed producer (PostgreSQL, MongoDB, etc.).
/// * `bus_pub` — The shared event bus publisher.
/// * `adapter_name` — Human-readable adapter name for log messages.
fn spawn_producer_task(
    producer: Box<dyn DatabaseProducer>,
    bus_pub: Arc<dyn EventBusPublisher>,
    adapter_name: String,
) {
    tokio::spawn(async move {
        match producer.start().await {
            Ok(mut stream) => {
                while let Some(event) = stream.next_event().await {
                    if let Err(e) = bus_pub.publish(event.topic.as_str(), &event).await {
                        error!(adapter = %adapter_name, "Failed to publish event: {}", e);
                    }
                }
            }
            Err(e) => error!(adapter = %adapter_name, "Failed to start producer: {}", e),
        }
    });
}

/// Build and run the full realtime server.
///
/// This is the main entry point for the server. It assembles all components
/// from the given configuration and blocks until the server is shut down.
///
/// # Arguments
///
/// * `config` — The fully constructed [`ServerConfig`].
///
/// # Errors
///
/// Returns an error if the TCP listener cannot bind or any component
/// fails to initialise.
pub async fn run(config: ServerConfig) -> anyhow::Result<()> {
    // ─── Build event bus ─────────────────────────────────────────────
    let bus: Arc<dyn EventBus> = match &config.event_bus {
        EventBusConfig::InProcess { capacity } => {
            Arc::new(InProcessBus::new(*capacity))
        }
    };

    let publisher: Arc<dyn EventBusPublisher> = {
        let p = bus.publisher().await?;
        Arc::from(p)
    };

    // ─── Build auth provider ─────────────────────────────────────────
    let auth_provider: Arc<dyn AuthProvider> = match &config.auth {
        AuthConfig::NoAuth => Arc::new(NoAuthProvider::new()),
        AuthConfig::Jwt {
            secret,
            issuer,
            audience,
        } => {
            let mut jwt_config = realtime_auth::JwtConfig::hmac(secret.clone());
            jwt_config.issuer = issuer.clone();
            jwt_config.audience = audience.clone();
            Arc::new(realtime_auth::JwtAuthProvider::new(jwt_config))
        }
    };

    // ─── Build core components ───────────────────────────────────────
    let registry = Arc::new(SubscriptionRegistry::new());
    let sequence_gen = Arc::new(SequenceGenerator::new());
    let conn_manager = Arc::new(ConnectionManager::new(config.performance.send_queue_capacity));

    // ─── Build fan-out pool ──────────────────────────────────────────
    let fanout_pool = FanOutWorkerPool::new(
        Arc::clone(&conn_manager),
        config.performance.fanout_workers,
    );
    let dispatch_tx = fanout_pool.start();

    // ─── Build event router ─────────────────────────────────────────
    let router = Arc::new(EventRouter::new(
        Arc::clone(&registry),
        Arc::clone(&sequence_gen),
        dispatch_tx,
    ));

    // ─── Start bus subscriber → router loop ─────────────────────────
    let bus_subscriber = bus.subscriber("*").await?;
    let router_clone = Arc::clone(&router);
    tokio::spawn(async move {
        router_clone.run_with_subscriber(bus_subscriber).await;
    });

    // ─── Start database producers (generic via ProducerRegistry) ────
    let producer_registry = default_producer_registry();
    info!("Available adapters: {:?}", producer_registry.adapters());

    for db_config in &config.databases {
        let adapter = &db_config.adapter;
        match producer_registry.create_producer(adapter, db_config.config.clone()) {
            Ok(producer) => {
                let bus_pub = Arc::clone(&publisher);
                let adapter_name = adapter.clone();
                spawn_producer_task(producer, bus_pub, adapter_name.clone());
                info!(adapter = %adapter_name, "CDC producer configured");
            }
            Err(e) => {
                error!(adapter = %adapter, "Failed to create producer: {}", e);
            }
        }
    }

    // ─── Build HTTP/WS server ────────────────────────────────────────
    let app_state = AppState {
        conn_manager: Arc::clone(&conn_manager),
        registry: Arc::clone(&registry),
        auth_provider,
        bus_publisher: Arc::clone(&publisher),
    };

    let app = Router::new()
        .route("/ws", get(ws_handler::ws_upgrade))
        .route("/v1/publish", post(rest_api::publish_event))
        .route("/v1/publish/batch", post(rest_api::publish_batch))
        .route("/v1/health", get(rest_api::health_check))
        .fallback_service(tower_http::services::ServeDir::new(
            std::env::var("REALTIME_STATIC_DIR")
                .unwrap_or_else(|_| "sandbox/static".into()),
        ))
        .layer(CorsLayer::permissive())
        .with_state(app_state);

    let addr = format!("{}:{}", config.host, config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("Realtime server listening on {}", addr);

    axum::serve(listener, app).await?;

    // Shutdown
    bus.shutdown().await.ok();

    Ok(())
}
