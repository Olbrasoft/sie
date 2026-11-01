use anyhow::{Result, ensure};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, MutexGuard};
use tokio_postgres::{Client, NoTls};

use super::Request;

pub struct Store {
    client: Mutex<Client>,
    capacity: usize,
    configuration: tokio_postgres::Config,
    consumer: AtomicBool,
}

pub struct Job {
    pub id: i64,
    pub request: Request,
}

impl Store {
    #[cfg(test)]
    pub(super) async fn isolated_test_store(url: &str, capacity: usize) -> Result<(Self, Client)> {
        let mut configuration: tokio_postgres::Config = url.parse()?;
        let admin = connect_client(&configuration).await?;
        let name: String = admin
            .query_one("SELECT current_database()", &[])
            .await?
            .get(0);
        ensure!(
            name.starts_with("test_sie_"),
            "Refusing a non-test notification database"
        );
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let schema = format!("sie_notify_{}_{stamp}", std::process::id());
        admin
            .batch_execute(&format!("CREATE SCHEMA {schema}"))
            .await?;
        configuration.options(format!("-c search_path={schema}"));
        let store = Self::from_config(configuration.clone(), capacity).await?;
        Ok((store, connect_client(&configuration).await?))
    }
    pub async fn connect(url: &str, capacity: usize) -> Result<Self> {
        Self::from_config(url.parse()?, capacity).await
    }

    async fn from_config(configuration: tokio_postgres::Config, capacity: usize) -> Result<Self> {
        let client = connect_client(&configuration).await?;
        let name: String = client
            .query_one("SELECT current_database()", &[])
            .await?
            .get(0);
        ensure!(
            name != "virtual_assistant",
            "Notifications require the independent SIE database"
        );
        client
            .batch_execute(include_str!("../../migrations/002_notifications.sql"))
            .await?;
        Ok(Self {
            client: Mutex::new(client),
            capacity,
            configuration,
            consumer: AtomicBool::new(false),
        })
    }

    async fn client(&self) -> Result<MutexGuard<'_, Client>> {
        let mut client = self.client.lock().await;
        if client.is_closed() {
            let next = connect_client(&self.configuration).await?;
            if self.consumer.load(Ordering::SeqCst) {
                let owned: bool = next
                    .query_one("SELECT pg_try_advisory_lock(5458245,85)", &[])
                    .await?
                    .get(0);
                ensure!(owned, "Notification consumer ownership changed");
            }
            *client = next;
        }
        Ok(client)
    }

    pub async fn own_consumer(&self) -> Result<()> {
        let owned: bool = self
            .client()
            .await?
            .query_one("SELECT pg_try_advisory_lock(5458245,85)", &[])
            .await?
            .get(0);
        ensure!(owned, "Another notification consumer owns this database");
        self.consumer.store(true, Ordering::SeqCst);
        Ok(())
    }

    pub async fn enqueue(&self, request: &Request) -> Result<i64> {
        let mut client = self.client().await?;
        let tx = client.transaction().await?;
        // Serialize capacity checks with inserts, including across connections.
        tx.query_one("SELECT pg_advisory_xact_lock(5458245,86)", &[])
            .await?;
        let count: i64 = tx
            .query_one("SELECT count(*) FROM notification_queue", &[])
            .await?
            .get(0);
        ensure!(count < self.capacity as i64, "Notification queue is full");
        let payload = serde_json::to_value(request)?;
        let id = tx
            .query_one(
                "INSERT INTO notification_queue(payload) VALUES ($1) RETURNING id",
                &[&payload],
            )
            .await?
            .get(0);
        tx.commit().await?;
        Ok(id)
    }

    pub async fn next(&self) -> Result<Option<Job>> {
        let row = self
            .client()
            .await?
            .query_opt(
                "SELECT id,payload,retry_at<=NOW() FROM notification_queue ORDER BY id LIMIT 1",
                &[],
            )
            .await?;
        match row {
            Some(row) if row.get::<_, bool>(2) => Ok(Some(Job {
                id: row.get(0),
                request: serde_json::from_value(row.get(1))?,
            })),
            _ => Ok(None),
        }
    }

    pub async fn finish(&self, id: i64) -> Result<()> {
        self.client()
            .await?
            .execute("DELETE FROM notification_queue WHERE id=$1", &[&id])
            .await?;
        Ok(())
    }

    pub async fn retry(&self, id: i64) -> Result<()> {
        self.client().await?.execute("UPDATE notification_queue SET attempts=LEAST(attempts+1,100),retry_at=NOW()+make_interval(secs=>LEAST(300,power(2,LEAST(attempts,8)))::int) WHERE id=$1", &[&id]).await?;
        Ok(())
    }

    pub async fn count(&self) -> Result<i64> {
        Ok(self
            .client()
            .await?
            .query_one("SELECT count(*) FROM notification_queue", &[])
            .await?
            .get(0))
    }

    pub async fn key_usage(
        &self,
        credentials: &[super::config::Credential],
    ) -> Result<serde_json::Value> {
        let client = self.client().await?;
        let mut values = Vec::new();
        for (index, credential) in credentials.iter().enumerate() {
            let row=client.query_opt("SELECT CASE WHEN counter_month=date_trunc('month',NOW() AT TIME ZONE 'UTC')::date THEN characters ELSE 0 END,disabled,cooldown_until::text,COALESCE(cooldown_until>NOW(),FALSE) FROM notification_key_usage WHERE reference=$1",&[&credential.reference]).await?;
            let characters = row.as_ref().map(|r| r.get::<_, i64>(0)).unwrap_or(0);
            let disabled = row.as_ref().is_some_and(|r| r.get::<_, bool>(1));
            let cooldown: Option<String> = row.as_ref().and_then(|r| r.get(2));
            let cooling = row.as_ref().is_some_and(|r| r.get::<_, bool>(3));
            values.push(serde_json::json!({"index":index,"name":credential.name,"monthlyCharacterCount":characters,"monthlyCharacterLimit":credential.monthly_limit,"cooldownUntilUtc":cooldown,"state":if disabled {"Invalid"} else if characters>=credential.monthly_limit {"MonthlyLimitExceeded"} else if cooling {"Cooldown"} else {"Available"}}));
        }
        Ok(serde_json::Value::Array(values))
    }

    pub async fn reserve_key(&self, reference: &str, chars: i64, limit: i64) -> Result<bool> {
        ensure!(chars > 0 && limit > 0, "Invalid quota reservation");
        let mut client = self.client().await?;
        let tx = client.transaction().await?;
        tx.execute(
            "INSERT INTO notification_key_usage(reference) VALUES ($1) ON CONFLICT DO NOTHING",
            &[&reference],
        )
        .await?;
        tx.execute("UPDATE notification_key_usage SET characters=0,counter_month=date_trunc('month',NOW() AT TIME ZONE 'UTC')::date WHERE reference=$1 AND counter_month<>date_trunc('month',NOW() AT TIME ZONE 'UTC')::date", &[&reference]).await?;
        let updated=tx.execute("UPDATE notification_key_usage SET characters=characters+$2 WHERE reference=$1 AND NOT disabled AND (cooldown_until IS NULL OR cooldown_until<=NOW()) AND characters+$2<=$3", &[&reference,&chars,&limit]).await?;
        tx.commit().await?;
        Ok(updated == 1)
    }

    pub async fn key_error(&self, reference: &str, status: u16, quota: bool) -> Result<()> {
        let seconds: f64 = if quota {
            86400.0
        } else if status == 429 {
            3600.0
        } else {
            300.0
        };
        let disabled = matches!(status, 401 | 403) && !quota;
        self.client().await?.execute("UPDATE notification_key_usage SET cooldown_until=NOW()+make_interval(secs=>$2),disabled=disabled OR $3 WHERE reference=$1", &[&reference,&seconds,&disabled]).await?;
        Ok(())
    }
}

async fn connect_client(configuration: &tokio_postgres::Config) -> Result<Client> {
    let mut config = configuration.clone();
    config.connect_timeout(std::time::Duration::from_secs(3));
    let (client, connection) = config.connect(NoTls).await?;
    tokio::spawn(async move {
        if connection.await.is_err() {
            tracing::warn!("Notification database connection closed");
        }
    });
    client
        .batch_execute("SET statement_timeout='3s'; SET idle_in_transaction_session_timeout='5s'")
        .await?;
    Ok(client)
}
