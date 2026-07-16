use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkDraft {
    pub message_id: String,
    pub session_id: String,
    pub platform: String,
    pub chunk_index: i64,
    pub role: String,
    pub text: String,
    pub content_hash: String,
}

const TARGET_CHARS: usize = 700;
const OVERLAP_CHARS: usize = 80;

pub fn chunk_message(
    platform: &str,
    title: &str,
    message_id: &str,
    session_id: &str,
    role: &str,
    content: &str,
) -> Vec<ChunkDraft> {
    let body = content.trim();
    if body.is_empty() {
        return Vec::new();
    }
    let prefix = format!("[{platform}] {title}\n{role}: ");
    let parts = split_text(body, TARGET_CHARS, OVERLAP_CHARS);
    parts
        .into_iter()
        .enumerate()
        .map(|(index, part)| {
            let text = format!("{prefix}{part}");
            let content_hash = hash_text(&text);
            ChunkDraft {
                message_id: message_id.to_owned(),
                session_id: session_id.to_owned(),
                platform: platform.to_owned(),
                chunk_index: index as i64,
                role: role.to_owned(),
                text,
                content_hash,
            }
        })
        .collect()
}

fn split_text(text: &str, target: usize, overlap: usize) -> Vec<String> {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() <= target {
        return vec![text.to_owned()];
    }
    let mut parts = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let end = (start + target).min(chars.len());
        parts.push(chars[start..end].iter().collect());
        if end >= chars.len() {
            break;
        }
        start = end.saturating_sub(overlap);
        if start == 0 {
            start = end;
        }
    }
    parts
}

pub fn hash_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_message_is_single_chunk() {
        let chunks = chunk_message("deepseek", "title", "m1", "s1", "user", "hello world");
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].text.contains("hello world"));
        assert!(!chunks[0].content_hash.is_empty());
    }

    #[test]
    fn long_message_is_split_with_overlap() {
        let body = "字".repeat(1600);
        let chunks = chunk_message("deepseek", "title", "m1", "s1", "assistant", &body);
        assert!(chunks.len() >= 2);
        assert_eq!(chunks[0].chunk_index, 0);
        assert_eq!(chunks[1].chunk_index, 1);
    }
}
