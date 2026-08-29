use sha2::{Digest, Sha256};
use unicode_segmentation::UnicodeSegmentation;

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
/// Windows smaller than this drop the overlap: with a tiny body budget
/// (long session prefix) the overlap would halve the effective step and
/// roughly double the chunk count without adding retrieval value.
const MIN_TARGET_FOR_OVERLAP: usize = 200;
/// Sentence / newline boundaries preferred when cutting windows.
const SENTENCE_BOUNDARIES: [char; 10] = ['.', '!', '?', ';', '。', '！', '？', '；', '\n', '\r'];

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
    let truncated_title = if title.graphemes(true).count() > 100 {
        title.graphemes(true).take(100).collect::<String>()
    } else {
        title.to_owned()
    };
    let prefix = format!("[{platform}] {truncated_title}\n{role}: ");
    let prefix_len = prefix.graphemes(true).count();
    let target_body = TARGET_CHARS.saturating_sub(prefix_len).max(100);
    let overlap_body = overlap_for_target(target_body);
    let parts = split_text(body, target_body, overlap_body);
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

fn overlap_for_target(target: usize) -> usize {
    if target < MIN_TARGET_FOR_OVERLAP {
        0
    } else {
        OVERLAP_CHARS.min(target / 2)
    }
}

fn split_text(text: &str, target: usize, overlap: usize) -> Vec<String> {
    let graphemes = text.graphemes(true).collect::<Vec<_>>();
    if graphemes.len() <= target {
        return vec![text.to_owned()];
    }
    let overlap = overlap.min(target / 2);
    // Every window must advance by at least this much so the loop can neither
    // stall nor produce duplicate chunks.
    let min_step = (target / 3).max(1);
    let mut parts = Vec::new();
    let mut start = 0usize;
    while start < graphemes.len() {
        let remaining = graphemes.len() - start;
        if remaining <= target {
            parts.push(graphemes[start..].concat());
            break;
        }
        let window_end = start + target;
        // Prefer cutting at the last sentence/newline boundary inside the
        // window when that still leaves a reasonably sized chunk.
        let end = sentence_cut(&graphemes[start..window_end], min_step)
            .map(|cut| start + cut)
            .unwrap_or(window_end);
        parts.push(graphemes[start..end].concat());
        // A tail that fits inside the overlap would be a mostly duplicated
        // final chunk; emit it whole instead.
        let tail = graphemes.len() - end;
        if tail <= overlap {
            parts.push(graphemes[end..].concat());
            break;
        }
        let mut next = end.saturating_sub(overlap);
        if next <= start {
            next = (start + min_step).min(end);
        }
        start = next.max(start + 1);
    }
    parts
}

/// Returns the length (in graphemes) of the longest prefix ending at a
/// sentence/newline boundary, provided it is at least `min_len`; `None` when
/// the window has no usable boundary.
fn sentence_cut(window: &[&str], min_len: usize) -> Option<usize> {
    let min_len = min_len.max(1);
    for (index, grapheme) in window.iter().enumerate().rev() {
        if index + 1 < min_len {
            break;
        }
        let Some(first) = grapheme.chars().next() else {
            continue;
        };
        if SENTENCE_BOUNDARIES.contains(&first) {
            return Some(index + 1);
        }
    }
    None
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
        for chunk in &chunks {
            assert!(chunk.text.graphemes(true).count() <= TARGET_CHARS);
        }
    }

    #[test]
    fn complex_unicode_grapheme_clusters_preserved() {
        // Multi-codepoint emoji sequence (family emoji with zero-width joiners)
        let family_emoji = "👩‍👩‍👦‍👦";
        let body = family_emoji.repeat(100);
        let chunks = chunk_message("deepseek", "测试标题", "m1", "s1", "user", &body);
        assert!(!chunks.is_empty());
        for chunk in &chunks {
            assert!(chunk.text.contains(family_emoji));
            assert!(chunk.text.graphemes(true).count() <= TARGET_CHARS);
        }
    }

    #[test]
    fn split_text_prefers_sentence_boundaries() {
        let body = "这是第一句话。".repeat(30);
        let parts = split_text(&body, 50, 10);
        assert!(parts.len() >= 2, "expected multiple chunks");
        for part in &parts {
            assert!(
                part.ends_with('。'),
                "window cuts should land on sentence boundaries: {part}"
            );
        }
    }

    #[test]
    fn split_text_always_advances_and_never_duplicates() {
        // Unique 4-digit markers make every window distinct, so equal chunks
        // can only come from a stalled step. No sentence boundaries here.
        let body: String = (0..250).map(|index| format!("{index:04}")).collect();
        assert_eq!(body.chars().count(), 1000);
        let parts = split_text(&body, 100, 50);
        assert!(
            (10..=20).contains(&parts.len()),
            "window step must stay >= target/2, got {} chunks",
            parts.len()
        );
        for part in &parts {
            assert!(!part.is_empty());
            assert!(part.chars().count() <= 100);
        }
        for pair in parts.windows(2) {
            assert_ne!(pair[0], pair[1], "consecutive chunks must differ");
        }
        assert!(parts[0].starts_with("0000"));
        assert!(parts.last().unwrap().ends_with("0249"));
    }

    #[test]
    fn small_window_drops_overlap_to_avoid_chunk_inflation() {
        // The body budget clamped to the 100-char floor used to keep a
        // 50-char overlap, halving the step and roughly doubling the chunk
        // count. Tiny windows must drop the overlap entirely.
        assert_eq!(overlap_for_target(100), 0);
        assert_eq!(overlap_for_target(199), 0);
        assert_eq!(overlap_for_target(200), OVERLAP_CHARS.min(100));
        assert_eq!(overlap_for_target(672), OVERLAP_CHARS);
        let parts = split_text(&"字".repeat(800), 100, overlap_for_target(100));
        assert_eq!(parts.len(), 8, "step must equal the window without overlap");

        // End to end: a long platform name pushes the body budget down to the
        // 100-char floor (prefix is platform + title + role + 6 fixed chars).
        let chunks = chunk_message(
            &"p".repeat(588),
            "标题",
            "m1",
            "s1",
            "user",
            &"字".repeat(800),
        );
        assert!(
            chunks.len() <= 9,
            "overlap must be dropped for tiny windows, got {} chunks",
            chunks.len()
        );
        assert!(chunks.len() >= 8);
    }
}
