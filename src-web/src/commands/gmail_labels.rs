use pebble_core::FolderRole;

#[derive(Debug, PartialEq, Eq)]
pub struct GmailLabelDelta {
    pub add_labels: Vec<String>,
    pub remove_labels: Vec<String>,
}

pub fn gmail_move_label_delta(
    source_remote_id: Option<&str>,
    target_remote_id: &str,
    target_role: Option<FolderRole>,
) -> GmailLabelDelta {
    if target_role == Some(FolderRole::Spam) {
        return GmailLabelDelta {
            add_labels: vec!["SPAM".to_string()],
            remove_labels: vec!["INBOX".to_string()],
        };
    }

    if target_role == Some(FolderRole::Archive) {
        return GmailLabelDelta {
            add_labels: vec![],
            remove_labels: vec!["INBOX".to_string()],
        };
    }

    let target = valid_gmail_label(target_remote_id);
    let source = source_remote_id.and_then(valid_gmail_label);

    let mut add_labels = Vec::new();
    if let Some(label) = target {
        push_unique(&mut add_labels, label);
    }

    let mut remove_labels = Vec::new();
    match source {
        Some(label) if Some(label) != target => push_unique(&mut remove_labels, label),
        Some(_) => {}
        None => push_unique(&mut remove_labels, "INBOX"),
    }

    remove_labels.retain(|label| !add_labels.contains(label));

    GmailLabelDelta {
        add_labels,
        remove_labels,
    }
}

fn valid_gmail_label(label: &str) -> Option<&str> {
    let trimmed = label.trim();
    if trimmed.is_empty() || trimmed.starts_with("__local_") {
        None
    } else {
        Some(trimmed)
    }
}

fn push_unique(labels: &mut Vec<String>, label: &str) {
    if !labels.iter().any(|existing| existing == label) {
        labels.push(label.to_string());
    }
}
