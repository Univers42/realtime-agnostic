/* ************************************************************************** */
/*                                                                            */
/*                                                        :::      ::::::::   */
/*   lib.rs                                             :+:      :+:    :+:   */
/*                                                    +:+ +:+         +:+     */
/*   By: dlesieur <dlesieur@student.42.fr>          +#+  +:+       +#+        */
/*                                                +#+#+#+#+#+   +#+           */
/*   Created: 2026/04/07 11:11:57 by dlesieur          #+#    #+#             */
/*   Updated: 2026/04/07 11:11:58 by dlesieur         ###   ########.fr       */
/*                                                                            */
/* ************************************************************************** */

//! # realtime-client
//!
//! Rust client SDK for the Realtime-Agnostic event engine.
//!
//! Features:
//!
//! - **Auto-reconnect** with exponential backoff + jitter (prevents thundering herd)
//! - **Automatic re-subscribe** on reconnection with `resume_from` sequence tracking
//! - **Client-side deduplication** via sliding-window HashSet of seen event IDs
//! - **Builder pattern** for ergonomic configuration
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! let client = RealtimeClient::builder("ws://localhost:4002/ws")
//!     .token("my-auth-token")
//!     .reconnect(true)
//!     .build()
//!     .await?;
//!
//! let mut events = client.connect().await?;
//! client.subscribe("my-sub", "orders/*", None).await?;
//!
//! while let Some(event) = events.recv().await {
//!     println!("{}: {}", event.topic, event.event_type);
//! }
//! ```

mod client;
mod subscription;

pub use client::{RealtimeClient, RealtimeClientBuilder};
pub use subscription::ClientSubscription;
