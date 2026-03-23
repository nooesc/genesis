//! Scheduler runtime and cron matching helpers.
//!
//! The cron expression parser and validator live in [`genesis_storage::cron`].
//! This module re-exports the key types and adds timezone-aware scheduling on
//! top.

use chrono::Datelike;
use chrono::Timelike;

// Re-export the canonical cron types from genesis-storage so existing
// callers of `genesis_core::scheduler::{CronExpr, CronField, …}` keep working.
pub use genesis_storage::cron::{validate_cron, CronExpr, CronField, CronParseError};

/// Parsed time components for matching against a cron expression.
#[derive(Debug, Clone)]
pub struct CronTime {
    pub minute: u32,
    pub hour: u32,
    pub day_of_month: u32,
    pub month: u32,
    pub day_of_week: u32, // 0 = Sunday
}

impl CronTime {
    /// Check whether a `CronExpr` matches this time.
    pub fn matches_expr(&self, expr: &CronExpr) -> bool {
        expr.matches(
            self.minute,
            self.hour,
            self.day_of_month,
            self.month,
            self.day_of_week,
        )
    }
}

/// Resolve a schedule's timezone to a `chrono_tz::Tz`.
///
/// Returns `chrono_tz::UTC` when `timezone` is `None`.
/// Returns an error string if the timezone name is invalid.
pub fn resolve_timezone(timezone: Option<&str>) -> Result<chrono_tz::Tz, String> {
    match timezone {
        None => Ok(chrono_tz::UTC),
        Some(tz_name) => tz_name
            .parse::<chrono_tz::Tz>()
            .map_err(|_| format!("invalid timezone: {tz_name}")),
    }
}

/// Build a `CronTime` from the current wall-clock time in the given timezone.
pub fn cron_time_now(tz: chrono_tz::Tz) -> CronTime {
    let now = chrono::Utc::now().with_timezone(&tz);
    CronTime {
        minute: now.minute(),
        hour: now.hour(),
        day_of_month: now.day(),
        month: now.month(),
        day_of_week: now.weekday().num_days_from_sunday(),
    }
}

/// A schedule that is due for execution at the current time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueSchedule {
    pub id: String,
    pub destination: String,
    pub prompt: String,
}

/// Check which schedules from the given list are due right now.
///
/// Each schedule is evaluated in its own configured timezone (defaulting to
/// UTC when no timezone is set). Schedules with unparseable cron expressions
/// are logged as warnings and skipped.
pub fn check_due_schedules(schedules: &[genesis_storage::StoredSchedule]) -> Vec<DueSchedule> {
    schedules
        .iter()
        .filter(|s| s.enabled)
        .filter_map(|s| {
            let expr = match CronExpr::parse(&s.cron_expression) {
                Ok(e) => e,
                Err(err) => {
                    tracing::warn!(
                        schedule_id = s.id.as_str(),
                        cron = s.cron_expression.as_str(),
                        error = %err,
                        "skipping schedule with invalid cron expression"
                    );
                    return None;
                }
            };

            let tz = match resolve_timezone(s.timezone.as_deref()) {
                Ok(tz) => tz,
                Err(err) => {
                    tracing::warn!(
                        schedule_id = s.id.as_str(),
                        error = err.as_str(),
                        "invalid timezone for schedule, falling back to UTC"
                    );
                    chrono_tz::UTC
                }
            };

            let now = cron_time_now(tz);
            if now.matches_expr(&expr) {
                Some(DueSchedule {
                    id: s.id.clone(),
                    destination: s.destination.clone(),
                    prompt: s.prompt.clone(),
                })
            } else {
                None
            }
        })
        .collect()
}

/// Legacy version for callers that pass an explicit `CronTime`. Schedules with
/// unparseable cron expressions are silently skipped.
pub fn check_due_schedules_at(
    schedules: &[genesis_storage::StoredSchedule],
    now: &CronTime,
) -> Vec<DueSchedule> {
    schedules
        .iter()
        .filter(|s| s.enabled)
        .filter_map(|s| {
            let expr = CronExpr::parse(&s.cron_expression).ok()?;
            if now.matches_expr(&expr) {
                Some(DueSchedule {
                    id: s.id.clone(),
                    destination: s.destination.clone(),
                    prompt: s.prompt.clone(),
                })
            } else {
                None
            }
        })
        .collect()
}

/// Callback trait for schedule execution.
///
/// The scheduler runtime calls this when a schedule fires. The implementor
/// is responsible for actually executing the prompt (e.g., running it
/// through `SessionExecutionService`).
pub trait ScheduleExecutor: Send + Sync + 'static {
    /// Execute a due schedule. The `schedule` contains the destination and prompt.
    fn execute(
        &self,
        schedule: DueSchedule,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + '_>>;
}

/// Background scheduler that polls every minute and fires due schedules.
///
/// Spawned as a `tokio::spawn` task. Cancellation is cooperative via an
/// `AtomicBool` flag.
pub struct SchedulerRuntime {
    database_path: std::path::PathBuf,
    executor: std::sync::Arc<dyn ScheduleExecutor>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl SchedulerRuntime {
    pub fn new(
        database_path: std::path::PathBuf,
        executor: std::sync::Arc<dyn ScheduleExecutor>,
    ) -> Self {
        Self {
            database_path,
            executor,
            cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Returns a handle that can be used to stop the scheduler.
    pub fn cancellation_handle(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        std::sync::Arc::clone(&self.cancelled)
    }

    /// Run the scheduler loop. This blocks until cancelled.
    pub async fn run(self) {
        tracing::info!("scheduler runtime started");
        loop {
            if self.cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                tracing::info!("scheduler runtime cancelled");
                return;
            }

            self.tick().await;

            // Sleep until the next minute boundary (use UTC — each schedule
            // resolves its own timezone when checking whether it is due).
            let now = chrono::Utc::now();
            let secs_until_next_minute = 60 - now.second();
            tokio::time::sleep(std::time::Duration::from_secs(
                secs_until_next_minute as u64,
            ))
            .await;
        }
    }

    /// Check for due schedules and execute them.
    async fn tick(&self) {
        let store = genesis_storage::ScheduleStore::new(&self.database_path);
        let schedules = match store.list_enabled() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to load schedules");
                return;
            }
        };

        if schedules.is_empty() {
            return;
        }

        let due = check_due_schedules(&schedules);
        if due.is_empty() {
            return;
        }

        tracing::info!(count = due.len(), "executing due schedules");
        for schedule in due {
            let id = schedule.id.clone();
            let start = std::time::Instant::now();
            match self.executor.execute(schedule).await {
                Ok(()) => {
                    let duration_ms = start.elapsed().as_millis() as i64;
                    tracing::info!(schedule_id = id.as_str(), "schedule executed");
                    if let Err(e) = store.record_execution(&id, "success", None, Some(duration_ms))
                    {
                        tracing::warn!(
                            schedule_id = id.as_str(),
                            error = %e,
                            "failed to record schedule execution"
                        );
                    }
                }
                Err(e) => {
                    let duration_ms = start.elapsed().as_millis() as i64;
                    tracing::warn!(
                        schedule_id = id.as_str(),
                        error = e.as_str(),
                        "schedule execution failed"
                    );
                    if let Err(re) =
                        store.record_execution(&id, "error", Some(&e), Some(duration_ms))
                    {
                        tracing::warn!(
                            schedule_id = id.as_str(),
                            error = %re,
                            "failed to record schedule execution"
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_every_5_minutes() {
        let expr = CronExpr::parse("*/5 * * * *").expect("should parse");
        assert_eq!(expr.minute, CronField::Step(5));
        assert_eq!(expr.hour, CronField::Any);
    }

    #[test]
    fn parse_exact_time() {
        let expr = CronExpr::parse("30 14 * * *").expect("should parse");
        assert_eq!(expr.minute, CronField::Exact(30));
        assert_eq!(expr.hour, CronField::Exact(14));
    }

    #[test]
    fn parse_rejects_wrong_field_count() {
        let err = CronExpr::parse("*/5 *").unwrap_err();
        assert_eq!(err, CronParseError::WrongFieldCount(2));
    }

    #[test]
    fn parse_rejects_zero_step() {
        let err = CronExpr::parse("*/0 * * * *").unwrap_err();
        assert!(matches!(err, CronParseError::InvalidField { .. }));
    }

    #[test]
    fn matches_every_5_minutes() {
        let expr = CronExpr::parse("*/5 * * * *").unwrap();

        let t = |m| CronTime {
            minute: m,
            hour: 12,
            day_of_month: 1,
            month: 1,
            day_of_week: 1,
        };
        assert!(t(0).matches_expr(&expr));
        assert!(t(15).matches_expr(&expr));
        assert!(!t(7).matches_expr(&expr));
    }

    #[test]
    fn matches_exact_time() {
        let expr = CronExpr::parse("30 14 * * *").unwrap();

        assert!(CronTime {
            minute: 30,
            hour: 14,
            day_of_month: 5,
            month: 3,
            day_of_week: 6
        }
        .matches_expr(&expr));
        assert!(!CronTime {
            minute: 31,
            hour: 14,
            day_of_month: 5,
            month: 3,
            day_of_week: 6
        }
        .matches_expr(&expr));
        assert!(!CronTime {
            minute: 30,
            hour: 15,
            day_of_month: 5,
            month: 3,
            day_of_week: 6
        }
        .matches_expr(&expr));
    }

    #[test]
    fn matches_daily_at_midnight() {
        let expr = CronExpr::parse("0 0 * * *").unwrap();

        assert!(CronTime {
            minute: 0,
            hour: 0,
            day_of_month: 15,
            month: 6,
            day_of_week: 3
        }
        .matches_expr(&expr));
        assert!(!CronTime {
            minute: 0,
            hour: 1,
            day_of_month: 15,
            month: 6,
            day_of_week: 3
        }
        .matches_expr(&expr));
    }

    #[test]
    fn matches_specific_day_of_week() {
        let expr = CronExpr::parse("0 9 * * 1").unwrap(); // Mon 9:00

        assert!(CronTime {
            minute: 0,
            hour: 9,
            day_of_month: 10,
            month: 3,
            day_of_week: 1
        }
        .matches_expr(&expr));
        assert!(!CronTime {
            minute: 0,
            hour: 9,
            day_of_month: 11,
            month: 3,
            day_of_week: 2
        }
        .matches_expr(&expr));
    }

    #[test]
    fn check_due_schedules_at_returns_matching() {
        let schedules = vec![
            genesis_storage::StoredSchedule {
                id: "s1".to_owned(),
                cron_expression: "*/5 * * * *".to_owned(),
                destination: "cli".to_owned(),
                prompt: "check status".to_owned(),
                enabled: true,
                created_at: "2026-03-08".to_owned(),
                timezone: None,
            },
            genesis_storage::StoredSchedule {
                id: "s2".to_owned(),
                cron_expression: "0 9 * * *".to_owned(),
                destination: "cli".to_owned(),
                prompt: "morning report".to_owned(),
                enabled: true,
                created_at: "2026-03-08".to_owned(),
                timezone: None,
            },
            genesis_storage::StoredSchedule {
                id: "s3".to_owned(),
                cron_expression: "*/5 * * * *".to_owned(),
                destination: "cli".to_owned(),
                prompt: "disabled job".to_owned(),
                enabled: false,
                created_at: "2026-03-08".to_owned(),
                timezone: None,
            },
        ];

        let now = CronTime {
            minute: 10,
            hour: 14,
            day_of_month: 8,
            month: 3,
            day_of_week: 6,
        };

        let due = check_due_schedules_at(&schedules, &now);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, "s1");
        assert_eq!(due[0].prompt, "check status");
    }

    #[test]
    fn parse_range() {
        let expr = CronExpr::parse("0 9-17 * * *").unwrap();
        assert_eq!(expr.hour, CronField::Range(9, 17));
    }

    #[test]
    fn matches_range() {
        let expr = CronExpr::parse("0 9-17 * * *").unwrap(); // 9am-5pm hourly

        let t = |h| CronTime {
            minute: 0,
            hour: h,
            day_of_month: 1,
            month: 1,
            day_of_week: 1,
        };
        assert!(t(9).matches_expr(&expr));
        assert!(t(13).matches_expr(&expr));
        assert!(t(17).matches_expr(&expr));
        assert!(!t(8).matches_expr(&expr));
        assert!(!t(18).matches_expr(&expr));
    }

    #[test]
    fn parse_list() {
        let expr = CronExpr::parse("0,15,30,45 * * * *").unwrap();
        match &expr.minute {
            CronField::List(items) => {
                assert_eq!(items.len(), 4);
                assert_eq!(items[0], CronField::Exact(0));
                assert_eq!(items[1], CronField::Exact(15));
                assert_eq!(items[2], CronField::Exact(30));
                assert_eq!(items[3], CronField::Exact(45));
            }
            other => panic!("expected List, got {other:?}"),
        }
    }

    #[test]
    fn matches_list() {
        let expr = CronExpr::parse("0,15,30,45 * * * *").unwrap();

        let t = |m| CronTime {
            minute: m,
            hour: 12,
            day_of_month: 1,
            month: 1,
            day_of_week: 1,
        };
        assert!(t(0).matches_expr(&expr));
        assert!(t(30).matches_expr(&expr));
        assert!(!t(10).matches_expr(&expr));
    }

    #[test]
    fn parse_list_with_range() {
        // Weekdays only (Mon-Fri)
        let expr = CronExpr::parse("0 9 * * 1-5").unwrap();
        assert_eq!(expr.day_of_week, CronField::Range(1, 5));
    }

    #[test]
    fn matches_weekdays_only() {
        let expr = CronExpr::parse("0 9 * * 1-5").unwrap();

        let t = |dow| CronTime {
            minute: 0,
            hour: 9,
            day_of_month: 10,
            month: 3,
            day_of_week: dow,
        };
        assert!(t(1).matches_expr(&expr)); // Monday
        assert!(t(5).matches_expr(&expr)); // Friday
        assert!(!t(0).matches_expr(&expr)); // Sunday
        assert!(!t(6).matches_expr(&expr)); // Saturday
    }

    #[test]
    fn parse_mixed_list() {
        // Mixed list with ranges and exact values: "1,3-5,7"
        let expr = CronExpr::parse("0 1,3-5,7 * * *").unwrap();
        match &expr.hour {
            CronField::List(items) => {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0], CronField::Exact(1));
                assert_eq!(items[1], CronField::Range(3, 5));
                assert_eq!(items[2], CronField::Exact(7));
            }
            other => panic!("expected List, got {other:?}"),
        }

        let t = |h| CronTime {
            minute: 0,
            hour: h,
            day_of_month: 1,
            month: 1,
            day_of_week: 1,
        };
        assert!(t(1).matches_expr(&expr));
        assert!(t(4).matches_expr(&expr));
        assert!(t(7).matches_expr(&expr));
        assert!(!t(2).matches_expr(&expr));
        assert!(!t(6).matches_expr(&expr));
    }

    #[test]
    fn parse_rejects_inverted_range() {
        let err = CronExpr::parse("0 17-9 * * *").unwrap_err();
        assert!(matches!(err, CronParseError::InvalidField { .. }));
    }

    #[test]
    fn check_due_schedules_at_skips_invalid_cron() {
        let schedules = vec![genesis_storage::StoredSchedule {
            id: "bad".to_owned(),
            cron_expression: "not-valid".to_owned(),
            destination: "cli".to_owned(),
            prompt: "broken".to_owned(),
            enabled: true,
            created_at: "2026-03-08".to_owned(),
            timezone: None,
        }];

        let now = CronTime {
            minute: 0,
            hour: 0,
            day_of_month: 1,
            month: 1,
            day_of_week: 0,
        };

        let due = check_due_schedules_at(&schedules, &now);
        assert!(due.is_empty());
    }

    #[test]
    fn validate_cron_accepts_valid() {
        assert!(validate_cron("*/5 * * * *").is_ok());
        assert!(validate_cron("0 9 * * 1-5").is_ok());
        assert!(validate_cron("0,15,30,45 * * * *").is_ok());
    }

    #[test]
    fn validate_cron_rejects_invalid() {
        assert!(validate_cron("bad").is_err());
        assert!(validate_cron("*/0 * * * *").is_err());
        assert!(validate_cron("* *").is_err());
    }

    #[test]
    fn resolve_timezone_defaults_to_utc() {
        let tz = resolve_timezone(None).unwrap();
        assert_eq!(tz, chrono_tz::UTC);
    }

    #[test]
    fn resolve_timezone_parses_valid() {
        let tz = resolve_timezone(Some("America/New_York")).unwrap();
        assert_eq!(tz, chrono_tz::America::New_York);

        let tz = resolve_timezone(Some("Asia/Tokyo")).unwrap();
        assert_eq!(tz, chrono_tz::Asia::Tokyo);

        let tz = resolve_timezone(Some("Europe/London")).unwrap();
        assert_eq!(tz, chrono_tz::Europe::London);
    }

    #[test]
    fn resolve_timezone_rejects_invalid() {
        let err = resolve_timezone(Some("Not/A/Timezone")).unwrap_err();
        assert!(err.contains("invalid timezone"));
    }

    #[test]
    fn cron_time_now_uses_timezone() {
        // Just verify it doesn't panic and produces valid ranges
        let utc_time = cron_time_now(chrono_tz::UTC);
        assert!(utc_time.minute < 60);
        assert!(utc_time.hour < 24);
        assert!(utc_time.day_of_month >= 1 && utc_time.day_of_month <= 31);
        assert!(utc_time.month >= 1 && utc_time.month <= 12);
        assert!(utc_time.day_of_week < 7);

        let tokyo_time = cron_time_now(chrono_tz::Asia::Tokyo);
        assert!(tokyo_time.minute < 60);
        assert!(tokyo_time.hour < 24);
    }

    #[test]
    fn check_due_schedules_respects_timezone() {
        // Create two schedules with the same cron but different timezones.
        // At a given instant, one might be due and the other not, depending
        // on how the wall-clock differs. We verify the function at least runs
        // without errors and returns a consistent result.
        let schedules = vec![
            genesis_storage::StoredSchedule {
                id: "utc-sched".to_owned(),
                cron_expression: "* * * * *".to_owned(), // every minute
                destination: "cli".to_owned(),
                prompt: "utc job".to_owned(),
                enabled: true,
                created_at: "2026-03-08".to_owned(),
                timezone: None, // defaults to UTC
            },
            genesis_storage::StoredSchedule {
                id: "tokyo-sched".to_owned(),
                cron_expression: "* * * * *".to_owned(), // every minute
                destination: "cli".to_owned(),
                prompt: "tokyo job".to_owned(),
                enabled: true,
                created_at: "2026-03-08".to_owned(),
                timezone: Some("Asia/Tokyo".to_owned()),
            },
        ];

        // Both should fire since both match every minute
        let due = check_due_schedules(&schedules);
        assert_eq!(due.len(), 2);
    }
}
