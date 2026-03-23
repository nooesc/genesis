use std::path::PathBuf;

use genesis_config::load;
use genesis_storage::PairingStore;

use crate::{CliError, PairingCommand};

pub(crate) async fn run_pairing(
    config_path: Option<PathBuf>,
    command: PairingCommand,
    json: bool,
) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;
    let store = PairingStore::new(&loaded.config.storage.database_path);

    match command {
        PairingCommand::List { platform } => {
            let users = store
                .list_approved(platform.as_deref())
                .map_err(|e| CliError::Other(format!("storage error: {e}")))?;

            if json {
                return Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "approved": users,
                    "count": users.len(),
                }))
                .unwrap());
            }

            if users.is_empty() {
                return Ok("No approved users.".to_owned());
            }

            let mut lines = vec![format!(
                "{:<12} {:<20} {:<20} {}",
                "PLATFORM", "USER_ID", "NAME", "APPROVED_AT"
            )];
            for u in &users {
                lines.push(format!(
                    "{:<12} {:<20} {:<20} {}",
                    u.platform, u.user_id, u.user_name, u.approved_at
                ));
            }
            lines.push(format!("\n{} approved user(s)", users.len()));
            Ok(lines.join("\n"))
        }
        PairingCommand::Pending { platform } => {
            let pending = store
                .list_pending(platform.as_deref())
                .map_err(|e| CliError::Other(format!("storage error: {e}")))?;

            if json {
                return Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "pending": pending,
                    "count": pending.len(),
                }))
                .unwrap());
            }

            if pending.is_empty() {
                return Ok("No pending pairing requests.".to_owned());
            }

            let mut lines = vec![format!(
                "{:<12} {:<10} {:<20} {:<20} {}",
                "PLATFORM", "CODE", "USER_ID", "NAME", "CREATED_AT"
            )];
            for p in &pending {
                lines.push(format!(
                    "{:<12} {:<10} {:<20} {:<20} {}",
                    p.platform, p.code, p.user_id, p.user_name, p.created_at
                ));
            }
            lines.push(format!("\n{} pending request(s)", pending.len()));
            Ok(lines.join("\n"))
        }
        PairingCommand::Approve { platform, code } => {
            let result = store
                .approve_code(&platform, &code)
                .map_err(|e| CliError::Other(format!("storage error: {e}")))?;

            match result {
                Some(user) => {
                    if json {
                        Ok(serde_json::to_string_pretty(&serde_json::json!({
                            "approved": true,
                            "user": user,
                        }))
                        .unwrap())
                    } else {
                        Ok(format!(
                            "Approved {} ({}) on {}",
                            user.user_name, user.user_id, user.platform
                        ))
                    }
                }
                None => Err(CliError::Other(
                    "Invalid or expired pairing code.".to_owned(),
                )),
            }
        }
        PairingCommand::Revoke { platform, user_id } => {
            let revoked = store
                .revoke(&platform, &user_id)
                .map_err(|e| CliError::Other(format!("storage error: {e}")))?;

            if json {
                return Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "revoked": revoked,
                    "platform": platform,
                    "user_id": user_id,
                }))
                .unwrap());
            }

            if revoked {
                Ok(format!("Revoked access for {} on {}", user_id, platform))
            } else {
                Err(CliError::Other(format!(
                    "No approved user '{}' on platform '{}'",
                    user_id, platform
                )))
            }
        }
        PairingCommand::ClearPending { platform } => {
            let cleared = store
                .clear_pending(platform.as_deref())
                .map_err(|e| CliError::Other(format!("storage error: {e}")))?;

            if json {
                return Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "cleared": cleared,
                    "platform": platform,
                }))
                .unwrap());
            }

            match platform {
                Some(p) => Ok(format!("Cleared {} pending code(s) for {}", cleared, p)),
                None => Ok(format!(
                    "Cleared {} pending code(s) across all platforms",
                    cleared
                )),
            }
        }
    }
}
