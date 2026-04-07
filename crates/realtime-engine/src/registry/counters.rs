//! Counter queries for observability and testing.

use super::SubscriptionRegistry;

impl SubscriptionRegistry {
    /// Return the total count of active subscriptions across all connections.
    #[must_use]
    pub fn subscription_count(&self) -> usize {
        self.by_sub_id.len()
    }

    /// Return the number of connections that have at least one subscription.
    #[must_use]
    pub fn connection_count(&self) -> usize {
        self.by_connection.len()
    }
}
