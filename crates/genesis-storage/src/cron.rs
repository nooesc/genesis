//! Cron expression parser and validator.
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
//!
//! Each field is range-checked:
//!   minute: 0-59, hour: 0-23, day-of-month: 1-31, month: 1-12, day-of-week: 0-6

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

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CronParseError {
    #[error("expected 5 fields, got {0}")]
    WrongFieldCount(usize),
    #[error("invalid field `{field}`: {reason}")]
    InvalidField { field: String, reason: String },
}

/// Valid ranges for each cron field position.
struct FieldBounds {
    label: &'static str,
    min: u32,
    max: u32,
}

const FIELD_BOUNDS: [FieldBounds; 5] = [
    FieldBounds {
        label: "minute",
        min: 0,
        max: 59,
    },
    FieldBounds {
        label: "hour",
        min: 0,
        max: 23,
    },
    FieldBounds {
        label: "day-of-month",
        min: 1,
        max: 31,
    },
    FieldBounds {
        label: "month",
        min: 1,
        max: 12,
    },
    FieldBounds {
        label: "day-of-week",
        min: 0,
        max: 6,
    },
];

impl CronExpr {
    /// Parse a 5-field cron expression string.
    pub fn parse(expression: &str) -> Result<Self, CronParseError> {
        let fields: Vec<&str> = expression.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(CronParseError::WrongFieldCount(fields.len()));
        }

        Ok(Self {
            minute: CronField::parse(fields[0], &FIELD_BOUNDS[0])?,
            hour: CronField::parse(fields[1], &FIELD_BOUNDS[1])?,
            day_of_month: CronField::parse(fields[2], &FIELD_BOUNDS[2])?,
            month: CronField::parse(fields[3], &FIELD_BOUNDS[3])?,
            day_of_week: CronField::parse(fields[4], &FIELD_BOUNDS[4])?,
        })
    }

    /// Check whether the given time matches this cron expression.
    pub fn matches(&self, minute: u32, hour: u32, day_of_month: u32, month: u32, day_of_week: u32) -> bool {
        self.minute.matches(minute)
            && self.hour.matches(hour)
            && self.day_of_month.matches(day_of_month)
            && self.month.matches(month)
            && self.day_of_week.matches(day_of_week)
    }
}

impl CronField {
    fn parse(field: &str, bounds: &FieldBounds) -> Result<Self, CronParseError> {
        // Check for comma-separated list first
        if field.contains(',') {
            let items: Result<Vec<CronField>, CronParseError> = field
                .split(',')
                .map(|item| Self::parse_single(item, field, bounds))
                .collect();
            return Ok(Self::List(items?));
        }

        Self::parse_single(field, field, bounds)
    }

    /// Parse a single (non-list) cron field token.
    fn parse_single(token: &str, original: &str, bounds: &FieldBounds) -> Result<Self, CronParseError> {
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
            check_bounds(start, bounds, original)?;
            check_bounds(end, bounds, original)?;
            return Ok(Self::Range(start, end));
        }

        let n: u32 = token.parse().map_err(|_| CronParseError::InvalidField {
            field: original.to_owned(),
            reason: "not a valid number, *, */N, N-M, or comma-separated list".to_owned(),
        })?;
        check_bounds(n, bounds, original)?;
        Ok(Self::Exact(n))
    }

    /// Check whether a concrete value matches this field.
    pub fn matches(&self, value: u32) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(n) => value == *n,
            Self::Step(n) => value.is_multiple_of(*n),
            Self::Range(start, end) => value >= *start && value <= *end,
            Self::List(items) => items.iter().any(|item| item.matches(value)),
        }
    }
}

/// Validate that a value is within the legal range for its field.
fn check_bounds(value: u32, bounds: &FieldBounds, original: &str) -> Result<(), CronParseError> {
    if value < bounds.min || value > bounds.max {
        return Err(CronParseError::InvalidField {
            field: original.to_owned(),
            reason: format!(
                "{} value {value} is out of range ({}-{})",
                bounds.label, bounds.min, bounds.max
            ),
        });
    }
    Ok(())
}

/// Validate that a cron expression is syntactically correct.
///
/// Returns `Ok(())` if valid, or an error describing what is wrong.
pub fn validate_cron(expression: &str) -> Result<(), CronParseError> {
    CronExpr::parse(expression).map(|_| ())
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
        assert!(expr.matches(0, 12, 1, 1, 1));
        assert!(expr.matches(15, 3, 1, 1, 1));
        assert!(!expr.matches(7, 3, 1, 1, 1));
    }

    #[test]
    fn matches_exact_time() {
        let expr = CronExpr::parse("30 14 * * *").unwrap();
        assert!(expr.matches(30, 14, 5, 3, 6));
        assert!(!expr.matches(31, 14, 5, 3, 6));
        assert!(!expr.matches(30, 15, 5, 3, 6));
    }

    #[test]
    fn matches_daily_at_midnight() {
        let expr = CronExpr::parse("0 0 * * *").unwrap();
        assert!(expr.matches(0, 0, 15, 6, 3));
        assert!(!expr.matches(0, 1, 15, 6, 3));
    }

    #[test]
    fn matches_specific_day_of_week() {
        let expr = CronExpr::parse("0 9 * * 1").unwrap(); // Mon 9:00
        assert!(expr.matches(0, 9, 10, 3, 1));
        assert!(!expr.matches(0, 9, 11, 3, 2));
    }

    #[test]
    fn parse_range() {
        let expr = CronExpr::parse("0 9-17 * * *").unwrap();
        assert_eq!(expr.hour, CronField::Range(9, 17));
    }

    #[test]
    fn matches_range() {
        let expr = CronExpr::parse("0 9-17 * * *").unwrap();
        assert!(expr.matches(0, 9, 1, 1, 1));
        assert!(expr.matches(0, 13, 1, 1, 1));
        assert!(expr.matches(0, 17, 1, 1, 1));
        assert!(!expr.matches(0, 8, 1, 1, 1));
        assert!(!expr.matches(0, 18, 1, 1, 1));
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
        assert!(expr.matches(0, 12, 1, 1, 1));
        assert!(expr.matches(30, 12, 1, 1, 1));
        assert!(!expr.matches(10, 12, 1, 1, 1));
    }

    #[test]
    fn parse_list_with_range() {
        let expr = CronExpr::parse("0 9 * * 1-5").unwrap();
        assert_eq!(expr.day_of_week, CronField::Range(1, 5));
    }

    #[test]
    fn matches_weekdays_only() {
        let expr = CronExpr::parse("0 9 * * 1-5").unwrap();
        assert!(expr.matches(0, 9, 10, 3, 1));  // Monday
        assert!(expr.matches(0, 9, 14, 3, 5));  // Friday
        assert!(!expr.matches(0, 9, 9, 3, 0));  // Sunday
        assert!(!expr.matches(0, 9, 15, 3, 6)); // Saturday
    }

    #[test]
    fn parse_mixed_list() {
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

        assert!(expr.matches(0, 1, 1, 1, 1));
        assert!(expr.matches(0, 4, 1, 1, 1));
        assert!(expr.matches(0, 7, 1, 1, 1));
        assert!(!expr.matches(0, 2, 1, 1, 1));
        assert!(!expr.matches(0, 6, 1, 1, 1));
    }

    #[test]
    fn parse_rejects_inverted_range() {
        let err = CronExpr::parse("0 17-9 * * *").unwrap_err();
        assert!(matches!(err, CronParseError::InvalidField { .. }));
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

    // P4: Range bounds validation tests
    #[test]
    fn rejects_minute_out_of_range() {
        assert!(validate_cron("60 * * * *").is_err());
        assert!(validate_cron("61 * * * *").is_err());
        assert!(validate_cron("59 * * * *").is_ok());
    }

    #[test]
    fn rejects_hour_out_of_range() {
        assert!(validate_cron("0 24 * * *").is_err());
        assert!(validate_cron("0 25 * * *").is_err());
        assert!(validate_cron("0 23 * * *").is_ok());
    }

    #[test]
    fn rejects_day_of_month_out_of_range() {
        assert!(validate_cron("0 0 0 * *").is_err());  // 0 is below min (1)
        assert!(validate_cron("0 0 32 * *").is_err());
        assert!(validate_cron("0 0 31 * *").is_ok());
        assert!(validate_cron("0 0 1 * *").is_ok());
    }

    #[test]
    fn rejects_month_out_of_range() {
        assert!(validate_cron("0 0 * 0 *").is_err());  // 0 is below min (1)
        assert!(validate_cron("0 0 * 13 *").is_err());
        assert!(validate_cron("0 0 * 12 *").is_ok());
        assert!(validate_cron("0 0 * 1 *").is_ok());
    }

    #[test]
    fn rejects_day_of_week_out_of_range() {
        assert!(validate_cron("0 0 * * 7").is_err());
        assert!(validate_cron("0 0 * * 6").is_ok());
        assert!(validate_cron("0 0 * * 0").is_ok());
    }

    #[test]
    fn rejects_range_with_out_of_bounds_values() {
        assert!(validate_cron("0-60 * * * *").is_err());
        assert!(validate_cron("0 0-24 * * *").is_err());
        assert!(validate_cron("0 0 0-31 * *").is_err());  // 0 is below min for day-of-month
        assert!(validate_cron("0 0 1-31 * *").is_ok());
    }
}
