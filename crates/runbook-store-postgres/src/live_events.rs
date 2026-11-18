//! Committed-change hints over LISTEN/NOTIFY. A payload is only an
//! execution ID and sequence; clients fetch authoritative rows by ID, and
//! after a reconnect they catch up from stored sequences, never from hints.
use crate::{DatabaseConfig, Error};
use runbook_model::ExecutionId;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_postgres::AsyncMessage;

pub(crate) const CHANNEL: &str = "runbook_execution";

/// The whole payload: IDs and sequence only, never outputs or credentials.
pub(crate) fn payload(id: &ExecutionId, sequence: u64) -> String {
    format!("{id}:{sequence}")
}
fn parse(payload: &str) -> Option<LiveEvent> {
    let (id, sequence) = payload.split_once(':')?;
    Some(LiveEvent {
        execution_id: id.parse().ok()?,
        sequence: sequence.parse().ok()?,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveEvent {
    pub execution_id: ExecutionId,
    pub sequence: u64,
}

/// One dedicated LISTEN connection. Dropping it ends the driver task.
/// A closed stream returns an error from `next`; listen again and refetch
/// authoritative state — missed hints are not replayed.
pub struct LiveEvents {
    rx: mpsc::UnboundedReceiver<LiveEvent>,
    _client: tokio_postgres::Client,
    driver: tokio::task::JoinHandle<()>,
}
impl LiveEvents {
    /// Listen on the direct endpoint (DATABASE_URL_DIRECT, else the primary).
    /// Transaction-mode poolers cannot carry LISTEN; use a direct connection.
    pub async fn listen(config: &DatabaseConfig) -> Result<Self, Error> {
        let direct = config.direct_connection();
        let (client, mut connection) = direct
            .primary()
            .connect(postgres_native_tls::MakeTlsConnector::new(
                direct.tls().connector()?,
            ))
            .await
            .map_err(|_| Error("postgres.connection_failed"))?;
        let (tx, rx) = mpsc::unbounded_channel();
        let driver = tokio::spawn(async move {
            loop {
                match std::future::poll_fn(|cx| connection.poll_message(cx)).await {
                    Some(Ok(AsyncMessage::Notification(notification))) => {
                        if notification.channel() == CHANNEL
                            && let Some(event) = parse(notification.payload())
                            && tx.send(event).is_err()
                        {
                            break;
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => break,
                }
            }
        });
        client.batch_execute(&format!("LISTEN {CHANNEL}")).await?;
        Ok(Self {
            rx,
            _client: client,
            driver,
        })
    }

    pub async fn next(&mut self) -> Result<LiveEvent, Error> {
        self.rx
            .recv()
            .await
            .ok_or(Error("postgres.connection_closed"))
    }

    pub async fn next_timeout(&mut self, timeout: Duration) -> Result<Option<LiveEvent>, Error> {
        match tokio::time::timeout(timeout, self.rx.recv()).await {
            Ok(Some(event)) => Ok(Some(event)),
            Ok(None) => Err(Error("postgres.connection_closed")),
            Err(_) => Ok(None),
        }
    }
}
impl Drop for LiveEvents {
    fn drop(&mut self) {
        self.driver.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_is_id_and_sequence_only() {
        let id: ExecutionId = "exe_00000000000000000000000001".parse().unwrap();
        let text = payload(&id, 7);
        assert_eq!(text, "exe_00000000000000000000000001:7");
        assert_eq!(
            parse(&text),
            Some(LiveEvent {
                execution_id: id,
                sequence: 7
            })
        );
        assert_eq!(parse("exe_00000000000000000000000001"), None);
        assert_eq!(parse("exe_00000000000000000000000001:x"), None);
    }
}
