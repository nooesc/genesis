//! Cron expression matcher for the scheduler.
//!
//! Supports standard 5-field cron:
//!   minute hour day-of-month month day-of-week
//!
//! Field syntax:
//!
//! ```text
//!   *     - matches any value
//!   N     - matches exact value
//!   */N   - matches every N (step)
//!   N-M   - matches range from N to M (inclusive)
//!   N,M,P - matches any listed value (items can be exact, range, or step)
//! ```

use chrono::Datelike;
use chrono::Timelike;

/// A parsed cron expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronExpr {
    pub minute: CronField,
    pub hour: CronField,
    pub day_of_month: CronField,
    pub month: CronField,
    pub day_of_week: CronField,
}

/// A single cron field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CronField {
    /// Matches any value.
    Any,
    /// Matches an exact value.
    Exact(u32),
    /// Matches every N values (step), starting from 0.
    Step(u32),
    /// Matches any value in an inclusive range.
    Range(u32, u32),
    /// Matches any value in a list of sub-fields.
    List(Vec<CronField>),
}

/// Parsed time components for matching against a cron expression.
#[derive(Debug, Clone)]
pub struct CronTime {
    pub minute: u32,
    pub hour: u32,
    pub day_of_month: u32,
    pub month: u32,
    pub day_of_week: u32, // 0 = Sunday
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CronParseError {
    #[error("expected 5 fields, got {0}")]
    WrongFieldCount(usize),
    #[error("invalid field `{field}`: {reason}")]
    InvalidField { field: String, reason: String },
}

impl CronExpr {
    /// Parse a 5-field cron expression string.
    pub fn parse(expression: &str) -> Result<Self, CronParseError> {
        let fields: Vec<&str> = expression.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(CronParseError::WrongFieldCount(fields.len()));
        }

        Ok(Self {
            minute: CronField::parse(fields[0])?,
            hour: CronField::parse(fields[1])?,
            day_of_month: CronField::parse(fields[2])?,
            month: CronField::parse(fields[3])?,
            day_of_week: CronField::parse(fields[4])?,
        })
    }

    /// Check whether the given time matches this cron expression.
    pub fn matches(&self, time: &CronTime) -> bool {
        self.minute.matches(time.minute)
            && self.hour.matches(time.hour)
            && self.day_of_month.matches(time.day_of_month)
            && self.month.matches(time.month)
            && self.day_of_week.matches(time.day_of_week)
    }
}

impl CronField {
    fn parse(field: &str) -> Result<Self, CronParseError> {
        // Check for comma-separated list first
        if field.contains(',') {
            let items: Result<Vec<CronField>, CronParseError> = field
                .split(',')
                .map(|item| Self::parse_single(item, field))
                .collect();
            return Ok(Self::List(items?));
        }

        Self::parse_single(field, field)
    }

    /// Parse a single (non-list) cron field token.
    fn parse_single(token: &str, original: &str) -> Result<Self, CronParseError> {
        if token == "*" {
            return Ok(Self::Any);
        }

        if let Some(step) = token.strip_prefix("*/") {
            let n: u32 = step.parse().map_err(|_| CronParseError::InvalidField {
                field: original.to_owned(),
                reason: format!("step value `{step}` is not a valid number"),
            })?;
            if n == 0 {
                return Err(CronParseError::InvalidField {
                    field: original.to_owned(),
                    reason: "step value cannot be 0".to_owned(),
                });
            }
            return Ok(Self::Step(n));
        }

        // Check for range: N-M
        if let Some(dash_pos) = token.find('-') {
            let start_str = &token[..dash_pos];
            let end_str = &token[dash_pos + 1..];
            let start: u32 = start_str
                .parse()
                .map_err(|_| CronParseError::InvalidField {
                    field: original.to_owned(),
                    reason: format!("range start `{start_str}` is not a valid number"),
                })?;
            let end: u32 = end_str.parse().map_err(|_| CronParseError::InvalidField {
                field: original.to_owned(),
                reason: format!("range end `{end_str}` is not a valid number"),
            })?;
            if start > end {
                return Err(CronParseError::InvalidField {
                    field: original.to_owned(),
                    reason: format!("range start ({start}) is greater than end ({end})"),
                });
            }
            return Ok(Self::Range(start, end));
        }

        let n: u32 = token.parse().map_err(|_| CronParseError::InvalidField {
            field: original.to_owned(),
            reason: "not a valid number, *, */N, N-M, or comma-separated list".to_owned(),
        })?;
        Ok(Self::Exact(n))
    }

    fn matches(&self, value: u32) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(n) => value == *n,
            Self::Step(n) => value.is_multiple_of(*n),
            Self::Range(start, end) => value >= *start && value <= *end,
            Self::List(items) => items.iter().any(|item| item.matches(value)),
        }
    }
}

/// A schedule that is due for execution at the current time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueSchedule {
    pub id: String,
    pub destination: String,
    pub prompt: String,
}

/// Check which schedules from the given list are due at `now`.
///
/// Schedules with unparseable cron expressions are silently skipped.
pub fn check_due_schedules(
    schedules: &[genesis_storage::StoredSchedule],
    now: &CronTime,
) -> Vec<DueSchedule> {
    schedules
        .iter()
        .filter(|s| s.enabled)
        .filter_map(|s| {
            let expr = CronExpr::parse(&s.cron_expression).ok()?;
            if expr.matches(now) {
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

            // Sleep until the next minute boundary
            let now = chrono::Local::now();
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

        let now = chrono::Local::now();
        let cron_now = CronTime {
            minute: now.minute(),
            hour: now.hour(),
            day_of_month: now.day(),
            month: now.month(),
            day_of_week: now.weekday().num_days_from_sunday(),
        };

        let due = check_due_schedules(&schedules, &cron_now);
        if due.is_empty() {
            return;
        }

        tracing::info!(count = due.len(), "executing due schedules");
        for schedule in due {
            let id = schedule.id.clone();
            match self.executor.execute(schedule).await {
                Ok(()) => tracing::info!(schedule_id = id.as_str(), "schedule executed"),
                Err(e) => tracing::warn!(
                    schedule_id = id.as_str(),
                    error = e.as_str(),
                    "schedule execution failed"
                ),
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

        assert!(expr.matches(&CronTime {
            minute: 0,
            hour: 12,
            day_of_month: 1,
            month: 1,
            day_of_week: 1,
        }));
        assert!(expr.matches(&CronTime {
            minute: 15,
            hour: 3,
            day_of_month: 1,
            month: 1,
            day_of_week: 1,
        }));
        assert!(!expr.matches(&CronTime {
            minute: 7,
            hour: 3,
            day_of_month: 1,
            month: 1,
            day_of_week: 1,
        }));
    }

    #[test]
    fn matches_exact_time() {
        let expr = CronExpr::parse("30 14 * * *").unwrap();

        assert!(expr.matches(&CronTime {
            minute: 30,
            hour: 14,
            day_of_month: 5,
            month: 3,
            day_of_week: 6,
        }));
        assert!(!expr.matches(&CronTime {
            minute: 31,
            hour: 14,
            day_of_month: 5,
            month: 3,
            day_of_week: 6,
        }));
        assert!(!expr.matches(&CronTime {
            minute: 30,
            hour: 15,
            day_of_month: 5,
            month: 3,
            day_of_week: 6,
        }));
    }

    #[test]
    fn matches_daily_at_midnight() {
        let expr = CronExpr::parse("0 0 * * *").unwrap();

        assert!(expr.matches(&CronTime {
            minute: 0,
            hour: 0,
            day_of_month: 15,
            month: 6,
            day_of_week: 3,
        }));
        assert!(!expr.matches(&CronTime {
            minute: 0,
            hour: 1,
            day_of_month: 15,
            month: 6,
            day_of_week: 3,
        }));
    }

    #[test]
    fn matches_specific_day_of_week() {
        let expr = CronExpr::parse("0 9 * * 1").unwrap(); // Mon 9:00

        assert!(expr.matches(&CronTime {
            minute: 0,
            hour: 9,
            day_of_month: 10,
            month: 3,
            day_of_week: 1,
        }));
        assert!(!expr.matches(&CronTime {
            minute: 0,
            hour: 9,
            day_of_month: 11,
            month: 3,
            day_of_week: 2,
        }));
    }

    #[test]
    fn check_due_schedules_returns_matching() {
        let schedules = vec![
            genesis_storage::StoredSchedule {
                id: "s1".to_owned(),
                cron_expression: "*/5 * * * *".to_owned(),
                destination: "cli".to_owned(),
                prompt: "check status".to_owned(),
                enabled: true,
                created_at: "2026-03-08".to_owned(),
            },
            genesis_storage::StoredSchedule {
                id: "s2".to_owned(),
                cron_expression: "0 9 * * *".to_owned(),
                destination: "cli".to_owned(),
                prompt: "morning report".to_owned(),
                enabled: true,
                created_at: "2026-03-08".to_owned(),
            },
            genesis_storage::StoredSchedule {
                id: "s3".to_owned(),
                cron_expression: "*/5 * * * *".to_owned(),
                destination: "cli".to_owned(),
                prompt: "disabled job".to_owned(),
                enabled: false,
                created_at: "2026-03-08".to_owned(),
            },
        ];

        let now = CronTime {
            minute: 10,
            hour: 14,
            day_of_month: 8,
            month: 3,
            day_of_week: 6,
        };

        let due = check_due_schedules(&schedules, &now);
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

        assert!(expr.matches(&CronTime {
            minute: 0,
            hour: 9,
            day_of_month: 1,
            month: 1,
            day_of_week: 1,
        }));
        assert!(expr.matches(&CronTime {
            minute: 0,
            hour: 13,
            day_of_month: 1,
            month: 1,
            day_of_week: 1,
        }));
        assert!(expr.matches(&CronTime {
            minute: 0,
            hour: 17,
            day_of_month: 1,
            month: 1,
            day_of_week: 1,
        }));
        assert!(!expr.matches(&CronTime {
            minute: 0,
            hour: 8,
            day_of_month: 1,
            month: 1,
            day_of_week: 1,
        }));
        assert!(!expr.matches(&CronTime {
            minute: 0,
            hour: 18,
            day_of_month: 1,
            month: 1,
            day_of_week: 1,
        }));
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

        assert!(expr.matches(&CronTime {
            minute: 0,
            hour: 12,
            day_of_month: 1,
            month: 1,
            day_of_week: 1,
        }));
        assert!(expr.matches(&CronTime {
            minute: 30,
            hour: 12,
            day_of_month: 1,
            month: 1,
            day_of_week: 1,
        }));
        assert!(!expr.matches(&CronTime {
            minute: 10,
            hour: 12,
            day_of_month: 1,
            month: 1,
            day_of_week: 1,
        }));
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

        // Monday
        assert!(expr.matches(&CronTime {
            minute: 0,
            hour: 9,
            day_of_month: 10,
            month: 3,
            day_of_week: 1,
        }));
        // Friday
        assert!(expr.matches(&CronTime {
            minute: 0,
            hour: 9,
            day_of_month: 14,
            month: 3,
            day_of_week: 5,
        }));
        // Sunday
        assert!(!expr.matches(&CronTime {
            minute: 0,
            hour: 9,
            day_of_month: 9,
            month: 3,
            day_of_week: 0,
        }));
        // Saturday
        assert!(!expr.matches(&CronTime {
            minute: 0,
            hour: 9,
            day_of_month: 15,
            month: 3,
            day_of_week: 6,
        }));
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

        assert!(expr.matches(&CronTime {
            minute: 0,
            hour: 1,
            day_of_month: 1,
            month: 1,
            day_of_week: 1,
        }));
        assert!(expr.matches(&CronTime {
            minute: 0,
            hour: 4,
            day_of_month: 1,
            month: 1,
            day_of_week: 1,
        }));
        assert!(expr.matches(&CronTime {
            minute: 0,
            hour: 7,
            day_of_month: 1,
            month: 1,
            day_of_week: 1,
        }));
        assert!(!expr.matches(&CronTime {
            minute: 0,
            hour: 2,
            day_of_month: 1,
            month: 1,
            day_of_week: 1,
        }));
        assert!(!expr.matches(&CronTime {
            minute: 0,
            hour: 6,
            day_of_month: 1,
            month: 1,
            day_of_week: 1,
        }));
    }

    #[test]
    fn parse_rejects_inverted_range() {
        let err = CronExpr::parse("0 17-9 * * *").unwrap_err();
        assert!(matches!(err, CronParseError::InvalidField { .. }));
    }

    #[test]
    fn check_due_schedules_skips_invalid_cron() {
        let schedules = vec![genesis_storage::StoredSchedule {
            id: "bad".to_owned(),
            cron_expression: "not-valid".to_owned(),
            destination: "cli".to_owned(),
            prompt: "broken".to_owned(),
            enabled: true,
            created_at: "2026-03-08".to_owned(),
        }];

        let now = CronTime {
            minute: 0,
            hour: 0,
            day_of_month: 1,
            month: 1,
            day_of_week: 0,
        };

        let due = check_due_schedules(&schedules, &now);
        assert!(due.is_empty());
    }
}
