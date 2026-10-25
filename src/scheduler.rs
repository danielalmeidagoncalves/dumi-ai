//! Fires scheduled jobs at the right time — reminders, recurring jobs and
//! the heartbeat.
//!
//! Deliberately simple design: every [`TICK`] seconds we read `jobs.json`,
//! run everything whose time has come, and write the file back. No timers,
//! no cron engine of our own — just a loop and time comparisons. For a
//! personal assistant, "exact to within 20 seconds" is plenty.

use crate::agent::Agent;
use crate::config::Config;
use crate::jobs::{self, Job};
use crate::timeparse;
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Local, Utc};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// How often we look at jobs.json for due work.
const TICK: Duration = Duration::from_secs(20);
/// How long one firing (model call + tools) may take before we give up.
const FIRE_TIMEOUT: Duration = Duration::from_secs(180);

/// Start the scheduler as a background task.
pub fn spawn(agent: Arc<Mutex<Agent>>, cfg: Config) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run(agent, cfg))
}

async fn run(agent: Arc<Mutex<Agent>>, cfg: Config) {
    // The heartbeat is a built-in recurring job derived from the config —
    // it deliberately lives only in memory, not in jobs.json.
    let mut heartbeat = match Heartbeat::new(&cfg.agent.heartbeat_cron) {
        Ok(hb) => Some(hb),
        Err(e) => {
            tracing::warn!(
                "invalid heartbeat_cron '{}', heartbeat disabled: {e:#}",
                cfg.agent.heartbeat_cron
            );
            None
        }
    };

    loop {
        // The `disable` flag works around a borrow rule: we cannot set
        // `heartbeat = None` while the `as_mut()` borrow is alive.
        let mut disable = false;
        if let Some(hb) = heartbeat.as_mut()
            && Utc::now() >= hb.next
        {
            fire_heartbeat(&agent, &cfg).await;
            if let Err(e) = hb.reschedule() {
                tracing::error!("cannot re-schedule heartbeat, disabling: {e:#}");
                disable = true;
            }
        }
        if disable {
            heartbeat = None;
        }

        if let Err(e) = fire_due_jobs(&agent, &cfg).await {
            tracing::error!("scheduler tick failed: {e:#}");
        }
        tokio::time::sleep(TICK).await;
    }
}

/// The heartbeat state: which cron we run on, and when it fires next.
struct Heartbeat {
    cron: String,
    next: DateTime<Utc>,
}

impl Heartbeat {
    fn new(cron: &str) -> Result<Self> {
        // next_run validates the expression at the same time.
        Ok(Self {
            cron: cron.to_string(),
            next: timeparse::next_run(cron)?,
        })
    }

    fn reschedule(&mut self) -> Result<()> {
        self.next = timeparse::next_run(&self.cron)?;
        Ok(())
    }
}

/// Ping the model: "look at your notes; anything the owner should hear?"
/// A heartbeat that has nothing to say stays silent, so it never spams.
async fn fire_heartbeat(agent: &Arc<Mutex<Agent>>, _cfg: &Config) {
    let now = Local::now().format("%Y-%m-%d %H:%M");
    let prompt = format!(
        "[heartbeat] Local time is {now}. Review your long-term memory and the scheduled \
         reminders. If the owner should hear something right now — an overdue promise, a \
         task worth surfacing, something you said you would check — reply with a short \
         message for them. Otherwise reply with exactly: nothing to report"
    );
    match call_agent(agent, &prompt).await {
        Ok(reply) => deliver("heartbeat", &reply),
        Err(e) => tracing::warn!("heartbeat failed: {e:#}"),
    }
}

/// Find due jobs, re-schedule or drop them, then fire them one by one.
async fn fire_due_jobs(agent: &Arc<Mutex<Agent>>, cfg: &Config) -> Result<()> {
    let now = Utc::now();
    let (due, mut rest) = split_due(jobs::load(&cfg.agent.data_dir), now);
    if due.is_empty() {
        return Ok(());
    }

    // Persist BEFORE firing: if dumi crashes mid-firing, a one-shot job
    // cannot fire twice, and recurring jobs keep their updated next time.
    for job in &due {
        if let Some(cron) = &job.recur {
            let mut next = job.clone();
            next.at = timeparse::next_run(cron)
                .with_context(|| format!("job {} has a broken cron expression", job.id))?;
            rest.push(next);
        }
        // One-shot jobs are simply not carried over — dropping = done.
    }
    jobs::save(&cfg.agent.data_dir, &rest)?;

    for job in &due {
        tracing::info!(job = %job.id, "firing job");
        let prompt = format!("[scheduled job {}] {}", job.id, job.title);
        match call_agent(agent, &prompt).await {
            Ok(reply) => deliver(&job.title, &reply),
            Err(e) => tracing::error!("job {} failed: {e:#}", job.id),
        }
    }
    Ok(())
}

/// Split jobs into (due now, not yet due). Pure function — easy to test.
fn split_due(all: Vec<Job>, now: DateTime<Utc>) -> (Vec<Job>, Vec<Job>) {
    all.into_iter().partition(|job| job.at <= now)
}

/// One bounded call into the agent, shared by jobs and the heartbeat.
async fn call_agent(agent: &Arc<Mutex<Agent>>, prompt: &str) -> Result<String> {
    // The timeout also covers waiting for the agent lock, so a stuck
    // conversation cannot keep a reminder waiting forever.
    match tokio::time::timeout(FIRE_TIMEOUT, async {
        agent.lock().await.handle(prompt).await
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(anyhow!(
            "timed out after {} seconds",
            FIRE_TIMEOUT.as_secs()
        )),
    }
}

/// Deliver a fired job's result to the owner. Until more channels exist,
/// this prints to dumi's terminal (the console may currently be sitting at
/// its "you> " prompt; the message simply appears above it — harmless).
fn deliver(title: &str, reply: &str) {
    let trimmed = reply.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("nothing to report") {
        return;
    }
    println!("\n⏰ {title}\n{trimmed}\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job_at(id: &str, now: DateTime<Utc>, minutes_from_now: i64) -> Job {
        Job {
            id: id.to_string(),
            title: "test".to_string(),
            // Computed from the FIXED `now` the test passes in, so there is
            // no race between two separate Utc::now() calls.
            at: now + chrono::TimeDelta::try_minutes(minutes_from_now).unwrap(),
            recur: None,
        }
    }

    #[test]
    fn split_due_separates_past_and_future() {
        let now = Utc::now();
        let jobs = vec![
            job_at("past", now, -5),
            job_at("now", now, 0),
            job_at("future", now, 5),
        ];
        let (due, rest) = split_due(jobs, now);
        let due_ids: Vec<&str> = due.iter().map(|j| j.id.as_str()).collect();
        assert_eq!(due_ids, vec!["past", "now"]); // "at <= now" counts as due
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].id, "future");
    }

    #[test]
    fn heartbeat_validates_its_cron() {
        assert!(Heartbeat::new("*/30 * * * *").is_ok());
        assert!(Heartbeat::new("not a cron").is_err());
    }
}
