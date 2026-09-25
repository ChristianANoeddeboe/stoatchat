use std::time::{Duration, SystemTime};

use iso8601_timestamp::Timestamp;
use log::{info, warn};
use revolt_database::{Channel, Database, PartialChannel};
use revolt_result::Result;
use tokio::time::sleep;
use ulid::Ulid;

/// When something last happened in a thread
fn last_activity(thread: &Channel) -> Option<SystemTime> {
    let Channel::Thread {
        id,
        last_message_id,
        archived_at,
        ..
    } = thread
    else {
        return None;
    };

    [Some(id), last_message_id.as_ref()]
        .into_iter()
        .flatten()
        .filter_map(|id| Ulid::from_string(id).ok())
        .map(|id| id.datetime())
        .chain(archived_at.map(SystemTime::from))
        .max()
}

/// Archive threads which have been inactive for longer than their auto archive duration
pub async fn task(db: Database, _: revolt_database::AMQP) -> Result<()> {
    loop {
        match db.fetch_all_active_threads().await {
            Ok(threads) => {
                let now = SystemTime::now();
                for mut thread in threads {
                    let Channel::Thread {
                        auto_archive_minutes,
                        ..
                    } = &thread
                    else {
                        continue;
                    };

                    let limit = Duration::from_secs(*auto_archive_minutes as u64 * 60);
                    let Some(last) = last_activity(&thread) else {
                        continue;
                    };

                    if now.duration_since(last).unwrap_or_default() < limit {
                        continue;
                    }

                    info!("Archiving inactive thread {}", thread.id());
                    if let Err(error) = thread
                        .update(
                            &db,
                            PartialChannel {
                                archived: Some(true),
                                archived_at: Some(Timestamp::now_utc()),
                                ..Default::default()
                            },
                            vec![],
                        )
                        .await
                    {
                        revolt_config::capture_error(&error);
                        warn!("Failed to archive thread {}: {:?}", thread.id(), &error);
                    }
                }
            }
            Err(error) => {
                revolt_config::capture_error(&error);
                warn!("Failed to fetch active threads: {:?}", &error);
            }
        }

        sleep(Duration::from_secs(60)).await;
    }
}
