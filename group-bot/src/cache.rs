//! Redis: caches in front of PostgreSQL, and short-lived real-time state
//! (rate counters, throttles, captcha deadlines, recent message ids).
//!
//! Every key is prefixed with `REDIS_PREFIX`. Callers treat cache failures
//! as misses and fall back to the database.

use std::collections::HashMap;

use anyhow::{Context, Result};
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use serde::de::DeserializeOwned;
use serde::Serialize;

/// How many recent message ids are kept per chat for bulk deletion.
const RECENT_MESSAGES: isize = 2000;

#[derive(Clone)]
pub struct Cache {
    conn: ConnectionManager,
    prefix: String,
}

impl Cache {
    pub async fn connect(url: &str, prefix: &str) -> Result<Cache> {
        let client = redis::Client::open(url).context("invalid REDIS_URL")?;
        let conn = ConnectionManager::new(client).await.context("cannot connect to Redis")?;
        Ok(Cache { conn, prefix: prefix.to_owned() })
    }

    fn key(&self, key: &str) -> String {
        format!("{}{key}", self.prefix)
    }

    pub async fn get_json<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let value: Option<String> = self.conn.clone().get(self.key(key)).await?;
        Ok(value.and_then(|v| serde_json::from_str(&v).ok()))
    }

    pub async fn set_json<T: Serialize>(&self, key: &str, value: &T, ttl_secs: u64) -> Result<()> {
        let json = serde_json::to_string(value)?;
        let _: () = self.conn.clone().set_ex(self.key(key), json, ttl_secs).await?;
        Ok(())
    }

    pub async fn del(&self, key: &str) -> Result<()> {
        let _: () = self.conn.clone().del(self.key(key)).await?;
        Ok(())
    }

    /// Sets a key only if it does not exist. Returns true if it was set:
    /// use it for "at most once per `ttl`" throttles and deduplication.
    pub async fn once(&self, key: &str, ttl_secs: u64) -> Result<bool> {
        let set: Option<String> = redis::cmd("SET")
            .arg(self.key(key))
            .arg(1)
            .arg("NX")
            .arg("EX")
            .arg(ttl_secs.max(1))
            .query_async(&mut self.conn.clone())
            .await?;
        Ok(set.is_some())
    }

    /// Counts an event in a fixed window of `window_secs` and returns the count.
    pub async fn hit(&self, key: &str, window_secs: u64) -> Result<i64> {
        let key = self.key(key);
        let mut conn = self.conn.clone();
        let (count,): (i64,) = redis::pipe()
            .atomic()
            .incr(&key, 1)
            .cmd("EXPIRE")
            .arg(&key)
            .arg(window_secs.max(1))
            .arg("NX")
            .ignore()
            .query_async(&mut conn)
            .await?;
        Ok(count)
    }

    pub async fn exists(&self, key: &str) -> Result<bool> {
        Ok(self.conn.clone().exists(self.key(key)).await?)
    }

    /// Deletes a key and reports whether it existed.
    pub async fn take(&self, key: &str) -> Result<bool> {
        let removed: i64 = self.conn.clone().del(self.key(key)).await?;
        Ok(removed > 0)
    }

    // ------------------------------------------------------- recent messages

    pub async fn push_recent(&self, chat_id: i64, message_id: i64) -> Result<()> {
        let key = self.key(&format!("recent:{chat_id}"));
        let mut conn = self.conn.clone();
        let _: () = redis::pipe()
            .lpush(&key, message_id)
            .ignore()
            .ltrim(&key, 0, RECENT_MESSAGES - 1)
            .ignore()
            .expire(&key, 3 * 86_400)
            .ignore()
            .query_async(&mut conn)
            .await?;
        Ok(())
    }

    /// Removes and returns the newest `count` message ids (all when `None`).
    pub async fn take_recent(&self, chat_id: i64, count: Option<isize>) -> Result<Vec<i64>> {
        let key = self.key(&format!("recent:{chat_id}"));
        let mut conn = self.conn.clone();
        let (ids,): (Vec<i64>,) = match count {
            Some(count) => {
                redis::pipe().atomic().lrange(&key, 0, count - 1).ltrim(&key, count, -1).ignore().query_async(&mut conn).await?
            }
            None => redis::pipe().atomic().lrange(&key, 0, -1).del(&key).ignore().query_async(&mut conn).await?,
        };
        Ok(ids)
    }

    // ------------------------------------------------------- message counters

    /// Buffers one message of `user` on local `day`; flushed by `drain_stats`.
    pub async fn incr_stat(&self, chat_id: i64, day: i32, user_id: i64) -> Result<()> {
        let key = self.key(&format!("st:{chat_id}:{day}"));
        let mut conn = self.conn.clone();
        let _: () = redis::pipe()
            .hincr(&key, user_id, 1)
            .ignore()
            .sadd(self.key("st:keys"), format!("{chat_id}:{day}"))
            .ignore()
            .query_async(&mut conn)
            .await?;
        Ok(())
    }

    /// Takes all buffered counters: (chat, day, user, messages).
    pub async fn drain_stats(&self) -> Result<Vec<(i64, i32, i64, i32)>> {
        let mut conn = self.conn.clone();
        let members: Vec<String> = conn.smembers(self.key("st:keys")).await?;
        let mut rows = Vec::new();
        for member in members {
            let Some((chat, day)) = member.split_once(':') else { continue };
            let (Ok(chat_id), Ok(day)) = (chat.parse::<i64>(), day.parse::<i32>()) else { continue };
            let key = self.key(&format!("st:{chat_id}:{day}"));
            // Read and delete atomically, so increments after this point are kept for the next drain.
            let (counts, _, _): (Vec<(i64, i32)>, (), ()) = redis::pipe()
                .atomic()
                .hgetall(&key)
                .del(&key)
                .srem(self.key("st:keys"), &member)
                .query_async(&mut conn)
                .await?;
            rows.extend(counts.into_iter().map(|(user, n)| (chat_id, day, user, n)));
        }
        Ok(rows)
    }

    // --------------------------------------------------- scheduled deadlines

    /// Schedules a deadline for `member` (an opaque string) in a sorted set.
    pub async fn schedule(&self, set: &str, member: &str, at: i64) -> Result<()> {
        let _: () = self.conn.clone().zadd(self.key(set), member, at).await?;
        Ok(())
    }

    pub async fn unschedule(&self, set: &str, member: &str) -> Result<()> {
        let _: () = self.conn.clone().zrem(self.key(set), member).await?;
        Ok(())
    }

    /// Removes and returns members whose deadline has passed.
    pub async fn take_due(&self, set: &str, now: i64) -> Result<Vec<String>> {
        let key = self.key(set);
        let mut conn = self.conn.clone();
        let (due,): (Vec<String>,) = redis::pipe()
            .atomic()
            .zrangebyscore(&key, "-inf", now)
            .zrembyscore(&key, "-inf", now)
            .ignore()
            .query_async(&mut conn)
            .await?;
        Ok(due)
    }

    pub async fn ping(&self) -> Result<()> {
        let _: String = redis::cmd("PING").query_async(&mut self.conn.clone()).await?;
        Ok(())
    }

    // ---------------------------------------------------- administration

    /// Adds to several fields of a hash and sets its lifetime.
    pub async fn hincr_many(&self, key: &str, fields: &[(String, i64)], ttl_secs: u64) -> Result<()> {
        let key = self.key(key);
        let mut pipe = redis::pipe();
        for (field, n) in fields {
            pipe.hincr(&key, field, *n).ignore();
        }
        pipe.expire(&key, ttl_secs as i64).ignore();
        let _: () = pipe.query_async(&mut self.conn.clone()).await?;
        Ok(())
    }

    /// Reads several hashes of counters in one round trip (missing ones are empty).
    pub async fn hgetall_many(&self, keys: &[String]) -> Result<Vec<HashMap<String, i64>>> {
        let mut pipe = redis::pipe();
        for key in keys {
            pipe.hgetall(self.key(key));
        }
        Ok(pipe.query_async(&mut self.conn.clone()).await?)
    }

    /// Deletes every key matching a glob pattern (after the prefix); returns how many.
    pub async fn del_matching(&self, pattern: &str) -> Result<usize> {
        let mut conn = self.conn.clone();
        let mut cursor: u64 = 0;
        let mut deleted = 0;
        loop {
            let (next, keys): (u64, Vec<String>) =
                redis::cmd("SCAN").arg(cursor).arg("MATCH").arg(self.key(pattern)).arg("COUNT").arg(500).query_async(&mut conn).await?;
            if !keys.is_empty() {
                deleted += keys.len();
                let _: () = conn.del(keys).await?;
            }
            if next == 0 {
                return Ok(deleted);
            }
            cursor = next;
        }
    }

    /// Server facts from `INFO`: version, memory, uptime and clients.
    pub async fn info(&self) -> Result<HashMap<String, String>> {
        let text: String = redis::cmd("INFO").query_async(&mut self.conn.clone()).await?;
        Ok(text
            .lines()
            .filter_map(|line| line.split_once(':'))
            .map(|(k, v)| (k.to_owned(), v.trim().to_owned()))
            .collect())
    }

    /// Keys in the current Redis database (all prefixes).
    pub async fn dbsize(&self) -> Result<i64> {
        Ok(redis::cmd("DBSIZE").query_async(&mut self.conn.clone()).await?)
    }

    pub async fn set_size(&self, key: &str) -> Result<i64> {
        Ok(self.conn.clone().scard(self.key(key)).await?)
    }
}

/// Needs a Redis server: `REDIS_URL=redis://127.0.0.1:6379 cargo test -p group-bot -- --ignored`.
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "needs Redis (REDIS_URL)"]
    async fn real_time_state_round_trips() {
        let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
        let prefix = format!("gbtest{}:", crate::text::random(1_000_000_000));
        let cache = Cache::connect(&url, &prefix).await.unwrap();
        cache.ping().await.unwrap();

        cache.set_json("k", &vec![1, 2, 3], 60).await.unwrap();
        assert_eq!(cache.get_json::<Vec<i32>>("k").await.unwrap(), Some(vec![1, 2, 3]));
        assert!(cache.take("k").await.unwrap());
        assert!(!cache.exists("k").await.unwrap());

        assert!(cache.once("o", 60).await.unwrap());
        assert!(!cache.once("o", 60).await.unwrap());
        assert_eq!(cache.hit("h", 60).await.unwrap(), 1);
        assert_eq!(cache.hit("h", 60).await.unwrap(), 2);

        for id in 1..=5 {
            cache.push_recent(-1, id).await.unwrap();
        }
        assert_eq!(cache.take_recent(-1, Some(2)).await.unwrap(), vec![5, 4]);
        assert_eq!(cache.take_recent(-1, None).await.unwrap(), vec![3, 2, 1]);

        cache.incr_stat(-1, 100, 7).await.unwrap();
        cache.incr_stat(-1, 100, 7).await.unwrap();
        cache.incr_stat(-1, 100, 8).await.unwrap();
        let mut rows = cache.drain_stats().await.unwrap();
        rows.sort();
        assert_eq!(rows, vec![(-1, 100, 7, 2), (-1, 100, 8, 1)]);
        assert!(cache.drain_stats().await.unwrap().is_empty());

        cache.schedule("due", "a", 10).await.unwrap();
        cache.schedule("due", "b", 99_999_999_999).await.unwrap();
        assert_eq!(cache.take_due("due", 20).await.unwrap(), vec!["a".to_owned()]);
        cache.unschedule("due", "b").await.unwrap();
        for key in ["o", "h"] {
            cache.del(key).await.unwrap();
        }
    }
}
