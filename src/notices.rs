//! Telling people. A rule says when, who and how, in steps that wait: tell the engineer now, the
//! manager in thirty minutes if nobody has done anything. Each step becomes a notice in a table before
//! anything is sent, so a restart loses none of them and escalation survives whatever happens to a
//! replica. A person dealing with an alarm drops what was still waiting.
//!
//! A notice goes out at least once: a crash between sending and saying so sends it again, and a
//! receiver that minds can use its id.

use crate::alarms::Alarm;
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use ferrum_kernel::jobs::{self, Job, JobHandler, NewJob};
use ferrum_kernel::{Ctx, Error, Kernel, Result, Tx, notify};
use serde_json::{Value, json};
use std::time::Duration as Wait;
use uuid::Uuid;

/// Attempts at a notice before it is marked failed.
const ATTEMPTS: i32 = 5;
/// Notices one round sends. The rest wait for the next.
const ROUND: i64 = 200;
const JOB: &str = "edge.notices";

pub const RAISED: &str = "Alarm raised";
pub const CLEARED: &str = "Alarm cleared";
pub const ACKNOWLEDGED: &str = "Alarm acknowledged";

/// How serious, so that a rule can say "this or worse".
pub fn rank(severity: &str) -> u8 {
    match severity {
        "Critical" => 4,
        "Major" => 3,
        "Minor" => 2,
        "Warning" => 1,
        _ => 0,
    }
}

/// `{summary}` and friends. What is not a known word stays as it was typed.
pub fn fill(template: &str, words: &[(&str, &str)]) -> String {
    words.iter().fold(template.to_string(), |text, (key, value)| text.replace(&format!("{{{key}}}"), value))
}

#[derive(sqlx::FromRow)]
struct RuleRow {
    id: Uuid,
    subject: Option<String>,
    body: Option<String>,
    min_severity: Option<String>,
    after_minutes: Option<i64>,
    channel: Option<String>,
    target: Option<String>,
    step: Option<i64>,
}

/// Puts what the rules say about something an alarm did in the table, to go when each is due.
pub async fn alarm_changed(tx: &mut Tx, ctx: &Ctx, alarm: &Alarm, trigger: &str, at: DateTime<Utc>) -> Result<()> {
    let rows: Vec<RuleRow> = sqlx::query_as(
        "select r.id, r.subject, r.body, r.min_severity, s.after_minutes, s.channel, s.target, s.idx::int8 as step \
         from e_edge_notify_rule r join e_edge_notify_rule__steps s on s.tenant_id = r.tenant_id and s.parent_id = r.id \
         left join e_device d on d.tenant_id = r.tenant_id and d.id = $4 \
         where r.tenant_id = $1 and coalesce(r.enabled, true) and r.trigger = $2 and (r.site is null or r.site = $3) and (r.device is null or r.device = $4) \
           and (r.definition is null or r.definition = d.edge_definition)",
    )
    .bind(ctx.tenant)
    .bind(trigger)
    .bind(alarm.site)
    .bind(alarm.device)
    .fetch_all(&mut **tx)
    .await?;
    let words = alarm.words();
    let words: Vec<(&str, &str)> = words.iter().map(|(k, v)| (*k, v.as_str())).collect();
    for r in rows {
        if rank(&alarm.severity) < rank(r.min_severity.as_deref().unwrap_or("Info")) {
            continue;
        }
        let (Some(channel), Some(target)) = (r.channel, r.target) else { continue };
        let subject = fill(r.subject.as_deref().filter(|s| !s.is_empty()).unwrap_or("{summary}"), &words);
        let body = fill(r.body.as_deref().filter(|b| !b.is_empty()).unwrap_or("{summary}\nSeverity: {severity}\nRaised: {raised_at}"), &words);
        sqlx::query(
            "insert into edge_notice (tenant_id, id, rule_id, step, alarm_id, trigger, channel, target, subject, body, due_at) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) on conflict do nothing",
        )
        .bind(ctx.tenant)
        .bind(Uuid::now_v7())
        .bind(r.id)
        .bind(r.step.unwrap_or(0) as i32)
        .bind(alarm.id)
        .bind(trigger)
        .bind(channel)
        .bind(target)
        .bind(subject)
        .bind(body)
        .bind(at + Duration::minutes(r.after_minutes.unwrap_or(0).max(0)))
        .execute(&mut **tx)
        .await?;
    }
    // Whatever is due now goes now, not at the next round of upkeep.
    jobs::enqueue(tx, ctx.tenant, None, NewJob { dedupe_key: Some(JOB.into()), ..NewJob::new(JOB, json!({})) }).await?;
    Ok(())
}

/// Someone dealt with it: what was waiting to raise the alarm with someone else goes no further.
pub async fn stand_down(tx: &mut Tx, ctx: &Ctx, alarm: Uuid) -> Result<()> {
    sqlx::query("update edge_notice set status = 'Dropped' where tenant_id = $1 and alarm_id = $2 and status = 'Waiting' and trigger = $3")
        .bind(ctx.tenant)
        .bind(alarm)
        .bind(RAISED)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct Due {
    id: Uuid,
    alarm_id: Uuid,
    channel: String,
    target: String,
    subject: String,
    body: String,
    attempts: i32,
}

async fn people(tx: &mut Tx, tenant: Uuid, target: &str) -> Result<Vec<(Uuid, String)>> {
    Ok(sqlx::query_as(
        "select u.id, u.email from app_user u where u.tenant_id = $1 and (u.email = $2 or exists (select 1 from user_role r where r.tenant_id = u.tenant_id and r.user_id = u.id and r.role = $2))",
    )
    .bind(tenant)
    .bind(target)
    .fetch_all(&mut **tx)
    .await?)
}

/// What goes over the wire for each kind of receiver.
pub fn message(channel: &str, due_id: Uuid, subject: &str, body: &str) -> Value {
    match channel {
        // Slack's and Teams' incoming webhooks both take this.
        "Slack" | "Teams" => json!({"text": format!("*{subject}*\n{body}")}),
        _ => json!({"id": due_id, "subject": subject, "body": body}),
    }
}

async fn deliver(k: &Kernel, tenant: Uuid, d: &Due) -> Result<()> {
    match d.channel.as_str() {
        "Bell" | "Email" => {
            let mut tx = k.db.tenant_tx(tenant).await?;
            let found =
                if d.target.contains('@') && d.channel == "Email" { vec![(Uuid::nil(), d.target.clone())] } else { people(&mut tx, tenant, &d.target).await? };
            if found.is_empty() {
                return Err(Error::invalid(format!("nobody is {}", d.target)));
            }
            for (user, email) in found {
                if d.channel == "Bell" {
                    notify::send_kind(&mut tx, tenant, user, "edge.alarm", "inapp", &d.subject, Some(&d.body), Some("EdgeAlarm"), Some(d.alarm_id)).await?;
                } else {
                    let html = format!("<p>{}</p>", d.body.replace('&', "&amp;").replace('<', "&lt;").replace('\n', "<br>"));
                    k.queue_mail(&mut tx, tenant, &email, &d.subject, &html).await?;
                }
            }
            tx.commit().await?;
            Ok(())
        }
        "Webhook" | "Slack" | "Teams" => {
            let (client, url) = k.vetted_client(&d.target, k.settings.webhooks_allow_private, Wait::from_secs(15), "Ferrum IoT Edge").await?;
            let answer = client.post(url).json(&message(&d.channel, d.id, &d.subject, &d.body)).send().await.map_err(|e| Error::Unavailable(e.to_string()))?;
            if answer.status().is_success() { Ok(()) } else { Err(Error::Unavailable(format!("it answered {}", answer.status()))) }
        }
        other => Err(Error::invalid(format!("{other} is not a way of telling anyone"))),
    }
}

/// Sends whatever is due, one round of it. Anything that fails tries again later, a few times, and
/// is then kept as failed, with why, for a person to see.
pub async fn send_due(k: &Kernel, ctx: &Ctx) -> Result<usize> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let due: Vec<Due> = sqlx::query_as(
        "update edge_notice set status = 'Sending', attempts = attempts + 1, claimed_at = now() where tenant_id = $1 and id in ( \
           select n.id from edge_notice n left join e_edge_alarm a on a.tenant_id = n.tenant_id and a.id = n.alarm_id \
           where n.tenant_id = $1 and ((n.status = 'Waiting' and n.due_at <= now()) or (n.status = 'Sending' and n.claimed_at < now() - interval '5 minutes')) \
             and (a.shelved_until is null or a.shelved_until <= now() or n.trigger <> 'Alarm raised') \
           order by n.due_at limit $2 for update of n skip locked) \
         returning id, alarm_id, channel, target, subject, body, attempts",
    )
    .bind(ctx.tenant)
    .bind(ROUND)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let count = due.len();
    for d in due {
        let outcome = deliver(k, ctx.tenant, &d).await;
        let mut tx = k.db.tenant_tx(ctx.tenant).await?;
        match outcome {
            Ok(()) => {
                sqlx::query("update edge_notice set status = 'Sent', sent_at = now(), error = null where tenant_id = $1 and id = $2")
                    .bind(ctx.tenant)
                    .bind(d.id)
                    .execute(&mut *tx)
                    .await?;
            }
            Err(e) => {
                let (status, wait) = if d.attempts >= ATTEMPTS || matches!(e, Error::Invalid(_) | Error::Validation(_)) {
                    ("Failed", 0)
                } else {
                    ("Waiting", i64::from(d.attempts).pow(2))
                };
                sqlx::query("update edge_notice set status = $3, error = $4, due_at = now() + make_interval(mins => $5) where tenant_id = $1 and id = $2")
                    .bind(ctx.tenant)
                    .bind(d.id)
                    .bind(status)
                    .bind(e.to_string())
                    .bind(wait as i32)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        tx.commit().await?;
    }
    Ok(count)
}

/// Runs a round as soon as an alarm has something to say.
pub struct Notices;

#[async_trait]
impl JobHandler for Notices {
    fn kind(&self) -> &'static str {
        JOB
    }

    fn title(&self) -> &'static str {
        "Alarm notices"
    }

    async fn run(&self, k: &Kernel, job: &Job) -> Result<Value> {
        let ctx = Ctx::system(job.tenant_id, k.registry.schema(job.tenant_id).await?);
        Ok(json!({"sent": send_due(k, &ctx).await?}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serious_goes_in_order() {
        assert!(rank("Critical") > rank("Major") && rank("Major") > rank("Minor") && rank("Minor") > rank("Warning") && rank("Warning") > rank("Info"));
        assert_eq!(rank("Something else"), 0);
    }

    #[test]
    fn a_template_says_what_it_knows() {
        assert_eq!(fill("{summary} on {device}, {nothing}", &[("summary", "Hot"), ("device", "P-1")]), "Hot on P-1, {nothing}");
    }

    #[test]
    fn a_chat_gets_text_and_anything_else_gets_the_fields() {
        let id = Uuid::nil();
        assert_eq!(message("Slack", id, "Hot", "Very")["text"], json!("*Hot*\nVery"));
        assert_eq!(message("Teams", id, "Hot", "Very")["text"], json!("*Hot*\nVery"));
        let hook = message("Webhook", id, "Hot", "Very");
        assert_eq!((hook["subject"].as_str(), hook["body"].as_str()), (Some("Hot"), Some("Very")));
    }
}
