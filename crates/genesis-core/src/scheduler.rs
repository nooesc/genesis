//! Minimal cron expression matcher for the scheduler.
//!
//! Supports a subset of standard 5-field cron:
//!   minute hour day-of-month month day-of-week
//!
//! Field syntax:
//!   * — matches any value
//!   N — matches exact value
//!   */N — matches every N (step)

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
        if field == "*" {
            return Ok(Self::Any);
        }

        if let Some(step) = field.strip_prefix("*/") {
            let n: u32 = step.parse().map_err(|_| CronParseError::InvalidField {
                field: field.to_owned(),
                reason: format!("step value `{step}` is not a valid number"),
            })?;
            if n == 0 {
                return Err(CronParseError::InvalidField {
                    field: field.to_owned(),
                    reason: "step value cannot be 0".to_owned(),
                });
            }
            return Ok(Self::Step(n));
        }

        let n: u32 = field.parse().map_err(|_| CronParseError::InvalidField {
            field: field.to_owned(),
            reason: "not a valid number, *, or */N".to_owned(),
        })?;
        Ok(Self::Exact(n))
    }

    fn matches(&self, value: u32) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(n) => value == *n,
            Self::Step(n) => value % n == 0,
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
            minute: 0, hour: 12, day_of_month: 1, month: 1, day_of_week: 1,
        }));
        assert!(expr.matches(&CronTime {
            minute: 15, hour: 3, day_of_month: 1, month: 1, day_of_week: 1,
        }));
        assert!(!expr.matches(&CronTime {
            minute: 7, hour: 3, day_of_month: 1, month: 1, day_of_week: 1,
        }));
    }

    #[test]
    fn matches_exact_time() {
        let expr = CronExpr::parse("30 14 * * *").unwrap();

        assert!(expr.matches(&CronTime {
            minute: 30, hour: 14, day_of_month: 5, month: 3, day_of_week: 6,
        }));
        assert!(!expr.matches(&CronTime {
            minute: 31, hour: 14, day_of_month: 5, month: 3, day_of_week: 6,
        }));
        assert!(!expr.matches(&CronTime {
            minute: 30, hour: 15, day_of_month: 5, month: 3, day_of_week: 6,
        }));
    }

    #[test]
    fn matches_daily_at_midnight() {
        let expr = CronExpr::parse("0 0 * * *").unwrap();

        assert!(expr.matches(&CronTime {
            minute: 0, hour: 0, day_of_month: 15, month: 6, day_of_week: 3,
        }));
        assert!(!expr.matches(&CronTime {
            minute: 0, hour: 1, day_of_month: 15, month: 6, day_of_week: 3,
        }));
    }

    #[test]
    fn matches_specific_day_of_week() {
        let expr = CronExpr::parse("0 9 * * 1").unwrap(); // Mon 9:00

        assert!(expr.matches(&CronTime {
            minute: 0, hour: 9, day_of_month: 10, month: 3, day_of_week: 1,
        }));
        assert!(!expr.matches(&CronTime {
            minute: 0, hour: 9, day_of_month: 11, month: 3, day_of_week: 2,
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
            minute: 10, hour: 14, day_of_month: 8, month: 3, day_of_week: 6,
        };

        let due = check_due_schedules(&schedules, &now);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, "s1");
        assert_eq!(due[0].prompt, "check status");
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
            minute: 0, hour: 0, day_of_month: 1, month: 1, day_of_week: 0,
        };

        let due = check_due_schedules(&schedules, &now);
        assert!(due.is_empty());
    }
}
