use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::PathBuf;

use genesis_core::replay::{load_and_report, ReplayEventCounts, ReplayReport};

use crate::CliError;
use crate::percentile;
use crate::sha256_hex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AggregatedToolUsage {
    pub(crate) name: String,
    pub(crate) call_count: usize,
    pub(crate) result_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvalSummary {
    pub(crate) directory: String,
    pub(crate) recursive: bool,
    pub(crate) model_filter: Option<String>,
    pub(crate) tag_filter: Option<String>,
    pub(crate) tool_filter: Option<String>,
    pub(crate) failures_only: bool,
    pub(crate) warnings_only: bool,
    pub(crate) min_warnings: Option<usize>,
    pub(crate) files_processed: usize,
    pub(crate) total_events: usize,
    pub(crate) event_counts: ReplayEventCounts,
    pub(crate) warnings: usize,
    pub(crate) success_count: usize,
    pub(crate) failure_count: usize,
    pub(crate) abandoned_count: usize,
    pub(crate) missing_outcome_count: usize,
    pub(crate) top_warning_messages: Vec<(String, usize)>,
    pub(crate) top_failure_reasons: Vec<(String, usize)>,
    pub(crate) models: Vec<(String, usize)>,
    pub(crate) tags: Vec<(String, usize)>,
    pub(crate) tools: Vec<AggregatedToolUsage>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EvalStats {
    pub(crate) directory: String,
    pub(crate) recursive: bool,
    pub(crate) model_filter: Option<String>,
    pub(crate) tag_filter: Option<String>,
    pub(crate) tool_filter: Option<String>,
    pub(crate) failures_only: bool,
    pub(crate) total_trajectories: usize,
    pub(crate) total_turns: usize,
    pub(crate) average_turns_per_trajectory: f64,
    pub(crate) min_turns: usize,
    pub(crate) max_turns: usize,
    pub(crate) p50_turns: usize,
    pub(crate) p90_turns: usize,
    pub(crate) p99_turns: usize,
    pub(crate) average_tool_calls_per_trajectory: f64,
    pub(crate) tool_usage: Vec<AggregatedToolUsage>,
    pub(crate) model_distribution: Vec<(String, usize)>,
    pub(crate) tag_distribution: Vec<(String, usize)>,
    pub(crate) outcome_distribution: Vec<(String, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReplayEventDelta {
    pub(crate) user: i64,
    pub(crate) assistant: i64,
    pub(crate) tool_call: i64,
    pub(crate) tool_result: i64,
    pub(crate) system: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolUsageDelta {
    pub(crate) name: String,
    pub(crate) left_call_count: usize,
    pub(crate) right_call_count: usize,
    pub(crate) left_result_count: usize,
    pub(crate) right_result_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvalComparison {
    pub(crate) left_path: String,
    pub(crate) right_path: String,
    pub(crate) left_session_id: String,
    pub(crate) right_session_id: String,
    pub(crate) left_model: String,
    pub(crate) right_model: String,
    pub(crate) left_total_events: usize,
    pub(crate) right_total_events: usize,
    pub(crate) left_warning_count: usize,
    pub(crate) right_warning_count: usize,
    pub(crate) event_delta: ReplayEventDelta,
    pub(crate) tools: Vec<ToolUsageDelta>,
    pub(crate) left_only_tags: Vec<String>,
    pub(crate) right_only_tags: Vec<String>,
}


#[allow(clippy::too_many_arguments)]
pub(crate) fn summarize_replay_reports(
    dir: &str,
    recursive: bool,
    model_filter: Option<&str>,
    tag_filter: Option<&str>,
    tool_filter: Option<&str>,
    failures_only: bool,
    warnings_only: bool,
    min_warnings: Option<usize>,
) -> Result<EvalSummary, CliError> {
    let reports = load_filtered_replay_reports(
        dir,
        recursive,
        model_filter,
        tag_filter,
        tool_filter,
        failures_only,
        warnings_only,
        min_warnings,
    )?;

    let mut model_counts = BTreeMap::<String, usize>::new();
    let mut tag_counts = BTreeMap::<String, usize>::new();
    let mut tool_counts = BTreeMap::<String, AggregatedToolUsage>::new();
    let mut event_counts = ReplayEventCounts::default();
    let mut total_events = 0usize;
    let mut warnings = 0usize;
    let mut success_count = 0usize;
    let mut failure_count = 0usize;
    let mut abandoned_count = 0usize;
    let mut missing_outcome_count = 0usize;
    let mut warning_counts = BTreeMap::<String, usize>::new();
    let mut failure_reasons = BTreeMap::<String, usize>::new();

    for report in &reports {
        total_events += report.total_events;
        warnings += report.warnings.len();
        event_counts.user += report.event_counts.user;
        event_counts.assistant += report.event_counts.assistant;
        event_counts.tool_call += report.event_counts.tool_call;
        event_counts.tool_result += report.event_counts.tool_result;
        event_counts.system += report.event_counts.system;

        *model_counts.entry(report.model.clone()).or_default() += 1;
        for tag in &report.tags {
            *tag_counts.entry(tag.clone()).or_default() += 1;
        }
        for tool in &report.tool_usage {
            let entry = tool_counts
                .entry(tool.name.clone())
                .or_insert_with(|| AggregatedToolUsage {
                    name: tool.name.clone(),
                    call_count: 0,
                    result_count: 0,
                });
            entry.call_count += tool.call_count;
            entry.result_count += tool.result_count;
        }
        for warning in &report.warnings {
            *warning_counts.entry(warning.message.clone()).or_default() += 1;
        }

        match &report.outcome {
            Some(genesis_core::trajectory::TrajectoryOutcome::Success) => success_count += 1,
            Some(genesis_core::trajectory::TrajectoryOutcome::Failure { .. }) => {
                failure_count += 1;
                if let Some(genesis_core::trajectory::TrajectoryOutcome::Failure { reason }) =
                    &report.outcome
                {
                    *failure_reasons.entry(reason.clone()).or_default() += 1;
                }
            }
            Some(genesis_core::trajectory::TrajectoryOutcome::Abandoned) => {
                abandoned_count += 1
            }
            None => missing_outcome_count += 1,
        }
    }

    let mut tools = tool_counts.into_values().collect::<Vec<_>>();
    tools.sort_by(|left, right| {
        right
            .call_count
            .cmp(&left.call_count)
            .then(right.result_count.cmp(&left.result_count))
            .then(left.name.cmp(&right.name))
    });
    let mut top_warning_messages = warning_counts.into_iter().collect::<Vec<_>>();
    top_warning_messages.sort_by(|left, right| {
        right.1.cmp(&left.1).then(left.0.cmp(&right.0))
    });
    top_warning_messages.truncate(5);

    let mut top_failure_reasons = failure_reasons.into_iter().collect::<Vec<_>>();
    top_failure_reasons.sort_by(|left, right| {
        right.1.cmp(&left.1).then(left.0.cmp(&right.0))
    });
    top_failure_reasons.truncate(5);

    Ok(EvalSummary {
        directory: dir.to_owned(),
        recursive,
        model_filter: model_filter.map(str::to_owned),
        tag_filter: tag_filter.map(str::to_owned),
        tool_filter: tool_filter.map(str::to_owned),
        failures_only,
        warnings_only,
        min_warnings,
        files_processed: reports.len(),
        total_events,
        event_counts,
        warnings,
        success_count,
        failure_count,
        abandoned_count,
        missing_outcome_count,
        top_warning_messages,
        top_failure_reasons,
        models: model_counts.into_iter().collect(),
        tags: tag_counts.into_iter().collect(),
        tools,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn load_filtered_replay_reports(
    dir: &str,
    recursive: bool,
    model_filter: Option<&str>,
    tag_filter: Option<&str>,
    tool_filter: Option<&str>,
    failures_only: bool,
    warnings_only: bool,
    min_warnings: Option<usize>,
) -> Result<Vec<ReplayReport>, CliError> {
    let mut reports = Vec::new();

    for path in collect_eval_files(PathBuf::from(dir), recursive)? {
        let report = load_and_report(&path).map_err(|e| CliError::Replay(e.to_string()))?;
        if let Some(model_filter) = model_filter {
            if report.model != model_filter {
                continue;
            }
        }
        if let Some(tag_filter) = tag_filter {
            if !report.tags.iter().any(|tag| tag == tag_filter) {
                continue;
            }
        }
        if let Some(tool_filter) = tool_filter {
            if !report.tool_usage.iter().any(|tool| tool.name == tool_filter) {
                continue;
            }
        }
        if failures_only
            && !matches!(
                report.outcome,
                Some(genesis_core::trajectory::TrajectoryOutcome::Failure { .. })
            )
        {
            continue;
        }
        if warnings_only && report.warnings.is_empty() {
            continue;
        }
        if let Some(min_warnings) = min_warnings {
            if report.warnings.len() < min_warnings {
                continue;
            }
        }
        reports.push(report);
    }

    Ok(reports)
}

pub(crate) fn compute_eval_stats(
    dir: &str,
    recursive: bool,
    model_filter: Option<&str>,
    tag_filter: Option<&str>,
    tool_filter: Option<&str>,
    failures_only: bool,
) -> Result<EvalStats, CliError> {
    let reports = load_filtered_replay_reports(
        dir,
        recursive,
        model_filter,
        tag_filter,
        tool_filter,
        failures_only,
        false,
        None,
    )?;

    let total_trajectories = reports.len();
    let mut turn_counts = reports.iter().map(|r| r.total_events).collect::<Vec<_>>();
    turn_counts.sort_unstable();

    let total_turns = turn_counts.iter().sum::<usize>();
    let min_turns = turn_counts.first().copied().unwrap_or(0);
    let max_turns = turn_counts.last().copied().unwrap_or(0);
    let average_turns_per_trajectory = if total_trajectories == 0 {
        0.0
    } else {
        total_turns as f64 / total_trajectories as f64
    };

    let total_tool_calls = reports
        .iter()
        .map(|report| report.event_counts.tool_call)
        .sum::<usize>();
    let average_tool_calls_per_trajectory = if total_trajectories == 0 {
        0.0
    } else {
        total_tool_calls as f64 / total_trajectories as f64
    };

    let mut tool_usage = BTreeMap::<String, AggregatedToolUsage>::new();
    let mut model_distribution = BTreeMap::<String, usize>::new();
    let mut tag_distribution = BTreeMap::<String, usize>::new();
    let mut outcome_distribution = BTreeMap::<String, usize>::new();

    for report in &reports {
        *model_distribution.entry(report.model.clone()).or_default() += 1;
        for tag in &report.tags {
            *tag_distribution.entry(tag.clone()).or_default() += 1;
        }

        let outcome = match &report.outcome {
            Some(genesis_core::trajectory::TrajectoryOutcome::Success) => "success",
            Some(genesis_core::trajectory::TrajectoryOutcome::Failure { .. }) => "failure",
            Some(genesis_core::trajectory::TrajectoryOutcome::Abandoned) => "abandoned",
            None => "missing",
        };
        *outcome_distribution.entry(outcome.to_owned()).or_default() += 1;

        for tool in &report.tool_usage {
            let entry = tool_usage
                .entry(tool.name.clone())
                .or_insert_with(|| AggregatedToolUsage {
                    name: tool.name.clone(),
                    call_count: 0,
                    result_count: 0,
                });
            entry.call_count += tool.call_count;
            entry.result_count += tool.result_count;
        }
    }

    let mut tool_usage = tool_usage.into_values().collect::<Vec<_>>();
    tool_usage.sort_by(|left, right| {
        right
            .call_count
            .cmp(&left.call_count)
            .then(right.result_count.cmp(&left.result_count))
            .then(left.name.cmp(&right.name))
    });

    Ok(EvalStats {
        directory: dir.to_owned(),
        recursive,
        model_filter: model_filter.map(str::to_owned),
        tag_filter: tag_filter.map(str::to_owned),
        tool_filter: tool_filter.map(str::to_owned),
        failures_only,
        total_trajectories,
        total_turns,
        average_turns_per_trajectory,
        min_turns,
        max_turns,
        p50_turns: percentile(&turn_counts, 50.0),
        p90_turns: percentile(&turn_counts, 90.0),
        p99_turns: percentile(&turn_counts, 99.0),
        average_tool_calls_per_trajectory,
        tool_usage,
        model_distribution: model_distribution.into_iter().collect(),
        tag_distribution: tag_distribution.into_iter().collect(),
        outcome_distribution: outcome_distribution.into_iter().collect(),
    })
}

pub(crate) fn collect_eval_files(dir: PathBuf, recursive: bool) -> Result<Vec<PathBuf>, CliError> {
    let mut files = Vec::new();

    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if recursive {
                files.extend(collect_eval_files(path, true)?);
            }
            continue;
        }

        if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            files.push(path);
        }
    }

    files.sort();
    Ok(files)
}

pub(crate) fn compare_replay_reports(left: &str, right: &str) -> Result<EvalComparison, CliError> {
    let left_report = load_and_report(left).map_err(|e| CliError::Replay(e.to_string()))?;
    let right_report = load_and_report(right).map_err(|e| CliError::Replay(e.to_string()))?;

    let mut tool_deltas = BTreeMap::<String, ToolUsageDelta>::new();
    for tool in &left_report.tool_usage {
        tool_deltas.insert(
            tool.name.clone(),
            ToolUsageDelta {
                name: tool.name.clone(),
                left_call_count: tool.call_count,
                right_call_count: 0,
                left_result_count: tool.result_count,
                right_result_count: 0,
            },
        );
    }
    for tool in &right_report.tool_usage {
        let entry = tool_deltas
            .entry(tool.name.clone())
            .or_insert_with(|| ToolUsageDelta {
                name: tool.name.clone(),
                left_call_count: 0,
                right_call_count: 0,
                left_result_count: 0,
                right_result_count: 0,
            });
        entry.right_call_count = tool.call_count;
        entry.right_result_count = tool.result_count;
    }

    let left_tags = left_report.tags.iter().cloned().collect::<HashSet<_>>();
    let right_tags = right_report.tags.iter().cloned().collect::<HashSet<_>>();

    let mut tools = tool_deltas.into_values().collect::<Vec<_>>();
    tools.sort_by(|left, right| left.name.cmp(&right.name));

    let mut left_only_tags = left_tags
        .difference(&right_tags)
        .cloned()
        .collect::<Vec<_>>();
    left_only_tags.sort();

    let mut right_only_tags = right_tags
        .difference(&left_tags)
        .cloned()
        .collect::<Vec<_>>();
    right_only_tags.sort();

    Ok(EvalComparison {
        left_path: left.to_owned(),
        right_path: right.to_owned(),
        left_session_id: left_report.session_id,
        right_session_id: right_report.session_id,
        left_model: left_report.model,
        right_model: right_report.model,
        left_total_events: left_report.total_events,
        right_total_events: right_report.total_events,
        left_warning_count: left_report.warnings.len(),
        right_warning_count: right_report.warnings.len(),
        event_delta: ReplayEventDelta {
            user: right_report.event_counts.user as i64 - left_report.event_counts.user as i64,
            assistant: right_report.event_counts.assistant as i64
                - left_report.event_counts.assistant as i64,
            tool_call: right_report.event_counts.tool_call as i64
                - left_report.event_counts.tool_call as i64,
            tool_result: right_report.event_counts.tool_result as i64
                - left_report.event_counts.tool_result as i64,
            system: right_report.event_counts.system as i64
                - left_report.event_counts.system as i64,
        },
        tools,
        left_only_tags,
        right_only_tags,
    })
}

pub(crate) fn format_replay_report(report: &ReplayReport) -> String {
    let mut output = String::new();
    output.push_str("genesis eval report\n");
    output.push_str(&format!("session:      {}\n", report.session_id));
    output.push_str(&format!("model:        {}\n", report.model));
    output.push_str(&format!("started:      {}\n", report.started_at));
    output.push_str(&format!(
        "completed:    {}\n",
        report.completed_at.as_deref().unwrap_or("<missing>")
    ));
    output.push_str(&format!(
        "outcome:      {}\n",
        match &report.outcome {
            Some(genesis_core::trajectory::TrajectoryOutcome::Success) => "success",
            Some(genesis_core::trajectory::TrajectoryOutcome::Failure { .. }) => "failure",
            Some(genesis_core::trajectory::TrajectoryOutcome::Abandoned) => "abandoned",
            None => "<missing>",
        }
    ));
    output.push_str(&format!("tags:         {}\n", report.tags.join(", ")));
    output.push_str(&format!("events:       {}\n", report.total_events));
    output.push_str(&format!(
        "event counts: user={} assistant={} tool_call={} tool_result={} system={}\n",
        report.event_counts.user,
        report.event_counts.assistant,
        report.event_counts.tool_call,
        report.event_counts.tool_result,
        report.event_counts.system
    ));
    output.push_str(&format!("warnings:     {}\n", report.warnings.len()));

    if !report.tool_usage.is_empty() {
        output.push_str("tools:\n");
        for tool in &report.tool_usage {
            output.push_str(&format!(
                "  - {}\tcall={} result={}\n",
                tool.name, tool.call_count, tool.result_count
            ));
        }
    }

    if !report.warnings.is_empty() {
        output.push_str("replay warnings:\n");
        for warning in &report.warnings {
            output.push_str(&format!("  - {}\n", warning.message));
        }
    }

    output
}

pub(crate) fn format_eval_summary(summary: &EvalSummary) -> String {
    let mut output = String::new();
    output.push_str("genesis eval summarize\n");
    output.push_str(&format!("directory:       {}\n", summary.directory));
    output.push_str(&format!("recursive:       {}\n", summary.recursive));
    output.push_str(&format!(
        "model filter:    {}\n",
        summary.model_filter.as_deref().unwrap_or("<none>")
    ));
    output.push_str(&format!(
        "tag filter:      {}\n",
        summary.tag_filter.as_deref().unwrap_or("<none>")
    ));
    output.push_str(&format!(
        "tool filter:     {}\n",
        summary.tool_filter.as_deref().unwrap_or("<none>")
    ));
    output.push_str(&format!("failures only:   {}\n", summary.failures_only));
    output.push_str(&format!("warnings only:   {}\n", summary.warnings_only));
    output.push_str(&format!(
        "min warnings:    {}\n",
        summary
            .min_warnings
            .map(|count| count.to_string())
            .unwrap_or_else(|| "<none>".to_owned())
    ));
    output.push_str(&format!("files:           {}\n", summary.files_processed));
    output.push_str(&format!("total events:    {}\n", summary.total_events));
    output.push_str(&format!(
        "event counts:    user={} assistant={} tool_call={} tool_result={} system={}\n",
        summary.event_counts.user,
        summary.event_counts.assistant,
        summary.event_counts.tool_call,
        summary.event_counts.tool_result,
        summary.event_counts.system
    ));
    output.push_str(&format!("warnings:        {}\n", summary.warnings));
    output.push_str(&format!(
        "outcomes:        success={} failure={} abandoned={} missing={}\n",
        summary.success_count,
        summary.failure_count,
        summary.abandoned_count,
        summary.missing_outcome_count
    ));

    if !summary.top_warning_messages.is_empty() {
        output.push_str("top warnings:\n");
        for (message, count) in &summary.top_warning_messages {
            output.push_str(&format!("  - {count}x {message}\n"));
        }
    }

    if !summary.top_failure_reasons.is_empty() {
        output.push_str("top failure reasons:\n");
        for (reason, count) in &summary.top_failure_reasons {
            output.push_str(&format!("  - {count}x {reason}\n"));
        }
    }

    if !summary.models.is_empty() {
        output.push_str("models:\n");
        for (model, count) in &summary.models {
            output.push_str(&format!("  - {model}: {count}\n"));
        }
    }

    if !summary.tags.is_empty() {
        output.push_str("tags:\n");
        for (tag, count) in &summary.tags {
            output.push_str(&format!("  - {tag}: {count}\n"));
        }
    }

    if !summary.tools.is_empty() {
        output.push_str("tools:\n");
        for tool in &summary.tools {
            output.push_str(&format!(
                "  - {}\tcall={} result={}\n",
                tool.name, tool.call_count, tool.result_count
            ));
        }
    }

    output
}

pub(crate) fn format_eval_comparison(comparison: &EvalComparison) -> String {
    let mut output = String::new();
    output.push_str("genesis eval compare\n");
    output.push_str(&format!("left:            {}\n", comparison.left_path));
    output.push_str(&format!("right:           {}\n", comparison.right_path));
    output.push_str(&format!(
        "sessions:        {} vs {}\n",
        comparison.left_session_id, comparison.right_session_id
    ));
    output.push_str(&format!(
        "models:          {} vs {}\n",
        comparison.left_model, comparison.right_model
    ));
    output.push_str(&format!(
        "total events:    {} -> {}\n",
        comparison.left_total_events, comparison.right_total_events
    ));
    output.push_str(&format!(
        "warnings:        {} -> {}\n",
        comparison.left_warning_count, comparison.right_warning_count
    ));
    output.push_str(&format!(
        "event delta:     user={:+} assistant={:+} tool_call={:+} tool_result={:+} system={:+}\n",
        comparison.event_delta.user,
        comparison.event_delta.assistant,
        comparison.event_delta.tool_call,
        comparison.event_delta.tool_result,
        comparison.event_delta.system
    ));

    if !comparison.left_only_tags.is_empty() || !comparison.right_only_tags.is_empty() {
        output.push_str("tag differences:\n");
        if !comparison.left_only_tags.is_empty() {
            output.push_str(&format!(
                "  - left only: {}\n",
                comparison.left_only_tags.join(", ")
            ));
        }
        if !comparison.right_only_tags.is_empty() {
            output.push_str(&format!(
                "  - right only: {}\n",
                comparison.right_only_tags.join(", ")
            ));
        }
    }

    if !comparison.tools.is_empty() {
        output.push_str("tool deltas:\n");
        for tool in &comparison.tools {
            output.push_str(&format!(
                "  - {}\tcall {} -> {}\tresult {} -> {}\n",
                tool.name,
                tool.left_call_count,
                tool.right_call_count,
                tool.left_result_count,
                tool.right_result_count
            ));
        }
    }

    output
}

pub(crate) fn format_eval_stats(stats: &EvalStats) -> String {
    let mut output = String::new();
    output.push_str("genesis eval stats\n");
    output.push_str(&format!("directory:                 {}\n", stats.directory));
    output.push_str(&format!("recursive:                 {}\n", stats.recursive));
    output.push_str(&format!(
        "model filter:              {}\n",
        stats.model_filter.as_deref().unwrap_or("<none>")
    ));
    output.push_str(&format!(
        "tag filter:                {}\n",
        stats.tag_filter.as_deref().unwrap_or("<none>")
    ));
    output.push_str(&format!(
        "tool filter:               {}\n",
        stats.tool_filter.as_deref().unwrap_or("<none>")
    ));
    output.push_str(&format!("failures only:             {}\n", stats.failures_only));
    output.push_str(&format!("total trajectories:        {}\n", stats.total_trajectories));
    output.push_str(&format!("total turns:               {}\n", stats.total_turns));
    output.push_str(&format!(
        "avg turns / trajectory:    {:.2}\n",
        stats.average_turns_per_trajectory
    ));
    output.push_str(&format!("min turns:                 {}\n", stats.min_turns));
    output.push_str(&format!("max turns:                 {}\n", stats.max_turns));
    output.push_str(&format!("p50 turns:                 {}\n", stats.p50_turns));
    output.push_str(&format!("p90 turns:                 {}\n", stats.p90_turns));
    output.push_str(&format!("p99 turns:                 {}\n", stats.p99_turns));
    output.push_str(&format!(
        "avg tool calls / traj:     {:.2}\n",
        stats.average_tool_calls_per_trajectory
    ));

    if !stats.tool_usage.is_empty() {
        output.push_str("tool usage:\n");
        for tool in &stats.tool_usage {
            output.push_str(&format!(
                "  - {}\tcall={} result={}\n",
                tool.name, tool.call_count, tool.result_count
            ));
        }
    }
    if !stats.model_distribution.is_empty() {
        output.push_str("model distribution:\n");
        for (model, count) in &stats.model_distribution {
            output.push_str(&format!("  - {model}: {count}\n"));
        }
    }
    if !stats.tag_distribution.is_empty() {
        output.push_str("tag distribution:\n");
        for (tag, count) in &stats.tag_distribution {
            output.push_str(&format!("  - {tag}: {count}\n"));
        }
    }
    if !stats.outcome_distribution.is_empty() {
        output.push_str("outcome distribution:\n");
        for (outcome, count) in &stats.outcome_distribution {
            output.push_str(&format!("  - {outcome}: {count}\n"));
        }
    }

    output
}

pub(crate) fn eval_summary_to_json(summary: &EvalSummary) -> serde_json::Value {
    serde_json::json!({
        "directory": summary.directory,
        "recursive": summary.recursive,
        "model_filter": summary.model_filter,
        "tag_filter": summary.tag_filter,
        "tool_filter": summary.tool_filter,
        "failures_only": summary.failures_only,
        "warnings_only": summary.warnings_only,
        "min_warnings": summary.min_warnings,
        "files_processed": summary.files_processed,
        "total_events": summary.total_events,
        "event_counts": {
            "user": summary.event_counts.user,
            "assistant": summary.event_counts.assistant,
            "tool_call": summary.event_counts.tool_call,
            "tool_result": summary.event_counts.tool_result,
            "system": summary.event_counts.system,
        },
        "warnings": summary.warnings,
        "success_count": summary.success_count,
        "failure_count": summary.failure_count,
        "abandoned_count": summary.abandoned_count,
        "missing_outcome_count": summary.missing_outcome_count,
        "top_warning_messages": summary.top_warning_messages.iter().map(|(message, count)| {
            serde_json::json!({
                "message": message,
                "count": count,
            })
        }).collect::<Vec<_>>(),
        "top_failure_reasons": summary.top_failure_reasons.iter().map(|(reason, count)| {
            serde_json::json!({
                "reason": reason,
                "count": count,
            })
        }).collect::<Vec<_>>(),
        "models": summary.models.iter().map(|(model, count)| {
            serde_json::json!({
                "model": model,
                "count": count,
            })
        }).collect::<Vec<_>>(),
        "tags": summary.tags.iter().map(|(tag, count)| {
            serde_json::json!({
                "tag": tag,
                "count": count,
            })
        }).collect::<Vec<_>>(),
        "tools": summary.tools.iter().map(|tool| {
            serde_json::json!({
                "name": tool.name,
                "call_count": tool.call_count,
                "result_count": tool.result_count,
            })
        }).collect::<Vec<_>>(),
    })
}

pub(crate) fn eval_comparison_to_json(comparison: &EvalComparison) -> serde_json::Value {
    serde_json::json!({
        "left_path": comparison.left_path,
        "right_path": comparison.right_path,
        "left_session_id": comparison.left_session_id,
        "right_session_id": comparison.right_session_id,
        "left_model": comparison.left_model,
        "right_model": comparison.right_model,
        "left_total_events": comparison.left_total_events,
        "right_total_events": comparison.right_total_events,
        "left_warning_count": comparison.left_warning_count,
        "right_warning_count": comparison.right_warning_count,
        "event_delta": {
            "user": comparison.event_delta.user,
            "assistant": comparison.event_delta.assistant,
            "tool_call": comparison.event_delta.tool_call,
            "tool_result": comparison.event_delta.tool_result,
            "system": comparison.event_delta.system,
        },
        "tools": comparison.tools.iter().map(|tool| {
            serde_json::json!({
                "name": tool.name,
                "left_call_count": tool.left_call_count,
                "right_call_count": tool.right_call_count,
                "left_result_count": tool.left_result_count,
                "right_result_count": tool.right_result_count,
            })
        }).collect::<Vec<_>>(),
        "left_only_tags": comparison.left_only_tags,
        "right_only_tags": comparison.right_only_tags,
    })
}

pub(crate) fn eval_stats_to_json(stats: &EvalStats) -> serde_json::Value {
    serde_json::json!({
        "directory": stats.directory,
        "recursive": stats.recursive,
        "model_filter": stats.model_filter,
        "tag_filter": stats.tag_filter,
        "tool_filter": stats.tool_filter,
        "failures_only": stats.failures_only,
        "total_trajectories": stats.total_trajectories,
        "total_turns": stats.total_turns,
        "average_turns_per_trajectory": stats.average_turns_per_trajectory,
        "min_turns": stats.min_turns,
        "max_turns": stats.max_turns,
        "p50_turns": stats.p50_turns,
        "p90_turns": stats.p90_turns,
        "p99_turns": stats.p99_turns,
        "average_tool_calls_per_trajectory": stats.average_tool_calls_per_trajectory,
        "tool_usage": stats.tool_usage.iter().map(|tool| {
            serde_json::json!({
                "name": tool.name,
                "call_count": tool.call_count,
                "result_count": tool.result_count,
            })
        }).collect::<Vec<_>>(),
        "model_distribution": stats.model_distribution.iter().map(|(model, count)| {
            serde_json::json!({
                "model": model,
                "count": count,
            })
        }).collect::<Vec<_>>(),
        "tag_distribution": stats.tag_distribution.iter().map(|(tag, count)| {
            serde_json::json!({
                "tag": tag,
                "count": count,
            })
        }).collect::<Vec<_>>(),
        "outcome_distribution": stats.outcome_distribution.iter().map(|(outcome, count)| {
            serde_json::json!({
                "outcome": outcome,
                "count": count,
            })
        }).collect::<Vec<_>>(),
    })
}

pub(crate) fn run_eval_export_chatml(dir: &str, recursive: bool) -> Result<String, CliError> {
    let mut lines = Vec::new();

    for path in collect_eval_files(PathBuf::from(dir), recursive)? {
        let compressed = load_training_compressed_trajectory(&path)?;
        let line = serde_json::json!({
            "session_id": compressed.session_id,
            "model": compressed.model,
            "tags": compressed.tags,
            "outcome": compressed.outcome,
            "chatml": genesis_core::compress::to_chatml(&compressed),
        });
        lines.push(serde_json::to_string(&line)?);
    }

    Ok(lines.join("\n"))
}

pub(crate) fn run_eval_import_sharegpt(file: &str, output_dir: &str) -> Result<String, CliError> {
    let contents = std::fs::read_to_string(file)
        .map_err(|e| CliError::Other(format!("failed to read {file}: {e}")))?;
    std::fs::create_dir_all(output_dir)
        .map_err(|e| CliError::Other(format!("failed to create {output_dir}: {e}")))?;

    let mut imported = 0usize;
    for (index, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }

        let entry: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| CliError::Other(format!("invalid JSONL line {}: {e}", index + 1)))?;
        let trajectory = trajectory_from_sharegpt_entry(&entry, index)?;
        let session_id = sanitize_session_id_for_filename(&trajectory.session_id);
        let output_path = std::path::Path::new(output_dir).join(format!("{session_id}.json"));
        let json = serde_json::to_string_pretty(&trajectory)?;
        std::fs::write(&output_path, json).map_err(|e| {
            CliError::Other(format!("failed to write {}: {e}", output_path.display()))
        })?;
        imported += 1;
    }

    Ok(format!("imported {imported} trajectories into {output_dir}"))
}

pub(crate) fn run_eval_merge(sources: &[String], output: &str, dedup: bool) -> Result<String, CliError> {
    std::fs::create_dir_all(output)
        .map_err(|e| CliError::Other(format!("failed to create {output}: {e}")))?;

    let mut copied = 0usize;
    let mut skipped = 0usize;
    let mut seen_ids: HashSet<String> = HashSet::new();

    for source in sources {
        let source_path = PathBuf::from(source);
        if !source_path.is_dir() {
            return Err(CliError::Other(format!("{source} is not a directory")));
        }

        for entry in std::fs::read_dir(&source_path).map_err(|e| {
            CliError::Other(format!("failed to read directory {source}: {e}"))
        })? {
            let entry = entry.map_err(|e| {
                CliError::Other(format!("failed to read entry in {source}: {e}"))
            })?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let filename = path.file_name().unwrap().to_string_lossy().to_string();

            if dedup {
                let raw = std::fs::read_to_string(&path).map_err(|e| {
                    CliError::Other(format!("failed to read {}: {e}", path.display()))
                })?;
                if let Ok(traj) =
                    serde_json::from_str::<genesis_core::trajectory::Trajectory>(&raw)
                {
                    if !seen_ids.insert(traj.session_id.clone()) {
                        skipped += 1;
                        continue;
                    }
                }
            }

            let dest = std::path::Path::new(output).join(&filename);
            if dest.exists() {
                skipped += 1;
                continue;
            }

            std::fs::copy(&path, &dest).map_err(|e| {
                CliError::Other(format!(
                    "failed to copy {} -> {}: {e}",
                    path.display(),
                    dest.display()
                ))
            })?;
            copied += 1;
        }
    }

    Ok(format!(
        "merged {copied} trajectories into {output} (skipped {skipped})"
    ))
}

pub(crate) fn run_eval_export_sharegpt(dir: &str, recursive: bool) -> Result<String, CliError> {
    let mut lines = Vec::new();

    for path in collect_eval_files(PathBuf::from(dir), recursive)? {
        let compressed = load_training_compressed_trajectory(&path)?;
        let line = serde_json::json!({
            "session_id": compressed.session_id,
            "model": compressed.model,
            "tags": compressed.tags,
            "outcome": compressed.outcome,
            "sharegpt": genesis_core::compress::to_sharegpt(&compressed),
        });
        lines.push(serde_json::to_string(&line)?);
    }

    Ok(lines.join("\n"))
}

pub(crate) fn run_eval_import_chatml(file: &str, output_dir: &str) -> Result<String, CliError> {
    let contents = std::fs::read_to_string(file)
        .map_err(|e| CliError::Other(format!("failed to read {file}: {e}")))?;
    std::fs::create_dir_all(output_dir)
        .map_err(|e| CliError::Other(format!("failed to create {output_dir}: {e}")))?;

    let mut imported = 0usize;
    for (index, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }

        let entry: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| CliError::Other(format!("invalid JSONL line {}: {e}", index + 1)))?;
        let trajectory = trajectory_from_chatml_entry(&entry, index)?;
        let session_id = sanitize_session_id_for_filename(&trajectory.session_id);
        let output_path = std::path::Path::new(output_dir).join(format!("{session_id}.json"));
        let json = serde_json::to_string_pretty(&trajectory)?;
        std::fs::write(&output_path, json).map_err(|e| {
            CliError::Other(format!("failed to write {}: {e}", output_path.display()))
        })?;
        imported += 1;
    }

    Ok(format!("imported {imported} trajectories into {output_dir}"))
}


pub(crate) enum EvalFileFormat {
    TrajectoryJson,
    ChatmlJsonl,
    SharegptJsonl,
}

pub(crate) fn run_eval_convert(input: &str, output: &str, format: &str) -> Result<String, CliError> {
    let target = format.trim().to_ascii_lowercase();
    let input_format = detect_eval_input_format(input)?;

    let rendered = match (input_format, target.as_str()) {
        (EvalFileFormat::TrajectoryJson, "json") => {
            let raw = std::fs::read_to_string(input)
                .map_err(|e| CliError::Other(format!("failed to read {input}: {e}")))?;
            let trajectory: genesis_core::trajectory::Trajectory = serde_json::from_str(&raw)
                .map_err(|e| CliError::Other(format!("invalid trajectory JSON in {input}: {e}")))?;
            serde_json::to_string_pretty(&trajectory)?
        }
        (EvalFileFormat::TrajectoryJson, "chatml") => {
            let compressed =
                load_training_compressed_trajectory(std::path::Path::new(input))?;
            serde_json::to_string(&serde_json::json!({
                "session_id": compressed.session_id,
                "model": compressed.model,
                "tags": compressed.tags,
                "outcome": compressed.outcome,
                "chatml": genesis_core::compress::to_chatml(&compressed),
            }))?
        }
        (EvalFileFormat::TrajectoryJson, "sharegpt") => {
            let compressed =
                load_training_compressed_trajectory(std::path::Path::new(input))?;
            serde_json::to_string(&serde_json::json!({
                "session_id": compressed.session_id,
                "model": compressed.model,
                "tags": compressed.tags,
                "outcome": compressed.outcome,
                "sharegpt": genesis_core::compress::to_sharegpt(&compressed),
            }))?
        }
        (EvalFileFormat::ChatmlJsonl, "json") => {
            let entry = load_single_jsonl_entry(input)?;
            let trajectory = trajectory_from_chatml_entry(&entry, 0)?;
            serde_json::to_string_pretty(&trajectory)?
        }
        (EvalFileFormat::SharegptJsonl, "json") => {
            let entry = load_single_jsonl_entry(input)?;
            let trajectory = trajectory_from_sharegpt_entry(&entry, 0)?;
            serde_json::to_string_pretty(&trajectory)?
        }
        (_, other) => {
            return Err(CliError::Other(format!(
                "unsupported conversion to '{other}' from input {input}"
            )))
        }
    };

    if let Some(parent) = std::path::Path::new(output).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CliError::Other(format!(
                    "failed to create parent directory for {output}: {e}"
                ))
            })?;
        }
    }
    std::fs::write(output, rendered)
        .map_err(|e| CliError::Other(format!("failed to write {output}: {e}")))?;

    Ok(format!("converted {input} -> {output} ({target})"))
}

pub(crate) fn detect_eval_input_format(input: &str) -> Result<EvalFileFormat, CliError> {
    let path = std::path::Path::new(input);
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("json") => Ok(EvalFileFormat::TrajectoryJson),
        Some("jsonl") => {
            let entry = load_single_jsonl_entry(input)?;
            if entry.get("chatml").is_some() {
                Ok(EvalFileFormat::ChatmlJsonl)
            } else if entry.get("sharegpt").is_some() {
                Ok(EvalFileFormat::SharegptJsonl)
            } else {
                Err(CliError::Other(format!(
                    "cannot detect JSONL format for {input}: expected 'chatml' or 'sharegpt' field"
                )))
            }
        }
        _ => Err(CliError::Other(format!(
            "cannot detect input format for {input}: expected .json or .jsonl"
        ))),
    }
}

pub(crate) fn load_single_jsonl_entry(input: &str) -> Result<serde_json::Value, CliError> {
    let contents = std::fs::read_to_string(input)
        .map_err(|e| CliError::Other(format!("failed to read {input}: {e}")))?;
    let lines = contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();

    if lines.len() != 1 {
        return Err(CliError::Other(format!(
            "expected exactly 1 JSONL record in {input}, found {}",
            lines.len()
        )));
    }

    serde_json::from_str(lines[0])
        .map_err(|e| CliError::Other(format!("invalid JSONL in {input}: {e}")))
}

pub(crate) fn load_training_compressed_trajectory(
    path: &std::path::Path,
) -> Result<genesis_core::compress::CompressedTrajectory, CliError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| CliError::Other(format!("failed to read {}: {e}", path.display())))?;
    let trajectory: genesis_core::trajectory::Trajectory = serde_json::from_str(&raw).map_err(|e| {
        CliError::Other(format!(
            "invalid trajectory JSON in {}: {e}",
            path.display()
        ))
    })?;
    Ok(
        genesis_core::compress::TrajectoryCompressor::default()
            .compress_for_training(&trajectory),
    )
}

pub(crate) fn trajectory_from_chatml_entry(
    entry: &serde_json::Value,
    index: usize,
) -> Result<genesis_core::trajectory::Trajectory, CliError> {
    let chatml = entry
        .get("chatml")
        .and_then(|value| value.as_str())
        .ok_or_else(|| CliError::Other(format!("JSONL line {} missing 'chatml' field", index + 1)))?;

    let session_id = entry
        .get("session_id")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("chatml-import-{}", index + 1));
    let model = entry
        .get("model")
        .and_then(|value| value.as_str())
        .unwrap_or("imported-chatml")
        .to_owned();
    let tags = entry
        .get("tags")
        .and_then(|value| value.as_array())
        .map(|tags| {
            tags.iter()
                .filter_map(|tag| tag.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let outcome = entry
        .get("outcome")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| CliError::Other(format!("invalid outcome on line {}: {e}", index + 1)))?;

    let now = chrono::Utc::now().to_rfc3339();
    let messages = parse_chatml_blocks(chatml)?;
    let steps = messages
        .into_iter()
        .enumerate()
        .map(|(step_index, (role, content))| genesis_core::trajectory::TrajectoryStep {
            step_index,
            timestamp: now.clone(),
            action_type: match role.as_str() {
                "system" => genesis_core::trajectory::ActionType::SystemMessage,
                "user" => genesis_core::trajectory::ActionType::UserMessage,
                "assistant" | "tool" => genesis_core::trajectory::ActionType::AssistantMessage,
                _ => genesis_core::trajectory::ActionType::AssistantMessage,
            },
            content,
            tool_name: None,
            tool_arguments: None,
            tool_result: None,
            tokens: None,
        })
        .collect::<Vec<_>>();

    Ok(genesis_core::trajectory::Trajectory {
        session_id,
        model,
        system_prompt_hash: sha256_hex(chatml),
        started_at: now.clone(),
        completed_at: Some(now),
        steps,
        outcome,
        tags,
    })
}

pub(crate) fn trajectory_from_sharegpt_entry(
    entry: &serde_json::Value,
    index: usize,
) -> Result<genesis_core::trajectory::Trajectory, CliError> {
    let sharegpt = entry
        .get("sharegpt")
        .and_then(|value| value.as_array())
        .ok_or_else(|| {
            CliError::Other(format!("JSONL line {} missing 'sharegpt' field", index + 1))
        })?;

    let session_id = entry
        .get("session_id")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("sharegpt-import-{}", index + 1));
    let model = entry
        .get("model")
        .and_then(|value| value.as_str())
        .unwrap_or("imported-sharegpt")
        .to_owned();
    let tags = entry
        .get("tags")
        .and_then(|value| value.as_array())
        .map(|tags| {
            tags.iter()
                .filter_map(|tag| tag.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let outcome = entry
        .get("outcome")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| CliError::Other(format!("invalid outcome on line {}: {e}", index + 1)))?;

    let now = chrono::Utc::now().to_rfc3339();
    let steps = sharegpt
        .iter()
        .enumerate()
        .map(|(step_index, item)| {
            let from = item
                .get("from")
                .and_then(|value| value.as_str())
                .ok_or_else(|| CliError::Other("ShareGPT item missing 'from'".to_owned()))?;
            let value = item
                .get("value")
                .and_then(|value| value.as_str())
                .ok_or_else(|| CliError::Other("ShareGPT item missing 'value'".to_owned()))?;
            let action_type = match from {
                "human" => genesis_core::trajectory::ActionType::UserMessage,
                "gpt" | "thought" => genesis_core::trajectory::ActionType::AssistantMessage,
                _ => genesis_core::trajectory::ActionType::AssistantMessage,
            };

            Ok(genesis_core::trajectory::TrajectoryStep {
                step_index,
                timestamp: now.clone(),
                action_type,
                content: value.to_owned(),
                tool_name: None,
                tool_arguments: None,
                tool_result: None,
                tokens: None,
            })
        })
        .collect::<Result<Vec<_>, CliError>>()?;

    Ok(genesis_core::trajectory::Trajectory {
        session_id,
        model,
        system_prompt_hash: sha256_hex(&serde_json::to_string(sharegpt).unwrap_or_default()),
        started_at: now.clone(),
        completed_at: Some(now),
        steps,
        outcome,
        tags,
    })
}

pub(crate) fn parse_chatml_blocks(chatml: &str) -> Result<Vec<(String, String)>, CliError> {
    let mut messages = Vec::new();
    let mut rest = chatml;

    while let Some(start_idx) = rest.find("<|im_start|>") {
        rest = &rest[start_idx + "<|im_start|>".len()..];
        let end_idx = rest.find("<|im_end|>").ok_or_else(|| {
            CliError::Other("invalid ChatML: missing <|im_end|> marker".to_owned())
        })?;
        let block = &rest[..end_idx];
        rest = &rest[end_idx + "<|im_end|>".len()..];

        let Some((role, content)) = block.split_once('\n') else {
            return Err(CliError::Other(
                "invalid ChatML: block missing role/content separator".to_owned(),
            ));
        };

        messages.push((role.trim().to_owned(), content.to_owned()));
    }

    if messages.is_empty() {
        return Err(CliError::Other("invalid ChatML: no messages found".to_owned()));
    }

    Ok(messages)
}

pub(crate) fn sanitize_session_id_for_filename(session_id: &str) -> String {
    session_id
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => ch,
            _ => '_',
        })
        .collect()
}

pub(crate) fn run_eval_quality(
    dir: &str,
    recursive: bool,
    min_score: Option<f64>,
    worst_first: bool,
    json: bool,
) -> Result<String, CliError> {
    let files = collect_eval_files(PathBuf::from(dir), recursive)?;

    if files.is_empty() {
        return Ok("No trajectory files found.".to_owned());
    }

    let mut scored: Vec<(String, genesis_core::quality::QualityScore)> = Vec::new();

    for path in &files {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| CliError::Other(format!("failed to read {}: {e}", path.display())))?;
        let trajectory: genesis_core::trajectory::Trajectory =
            serde_json::from_str(&raw).map_err(|e| {
                CliError::Other(format!(
                    "invalid trajectory JSON in {}: {e}",
                    path.display()
                ))
            })?;

        let quality = genesis_core::quality::score(&trajectory);
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_owned();

        if let Some(threshold) = min_score {
            if quality.overall < threshold {
                continue;
            }
        }

        scored.push((name, quality));
    }

    // Sort by overall score
    if worst_first {
        scored.sort_by(|a, b| a.1.overall.partial_cmp(&b.1.overall).unwrap());
    } else {
        scored.sort_by(|a, b| b.1.overall.partial_cmp(&a.1.overall).unwrap());
    }

    if json {
        let entries: Vec<serde_json::Value> = scored
            .iter()
            .map(|(name, q)| {
                serde_json::json!({
                    "file": name,
                    "overall": (q.overall * 100.0).round() / 100.0,
                    "outcome": (q.dimensions.outcome * 100.0).round() / 100.0,
                    "signal_to_noise": (q.dimensions.signal_to_noise * 100.0).round() / 100.0,
                    "tool_diversity": (q.dimensions.tool_diversity * 100.0).round() / 100.0,
                    "depth": (q.dimensions.depth * 100.0).round() / 100.0,
                    "efficiency": (q.dimensions.efficiency * 100.0).round() / 100.0,
                    "completeness": (q.dimensions.completeness * 100.0).round() / 100.0,
                    "issues": q.issues,
                })
            })
            .collect();

        let total = files.len();
        let passed = scored.len();
        let avg_score = if scored.is_empty() {
            0.0
        } else {
            scored.iter().map(|(_, q)| q.overall).sum::<f64>() / scored.len() as f64
        };

        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "total_files": total,
            "scored": passed,
            "filtered_out": total - passed,
            "average_score": (avg_score * 100.0).round() / 100.0,
            "trajectories": entries,
        }))?)
    } else {
        let mut lines = Vec::new();

        let total = files.len();
        let passed = scored.len();
        let avg_score = if scored.is_empty() {
            0.0
        } else {
            scored.iter().map(|(_, q)| q.overall).sum::<f64>() / scored.len() as f64
        };

        lines.push(format!(
            "Quality report: {passed}/{total} trajectories{}",
            if let Some(t) = min_score {
                format!(" (min score: {t:.2})")
            } else {
                String::new()
            }
        ));
        lines.push(format!("Average score: {avg_score:.2}"));
        lines.push(String::new());
        lines.push(format!(
            "{:<40} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6}",
            "FILE", "SCORE", "OUTCM", "S/N", "TOOLS", "DEPTH", "EFFIC", "COMPL"
        ));

        for (name, q) in &scored {
            let truncated_name = if name.len() > 38 {
                format!("{}...", &name[..35])
            } else {
                name.clone()
            };
            lines.push(format!(
                "{:<40} {:>5.2} {:>5.2} {:>5.2} {:>5.2} {:>5.2} {:>5.2} {:>5.2}",
                truncated_name,
                q.overall,
                q.dimensions.outcome,
                q.dimensions.signal_to_noise,
                q.dimensions.tool_diversity,
                q.dimensions.depth,
                q.dimensions.efficiency,
                q.dimensions.completeness,
            ));

            if !q.issues.is_empty() {
                for issue in &q.issues {
                    lines.push(format!("  -> {issue}"));
                }
            }
        }

        Ok(lines.join("\n"))
    }
}

pub(crate) fn run_eval_auto_tag(
    dir: &str,
    recursive: bool,
    dry_run: bool,
    json: bool,
) -> Result<String, CliError> {
    let files = collect_eval_files(PathBuf::from(dir), recursive)?;
    let mut updated = Vec::<(String, Vec<String>)>::new();

    for path in files {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| CliError::Other(format!("failed to read {}: {e}", path.display())))?;
        let mut trajectory: genesis_core::trajectory::Trajectory = serde_json::from_str(&raw)
            .map_err(|e| {
                CliError::Other(format!(
                    "invalid trajectory JSON in {}: {e}",
                    path.display()
                ))
            })?;

        let suggested = genesis_core::tagger::auto_tag(&trajectory);
        let existing = trajectory.tags.iter().cloned().collect::<HashSet<_>>();
        let mut additions = suggested
            .into_iter()
            .filter(|tag| !existing.contains(tag))
            .collect::<Vec<_>>();
        additions.sort();

        if additions.is_empty() {
            continue;
        }

        if !dry_run {
            trajectory.tags.extend(additions.clone());
            trajectory.tags.sort();
            trajectory.tags.dedup();
            let serialized = serde_json::to_string_pretty(&trajectory)?;
            std::fs::write(&path, serialized).map_err(|e| {
                CliError::Other(format!("failed to write {}: {e}", path.display()))
            })?;
        }

        updated.push((path.display().to_string(), additions));
    }

    if json {
        return Ok(serde_json::to_string_pretty(&serde_json::json!({
            "directory": dir,
            "recursive": recursive,
            "dry_run": dry_run,
            "updated": updated.iter().map(|(file, tags)| {
                serde_json::json!({
                    "file": file,
                    "added_tags": tags,
                })
            }).collect::<Vec<_>>(),
            "files_changed": updated.len(),
        }))?);
    }

    if updated.is_empty() {
        return Ok("No new tags to apply.".to_owned());
    }

    let mut lines = Vec::new();
    lines.push("genesis eval auto-tag".to_owned());
    lines.push(format!("directory:    {dir}"));
    lines.push(format!("recursive:    {recursive}"));
    lines.push(format!("dry run:      {dry_run}"));
    lines.push(format!("files changed: {}", updated.len()));
    for (file, tags) in &updated {
        lines.push(format!("{file}: {}", tags.join(", ")));
    }

    Ok(lines.join("\n"))
}

pub(crate) fn run_eval_tag_stats(dir: &str, recursive: bool, json: bool) -> Result<String, CliError> {
    let files = collect_eval_files(PathBuf::from(dir), recursive)?;
    let mut counts = BTreeMap::<String, usize>::new();

    for path in files {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| CliError::Other(format!("failed to read {}: {e}", path.display())))?;
        let trajectory: genesis_core::trajectory::Trajectory = serde_json::from_str(&raw)
            .map_err(|e| {
                CliError::Other(format!(
                    "invalid trajectory JSON in {}: {e}",
                    path.display()
                ))
            })?;
        for tag in trajectory.tags {
            *counts.entry(tag).or_default() += 1;
        }
    }

    let mut tags = counts.into_iter().collect::<Vec<_>>();
    tags.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));

    if json {
        return Ok(serde_json::to_string_pretty(&serde_json::json!({
            "directory": dir,
            "recursive": recursive,
            "tags": tags.iter().map(|(tag, count)| {
                serde_json::json!({
                    "tag": tag,
                    "count": count,
                })
            }).collect::<Vec<_>>(),
        }))?);
    }

    if tags.is_empty() {
        return Ok("No trajectory tags found.".to_owned());
    }

    let mut lines = Vec::new();
    lines.push("genesis eval tag-stats".to_owned());
    lines.push(format!("directory: {dir}"));
    lines.push(format!("recursive: {recursive}"));
    for (tag, count) in &tags {
        lines.push(format!("{tag}: {count}"));
    }
    Ok(lines.join("\n"))
}


pub(crate) struct DeduplicateGroup {
    pub(crate) key: String,
    pub(crate) files: Vec<String>,
}


#[allow(clippy::too_many_arguments)]
pub(crate) fn run_eval_filter(
    dir: &str,
    output: &str,
    recursive: bool,
    model: Option<&str>,
    tag: Option<&str>,
    min_quality: Option<f64>,
    max_quality: Option<f64>,
    success_only: bool,
    failure_only: bool,
    min_steps: Option<usize>,
    max_steps: Option<usize>,
    tool: Option<&str>,
) -> Result<String, CliError> {
    std::fs::create_dir_all(output)
        .map_err(|e| CliError::Other(format!("failed to create {output}: {e}")))?;

    let files = collect_eval_files(PathBuf::from(dir), recursive)?;
    let mut matched = 0usize;
    let mut total = 0usize;

    for path in files {
        total += 1;
        let raw = std::fs::read_to_string(&path).map_err(|e| {
            CliError::Other(format!("failed to read {}: {e}", path.display()))
        })?;
        let traj: genesis_core::trajectory::Trajectory = match serde_json::from_str(&raw) {
            Ok(t) => t,
            Err(_) => continue,
        };

        // Apply filters
        if let Some(m) = model {
            if !traj.model.contains(m) {
                continue;
            }
        }
        if let Some(t) = tag {
            if !traj.tags.iter().any(|tag| tag == t) {
                continue;
            }
        }
        if success_only
            && !matches!(
                traj.outcome,
                Some(genesis_core::trajectory::TrajectoryOutcome::Success)
            )
        {
            continue;
        }
        if failure_only
            && !matches!(
                traj.outcome,
                Some(genesis_core::trajectory::TrajectoryOutcome::Failure { .. })
            )
        {
            continue;
        }
        if let Some(min) = min_steps {
            if traj.steps.len() < min {
                continue;
            }
        }
        if let Some(max) = max_steps {
            if traj.steps.len() > max {
                continue;
            }
        }
        if let Some(tool_name) = tool {
            let has_tool = traj.steps.iter().any(|s| {
                s.tool_name.as_deref() == Some(tool_name)
            });
            if !has_tool {
                continue;
            }
        }
        if min_quality.is_some() || max_quality.is_some() {
            let score = genesis_core::quality::score(&traj).overall;
            if let Some(min) = min_quality {
                if score < min {
                    continue;
                }
            }
            if let Some(max) = max_quality {
                if score > max {
                    continue;
                }
            }
        }

        // Passed all filters — copy to output
        let filename = path.file_name().unwrap().to_string_lossy().to_string();
        let dest = std::path::Path::new(output).join(&filename);
        std::fs::copy(&path, &dest).map_err(|e| {
            CliError::Other(format!(
                "failed to copy {} -> {}: {e}",
                path.display(),
                dest.display()
            ))
        })?;
        matched += 1;
    }

    Ok(format!(
        "filtered {matched}/{total} trajectories into {output}"
    ))
}

pub(crate) fn run_eval_split(
    dir: &str,
    train_dir: &str,
    test_dir: &str,
    ratio: f64,
    seed: Option<u64>,
    recursive: bool,
) -> Result<String, CliError> {
    if !(0.0..=1.0).contains(&ratio) {
        return Err(CliError::Other(format!(
            "ratio must be between 0.0 and 1.0, got {ratio}"
        )));
    }

    std::fs::create_dir_all(train_dir)
        .map_err(|e| CliError::Other(format!("failed to create {train_dir}: {e}")))?;
    std::fs::create_dir_all(test_dir)
        .map_err(|e| CliError::Other(format!("failed to create {test_dir}: {e}")))?;

    let mut files = collect_eval_files(PathBuf::from(dir), recursive)?;
    if files.is_empty() {
        return Ok("no trajectory files found".to_owned());
    }

    // Sort for deterministic ordering, then shuffle with seed
    files.sort();
    use rand::seq::SliceRandom;
    let mut rng = match seed {
        Some(s) => {
            use rand::SeedableRng;
            rand::rngs::StdRng::seed_from_u64(s)
        }
        None => {
            use rand::SeedableRng;
            rand::rngs::StdRng::from_os_rng()
        }
    };
    files.shuffle(&mut rng);

    let split_point = (files.len() as f64 * ratio).round() as usize;
    let (train_files, test_files) = files.split_at(split_point);

    for path in train_files {
        let filename = path.file_name().unwrap().to_string_lossy().to_string();
        std::fs::copy(path, std::path::Path::new(train_dir).join(&filename)).map_err(|e| {
            CliError::Other(format!("failed to copy {}: {e}", path.display()))
        })?;
    }

    for path in test_files {
        let filename = path.file_name().unwrap().to_string_lossy().to_string();
        std::fs::copy(path, std::path::Path::new(test_dir).join(&filename)).map_err(|e| {
            CliError::Other(format!("failed to copy {}: {e}", path.display()))
        })?;
    }

    Ok(format!(
        "split {} trajectories: {} train, {} test (ratio {ratio})",
        files.len(),
        train_files.len(),
        test_files.len()
    ))
}

pub(crate) fn run_eval_sample(
    dir: &str,
    output: &str,
    count: usize,
    seed: Option<u64>,
    recursive: bool,
) -> Result<String, CliError> {
    std::fs::create_dir_all(output)
        .map_err(|e| CliError::Other(format!("failed to create {output}: {e}")))?;

    let mut files = collect_eval_files(PathBuf::from(dir), recursive)?;
    if files.is_empty() {
        return Ok("no trajectory files found".to_owned());
    }

    let actual_count = count.min(files.len());

    files.sort();
    use rand::seq::SliceRandom;
    let mut rng = match seed {
        Some(s) => {
            use rand::SeedableRng;
            rand::rngs::StdRng::seed_from_u64(s)
        }
        None => {
            use rand::SeedableRng;
            rand::rngs::StdRng::from_os_rng()
        }
    };
    files.shuffle(&mut rng);

    for path in &files[..actual_count] {
        let filename = path.file_name().unwrap().to_string_lossy().to_string();
        let dest = std::path::Path::new(output).join(&filename);
        std::fs::copy(path, &dest).map_err(|e| {
            CliError::Other(format!("failed to copy {}: {e}", path.display()))
        })?;
    }

    Ok(format!(
        "sampled {actual_count}/{} trajectories into {output}",
        files.len()
    ))
}

pub(crate) fn run_eval_manifest(
    dir: &str,
    name: &str,
    description: &str,
    save: bool,
    recursive: bool,
    json: bool,
) -> Result<String, CliError> {
    let manifest = genesis_core::dataset::build_manifest(
        name,
        description,
        std::path::Path::new(dir),
        recursive,
    )
    .map_err(|e| CliError::Other(format!("failed to build manifest: {e}")))?;

    if save {
        genesis_core::dataset::save_manifest(&manifest, std::path::Path::new(dir))
            .map_err(|e| CliError::Other(format!("failed to save manifest: {e}")))?;
    }

    if json || save {
        return Ok(serde_json::to_string_pretty(&manifest)?);
    }

    let mut lines = vec![
        format!("dataset: {}", manifest.name),
        format!("files: {}", manifest.file_count),
        format!("total steps: {}", manifest.total_steps),
        format!(
            "avg steps: {:.1}",
            manifest.statistics.avg_steps_per_trajectory
        ),
        format!(
            "step range: {}–{}",
            manifest.statistics.min_steps, manifest.statistics.max_steps
        ),
        format!("models: {}", manifest.models.join(", ")),
    ];

    if let Some(q) = manifest.statistics.avg_quality_score {
        lines.push(format!("avg quality: {q:.3}"));
    }

    if !manifest.statistics.outcome_counts.is_empty() {
        let outcomes: Vec<String> = manifest
            .statistics
            .outcome_counts
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        lines.push(format!("outcomes: {}", outcomes.join(", ")));
    }

    if save {
        lines.push(format!("saved to {dir}/dataset.json"));
    }

    Ok(lines.join("\n"))
}


#[allow(clippy::too_many_arguments)]
pub(crate) fn run_eval_pipeline(
    dir: &str,
    output: &str,
    recursive: bool,
    validate: bool,
    auto_tag: bool,
    min_quality: Option<f64>,
    success_only: bool,
    tag: Option<&str>,
    model: Option<&str>,
    format: Option<&str>,
    build_manifest: bool,
    limit: Option<usize>,
    seed: Option<u64>,
) -> Result<String, CliError> {
    std::fs::create_dir_all(output)
        .map_err(|e| CliError::Other(format!("failed to create {output}: {e}")))?;

    let files = collect_eval_files(PathBuf::from(dir), recursive)?;
    let mut log = Vec::new();
    log.push(format!("pipeline: {} source files", files.len()));

    // Step 1: Load and optionally validate
    let mut trajectories: Vec<(std::path::PathBuf, genesis_core::trajectory::Trajectory)> =
        Vec::new();
    let mut invalid = 0usize;

    for path in &files {
        let raw = match std::fs::read_to_string(path) {
            Ok(r) => r,
            Err(_) => {
                invalid += 1;
                continue;
            }
        };
        match serde_json::from_str::<genesis_core::trajectory::Trajectory>(&raw) {
            Ok(mut traj) => {
                if validate
                    && (traj.session_id.is_empty()
                        || traj.model.is_empty()
                        || traj.steps.is_empty())
                {
                    invalid += 1;
                    continue;
                }

                // Step 2: Auto-tag
                if auto_tag {
                    let new_tags = genesis_core::tagger::auto_tag(&traj);
                    let existing: HashSet<String> = traj.tags.iter().cloned().collect();
                    for t in new_tags {
                        if !existing.contains(&t) {
                            traj.tags.push(t);
                        }
                    }
                    traj.tags.sort();
                }

                trajectories.push((path.clone(), traj));
            }
            Err(_) => {
                invalid += 1;
            }
        }
    }

    if validate {
        log.push(format!("validate: {} valid, {invalid} invalid", trajectories.len()));
    }
    if auto_tag {
        log.push(format!("auto-tag: applied to {} trajectories", trajectories.len()));
    }

    // Step 3: Filter
    let before_filter = trajectories.len();
    trajectories.retain(|(_, traj)| {
        if success_only
            && !matches!(
                traj.outcome,
                Some(genesis_core::trajectory::TrajectoryOutcome::Success)
            )
        {
            return false;
        }
        if let Some(t) = tag {
            if !traj.tags.iter().any(|tag| tag == t) {
                return false;
            }
        }
        if let Some(m) = model {
            if !traj.model.contains(m) {
                return false;
            }
        }
        if let Some(min_q) = min_quality {
            let score = genesis_core::quality::score(traj).overall;
            if score < min_q {
                return false;
            }
        }
        true
    });

    if trajectories.len() != before_filter {
        log.push(format!(
            "filter: {} → {} trajectories",
            before_filter,
            trajectories.len()
        ));
    }

    // Step 4: Sample/limit
    if let Some(max) = limit {
        if trajectories.len() > max {
            use rand::seq::SliceRandom;
            let mut rng = match seed {
                Some(s) => {
                    use rand::SeedableRng;
                    rand::rngs::StdRng::seed_from_u64(s)
                }
                None => {
                    use rand::SeedableRng;
                    rand::rngs::StdRng::from_os_rng()
                }
            };
            trajectories.shuffle(&mut rng);
            trajectories.truncate(max);
            log.push(format!("sample: limited to {max} trajectories"));
        }
    }

    // Step 5: Write output
    let output_format = format.unwrap_or("json");
    match output_format {
        "json" => {
            for (_, traj) in &trajectories {
                let filename = format!(
                    "{}.json",
                    sanitize_session_id_for_filename(&traj.session_id)
                );
                let dest = std::path::Path::new(output).join(&filename);
                let json = serde_json::to_string_pretty(traj)?;
                std::fs::write(&dest, json).map_err(|e| {
                    CliError::Other(format!("failed to write {}: {e}", dest.display()))
                })?;
            }
            log.push(format!("output: {} JSON files in {output}", trajectories.len()));
        }
        "chatml" | "sharegpt" => {
            let output_file = std::path::Path::new(output).join(format!("dataset.{output_format}.jsonl"));
            let mut lines = Vec::new();
            for (original_path, _) in &trajectories {
                let compressed = load_training_compressed_trajectory(original_path)?;
                let data = if output_format == "chatml" {
                    serde_json::json!({
                        "session_id": compressed.session_id,
                        "model": compressed.model,
                        "tags": compressed.tags,
                        "outcome": compressed.outcome,
                        "chatml": genesis_core::compress::to_chatml(&compressed),
                    })
                } else {
                    serde_json::json!({
                        "session_id": compressed.session_id,
                        "model": compressed.model,
                        "tags": compressed.tags,
                        "outcome": compressed.outcome,
                        "sharegpt": genesis_core::compress::to_sharegpt(&compressed),
                    })
                };
                lines.push(serde_json::to_string(&data)?);
            }
            std::fs::write(&output_file, lines.join("\n"))
                .map_err(|e| CliError::Other(format!("failed to write {}: {e}", output_file.display())))?;
            log.push(format!(
                "output: {} records as {output_format} JSONL",
                trajectories.len()
            ));
        }
        other => {
            return Err(CliError::Other(format!(
                "unknown format '{other}', expected json, chatml, or sharegpt"
            )));
        }
    }

    // Step 6: Build manifest
    if build_manifest && output_format == "json" {
        let manifest = genesis_core::dataset::build_manifest(
            "pipeline-output",
            &log.join("; "),
            std::path::Path::new(output),
            false,
        )
        .map_err(|e| CliError::Other(format!("failed to build manifest: {e}")))?;
        genesis_core::dataset::save_manifest(&manifest, std::path::Path::new(output))
            .map_err(|e| CliError::Other(format!("failed to save manifest: {e}")))?;
        log.push("manifest: saved dataset.json".to_owned());
    }

    Ok(log.join("\n"))
}

pub(crate) fn run_eval_validate(
    dir: &str,
    recursive: bool,
    remove: bool,
) -> Result<String, CliError> {
    let files = collect_eval_files(PathBuf::from(dir), recursive)?;
    let mut valid = 0usize;
    let mut invalid = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for path in &files {
        let raw = match std::fs::read_to_string(path) {
            Ok(r) => r,
            Err(e) => {
                errors.push(format!("{}: read error: {e}", path.display()));
                invalid += 1;
                continue;
            }
        };

        match serde_json::from_str::<genesis_core::trajectory::Trajectory>(&raw) {
            Ok(traj) => {
                // Validate required fields
                let mut issues = Vec::new();
                if traj.session_id.is_empty() {
                    issues.push("empty session_id");
                }
                if traj.model.is_empty() {
                    issues.push("empty model");
                }
                if traj.started_at.is_empty() {
                    issues.push("empty started_at");
                }
                if traj.steps.is_empty() {
                    issues.push("no steps");
                }

                if issues.is_empty() {
                    valid += 1;
                } else {
                    errors.push(format!(
                        "{}: {}",
                        path.display(),
                        issues.join(", ")
                    ));
                    invalid += 1;
                    if remove {
                        let _ = std::fs::remove_file(path);
                    }
                }
            }
            Err(e) => {
                errors.push(format!("{}: invalid JSON: {e}", path.display()));
                invalid += 1;
                if remove {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    }

    let mut lines = vec![format!(
        "validated {} files: {valid} valid, {invalid} invalid",
        files.len()
    )];
    for err in errors.iter().take(20) {
        lines.push(format!("  {err}"));
    }
    if errors.len() > 20 {
        lines.push(format!("  ... and {} more", errors.len() - 20));
    }
    if remove && invalid > 0 {
        lines.push(format!("removed {invalid} invalid files"));
    }
    Ok(lines.join("\n"))
}

pub(crate) fn run_eval_deduplicate(
    dir: &str,
    recursive: bool,
    remove: bool,
    json: bool,
) -> Result<String, CliError> {
    let files = collect_eval_files(PathBuf::from(dir), recursive)?;
    let mut grouped = BTreeMap::<String, Vec<PathBuf>>::new();

    for path in files {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| CliError::Other(format!("failed to read {}: {e}", path.display())))?;
        let trajectory: genesis_core::trajectory::Trajectory = serde_json::from_str(&raw)
            .map_err(|e| {
                CliError::Other(format!(
                    "invalid trajectory JSON in {}: {e}",
                    path.display()
                ))
            })?;
        grouped
            .entry(deduplicate_key(&trajectory))
            .or_default()
            .push(path);
    }

    let mut groups = grouped
        .into_iter()
        .filter_map(|(key, mut files)| {
            if files.len() < 2 {
                return None;
            }
            files.sort();
            Some((key, files))
        })
        .collect::<Vec<_>>();
    groups.sort_by(|left, right| left.0.cmp(&right.0));

    let mut removed_files = 0usize;
    if remove {
        for (_, files) in &groups {
            for file in files.iter().skip(1) {
                std::fs::remove_file(file).map_err(|e| {
                    CliError::Other(format!("failed to remove {}: {e}", file.display()))
                })?;
                removed_files += 1;
            }
        }
    }

    let groups = groups
        .into_iter()
        .map(|(key, files)| DeduplicateGroup {
            key,
            files: files
                .into_iter()
                .map(|path| path.display().to_string())
                .collect(),
        })
        .collect::<Vec<_>>();

    if json {
        return Ok(serde_json::to_string_pretty(&serde_json::json!({
            "directory": dir,
            "recursive": recursive,
            "remove": remove,
            "duplicate_groups": groups.len(),
            "removed_files": removed_files,
            "groups": groups.iter().map(|group| {
                serde_json::json!({
                    "key": group.key,
                    "files": group.files,
                })
            }).collect::<Vec<_>>(),
        }))?);
    }

    if groups.is_empty() {
        return Ok("No duplicate trajectories found.".to_owned());
    }

    let mut lines = Vec::new();
    lines.push("genesis eval deduplicate".to_owned());
    lines.push(format!("directory:        {dir}"));
    lines.push(format!("recursive:        {recursive}"));
    lines.push(format!("remove:           {remove}"));
    lines.push(format!("duplicate groups: {}", groups.len()));
    lines.push(format!("removed files:    {removed_files}"));
    for group in &groups {
        lines.push(format!("group: {}", group.key));
        for file in &group.files {
            lines.push(format!("  - {file}"));
        }
    }

    Ok(lines.join("\n"))
}

pub(crate) fn deduplicate_key(trajectory: &genesis_core::trajectory::Trajectory) -> String {
    let first_user_message = trajectory
        .steps
        .iter()
        .find(|step| step.action_type == genesis_core::trajectory::ActionType::UserMessage)
        .map(|step| step.content.trim())
        .unwrap_or("");
    format!("{}::{}", trajectory.system_prompt_hash, first_user_message)
}
