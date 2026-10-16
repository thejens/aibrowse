use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::connection::{Connection, Event};
use crate::error::{CdpError, Result};
use chromiumoxide_types::Command;

/// A flat-mode CDP session: the browser itself (`id == None`) or one attached target.
#[derive(Clone)]
pub struct Session {
    conn: Connection,
    id: Option<Arc<str>>,
}

impl Session {
    pub(crate) fn new(conn: Connection, id: Option<String>) -> Self {
        Self {
            conn,
            id: id.map(Arc::from),
        }
    }

    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Sends a strongly typed command from the generated protocol.
    pub async fn send<C: Command>(&self, command: C) -> Result<C::Response> {
        let method = command.identifier();
        let params = serde_json::to_value(&command).map_err(|source| CdpError::Serde {
            method: method.to_string(),
            source,
        })?;
        let value = self.conn.call(self.id(), &method, params, None).await?;
        C::response_from_value(value).map_err(|source| CdpError::Serde {
            method: method.to_string(),
            source,
        })
    }

    /// Sends an untyped command and returns the raw `result` object.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.conn.call(self.id(), method, params, None).await
    }

    /// Like [`Session::call`] with a custom timeout, for calls that legitimately take long.
    pub async fn call_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        self.conn
            .call(self.id(), method, params, Some(timeout))
            .await
    }

    /// Sends an untyped command and deserializes the result into a caller-owned type.
    pub async fn call_as<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T> {
        let value = self.call(method, params).await?;
        serde_json::from_value(value).map_err(|source| CdpError::Serde {
            method: method.to_owned(),
            source,
        })
    }

    /// Events addressed to this session only.
    pub fn events(&self) -> SessionEvents {
        SessionEvents {
            rx: self.conn.subscribe(),
            session_id: self.id.clone(),
        }
    }
}

/// A filtered view of the connection's event stream.
pub struct SessionEvents {
    rx: broadcast::Receiver<Event>,
    session_id: Option<Arc<str>>,
}

impl SessionEvents {
    /// Next event for this session, or `None` once the connection is closed.
    pub async fn recv(&mut self) -> Option<Event> {
        loop {
            match self.rx.recv().await {
                Ok(event) if event.session_id.as_deref() == self.session_id.as_deref() => {
                    return Some(event);
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "event subscriber lagged; events were dropped");
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}
