/* ************************************************************************** */
/*                                                                            */
/*                                                        :::      ::::::::   */
/*   lib.rs                                             :+:      :+:    :+:   */
/*                                                    +:+ +:+         +:+     */
/*   By: dlesieur <dlesieur@student.42.fr>          +#+  +:+       +#+        */
/*                                                +#+#+#+#+#+   +#+           */
/*   Created: 2026/04/07 11:12:53 by dlesieur          #+#    #+#             */
/*   Updated: 2026/04/07 11:12:54 by dlesieur         ###   ########.fr       */
/*                                                                            */
/* ************************************************************************** */

mod producer;
mod config;

pub use producer::PostgresProducer;
pub use config::PostgresConfig;

/// Factory for creating PostgreSQL CDC producers from generic JSON config.
pub struct PostgresFactory;

impl realtime_core::ProducerFactory for PostgresFactory {
    fn name(&self) -> &str {
        "postgresql"
    }

    fn create(
        &self,
        config: serde_json::Value,
    ) -> realtime_core::Result<Box<dyn realtime_core::DatabaseProducer>> {
        let pg_config: PostgresConfig = serde_json::from_value(config)
            .map_err(|e| realtime_core::RealtimeError::Internal(
                format!("Invalid PostgreSQL config: {}", e)
            ))?;
        Ok(Box::new(PostgresProducer::new(pg_config)))
    }
}
