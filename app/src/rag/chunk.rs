//! Text chunking for the RAG indexer pipeline.
//!
//! Splits text into overlapping windows suitable for embedding, respecting
//! paragraph boundaries where possible.
//!
//! Design (TDD §7.2, §11.7):
//! - Default chunk size: 512 chars. Default overlap: 64 chars.
//! - Paragraph-aware: split on double newlines first; if a paragraph exceeds
//!   `chunk_size`, slide a window through it with `overlap` chars of context.
//! - Command blocks: the command itself is prepended to each output chunk so
//!   every chunk is self-contained. See [`chunk_command_block`].
//! - Conversations: messages are indexed whole if ≤ 512 chars, chunked otherwise.

/// Configuration for the chunker.
#[derive(Clone, Debug)]
pub struct ChunkConfig {
    /// Maximum characters per chunk.
    pub chunk_size: usize,
    /// Overlap between consecutive chunks from the same paragraph.
    pub overlap: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            chunk_size: 512,
            overlap: 64,
        }
    }
}

/// A single text chunk ready for embedding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub text: String,
    /// Index within the original document (0-based). Monotonically increasing.
    pub chunk_idx: usize,
}

/// Split `text` into overlapping chunks respecting paragraph boundaries.
///
/// The algorithm:
/// 1. Split on `\n\n` (double newline) to get paragraphs.
/// 2. Paragraphs that fit within `config.chunk_size` are emitted as-is.
/// 3. Paragraphs that exceed `chunk_size` are slid through with a window of
///    size `chunk_size` and step `chunk_size - overlap`.
pub fn chunk_text(text: &str, config: &ChunkConfig) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut idx = 0usize;

    let paragraphs: Vec<&str> = text.split("\n\n").collect();
    let mut current = String::new();

    for para in &paragraphs {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }

        // Would adding this paragraph overflow the current chunk?
        let would_exceed = if current.is_empty() {
            para.len() > config.chunk_size
        } else {
            current.len() + 1 + para.len() > config.chunk_size
        };

        if would_exceed {
            // Flush the current buffer first.
            if !current.is_empty() {
                chunks.push(Chunk {
                    text: current.trim().to_owned(),
                    chunk_idx: idx,
                });
                idx += 1;
                current.clear();
            }
            // If the paragraph itself is too big, slide a window through it.
            if para.len() > config.chunk_size {
                for window_chunk in slide_window(para, config) {
                    chunks.push(Chunk {
                        text: window_chunk,
                        chunk_idx: idx,
                    });
                    idx += 1;
                }
            } else {
                current.push_str(para);
            }
        } else {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(para);
        }
    }

    // Flush remainder.
    if !current.trim().is_empty() {
        chunks.push(Chunk {
            text: current.trim().to_owned(),
            chunk_idx: idx,
        });
    }

    chunks
}

/// Slide a fixed-size window through `text`, yielding chunks of at most
/// `config.chunk_size` chars with `config.overlap` chars of context carry-over.
///
/// Splits at character boundaries, preferring whitespace breaks within the
/// last 32 chars of the window to avoid splitting mid-word.
fn slide_window(text: &str, config: &ChunkConfig) -> Vec<String> {
    let mut chunks = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let total = chars.len();
    let step = config.chunk_size.saturating_sub(config.overlap).max(1);
    let mut start = 0;

    while start < total {
        let end = (start + config.chunk_size).min(total);
        // Prefer breaking at a whitespace boundary near the chunk end.
        let break_at = if end < total {
            find_break(&chars, start, end)
        } else {
            end
        };
        let chunk: String = chars[start..break_at].iter().collect();
        chunks.push(chunk.trim().to_owned());
        // Advance by step, but if we found an early break, still advance by at
        // least one char to prevent infinite loops.
        start += step.min(break_at - start).max(1);
    }

    chunks
}

/// Find the best character index to break at within `[start, end)`.
/// Prefers the last whitespace within the final 32 chars of the window.
fn find_break(chars: &[char], start: usize, end: usize) -> usize {
    let search_from = end.saturating_sub(32).max(start + 1);
    for i in (search_from..end).rev() {
        if chars[i].is_whitespace() {
            return i;
        }
    }
    end
}

// ── Specialised helpers ───────────────────────────────────────────────────────

/// Chunk a command+output pair for the command block indexer.
///
/// Per TDD §11.7: the command is prepended to every output chunk so each
/// chunk is self-contained for retrieval. The command itself is never split.
pub fn chunk_command_block(
    command: &str,
    output: &str,
    config: &ChunkConfig,
) -> Vec<Chunk> {
    // If the whole thing fits, return a single chunk.
    let combined = format!("{command}\n{output}");
    if combined.len() <= config.chunk_size {
        return vec![Chunk {
            text: combined,
            chunk_idx: 0,
        }];
    }

    // Chunk the output only; prepend command to each chunk.
    let output_chunks = if output.is_empty() {
        vec![Chunk { text: String::new(), chunk_idx: 0 }]
    } else {
        chunk_text(output, config)
    };

    output_chunks
        .into_iter()
        .enumerate()
        .map(|(i, c)| Chunk {
            text: format!("{command}\n{}", c.text),
            chunk_idx: i,
        })
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ChunkConfig {
        ChunkConfig::default()
    }

    fn short_cfg() -> ChunkConfig {
        ChunkConfig {
            chunk_size: 50,
            overlap: 10,
        }
    }

    #[test]
    fn short_text_returns_single_chunk() {
        let chunks = chunk_text("hello world", &cfg());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "hello world");
        assert_eq!(chunks[0].chunk_idx, 0);
    }

    #[test]
    fn empty_text_returns_no_chunks() {
        let chunks = chunk_text("", &cfg());
        assert!(chunks.is_empty());
    }

    #[test]
    fn whitespace_only_text_returns_no_chunks() {
        let chunks = chunk_text("   \n\n   ", &cfg());
        assert!(chunks.is_empty());
    }

    #[test]
    fn two_short_paragraphs_merge_into_one_chunk() {
        let text = "First paragraph.\n\nSecond paragraph.";
        let chunks = chunk_text(text, &cfg());
        // Both paragraphs together are well under 512 chars; should be 1 chunk.
        assert_eq!(chunks.len(), 1, "expected merge into single chunk, got: {chunks:?}");
        assert!(chunks[0].text.contains("First paragraph"));
        assert!(chunks[0].text.contains("Second paragraph"));
    }

    #[test]
    fn long_paragraph_is_split_into_multiple_chunks() {
        // Build a paragraph longer than the short chunk size (50).
        let long = "word ".repeat(20); // 100 chars
        let chunks = chunk_text(&long, &short_cfg());
        assert!(chunks.len() >= 2, "expected multiple chunks, got: {chunks:?}");
    }

    #[test]
    fn chunk_indices_are_monotonically_increasing() {
        let long = "a".repeat(600);
        let chunks = chunk_text(&long, &cfg());
        assert!(chunks.len() >= 2);
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.chunk_idx, i, "chunk_idx must equal position");
        }
    }

    #[test]
    fn chunks_within_size_limit() {
        let long = "word ".repeat(200); // ~1000 chars
        let chunks = chunk_text(&long, &cfg());
        for c in &chunks {
            assert!(
                c.text.len() <= cfg().chunk_size + cfg().overlap,
                "chunk exceeds size limit: len={}, text={:?}",
                c.text.len(),
                &c.text[..c.text.len().min(50)]
            );
        }
    }

    #[test]
    fn paragraph_break_between_paragraphs_is_respected() {
        // Each paragraph is under 50 chars; combined they'd exceed if counted
        // together with the short_cfg, but we want to test the boundary:
        // "123456789012345678901234567890" (30 chars) × 2 = 60 > 50
        let p1 = "a".repeat(30);
        let p2 = "b".repeat(30);
        let text = format!("{p1}\n\n{p2}");
        let chunks = chunk_text(&text, &short_cfg());
        // Since each paragraph alone is < 50 but both together exceed 50,
        // we expect 2 chunks (each paragraph as its own chunk).
        assert_eq!(chunks.len(), 2, "expected 2 chunks, got: {chunks:?}");
    }

    // ── Command block chunking ─────────────────────────────────────────────

    #[test]
    fn command_block_short_output_is_single_chunk() {
        let chunks = chunk_command_block("ls -la", "total 0\ndrwxr-xr-x", &cfg());
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].text.starts_with("ls -la"));
        assert!(chunks[0].text.contains("total 0"));
    }

    #[test]
    fn command_block_long_output_each_chunk_has_command_prepended() {
        let output = "line\n".repeat(200); // ~1000 chars
        let chunks = chunk_command_block("grep -r pattern .", &output, &cfg());
        assert!(chunks.len() >= 2, "expected multiple chunks for long output");
        for c in &chunks {
            assert!(
                c.text.starts_with("grep -r pattern ."),
                "every chunk must start with the command, got: {:?}",
                &c.text[..c.text.len().min(40)]
            );
        }
    }

    #[test]
    fn command_block_empty_output_is_single_chunk() {
        let chunks = chunk_command_block("echo hello", "", &cfg());
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].text.contains("echo hello"));
    }
}
